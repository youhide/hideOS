//! LUKS2, as far as opening a volume needs: the header, its JSON metadata,
//! a keyslot's key derivation, anti-forensic merge and digest check — the
//! steps `cryptsetup open` takes before it hands the kernel a dm-crypt
//! table. Writing headers is cryptsetup's, at install time; this only reads.
//!
//! The format is the LUKS2 on-disk specification (Broz, 2018-2022); the
//! names below are its names.

use std::os::unix::fs::FileExt;

use aes::Aes256;
use aes::cipher::KeyInit;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use sha2::{Digest, Sha256, Sha512};
use std::collections::BTreeMap;
use xts_mode::{Xts128, get_tweak_default};
use zeroize::Zeroizing;

use crate::Error;

const MAGIC: &[u8; 6] = b"LUKS\xba\xbe";
/// The secondary header's magic: the primary's, reversed.
const MAGIC_SECONDARY: &[u8; 6] = b"SKUL\xba\xbe";
const BINARY_HEADER: usize = 4096;
/// The header's checksum field: 64 bytes at this offset, zeroed while the
/// header is hashed.
const CSUM_OFFSET: usize = 448;
const CSUM_LEN: usize = 64;
/// Where the secondary header may start, after the primary at 0: the sizes
/// the specification allows for the primary.
const SECONDARY_OFFSETS: &[u64] = &[
    0x4000, 0x8000, 0x10000, 0x20000, 0x40000, 0x80000, 0x100000, 0x200000, 0x400000,
];
/// The largest JSON area the specification allows.
const MAX_HEADER: u64 = 0x400000;
/// Keyslot areas are encrypted in 512-byte sectors.
const AREA_SECTOR: usize = 512;

/// A key that decrypts the volume, or a keyslot. Cleared when dropped.
pub type Key = Zeroizing<Vec<u8>>;

/// What the header says about a volume.
#[derive(Debug, Clone)]
pub struct Header {
    pub uuid: String,
    pub label: String,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Metadata {
    pub keyslots: BTreeMap<String, Keyslot>,
    #[serde(default)]
    pub tokens: BTreeMap<String, serde_json::Value>,
    pub segments: BTreeMap<String, Segment>,
    pub digests: BTreeMap<String, DigestEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Keyslot {
    #[serde(rename = "type")]
    pub kind: String,
    pub key_size: usize,
    pub af: Af,
    pub area: Area,
    pub kdf: Kdf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Af {
    #[serde(rename = "type")]
    pub kind: String,
    pub stripes: usize,
    pub hash: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Area {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(deserialize_with = "number_string")]
    pub offset: u64,
    #[serde(deserialize_with = "number_string")]
    pub size: u64,
    pub encryption: String,
    pub key_size: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum Kdf {
    #[serde(rename = "argon2id")]
    Argon2id {
        time: u32,
        memory: u32,
        cpus: u32,
        salt: String,
    },
    #[serde(rename = "argon2i")]
    Argon2i {
        time: u32,
        memory: u32,
        cpus: u32,
        salt: String,
    },
    #[serde(rename = "pbkdf2")]
    Pbkdf2 {
        hash: String,
        iterations: u32,
        salt: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct Segment {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(deserialize_with = "number_string")]
    pub offset: u64,
    pub size: String,
    #[serde(deserialize_with = "number_string")]
    pub iv_tweak: u64,
    pub encryption: String,
    pub sector_size: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DigestEntry {
    #[serde(rename = "type")]
    pub kind: String,
    pub keyslots: Vec<String>,
    pub segments: Vec<String>,
    pub hash: String,
    pub iterations: u32,
    pub salt: String,
    pub digest: String,
}

/// LUKS2 writes 64-bit numbers as JSON strings, because JSON numbers lose
/// precision past 2^53.
fn number_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let text = String::deserialize(d)?;
    text.parse().map_err(serde::de::Error::custom)
}

/// Reads the header from a device: the primary copy, or the secondary when
/// the primary's checksum is wrong — a torn header write leaves one good.
pub fn read_header(device: &impl FileExt) -> Result<Header, Error> {
    let mut last = Error::NotLuks;
    for offset in std::iter::once(0).chain(SECONDARY_OFFSETS.iter().copied()) {
        match read_header_at(device, offset) {
            Ok(header) => return Ok(header),
            Err(error) => {
                if offset == 0 {
                    last = error;
                }
            }
        }
    }
    Err(last)
}

fn read_header_at(device: &impl FileExt, offset: u64) -> Result<Header, Error> {
    let mut binary = vec![0u8; BINARY_HEADER];
    device.read_exact_at(&mut binary, offset)?;
    let magic = if offset == 0 { MAGIC } else { MAGIC_SECONDARY };
    if binary.get(..6) != Some(magic.as_slice()) || be16(&binary, 6) != Some(2) {
        return Err(Error::NotLuks);
    }
    let size = be64(&binary, 8).ok_or(Error::NotLuks)?;
    if !(BINARY_HEADER as u64 + 1..=MAX_HEADER).contains(&size) {
        return Err(Error::Corrupt("header size"));
    }
    let size = usize::try_from(size).map_err(|_| Error::Corrupt("header size"))?;
    let mut whole = vec![0u8; size];
    device.read_exact_at(&mut whole, offset)?;

    let algorithm = text(&binary, 72, 32);
    let stored = whole
        .get(CSUM_OFFSET..CSUM_OFFSET + CSUM_LEN)
        .ok_or(Error::Corrupt("checksum"))?
        .to_vec();
    if let Some(field) = whole.get_mut(CSUM_OFFSET..CSUM_OFFSET + CSUM_LEN) {
        field.fill(0);
    }
    let computed = match algorithm.as_str() {
        "sha256" => Sha256::digest(&whole).to_vec(),
        "sha512" => Sha512::digest(&whole).to_vec(),
        _ => return Err(Error::Unsupported(format!("checksum {algorithm}"))),
    };
    if stored.get(..computed.len()) != Some(computed.as_slice()) {
        return Err(Error::Corrupt("checksum"));
    }

    let json = whole.get(BINARY_HEADER..).ok_or(Error::Corrupt("json"))?;
    let end = json.iter().position(|&b| b == 0).unwrap_or(json.len());
    let metadata: Metadata = serde_json::from_slice(json.get(..end).unwrap_or_default())
        .map_err(|e| Error::Json(e.to_string()))?;
    Ok(Header {
        uuid: text(&binary, 168, 40),
        label: text(&binary, 24, 48),
        metadata,
    })
}

impl Header {
    /// The volume key, from the first keyslot the passphrase opens.
    pub fn unlock(&self, device: &impl FileExt, passphrase: &[u8]) -> Result<Key, Error> {
        for (id, slot) in &self.metadata.keyslots {
            let Some(digest) = self
                .metadata
                .digests
                .values()
                .find(|d| d.keyslots.contains(id))
            else {
                continue;
            };
            let key = open_keyslot(device, slot, passphrase)?;
            if check_digest(digest, &key)? {
                return Ok(key);
            }
        }
        Err(Error::WrongPassphrase)
    }

    /// Whether `key` is this volume's key: what a key unsealed from a TPM is
    /// checked with before it is given to the kernel.
    pub fn verify_key(&self, key: &[u8]) -> Result<bool, Error> {
        for digest in self.metadata.digests.values() {
            if check_digest(digest, key)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The data segment, which the dm-crypt table maps.
    pub fn segment(&self) -> Result<&Segment, Error> {
        let segment = self
            .metadata
            .segments
            .get("0")
            .ok_or(Error::Corrupt("no segment 0"))?;
        if segment.kind != "crypt" || segment.size != "dynamic" {
            return Err(Error::Unsupported(format!(
                "segment {} of size {}",
                segment.kind, segment.size
            )));
        }
        Ok(segment)
    }

    /// The dm-crypt table line for this volume on `device`, of
    /// `device_sectors` 512-byte sectors, with `key`. As cryptsetup builds
    /// it: LUKS2 counts IVs in the segment's sector size.
    pub fn crypt_table(
        &self,
        key: &[u8],
        device: &str,
        device_sectors: u64,
    ) -> Result<(u64, u64, String), Error> {
        let segment = self.segment()?;
        let offset = segment.offset / 512;
        let length = device_sectors
            .checked_sub(offset)
            .filter(|l| *l > 0)
            .ok_or(Error::Corrupt("segment past the device"))?;
        let mut params = format!(
            "{} {} {} {device} {offset}",
            segment.encryption,
            hex(key),
            segment.iv_tweak
        );
        if segment.sector_size != 512 {
            params.push_str(&format!(
                " 2 sector_size:{} iv_large_sectors",
                segment.sector_size
            ));
        }
        Ok((0, length, params))
    }
}

fn open_keyslot(device: &impl FileExt, slot: &Keyslot, passphrase: &[u8]) -> Result<Key, Error> {
    if slot.kind != "luks2" || slot.af.kind != "luks1" || slot.area.kind != "raw" {
        return Err(Error::Unsupported(format!("keyslot {}", slot.kind)));
    }
    if slot.area.encryption != "aes-xts-plain64" || slot.area.key_size != 64 {
        return Err(Error::Unsupported(format!(
            "keyslot cipher {} with a {}-byte key",
            slot.area.encryption, slot.area.key_size
        )));
    }
    let area_key = derive(&slot.kdf, passphrase, slot.area.key_size)?;

    let split = slot
        .key_size
        .checked_mul(slot.af.stripes)
        .ok_or(Error::Corrupt("keyslot size"))?;
    let length = split.div_ceil(AREA_SECTOR) * AREA_SECTOR;
    if length as u64 > slot.area.size || slot.key_size == 0 || slot.af.stripes == 0 {
        return Err(Error::Corrupt("keyslot area"));
    }
    let mut material = Zeroizing::new(vec![0u8; length]);
    device.read_exact_at(&mut material, slot.area.offset)?;

    let (k1, k2) = area_key.split_at(32);
    let xts = Xts128::<Aes256>::new(
        Aes256::new_from_slice(k1).map_err(|_| Error::Corrupt("keyslot key"))?,
        Aes256::new_from_slice(k2).map_err(|_| Error::Corrupt("keyslot key"))?,
    );
    for (index, sector) in material
        .as_chunks_mut::<AREA_SECTOR>()
        .0
        .iter_mut()
        .enumerate()
    {
        xts.decrypt_sector(sector, get_tweak_default(index as u128));
    }

    af_merge(
        material
            .get(..split)
            .ok_or(Error::Corrupt("keyslot area"))?,
        slot.key_size,
        slot.af.stripes,
        &slot.af.hash,
    )
}

fn derive(kdf: &Kdf, passphrase: &[u8], length: usize) -> Result<Key, Error> {
    let mut out = Zeroizing::new(vec![0u8; length]);
    match kdf {
        Kdf::Argon2id {
            time,
            memory,
            cpus,
            salt,
        }
        | Kdf::Argon2i {
            time,
            memory,
            cpus,
            salt,
        } => {
            let algorithm = if matches!(kdf, Kdf::Argon2id { .. }) {
                argon2::Algorithm::Argon2id
            } else {
                argon2::Algorithm::Argon2i
            };
            let params = argon2::Params::new(*memory, *time, *cpus, Some(length))
                .map_err(|e| Error::Unsupported(format!("argon2 parameters: {e}")))?;
            argon2::Argon2::new(algorithm, argon2::Version::V0x13, params)
                .hash_password_into(passphrase, &decode(salt)?, &mut out)
                .map_err(|e| Error::Unsupported(format!("argon2: {e}")))?;
        }
        Kdf::Pbkdf2 {
            hash,
            iterations,
            salt,
        } => pbkdf2(hash, passphrase, &decode(salt)?, *iterations, &mut out)?,
    }
    Ok(out)
}

fn pbkdf2(
    hash: &str,
    password: &[u8],
    salt: &[u8],
    rounds: u32,
    out: &mut [u8],
) -> Result<(), Error> {
    match hash {
        "sha256" => pbkdf2::pbkdf2_hmac::<Sha256>(password, salt, rounds, out),
        "sha512" => pbkdf2::pbkdf2_hmac::<Sha512>(password, salt, rounds, out),
        other => return Err(Error::Unsupported(format!("hash {other}"))),
    }
    Ok(())
}

fn check_digest(digest: &DigestEntry, key: &[u8]) -> Result<bool, Error> {
    if digest.kind != "pbkdf2" {
        return Err(Error::Unsupported(format!("digest {}", digest.kind)));
    }
    let expected = decode(&digest.digest)?;
    let mut computed = vec![0u8; expected.len()];
    pbkdf2(
        &digest.hash,
        key,
        &decode(&digest.salt)?,
        digest.iterations,
        &mut computed,
    )?;
    // Not constant-time: the digest is public, in the header.
    Ok(computed == expected)
}

/// LUKS's anti-forensic merge: the key was split into `stripes` blocks, all
/// but the last diffused, so that losing any one block loses the key.
fn af_merge(split: &[u8], key_size: usize, stripes: usize, hash: &str) -> Result<Key, Error> {
    let mut buffer = Zeroizing::new(vec![0u8; key_size]);
    for stripe in 0..stripes.saturating_sub(1) {
        let block = split
            .get(stripe * key_size..(stripe + 1) * key_size)
            .ok_or(Error::Corrupt("keyslot stripes"))?;
        for (b, s) in buffer.iter_mut().zip(block) {
            *b ^= s;
        }
        diffuse(&mut buffer, hash)?;
    }
    let last = split
        .get((stripes - 1) * key_size..stripes * key_size)
        .ok_or(Error::Corrupt("keyslot stripes"))?;
    let mut key = Zeroizing::new(vec![0u8; key_size]);
    for ((k, b), s) in key.iter_mut().zip(buffer.iter()).zip(last) {
        *k = b ^ s;
    }
    Ok(key)
}

/// Each digest-sized block replaced by H(block index, big-endian u32 ||
/// block), the last block truncated.
fn diffuse(buffer: &mut [u8], hash: &str) -> Result<(), Error> {
    let size = match hash {
        "sha256" => 32,
        "sha512" => 64,
        other => return Err(Error::Unsupported(format!("hash {other}"))),
    };
    for (index, block) in buffer.chunks_mut(size).enumerate() {
        let iv = (index as u32).to_be_bytes();
        let digest = match hash {
            "sha256" => Sha256::new()
                .chain_update(iv)
                .chain_update(&*block)
                .finalize()
                .to_vec(),
            _ => Sha512::new()
                .chain_update(iv)
                .chain_update(&*block)
                .finalize()
                .to_vec(),
        };
        let len = block.len();
        block.copy_from_slice(digest.get(..len).ok_or(Error::Corrupt("diffuse"))?);
    }
    Ok(())
}

fn decode(text: &str) -> Result<Vec<u8>, Error> {
    BASE64
        .decode(text)
        .map_err(|_| Error::Corrupt("base64 in the metadata"))
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn be64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

fn text(bytes: &[u8], at: usize, len: usize) -> String {
    let field = bytes.get(at..at + len).unwrap_or_default();
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(field.get(..end).unwrap_or_default()).into_owned()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

//! hideOS's signatures on what it publishes beyond the image — system
//! extensions first: RSA, PKCS #1 v1.5, SHA-256, the key and scheme the UKI
//! is signed with, so that one key stands for "made by hideOS". Only
//! verification is here; signing is the build's, with openssl.
//!
//! The arithmetic is num-bigint's. A verifier handles only public values,
//! so it need not run in constant time.

use num_bigint::BigUint;
use sha2::{Digest, Sha256};

use crate::Error;

/// The key hideOS signs with, as the build that made this binary knew it.
/// Development builds carry Debian's published "snakeoil" key, which
/// anyone can sign with: see ARCHITECTURE.md, "Security". The text is the
/// modulus and exponent in hex and decimal, as `openssl rsa -modulus`
/// prints them.
pub const HIDEOS_KEY: &str = include_str!("../keys/dev.rsa");

/// The DER prefix of a SHA-256 DigestInfo (RFC 8017, section 9.2).
const SHA256_DIGEST_INFO: &[u8] = &[
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

pub struct PublicKey {
    n: BigUint,
    e: BigUint,
    /// The modulus' length in bytes, which every signature has.
    len: usize,
}

impl PublicKey {
    /// Parses `n=<hex>` and `e=<decimal>` lines.
    pub fn parse(text: &str) -> Result<PublicKey, Error> {
        let field = |key: &str| {
            text.lines()
                .find_map(|l| l.trim().strip_prefix(key))
                .ok_or(Error::Corrupt("public key"))
        };
        let n = BigUint::parse_bytes(field("n=")?.as_bytes(), 16)
            .ok_or(Error::Corrupt("public key modulus"))?;
        let e = BigUint::parse_bytes(field("e=")?.as_bytes(), 10)
            .ok_or(Error::Corrupt("public key exponent"))?;
        let len = n.bits().div_ceil(8) as usize;
        if len < 256 {
            return Err(Error::Unsupported("an RSA key under 2048 bits".into()));
        }
        Ok(PublicKey { n, e, len })
    }

    pub fn hideos() -> Result<PublicKey, Error> {
        PublicKey::parse(HIDEOS_KEY)
    }

    /// Whether `signature` is this key's over `message`.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        if signature.len() != self.len {
            return false;
        }
        let s = BigUint::from_bytes_be(signature);
        if s >= self.n {
            return false;
        }
        let m = s.modpow(&self.e, &self.n).to_bytes_be();
        let mut encoded = vec![0u8; self.len.saturating_sub(m.len())];
        encoded.extend_from_slice(&m);
        encoded == self.expected(message)
    }

    /// EMSA-PKCS1-v1_5: 00 01 FF..FF 00, the DigestInfo, the hash.
    fn expected(&self, message: &[u8]) -> Vec<u8> {
        let hash = Sha256::digest(message);
        let tail = SHA256_DIGEST_INFO.len() + hash.len();
        let mut out = vec![0x00, 0x01];
        out.resize(self.len.saturating_sub(tail + 1), 0xff);
        out.push(0x00);
        out.extend_from_slice(SHA256_DIGEST_INFO);
        out.extend_from_slice(&hash);
        out
    }
}

//! Against a header cryptsetup 2.8 wrote: two keyslots, argon2id and
//! pbkdf2, with cheap parameters, and the volume key `cryptsetup luksDump
//! --dump-volume-key` printed for it.

use std::fs::File;

use hidecrypt::luks2::{hex, read_header};

const VOLUME_KEY: &str = "e5777288ef1c9f3f907df723ce01d63f41fb76ece7f8bdd38ee7f07f56b634362b936bba6e2498a46c03e1fac6b75f1bd791f21138932e20bfd13fa69e4cde42";

fn fixture() -> File {
    File::open(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/luks2-header.img"
    ))
    .unwrap()
}

#[test]
fn the_header_reads() {
    let header = read_header(&fixture()).unwrap();
    assert_eq!(header.uuid, "d4683484-ddd0-4869-b568-7556ae7eef6b");
    assert_eq!(header.label, "hideos");
    assert_eq!(header.metadata.keyslots.len(), 2);
    let segment = header.segment().unwrap();
    assert_eq!(segment.offset, 2097152);
    assert_eq!(segment.sector_size, 4096);
}

#[test]
fn an_argon2id_keyslot_opens() {
    let device = fixture();
    let header = read_header(&device).unwrap();
    let key = header.unlock(&device, b"correct horse").unwrap();
    assert_eq!(hex(&key), VOLUME_KEY);
}

#[test]
fn a_pbkdf2_keyslot_opens() {
    let device = fixture();
    let header = read_header(&device).unwrap();
    let key = header.unlock(&device, b"battery staple").unwrap();
    assert_eq!(hex(&key), VOLUME_KEY);
}

#[test]
fn a_wrong_passphrase_opens_nothing() {
    let device = fixture();
    let header = read_header(&device).unwrap();
    assert!(matches!(
        header.unlock(&device, b"wrong"),
        Err(hidecrypt::Error::WrongPassphrase)
    ));
}

#[test]
fn a_key_is_checked_against_the_digest() {
    let header = read_header(&fixture()).unwrap();
    let mut key: Vec<u8> = (0..VOLUME_KEY.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&VOLUME_KEY[i..i + 2], 16).unwrap())
        .collect();
    assert!(header.verify_key(&key).unwrap());
    key[0] ^= 1;
    assert!(!header.verify_key(&key).unwrap());
}

#[test]
fn the_crypt_table_is_cryptsetups() {
    let header = read_header(&fixture()).unwrap();
    let (start, length, params) = header.crypt_table(&[0xab; 64], "/dev/vda2", 16384).unwrap();
    assert_eq!((start, length), (0, 16384 - 4096));
    assert_eq!(
        params,
        format!(
            "aes-xts-plain64 {} 0 /dev/vda2 4096 2 sector_size:4096 iv_large_sectors",
            "ab".repeat(64)
        )
    );
}

#[test]
fn a_damaged_primary_falls_back_to_the_secondary() {
    let mut bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/luks2-header.img"
    ))
    .unwrap();
    bytes[5000] ^= 0xff;
    let dir = std::env::temp_dir().join(format!("hidecrypt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("damaged.img");
    std::fs::write(&path, &bytes).unwrap();
    let header = read_header(&File::open(&path).unwrap()).unwrap();
    assert_eq!(header.label, "hideos");
    std::fs::remove_dir_all(&dir).unwrap();
}

//! Against a signature openssl made with the development key:
//! `openssl dgst -sha256 -sign`, as the build signs.

use hidecrypt::signature::PublicKey;

const MESSAGE: &[u8] = include_bytes!("fixtures/signed-message.txt");
const SIGNATURE: &[u8] = include_bytes!("fixtures/signed-message.sig");

#[test]
fn openssl_signatures_verify() {
    let key = PublicKey::hideos().unwrap();
    assert!(key.verify(MESSAGE, SIGNATURE));
}

#[test]
fn anything_changed_does_not() {
    let key = PublicKey::hideos().unwrap();
    let mut message = MESSAGE.to_vec();
    message[0] ^= 1;
    assert!(!key.verify(&message, SIGNATURE));
    let mut signature = SIGNATURE.to_vec();
    signature[100] ^= 1;
    assert!(!key.verify(MESSAGE, &signature));
    assert!(!key.verify(MESSAGE, &SIGNATURE[1..]));
}

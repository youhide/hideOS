//! Recovery keys: what opens an encrypted disk when its passphrase is
//! forgotten and the TPM cannot. 200 random bits, written in Crockford's
//! base32 — no I, L, O or U, so nothing reads as something else — in eight
//! groups of five, to be copied by hand.

/// Crockford's alphabet.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The key for 25 random bytes.
pub fn format_key(random: &[u8; 25]) -> String {
    let mut out = String::with_capacity(47);
    let mut bits: u32 = 0;
    let mut count = 0;
    let mut written = 0;
    for byte in random {
        bits = (bits << 8) | u32::from(*byte);
        count += 8;
        while count >= 5 {
            count -= 5;
            let index = ((bits >> count) & 0x1f) as usize;
            if written > 0 && written % 5 == 0 {
                out.push('-');
            }
            out.push(char::from(ALPHABET.get(index).copied().unwrap_or(b'0')));
            written += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_groups_of_five() {
        let key = format_key(&[0xff; 25]);
        assert_eq!(key, "ZZZZZ-ZZZZZ-ZZZZZ-ZZZZZ-ZZZZZ-ZZZZZ-ZZZZZ-ZZZZZ");
        let key = format_key(&[0; 25]);
        assert_eq!(key, "00000-00000-00000-00000-00000-00000-00000-00000");
        let mut bytes = [0u8; 25];
        bytes[0] = 0b0000_1000;
        assert!(format_key(&bytes).starts_with("10000"));
    }

    #[test]
    fn no_letters_that_read_as_others() {
        let key = format_key(&[0x5a; 25]);
        assert!(!key.contains(['I', 'L', 'O', 'U']));
    }
}

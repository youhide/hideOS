//! hideOS's entry in the firmware's boot menu: a `Boot####` variable
//! naming hideBoot on hideOS's ESP, first in `BootOrder`. Without it a PC
//! with Windows keeps starting Windows Boot Manager, its entry first, and
//! reaches hideOS's disk only through the firmware's own menu. See
//! ARCHITECTURE.md, "Beside Windows".
//!
//! The bytes, as UEFI 2.10 §3.1.3 (EFI_LOAD_OPTION) and §10.3 (device
//! paths) lay them out; writing them is the installer's.

/// `LOAD_OPTION_ACTIVE`: the firmware may boot it.
const ACTIVE: u32 = 0x1;
/// What the firmware's menu shows.
pub const DESCRIPTION: &str = "hideOS";

/// Where hideOS's ESP is, as the firmware finds a partition: by its GPT
/// entry, not by a device name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EspLocation {
    /// The partition's number in the table, from 1.
    pub number: u32,
    /// First sector and length, in the disk's logical blocks.
    pub start: u64,
    pub sectors: u64,
    /// The partition's unique GUID, as text.
    pub guid: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum EntryError {
    #[error("`{0}` is not a GUID")]
    Guid(String),
}

/// A GUID in the mixed-endian layout UEFI stores: the first three fields
/// little-endian, the last two as written.
pub fn guid_bytes(text: &str) -> Result<[u8; 16], EntryError> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    let bad = || EntryError::Guid(text.to_owned());
    if hex.len() != 32 || text.len() != 36 {
        return Err(bad());
    }
    let mut raw = [0u8; 16];
    for (i, byte) in raw.iter_mut().enumerate() {
        let pair = hex.get(i * 2..i * 2 + 2).ok_or_else(bad)?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| bad())?;
    }
    let [a0, a1, a2, a3, b0, b1, c0, c1, rest @ ..] = raw;
    let mut out = [0u8; 16];
    let mixed = [a3, a2, a1, a0, b1, b0, c1, c0];
    for (slot, byte) in out.iter_mut().zip(mixed.iter().chain(rest.iter())) {
        *slot = *byte;
    }
    Ok(out)
}

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// The device path to `file` on the ESP: the partition, the file, the end.
pub fn device_path(esp: &EspLocation, file: &str) -> Result<Vec<u8>, EntryError> {
    let mut path = Vec::new();
    // Hard drive: type 4 (media), subtype 1, 42 bytes.
    path.extend_from_slice(&[4, 1]);
    path.extend_from_slice(&42u16.to_le_bytes());
    path.extend_from_slice(&esp.number.to_le_bytes());
    path.extend_from_slice(&esp.start.to_le_bytes());
    path.extend_from_slice(&esp.sectors.to_le_bytes());
    path.extend_from_slice(&guid_bytes(&esp.guid)?);
    // Partition format 2: GPT; signature type 2: a GUID.
    path.extend_from_slice(&[2, 2]);
    // File path: type 4, subtype 4, the name in UTF-16, NUL-terminated.
    let name = utf16z(file);
    path.extend_from_slice(&[4, 4]);
    path.extend_from_slice(
        &u16::try_from(4 + name.len())
            .unwrap_or(u16::MAX)
            .to_le_bytes(),
    );
    path.extend_from_slice(&name);
    // End of the whole path.
    path.extend_from_slice(&[0x7f, 0xff, 4, 0]);
    Ok(path)
}

/// An `EFI_LOAD_OPTION` for `file` on the ESP, active, named hideOS.
pub fn load_option(esp: &EspLocation, file: &str) -> Result<Vec<u8>, EntryError> {
    let path = device_path(esp, file)?;
    let mut option = Vec::new();
    option.extend_from_slice(&ACTIVE.to_le_bytes());
    option.extend_from_slice(&u16::try_from(path.len()).unwrap_or(u16::MAX).to_le_bytes());
    option.extend_from_slice(&utf16z(DESCRIPTION));
    option.extend_from_slice(&path);
    Ok(option)
}

/// The description an `EFI_LOAD_OPTION` carries, to find hideOS's own
/// entry again and reuse its number.
pub fn description(option: &[u8]) -> Option<String> {
    let units: Vec<u16> = option
        .get(6..)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16(&units).ok()
}

/// `BootOrder` with `number` first, and nowhere else.
pub fn first_in_order(order: &[u16], number: u16) -> Vec<u16> {
    std::iter::once(number)
        .chain(order.iter().copied().filter(|n| *n != number))
        .collect()
}

/// The number for hideOS's entry: the one an earlier install made, or the
/// lowest that is free.
pub fn number(existing: &[(u16, Option<String>)]) -> u16 {
    if let Some((number, _)) = existing
        .iter()
        .find(|(_, description)| description.as_deref() == Some(DESCRIPTION))
    {
        return *number;
    }
    (0..=u16::MAX)
        .find(|n| !existing.iter().any(|(taken, _)| taken == n))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esp() -> EspLocation {
        EspLocation {
            number: 5,
            start: 402337792,
            sectors: 1048576,
            guid: "6F1A0E55-2B3C-4D5E-8F90-A1B2C3D4E5F6".into(),
        }
    }

    #[test]
    fn guids_are_stored_mixed_endian() {
        assert_eq!(
            guid_bytes("6F1A0E55-2B3C-4D5E-8F90-A1B2C3D4E5F6").unwrap(),
            [
                0x55, 0x0E, 0x1A, 0x6F, 0x3C, 0x2B, 0x5E, 0x4D, 0x8F, 0x90, 0xA1, 0xB2, 0xC3, 0xD4,
                0xE5, 0xF6
            ]
        );
        assert!(guid_bytes("not-a-guid").is_err());
    }

    #[test]
    fn the_load_option_is_laid_out_as_uefi_says() {
        let option = load_option(&esp(), "\\EFI\\BOOT\\BOOTX64.EFI").unwrap();
        // Active.
        assert_eq!(option.get(0..4), Some(&[1, 0, 0, 0][..]));
        let path_length = u16::from_le_bytes([option[4], option[5]]) as usize;
        // "hideOS" and its NUL, in UTF-16.
        let description_end = 6 + 7 * 2;
        assert_eq!(description(&option).as_deref(), Some("hideOS"));
        let path = &option[description_end..];
        assert_eq!(path.len(), path_length);
        // Hard drive node, 42 bytes, partition 5 at its start.
        assert_eq!(&path[0..4], &[4, 1, 42, 0]);
        assert_eq!(&path[4..8], &5u32.to_le_bytes());
        assert_eq!(&path[8..16], &402337792u64.to_le_bytes());
        assert_eq!(&path[40..42], &[2, 2]);
        // File node: 4 + 22 UTF-16 units (21 characters and the NUL).
        assert_eq!(&path[42..46], &[4, 4, 48, 0]);
        // The end.
        assert_eq!(&path[path.len() - 4..], &[0x7f, 0xff, 4, 0]);
    }

    #[test]
    fn hideos_goes_first_once_and_keeps_its_number() {
        assert_eq!(first_in_order(&[0, 3, 1], 3), [3, 0, 1]);
        assert_eq!(first_in_order(&[0, 1], 4), [4, 0, 1]);
        let existing = [
            (0, Some("Windows Boot Manager".to_owned())),
            (1, Some("UEFI OS".to_owned())),
            (3, Some("hideOS".to_owned())),
        ];
        assert_eq!(number(&existing), 3);
        assert_eq!(number(&existing[..2]), 2);
    }
}

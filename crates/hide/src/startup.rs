//! "Startup Disk", as on a Mac: which system starts when no key is held —
//! hideOS or Windows — and starting the other one once. hideBoot reads the
//! choice from the variables systemd-boot defines, under its vendor GUID:
//! `LoaderEntryDefault` and `LoaderEntryOneShot` name an entry, and
//! `LoaderEntries` lists the entries it found. See ARCHITECTURE.md,
//! "Beside Windows".

/// systemd-boot's vendor GUID, which hideBoot's variables are under.
pub const LOADER_GUID: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
/// Windows Boot Manager's entry id, systemd-boot's: copies on other disks
/// have a suffix.
pub const WINDOWS: &str = "auto-windows";

/// A system a person can start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum System {
    /// hideOS: its newest deployment that may be tried, as when nothing is
    /// chosen.
    HideOs,
    Windows,
}

impl System {
    pub fn parse(word: &str) -> Option<System> {
        match word.to_ascii_lowercase().as_str() {
            "hideos" => Some(System::HideOs),
            "windows" => Some(System::Windows),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            System::HideOs => "hideOS",
            System::Windows => "Windows",
        }
    }
}

/// A variable's value as hideBoot reads it: UTF-16LE, NUL-terminated.
pub fn encode(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// The strings in a UTF-16LE value, split at NULs: one for an entry id,
/// several for `LoaderEntries`.
pub fn decode(value: &[u8]) -> Vec<String> {
    let units: Vec<u16> = value
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    units
        .split(|unit| *unit == 0)
        .filter(|part| !part.is_empty())
        .filter_map(|part| String::from_utf16(part).ok())
        .collect()
}

/// The entry id that starts `system`: Windows's first entry hideBoot
/// found; for hideOS none, which leaves hideBoot to its own choice — the
/// newest deployment, with its boot counting.
pub fn entry(system: System, entries: &[String]) -> Result<Option<String>, StartupError> {
    match system {
        System::HideOs => Ok(None),
        System::Windows => entries
            .iter()
            .find(|id| id.starts_with(WINDOWS))
            .cloned()
            .map(Some)
            .ok_or(StartupError::NoWindows),
    }
}

/// Which system an entry id starts.
pub fn system_of(id: &str) -> System {
    if id.starts_with(WINDOWS) {
        System::Windows
    } else {
        System::HideOs
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartupError {
    #[error("hideBoot found no Windows on this machine")]
    NoWindows,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_utf16_with_a_nul() {
        assert_eq!(encode("auto-windows").len(), 13 * 2);
        assert_eq!(decode(&encode("auto-windows")), ["auto-windows"]);
        let mut list = encode("hideos-workstation-45-91638a7916d3.efi");
        list.extend(encode("auto-windows"));
        assert_eq!(decode(&list).len(), 2);
    }

    #[test]
    fn windows_is_its_entry_and_hideos_is_hideboots_own_choice() {
        let entries = vec![
            "hideos-workstation-45-91638a7916d3.efi".to_owned(),
            "auto-windows".to_owned(),
            "auto-windows-2".to_owned(),
        ];
        assert_eq!(
            entry(System::Windows, &entries),
            Ok(Some("auto-windows".to_owned()))
        );
        assert_eq!(entry(System::HideOs, &entries), Ok(None));
        assert_eq!(
            entry(System::Windows, &entries[..1]),
            Err(StartupError::NoWindows)
        );
        assert_eq!(system_of("auto-windows-2"), System::Windows);
        assert_eq!(System::parse("Windows"), Some(System::Windows));
    }
}

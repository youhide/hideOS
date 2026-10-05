//! How the hardware clock keeps time: in UTC, as Linux keeps it, or in
//! local time, as Windows does. A machine that also runs Windows keeps it
//! Windows's way, or each system shows the other's hours. See
//! ARCHITECTURE.md, "Beside Windows".
//!
//! `/usr/lib/hide/clock.conf` says `hardware-clock = utc`; the installer
//! writes `/etc/hide/clock.conf` with `local` when it finds Windows.

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClockError {
    #[error("line {0}: not `key = value`")]
    Syntax(usize),
    #[error("unknown key `{0}`")]
    Key(String),
    #[error("hardware-clock is utc or local, not `{0}`")]
    Value(String),
}

/// Whether the hardware clock is in local time: the vendor file, then the
/// override, the last one saying winning.
pub fn is_local(vendor: &str, overrides: &str) -> Result<bool, ClockError> {
    let mut local = false;
    for text in [vendor, overrides] {
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or(ClockError::Syntax(n + 1))?;
            match (key.trim(), value.trim()) {
                ("hardware-clock", "utc") => local = false,
                ("hardware-clock", "local") => local = true,
                ("hardware-clock", other) => return Err(ClockError::Value(other.to_owned())),
                (other, _) => return Err(ClockError::Key(other.to_owned())),
            }
        }
    }
    Ok(local)
}

/// What the installer writes beside Windows.
pub const LOCAL: &str = "\
# Written by the installer, which found Windows on this machine: the
# hardware clock in local time, as Windows keeps it.
hardware-clock = local
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_unless_the_machine_says_local() {
        let vendor = "# comment\nhardware-clock = utc\n";
        assert_eq!(is_local(vendor, ""), Ok(false));
        assert_eq!(is_local(vendor, LOCAL), Ok(true));
        assert_eq!(
            is_local(vendor, "hardware-clock = windows"),
            Err(ClockError::Value("windows".into()))
        );
        assert_eq!(
            is_local("clock = utc", ""),
            Err(ClockError::Key("clock".into()))
        );
    }
}

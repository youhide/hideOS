//! The parts of hidestage that are policy, not Linux: what the kernel command
//! line says, and how to find the root partition in a uevent. Tested on any
//! host. Everything that mounts something is in the binary.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;

/// What hidestage needs to know, all of it from the kernel command line. The
/// command line is inside the signed UKI, so every value here is as trusted
/// as the kernel itself — in particular the image digest, which is the seal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The fs-verity SHA-256 digest of the deployment's EROFS image.
    pub image: [u8; 32],
    /// GPT partition name of the btrfs root.
    pub root_label: String,
    /// What to run as PID 1 once the root is assembled.
    pub init: PathBuf,
    /// Seconds the system gets to come up before the hardware watchdog
    /// resets the machine, which the boot manager counts as a failed
    /// attempt. `None`: no watchdog (`hideos.watchdog=0`).
    pub watchdog: Option<u32>,
}

/// How long a boot may take before it counts as hung: long enough for a
/// slow disk and a first-boot setup, short enough that a machine stuck on
/// an update gives up within minutes.
pub const DEFAULT_WATCHDOG_SECONDS: u32 = 180;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("the kernel command line has no hideos.image=sha256:<digest>; nothing to boot")]
    MissingImage,
    #[error("hideos.image `{0}` is not sha256: followed by 64 lowercase hex digits")]
    BadImage(String),
    #[error("{0} must be an absolute path, not `{1}`")]
    NotAbsolute(&'static str, String),
    #[error("hideos.watchdog `{0}` is not a number of seconds")]
    BadWatchdog(String),
}

impl Config {
    /// Parses `/proc/cmdline`. Keys hidestage does not know are someone
    /// else's — the kernel's, oxinit's — and are ignored.
    pub fn from_cmdline(cmdline: &str) -> Result<Config, ConfigError> {
        let mut image = None;
        let mut root_label = "hideos-root".to_owned();
        let mut init = PathBuf::from("/usr/bin/oxinit");
        let mut watchdog = Some(DEFAULT_WATCHDOG_SECONDS);
        for word in cmdline.split_ascii_whitespace() {
            let Some((key, value)) = word.split_once('=') else {
                continue;
            };
            match key {
                "hideos.image" => image = Some(parse_digest(value)?),
                "hideos.root" => root_label = value.to_owned(),
                "hideos.init" => {
                    if !value.starts_with('/') {
                        return Err(ConfigError::NotAbsolute("hideos.init", value.to_owned()));
                    }
                    init = PathBuf::from(value);
                }
                "hideos.watchdog" => {
                    let seconds: u32 = value
                        .parse()
                        .map_err(|_| ConfigError::BadWatchdog(value.to_owned()))?;
                    watchdog = (seconds > 0).then_some(seconds);
                }
                _ => {}
            }
        }
        Ok(Config {
            image: image.ok_or(ConfigError::MissingImage)?,
            root_label,
            init,
            watchdog,
        })
    }
}

fn parse_digest(value: &str) -> Result<[u8; 32], ConfigError> {
    let bad = || ConfigError::BadImage(value.to_owned());
    let hex = value.strip_prefix("sha256:").ok_or_else(bad)?;
    if hex.len() != 64 {
        return Err(bad());
    }
    let mut digest = [0u8; 32];
    for (byte, pair) in digest.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let [high, low] = pair else {
            return Err(bad());
        };
        *byte = (nibble(*high).ok_or_else(bad)? << 4) | nibble(*low).ok_or_else(bad)?;
    }
    Ok(digest)
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Lowercase hex, for printing digests the way the command line writes them.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Whether a block device's uevent (`/sys/class/block/<dev>/uevent`) is the
/// partition named `label`, and if so its device name.
pub fn partition_named<'a>(uevent: &'a str, label: &str) -> Option<&'a str> {
    let mut devname = None;
    let mut partname = None;
    for line in uevent.lines() {
        match line.split_once('=') {
            Some(("DEVNAME", value)) => devname = Some(value),
            Some(("PARTNAME", value)) => partname = Some(value),
            _ => {}
        }
    }
    if partname == Some(label) {
        devname
    } else {
        None
    }
}

/// The `SecureBoot` EFI variable, as efivarfs presents it: four bytes of
/// attributes, then the value — 1 when the firmware enforces signatures.
pub const SECURE_BOOT_VARIABLE: &str =
    "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// Whether Secure Boot is on, from the variable's contents. `None` for
/// anything that is not a one-byte boolean after the attributes.
pub fn secure_boot(variable: &[u8]) -> Option<bool> {
    match variable.get(4..) {
        Some([0]) => Some(false),
        Some([1]) => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "d601ed782b132ffcf9b054d01006ba6e426b57ba5262847081def2fbd12f8ce9";

    #[test]
    fn a_full_command_line_parses() {
        let line = format!(
            "console=ttyS0 hideos.image=sha256:{DIGEST} hideos.root=disk-a quiet hideos.init=/sbin/x\n"
        );
        let config = Config::from_cmdline(&line).unwrap();
        assert_eq!(hex(&config.image), DIGEST);
        assert_eq!(config.root_label, "disk-a");
        assert_eq!(config.init, PathBuf::from("/sbin/x"));
    }

    #[test]
    fn defaults_apply_when_only_the_image_is_given() {
        let config = Config::from_cmdline(&format!("hideos.image=sha256:{DIGEST}")).unwrap();
        assert_eq!(config.root_label, "hideos-root");
        assert_eq!(config.init, PathBuf::from("/usr/bin/oxinit"));
        assert_eq!(config.watchdog, Some(DEFAULT_WATCHDOG_SECONDS));
    }

    #[test]
    fn the_watchdog_can_be_shortened_or_turned_off() {
        let with = |w: &str| Config::from_cmdline(&format!("hideos.image=sha256:{DIGEST} {w}"));
        assert_eq!(with("hideos.watchdog=60").unwrap().watchdog, Some(60));
        assert_eq!(with("hideos.watchdog=0").unwrap().watchdog, None);
        assert_eq!(
            with("hideos.watchdog=soon"),
            Err(ConfigError::BadWatchdog("soon".to_owned()))
        );
    }

    #[test]
    fn no_image_means_nothing_to_boot() {
        assert_eq!(
            Config::from_cmdline("console=ttyS0 quiet"),
            Err(ConfigError::MissingImage)
        );
    }

    #[test]
    fn malformed_digests_are_refused() {
        for bad in [
            DIGEST.to_owned(),
            format!("sha512:{DIGEST}"),
            format!("sha256:{}", &DIGEST[1..]),
            format!("sha256:{}", DIGEST.to_uppercase()),
            format!("sha256:{}g", &DIGEST[1..]),
        ] {
            let result = Config::from_cmdline(&format!("hideos.image={bad}"));
            assert!(matches!(result, Err(ConfigError::BadImage(_))), "{bad}");
        }
    }

    #[test]
    fn init_must_be_absolute() {
        let line = format!("hideos.image=sha256:{DIGEST} hideos.init=oxinit");
        assert!(matches!(
            Config::from_cmdline(&line),
            Err(ConfigError::NotAbsolute(..))
        ));
    }

    #[test]
    fn partitions_are_found_by_name() {
        let uevent =
            "MAJOR=253\nMINOR=2\nDEVNAME=vda2\nDEVTYPE=partition\nPARTN=2\nPARTNAME=hideos-root\n";
        assert_eq!(partition_named(uevent, "hideos-root"), Some("vda2"));
        assert_eq!(partition_named(uevent, "hideos-esp"), None);
        assert_eq!(
            partition_named("DEVNAME=vda\nDEVTYPE=disk\n", "hideos-root"),
            None
        );
    }

    #[test]
    fn secure_boot_is_the_byte_after_the_attributes() {
        assert_eq!(secure_boot(&[6, 0, 0, 0, 1]), Some(true));
        assert_eq!(secure_boot(&[6, 0, 0, 0, 0]), Some(false));
        assert_eq!(secure_boot(&[6, 0, 0, 0]), None);
        assert_eq!(secure_boot(&[6, 0, 0, 0, 2]), None);
        assert_eq!(secure_boot(&[6, 0, 0, 0, 1, 0]), None);
    }
}

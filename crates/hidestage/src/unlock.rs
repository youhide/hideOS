//! The encrypted root: when the root partition is LUKS2, open it — with the
//! key the TPM unseals when the boot chain is the one it was sealed to,
//! otherwise with a passphrase typed on the console — and map it with
//! dm-crypt. See ARCHITECTURE.md, "Boot chain", step 4.
//!
//! The header, the key derivation and the TPM commands are hidecrypt's;
//! this module moves bytes between them and the devices.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use hidecrypt::luks2::{Header, Key, read_header};
use hidecrypt::tpm2::{Sealed, Transport, unseal};
use rustix::mount::{MountFlags, UnmountFlags, mount, unmount};
use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};

use crate::boot::{BootError, say};
use crate::sys::dm;

/// The mapped root, as `hide` and dmsetup know it.
pub const MAPPED_NAME: &str = "hideos-root";
/// Where the sealed key is on the ESP. See `hide tpm-enroll`.
const SEALED_PATH: &str = "EFI/hideos/root.tpm2";
/// How the root was opened, for `hide tpm-enroll` once the system runs:
/// `tpm`, `passphrase`, or `passphrase tpm-refused`.
pub const UNLOCK_NOTE: &str = "/run/hidestage/unlock";
const ESP_MOUNT: &str = "/run/hidestage/esp";
const TPM: &str = "/dev/tpmrm0";

/// The device to mount as the root: `device` itself when it is not LUKS2,
/// the mapped device when it is, once opened.
pub fn open(device: &Path, esp_label: &str) -> Result<PathBuf, BootError> {
    let file = File::open(device).map_err(|source| BootError::Os {
        what: format!("opening {}", device.display()),
        source,
    })?;
    let header = match read_header(&file) {
        Ok(header) => header,
        Err(hidecrypt::Error::NotLuks) => return Ok(device.to_path_buf()),
        Err(error) => return Err(BootError::Crypt(error.to_string())),
    };
    say(&format!("hidestage: {} is encrypted", device.display()));

    let (key, how) = match tpm_key(&header, esp_label) {
        TpmOutcome::Key(key) => (key, "tpm"),
        TpmOutcome::Refused => (ask(&header, &file)?, "passphrase tpm-refused"),
        TpmOutcome::Absent => (ask(&header, &file)?, "passphrase"),
    };
    let path = map(&header, &key, device)?;
    let _ = fs::create_dir_all("/run/hidestage");
    let _ = fs::write(UNLOCK_NOTE, how);
    say(&format!(
        "hidestage: unlocked with the {}",
        how.split(' ').next().unwrap_or(how)
    ));
    Ok(path)
}

enum TpmOutcome {
    Key(Key),
    /// There is a sealed key and a TPM, and the TPM would not give it up:
    /// the boot chain changed.
    Refused,
    /// No TPM, or nothing sealed to it.
    Absent,
}

fn tpm_key(header: &Header, esp_label: &str) -> TpmOutcome {
    // The TPM driver probes while the initrd starts; give it a moment
    // when the firmware says there is one.
    if Path::new("/sys/class/tpm/tpm0").exists() {
        let started = Instant::now();
        while !Path::new(TPM).exists() && started.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(50));
        }
    }
    if !Path::new(TPM).exists() {
        return TpmOutcome::Absent;
    }
    let Some(sealed) = read_sealed(esp_label) else {
        return TpmOutcome::Absent;
    };
    let mut tpm = match OpenOptions::new().read(true).write(true).open(TPM) {
        Ok(file) => Device(file),
        Err(_) => return TpmOutcome::Absent,
    };
    match unseal(&mut tpm, &sealed) {
        Ok(Some(secret)) => {
            let key: Key = secret.into();
            // The key the TPM gave must be this volume's: a sealed blob
            // from another install would otherwise map garbage.
            if header.verify_key(&key).unwrap_or(false) {
                TpmOutcome::Key(key)
            } else {
                say("hidestage: the TPM's key is not this volume's");
                TpmOutcome::Refused
            }
        }
        Ok(None) => {
            say("hidestage: the boot chain changed since the key was sealed to the TPM");
            TpmOutcome::Refused
        }
        Err(error) => {
            say(&format!("hidestage: TPM: {error}"));
            TpmOutcome::Refused
        }
    }
}

/// The sealed key, from the ESP, mounted read-only for as long as it takes
/// to read one file.
fn read_sealed(esp_label: &str) -> Option<Sealed> {
    let esp = find_partition(esp_label)?;
    fs::create_dir_all(ESP_MOUNT).ok()?;
    mount(&esp, ESP_MOUNT, "vfat", MountFlags::RDONLY, None).ok()?;
    let bytes = fs::read(Path::new(ESP_MOUNT).join(SEALED_PATH));
    let _ = unmount(ESP_MOUNT, UnmountFlags::empty());
    Sealed::from_bytes(&bytes.ok()?).ok()
}

fn find_partition(label: &str) -> Option<PathBuf> {
    for entry in fs::read_dir("/sys/class/block").ok()?.flatten() {
        let uevent = fs::read_to_string(entry.path().join("uevent")).unwrap_or_default();
        if let Some(name) = hidestage::partition_named(&uevent, label) {
            return Some(Path::new("/dev").join(name));
        }
    }
    None
}

/// Asks on the console until a passphrase opens a keyslot. A person is
/// there, or nobody is and the machine waits — which is what a locked
/// disk should do.
fn ask(header: &Header, device: &File) -> Result<Key, BootError> {
    loop {
        let passphrase = prompt("Passphrase for the hideOS disk: ")?;
        match header.unlock(device, passphrase.as_bytes()) {
            Ok(key) => return Ok(key),
            Err(hidecrypt::Error::WrongPassphrase) => {
                say("hidestage: that passphrase opens nothing")
            }
            Err(error) => return Err(BootError::Crypt(error.to_string())),
        }
    }
}

/// Reads one line from the console with echo off.
fn prompt(question: &str) -> Result<zeroize::Zeroizing<String>, BootError> {
    let os = |what: &str| {
        let what = what.to_owned();
        move |source: std::io::Error| BootError::Os { what, source }
    };
    let mut console = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/console")
        .map_err(os("opening the console"))?;
    let saved = tcgetattr(&console).ok();
    if let Some(mut quiet) = saved.clone() {
        quiet.local_modes.remove(LocalModes::ECHO);
        let _ = tcsetattr(&console, OptionalActions::Now, &quiet);
    }
    let _ = console.write_all(question.as_bytes());
    let mut line = zeroize::Zeroizing::new(String::new());
    let mut byte = [0u8; 1];
    let result = loop {
        match console.read(&mut byte) {
            Ok(0) => break Ok(()),
            Ok(_) => match byte {
                [b'\n'] | [b'\r'] => break Ok(()),
                [b] => line.push(b as char),
            },
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => break Err(error),
        }
    };
    if let Some(saved) = saved {
        let _ = tcsetattr(&console, OptionalActions::Now, &saved);
    }
    let _ = console.write_all(b"\n");
    result.map_err(os("reading the console"))?;
    Ok(line)
}

/// Maps the volume with dm-crypt and returns the mapped device.
fn map(header: &Header, key: &[u8], device: &Path) -> Result<PathBuf, BootError> {
    let name = device
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let sectors: u64 = fs::read_to_string(format!("/sys/class/block/{name}/size"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| BootError::Crypt(format!("no size for {}", device.display())))?;
    let (start, length, params) = header
        .crypt_table(key, &device.to_string_lossy(), sectors)
        .map_err(|e| BootError::Crypt(e.to_string()))?;
    let params = zeroize::Zeroizing::new(params);
    // cryptsetup's naming, so that its tools recognise the mapping.
    let uuid = format!("CRYPT-LUKS2-{}-{MAPPED_NAME}", header.uuid.replace('-', ""));
    let dev =
        dm::create(MAPPED_NAME, &uuid, start, length, "crypt", &params).map_err(|source| {
            BootError::Os {
                what: "mapping the encrypted root".into(),
                source,
            }
        })?;
    let path = PathBuf::from(format!("/dev/dm-{}", rustix::fs::minor(dev)));
    let started = Instant::now();
    while !path.exists() && started.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(20));
    }
    Ok(path)
}

/// /dev/tpmrm0: one command written, one response read.
struct Device(File);

impl Transport for Device {
    fn transact(&mut self, command: &[u8]) -> std::io::Result<Vec<u8>> {
        self.0.write_all(command)?;
        let mut response = vec![0u8; 4096];
        let n = self.0.read(&mut response)?;
        response.truncate(n);
        Ok(response)
    }
}

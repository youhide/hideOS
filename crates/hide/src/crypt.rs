//! `hide tpm-enroll`: seals the root's volume key to this machine's TPM,
//! so that hidestage opens the disk without asking. See hidecrypt::tpm2 for
//! the policy, and hidestage's unlock.rs for the other half.
//!
//! The key comes from the running mapping — dmsetup prints the dm-crypt
//! table, key included, to root — so no keyslot is added and the LUKS2
//! header is not written. The sealed blob goes on the ESP: it is not
//! secret, only this TPM in this boot state opens it.
//!
//! With `--if-missing`, as the boot runs it: only when the root was opened
//! with a passphrase because nothing was sealed. When the TPM refused a
//! sealed key — the boot chain changed — it says so and does nothing: a
//! new seal is for the person who knows why it changed to make.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use hidecrypt::tpm2::{Sealed, Transport, seal, unseal};
use zeroize::Zeroizing;

use crate::deploy::{Esp, rename_durably};

const TPM: &str = "/dev/tpmrm0";
const UNLOCK_NOTE: &str = "/run/hidestage/unlock";
const MAPPED_NAME: &str = "hideos-root";
const SEALED_PATH: &str = "EFI/hideos/root.tpm2";
/// PCR 7: Secure Boot's state and keys.
const PCRS: u32 = 1 << 7;

pub fn tpm_enroll(args: &[String]) -> Result<()> {
    let if_missing = match args {
        [] => false,
        [flag] if flag == "--if-missing" => true,
        _ => bail!("usage: hide tpm-enroll [--if-missing]"),
    };
    let Ok(note) = fs::read_to_string(UNLOCK_NOTE) else {
        say("the root is not encrypted");
        return Ok(());
    };
    if !Path::new(TPM).exists() {
        say("this machine has no TPM");
        return Ok(());
    }
    if if_missing {
        match note.trim() {
            "tpm" => return Ok(()),
            "passphrase tpm-refused" => {
                say(
                    "the TPM refused the sealed key: the boot chain changed since it was \
                     sealed. Run `hide tpm-enroll` to seal it to this one.",
                );
                return Ok(());
            }
            _ => {}
        }
    }

    let key = volume_key()?;
    let mut tpm = Device(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(TPM)
            .context("opening the TPM")?,
    );
    let sealed = seal(&mut tpm, &key, PCRS).context("sealing the key")?;
    // Unsealed again before it is kept: a blob this TPM cannot open would
    // only be found out at the next boot, as a passphrase prompt.
    let back = unseal(&mut tpm, &sealed).context("checking the seal")?;
    ensure!(
        back.as_deref() == Some(&key[..]),
        "the TPM did not give back the key it sealed"
    );

    let esp = Esp::mount()?;
    let target = esp.path().join(SEALED_PATH);
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir)?;
    }
    let staged = target.with_extension("tmp");
    let mut file = File::create(&staged)?;
    file.write_all(&sealed.to_bytes())?;
    file.sync_all()?;
    drop(file);
    rename_durably(&staged, &target)?;
    esp.unmount()?;
    say("the disk's key is sealed to this TPM, for this boot chain (PCR 7)");
    Ok(())
}

/// The volume key of the mapped root, from its dm-crypt table.
fn volume_key() -> Result<Zeroizing<Vec<u8>>> {
    let out = Command::new("dmsetup")
        .args(["table", "--showkeys", MAPPED_NAME])
        .output()
        .context("running dmsetup")?;
    ensure!(
        out.status.success(),
        "dmsetup: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let table = Zeroizing::new(String::from_utf8_lossy(&out.stdout).into_owned());
    // start length crypt cipher key iv_offset device offset ...
    let hex = table
        .split_whitespace()
        .nth(4)
        .context("dmsetup printed no key")?;
    ensure!(
        hex.len() % 2 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "the root's table has no literal key"
    );
    let mut key = Zeroizing::new(Vec::with_capacity(hex.len() / 2));
    for pair in hex.as_bytes().chunks(2) {
        let text = std::str::from_utf8(pair)?;
        key.push(u8::from_str_radix(text, 16)?);
    }
    Ok(key)
}

/// How the root opens, for `hide status`.
pub fn describe() -> String {
    let Ok(note) = fs::read_to_string(UNLOCK_NOTE) else {
        return "not encrypted".to_owned();
    };
    let sealed = sealed().is_some();
    let opened = match note.trim() {
        "tpm" => "opened by the TPM",
        "passphrase tpm-refused" => "opened by passphrase: the TPM refused its sealed key",
        _ => "opened by passphrase",
    };
    format!(
        "encrypted (LUKS2), {opened}; {}",
        if sealed {
            "key sealed to the TPM"
        } else {
            "no key sealed to a TPM"
        }
    )
}

fn sealed() -> Option<Sealed> {
    let esp = Esp::mount().ok()?;
    let bytes = fs::read(esp.path().join(SEALED_PATH)).ok();
    let _ = esp.unmount();
    Sealed::from_bytes(&bytes?).ok()
}

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

fn say(line: &str) {
    eprintln!("hide tpm-enroll: {line}");
}

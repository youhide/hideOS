//! `hide secureboot status | enroll`: the firmware's Secure Boot state, and
//! hideOS's keys enrolled in it. See ARCHITECTURE.md, "Security".
//!
//! The keys are on the ESP, as the build left them: PK, KEK and db as EFI
//! signature lists, each signed by the key above it — PK by itself —
//! with Microsoft's KEK and db certificates in the lists beside hideOS's.
//! A firmware takes new keys only in setup mode, which on most machines
//! means clearing the factory keys in its setup screen. Written in the
//! order the firmware needs — db and KEK while there is no PK, the PK last,
//! which ends setup mode — through efivarfs.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use crate::deploy::Esp;

const EFIVARS: &str = "/sys/firmware/efi/efivars";
/// EFI_GLOBAL_VARIABLE: PK, KEK, SecureBoot, SetupMode.
const GLOBAL: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
/// EFI_IMAGE_SECURITY_DATABASE_GUID: db, dbx.
const SECURITY_DATABASE: &str = "d719b2cb-3d3a-4596-a3bc-dad00e67656f";
/// Non-volatile, boot and runtime access, time-based authenticated writes.
const ATTRIBUTES: u32 = 0x01 | 0x02 | 0x04 | 0x20;

pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("status") | None => status(),
        Some("enroll") => enroll(),
        _ => bail!("usage: hide secureboot status | enroll"),
    }
}

/// A one-byte boolean variable's value: efivarfs puts the attributes,
/// four bytes, before it.
fn flag(name: &str) -> Option<bool> {
    let data = fs::read(Path::new(EFIVARS).join(format!("{name}-{GLOBAL}"))).ok()?;
    data.get(4).map(|b| *b == 1)
}

fn status() -> Result<()> {
    let state = match (flag("SecureBoot"), flag("SetupMode")) {
        (None, _) => "not reported: no UEFI, or no Secure Boot",
        (Some(true), _) => "on",
        (Some(false), Some(true)) => {
            "off, in setup mode: `hide secureboot enroll` takes hideOS's keys"
        }
        (Some(false), _) => "off",
    };
    println!("Secure Boot: {state}");
    Ok(())
}

fn enroll() -> Result<()> {
    ensure!(
        flag("SetupMode") == Some(true),
        "the firmware is not in setup mode: clear its Secure Boot keys in its setup \
         screen first, then run this again"
    );
    let esp = Esp::mount()?;
    let keys = esp.path().join("EFI/hideos/keys");
    let read = |name: &str| -> Result<Vec<u8>> {
        fs::read(keys.join(name))
            .with_context(|| format!("reading {name}: this build carries no keys to enroll"))
    };
    let db = read("db.auth")?;
    let kek = read("KEK.auth")?;
    let pk = read("PK.auth")?;
    esp.unmount()?;
    for (name, guid, data) in [
        ("db", SECURITY_DATABASE, &db),
        ("KEK", GLOBAL, &kek),
        ("PK", GLOBAL, &pk),
    ] {
        let mut value = ATTRIBUTES.to_le_bytes().to_vec();
        value.extend_from_slice(data);
        // One write: efivarfs takes a variable whole, or not at all.
        let path = Path::new(EFIVARS).join(format!("{name}-{guid}"));
        fs::write(&path, &value).with_context(|| format!("writing {name} to the firmware"))?;
        eprintln!("hide secureboot: {name} enrolled");
    }
    ensure!(
        flag("SetupMode") == Some(false),
        "the firmware took the keys but is still in setup mode"
    );
    eprintln!(
        "hide secureboot: hideOS's keys are the firmware's; Secure Boot is on from the next boot"
    );
    Ok(())
}

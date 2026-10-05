//! System extensions: what hideOS publishes beyond the image — a driver, a
//! security fix — merged over /usr at boot. See ARCHITECTURE.md, "System
//! extensions".
//!
//! An extension is a composefs image in the same store as the system, its
//! files checked by fs-verity as the system's are. It is merged only when
//! three things hold: hideOS signed its image digest (with the key compiled
//! into this binary, which the firmware checked as part of the UKI); the
//! image measures to that digest; and the extension says it was built for
//! the system image that is booting. Anything else is left out, with a
//! line on the console — an extension never stops a boot.
//!
//! `hide ext add` and `hide update` write `/hideos/extensions/<name>`, one
//! entry for each system image the extension was built for; see
//! `hidestage::extension`. The entry for the booting system is the one
//! tried.

use std::fs;
use std::path::{Path, PathBuf};

use hidecrypt::signature::PublicKey;
use rustix::fs::CWD;
use rustix::mount::{MountFlags, MoveMountFlags, mount, move_mount};

use crate::boot::{SYSROOT, composefs_mount, open_verified_image, say};

const MOUNTS: &str = "/run/hidestage/extensions";

/// Merges every extension that may be merged. Returns nothing: an
/// extension that is refused is said, not fatal.
pub fn merge(store: &Path, booted: &[u8; 32]) {
    let Ok(entries) = fs::read_dir(store.join("extensions")) else {
        return;
    };
    let Ok(key) = PublicKey::hideos() else {
        say("hidestage: no key to check extensions with; none merged");
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut lower = Vec::new();
    for name in names {
        match prepare(store, &name, booted, &key) {
            Ok(usr) => {
                say(&format!("hidestage: extension {name} merged"));
                lower.push(usr);
            }
            Err(why) => say(&format!("hidestage: extension {name} left out: {why}")),
        }
    }
    if lower.is_empty() {
        return;
    }
    // Extensions over the system's /usr, the first named on top; no upper
    // directory, so the result is as read-only as the system.
    let usr = Path::new(SYSROOT).join("usr");
    let mut dirs: Vec<String> = lower.iter().map(|p| p.display().to_string()).collect();
    dirs.push(usr.display().to_string());
    let options = format!("lowerdir={}", dirs.join(":"));
    let Ok(options) = std::ffi::CString::new(options) else {
        return;
    };
    if let Err(error) = mount(
        "overlay",
        &usr,
        "overlay",
        MountFlags::RDONLY,
        Some(options.as_c_str()),
    ) {
        say(&format!(
            "hidestage: merging the extensions over /usr: {error}"
        ));
    }
}

/// One extension, checked and mounted; its /usr, for the overlay.
fn prepare(
    store: &Path,
    name: &str,
    booted: &[u8; 32],
    key: &PublicKey,
) -> Result<PathBuf, String> {
    let record = fs::read_to_string(store.join("extensions").join(name))
        .map_err(|e| format!("reading its record: {e}"))?;
    let system = format!("sha256:{}", hidestage::hex(booted));
    let entries = hidestage::extension::parse(&record);
    // None for this system: said with the systems it was built for, as
    // an entry for another one is.
    let entry = hidestage::extension::for_system(&entries, &system).ok_or_else(|| {
        let others: Vec<&str> = entries
            .iter()
            .filter_map(|e| e.built_for.as_deref())
            .collect();
        if others.is_empty() {
            "the machine has no build of it".to_owned()
        } else {
            format!("built for {}, not this system", others.join(", "))
        }
    })?;
    let image = entry.image.as_str();
    let signature = decode_hex(&entry.signature).ok_or("its signature is not hex")?;
    if !key.verify(image.as_bytes(), &signature) {
        return Err("hideOS did not sign it".into());
    }
    let digest: [u8; 32] = image
        .strip_prefix("sha256:")
        .and_then(decode_hex)
        .and_then(|b| b.try_into().ok())
        .ok_or("its image is not sha256:<hex>")?;

    let fd = open_verified_image(store, &digest).map_err(|e| e.to_string())?;
    let mount_fd = composefs_mount(fd, &store.join("objects")).map_err(|e| e.to_string())?;
    let target = Path::new(MOUNTS).join(name);
    fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    move_mount(
        &mount_fd,
        "",
        CWD,
        &target,
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
    )
    .map_err(|e| format!("attaching it: {e}"))?;

    // Built for this system and no other: an extension's files are made
    // against one image's libraries.
    let release = fs::read_to_string(
        target
            .join("usr/lib/extension-release.d")
            .join(format!("extension-release.{name}")),
    )
    .map_err(|_| "it has no extension-release file")?;
    let built_for = release
        .lines()
        .find_map(|l| l.trim().strip_prefix("HIDEOS_IMAGE="))
        .unwrap_or_default();
    if built_for != system {
        let _ = rustix::mount::unmount(&target, rustix::mount::UnmountFlags::DETACH);
        return Err(format!("built for {built_for}, not this system"));
    }
    Ok(target.join("usr"))
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

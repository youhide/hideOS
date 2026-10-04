//! `hide swap`: the swap file, and where a hibernated system will be found.
//! A oneshot unit runs it at every boot, after `hide setup`. See
//! ARCHITECTURE.md, "Disk layout", and hidestage's `resume`.
//!
//! In order: swap on — which also rewrites the signature of an image left
//! from a hibernation that was not resumed, so that a stale image is never
//! found later; then the kernel told where the file is, which hibernation
//! writes to; then the same location in an EFI variable, which is how
//! hidestage finds it before anything is mounted.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

const SWAP_FILE: &str = "/swap/swapfile";
/// See `hidestage::RESUME_VARIABLE`.
const RESUME_VARIABLE: &str =
    "/sys/firmware/efi/efivars/HideosResume-5e1f0c4a-7d2b-4b8e-9a63-1c4d2f8e0b75";
/// Non-volatile, and visible to boot services and at run time.
const ATTRIBUTES: [u8; 4] = [0x07, 0, 0, 0];

pub fn swap() -> Result<()> {
    if !Path::new(SWAP_FILE).exists() {
        say("no swap file");
        return Ok(());
    }
    let swaps = fs::read_to_string("/proc/swaps").unwrap_or_default();
    if !swaps.lines().any(|l| l.starts_with(SWAP_FILE)) {
        run(Command::new("swapon").arg(SWAP_FILE))?;
    }

    // The file's first page, in pages from the start of the partition:
    // btrfs maps it, the kernel's own fiemap is not the physical offset.
    let offset = output(
        Command::new("btrfs")
            .args(["inspect-internal", "map-swapfile", "-r"])
            .arg(SWAP_FILE),
    )?;
    let offset: u64 = offset
        .trim()
        .parse()
        .with_context(|| format!("btrfs gave `{}` as the offset", offset.trim()))?;
    let device = root_device()?;
    fs::write("/sys/power/resume_offset", offset.to_string())
        .context("writing /sys/power/resume_offset")?;
    fs::write("/sys/power/resume", &device).context("writing /sys/power/resume")?;

    let mut value = ATTRIBUTES.to_vec();
    value.extend_from_slice(offset.to_string().as_bytes());
    if fs::read(RESUME_VARIABLE).ok().as_deref() != Some(&value[..]) {
        write_variable(&value)?;
    }
    say(&format!(
        "swap on; hibernation resumes from {device}+{offset}"
    ));
    Ok(())
}

/// The block device /swap is on, as MAJOR:MINOR. A file's st_dev on
/// btrfs is the subvolume's anonymous device, not the disk's, so the
/// device comes from the mount table.
fn root_device() -> Result<String> {
    let mounts = fs::read_to_string("/proc/self/mountinfo")?;
    let source = mounts
        .lines()
        .find_map(|line| {
            let mut halves = line.split(" - ");
            let left: Vec<&str> = halves.next()?.split(' ').collect();
            let right: Vec<&str> = halves.next()?.split(' ').collect();
            (left.get(4) == Some(&"/swap")).then(|| right.get(1).map(|s| (*s).to_owned()))?
        })
        .context("/swap is not mounted")?;
    let rdev = fs::metadata(&source)
        .with_context(|| format!("reading {source}"))?
        .rdev();
    Ok(format!(
        "{}:{}",
        rustix::fs::major(rdev),
        rustix::fs::minor(rdev)
    ))
}

/// The resume variable, through efivarfs made writable for the write.
fn write_variable(value: &[u8]) -> Result<()> {
    crate::efivars::writable(|| crate::efivars::write(Path::new(RESUME_VARIABLE), value))
        .context("writing the resume variable")
}

fn run(command: &mut Command) -> Result<()> {
    let status = command.status().with_context(|| format!("{command:?}"))?;
    if !status.success() {
        bail!("{command:?} exited with {status}");
    }
    Ok(())
}

fn output(command: &mut Command) -> Result<String> {
    let out = command.output().with_context(|| format!("{command:?}"))?;
    if !out.status.success() {
        bail!(
            "{command:?} exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn say(line: &str) {
    eprintln!("hide swap: {line}");
}

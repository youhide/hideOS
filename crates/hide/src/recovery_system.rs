//! `hide recovery`: the recovery system's init. hideBoot boots it from
//! `\EFI\Recovery` when it is chosen from the menu, or when no deployment
//! will start. It runs from memory — Minimal, whole, in the initramfs — so
//! nothing on the disk has to work for it to.
//!
//! What it offers: choose which system boots next, which is a rollback a
//! person picks rather than one the boot counter made; a shell, with the
//! disk opened and mounted; restart; turn off.

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use hide::deployment::{self, Uki};
use rustix::mount::{MountFlags, UnmountFlags, mount, unmount};

use crate::install::mount_pseudo_filesystems;

const ESP_NAME: &str = "hideos-esp";
const ROOT_NAME: &str = "hideos-root";
const ESP_MOUNT: &str = "/run/hide-recovery/esp";
const DISK_MOUNT: &str = "/mnt";

pub fn run() -> Result<()> {
    if rustix::process::getpid().is_init() {
        mount_pseudo_filesystems()?;
    }
    loop {
        println!();
        println!("hideOS recovery");
        println!();
        println!("  1) Choose the system that starts next");
        println!("  2) Open a shell, with the disk at {DISK_MOUNT}");
        println!("  3) Restart");
        println!("  4) Turn off");
        println!();
        let choice = ask("Choose: ")?;
        let outcome = match choice.as_str() {
            "1" => choose_next(),
            "2" => shell(),
            "3" => finish(rustix::system::RebootCommand::Restart),
            "4" => finish(rustix::system::RebootCommand::PowerOff),
            _ => continue,
        };
        if let Err(error) = outcome {
            println!("  {error:#}");
        }
    }
}

/// The deployments on the ESP, and the one chosen made the one that starts:
/// marked good, and every one that would start before it marked bad. Bad
/// is not gone — each is still in hideBoot's menu.
fn choose_next() -> Result<()> {
    let esp = partition_named(ESP_NAME).context("no hideOS is installed on this machine")?;
    fs::create_dir_all(ESP_MOUNT)?;
    mount(&esp, ESP_MOUNT, "vfat", MountFlags::empty(), Some(c"quiet"))
        .context("mounting the ESP")?;
    let result = (|| -> Result<()> {
        let linux = Path::new(ESP_MOUNT).join("EFI/Linux");
        let mut ukis: Vec<Uki> = fs::read_dir(&linux)?
            .flatten()
            .filter_map(|e| Uki::parse(&e.file_name().to_string_lossy()))
            .collect();
        deployment::boot_order(&mut ukis);
        if ukis.is_empty() {
            bail!("there is no system on the ESP");
        }
        println!();
        for (i, uki) in ukis.iter().enumerate() {
            println!(
                "  {}) {} {:<6} {}  {}{}",
                i + 1,
                uki.edition,
                uki.version,
                uki.digest,
                uki.state(),
                if i == 0 { ", starts next" } else { "" }
            );
        }
        println!();
        let answer = ask("Which one should start next? ")?;
        let Some(index) = answer.parse::<usize>().ok().and_then(|n| n.checked_sub(1)) else {
            return Ok(());
        };
        let Some(chosen) = ukis.get(index).cloned() else {
            return Ok(());
        };
        for uki in ukis.iter().take(index) {
            rename(&linux, uki, &uki.bad())?;
        }
        rename(&linux, &chosen, &chosen.good())?;
        rustix::fs::sync();
        println!("  {} {} starts next.", chosen.edition, chosen.version);
        Ok(())
    })();
    let _ = unmount(ESP_MOUNT, UnmountFlags::empty());
    result
}

fn rename(dir: &Path, from: &Uki, to: &Uki) -> Result<()> {
    if from.file_name() != to.file_name() {
        fs::rename(dir.join(from.file_name()), dir.join(to.file_name()))?;
    }
    Ok(())
}

/// A shell on the console, with the disk opened — asking for its
/// passphrase when it is encrypted — and mounted, all subvolumes, at /mnt.
fn shell() -> Result<()> {
    if let Some(root) = partition_named(ROOT_NAME) {
        let device = open_disk(&root)?;
        fs::create_dir_all(DISK_MOUNT)?;
        match mount(&device, DISK_MOUNT, "btrfs", MountFlags::empty(), None) {
            Ok(()) => println!("  The disk is at {DISK_MOUNT}: @home, @etc, @var, @store."),
            Err(error) => println!("  The disk could not be mounted: {error}"),
        }
    } else {
        println!("  No hideOS disk on this machine.");
    }
    println!("  Leave the shell to come back here.");
    let _ = Command::new("/usr/bin/zsh")
        .arg("-l")
        .env("HOME", "/var/root")
        .status();
    let _ = unmount(DISK_MOUNT, UnmountFlags::DETACH);
    Ok(())
}

/// The root's device, opened with cryptsetup when it is LUKS2.
fn open_disk(partition: &Path) -> Result<PathBuf> {
    let file = fs::File::open(partition)?;
    if hidecrypt::luks2::read_header(&file).is_err() {
        return Ok(partition.to_path_buf());
    }
    let mapped = Path::new("/dev/mapper/hideos-root");
    if !mapped.exists() {
        println!("  The disk is encrypted: its passphrase, or the recovery key.");
        let status = Command::new("cryptsetup")
            .arg("open")
            .arg(partition)
            .arg("hideos-root")
            .status()
            .context("running cryptsetup")?;
        if !status.success() {
            bail!("the disk stays locked");
        }
    }
    Ok(mapped.to_path_buf())
}

fn finish(command: rustix::system::RebootCommand) -> Result<()> {
    let _ = unmount(DISK_MOUNT, UnmountFlags::DETACH);
    let _ = Command::new("cryptsetup")
        .args(["close", "hideos-root"])
        .status();
    rustix::fs::sync();
    rustix::system::reboot(command).context("the firmware refused")?;
    Ok(())
}

fn partition_named(label: &str) -> Option<PathBuf> {
    for entry in fs::read_dir("/sys/class/block").ok()?.flatten() {
        let uevent = fs::read_to_string(entry.path().join("uevent")).unwrap_or_default();
        let field = |key: &str| uevent.lines().find_map(|l| l.strip_prefix(key));
        if field("PARTNAME=") == Some(label)
            && let Some(dev) = field("DEVNAME=")
        {
            return Some(Path::new("/dev").join(dev));
        }
    }
    None
}

fn ask(question: &str) -> Result<String> {
    print!("{question}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

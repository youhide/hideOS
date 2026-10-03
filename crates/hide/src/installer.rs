//! `hide installer`: the installer a person meets, on the console of the
//! installer image. It asks the questions `hide install` takes as
//! arguments — which disk, a passphrase, the first account — makes a
//! recovery key, and installs from the payload partition of the medium it
//! booted from. See ARCHITECTURE.md, "Install and recover", and ROADMAP H7.
//!
//! Runs as PID 1 of the installer image, like `hide install`: there is
//! nothing else on that system.

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};

use crate::install::{Options, install, memory_mib, mount_pseudo_filesystems};

/// The installer medium's partition holding the payload, raw.
const PAYLOAD_NAME: &str = "hideos-payload";

pub fn run() -> Result<()> {
    let pid1 = rustix::process::getpid().is_init();
    if pid1 {
        mount_pseudo_filesystems()?;
    }
    let result = interact();
    if let Err(error) = &result {
        println!();
        println!("The installation failed: {error:#}");
    }
    if pid1 {
        println!();
        ask("Press Enter to turn the machine off.")?;
        rustix::fs::sync();
        let _ = rustix::system::reboot(rustix::system::RebootCommand::PowerOff);
    }
    result
}

fn interact() -> Result<()> {
    println!();
    println!("hideOS installer");
    println!();
    let payload = partition_named(PAYLOAD_NAME)
        .context("this medium has no hideos-payload partition: it is not a hideOS installer")?;
    let medium = whole_disk(&payload);

    let disks = disks(medium.as_deref())?;
    if disks.is_empty() {
        bail!("there is no disk to install on");
    }
    println!("Disks:");
    for (i, disk) in disks.iter().enumerate() {
        println!(
            "  {}) {:<8} {:>9}  {}",
            i + 1,
            disk.name,
            human(disk.bytes),
            disk.model
        );
    }
    let disk = loop {
        let answer = ask(&format!("Install on which disk? [1-{}]: ", disks.len()))?;
        let choice = if answer.is_empty() {
            Some(1)
        } else {
            answer.parse::<usize>().ok()
        };
        if let Some(disk) = choice.and_then(|n| disks.get(n.wrapping_sub(1))) {
            break disk;
        }
    };
    println!();
    println!("Everything on {} will be erased.", disk.name);
    if ask("Type `erase` to continue: ")? != "erase" {
        bail!("nothing was changed");
    }

    println!();
    let encrypt = !matches!(
        ask("Encrypt the disk? [Y/n]: ")?.to_lowercase().as_str(),
        "n" | "no"
    );
    let passphrase = if encrypt {
        Some(new_secret("Disk passphrase")?)
    } else {
        None
    };
    let recovery_key = if encrypt {
        let mut random = [0u8; 25];
        fs::File::open("/dev/urandom")
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut random))
            .context("reading /dev/urandom")?;
        Some(hide::recovery::format_key(&random))
    } else {
        None
    };

    println!();
    let name = loop {
        let name = ask("Your login name: ")?;
        if valid_login(&name) {
            break name;
        }
        println!("  Lowercase letters, digits, - and _, starting with a letter.");
    };
    let password = new_secret("Your password")?;

    println!();
    let options = Options {
        payload,
        disk: Path::new("/dev").join(&disk.name),
        poweroff: false,
        user: Some((name, password)),
        encrypt: passphrase,
        recovery_key: recovery_key.clone(),
        swap_mib: Some(memory_mib()?),
    };
    install(&options)?;

    println!();
    println!("hideOS is installed.");
    if let Some(key) = recovery_key {
        println!();
        println!("Your recovery key opens the disk if the passphrase is lost.");
        println!("Write it down and keep it away from this machine:");
        println!();
        println!("    {key}");
        println!();
        ask("Press Enter once it is written down.")?;
    }
    Ok(())
}

struct Disk {
    name: String,
    bytes: u64,
    model: String,
}

/// Disks a person might install on: not the medium, not RAM, loop or
/// optical devices, nothing of zero size.
fn disks(medium: Option<&str>) -> Result<Vec<Disk>> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/sys/block")?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if ["loop", "ram", "zram", "sr", "dm-", "md"]
            .iter()
            .any(|p| name.starts_with(p))
            || Some(name.as_str()) == medium
        {
            continue;
        }
        let sectors: u64 = fs::read_to_string(entry.path().join("size"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        if sectors == 0 {
            continue;
        }
        let model = fs::read_to_string(entry.path().join("device/model"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        found.push(Disk {
            name,
            bytes: sectors * 512,
            model,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
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

/// The disk a partition is on: sysfs puts a partition under its disk.
fn whole_disk(partition: &Path) -> Option<String> {
    let name = partition.file_name()?.to_string_lossy().into_owned();
    let link = fs::canonicalize(format!("/sys/class/block/{name}")).ok()?;
    Some(link.parent()?.file_name()?.to_string_lossy().into_owned())
}

fn valid_login(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && name.len() <= 32
}

fn human(bytes: u64) -> String {
    let gib = bytes as f64 / f64::from(1u32 << 30);
    if gib >= 1024.0 {
        format!("{:.1} TiB", gib / 1024.0)
    } else {
        format!("{gib:.1} GiB")
    }
}

/// A line from the console.
fn ask(question: &str) -> Result<String> {
    print!("{question}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

/// A secret, typed twice without echo.
fn new_secret(what: &str) -> Result<String> {
    loop {
        let first = ask_hidden(&format!("{what}: "))?;
        if first.is_empty() {
            println!("  It cannot be empty.");
            continue;
        }
        let second = ask_hidden(&format!("{what}, again: "))?;
        if first == second {
            return Ok(first);
        }
        println!("  They differ; once more.");
    }
}

fn ask_hidden(question: &str) -> Result<String> {
    let stdin = std::io::stdin();
    let saved = tcgetattr(&stdin).ok();
    if let Some(mut quiet) = saved.clone() {
        quiet.local_modes.remove(LocalModes::ECHO);
        let _ = tcsetattr(&stdin, OptionalActions::Now, &quiet);
    }
    let answer = ask(question);
    if let Some(saved) = saved {
        let _ = tcsetattr(&stdin, OptionalActions::Now, &saved);
    }
    println!();
    answer
}

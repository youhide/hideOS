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

use crate::install::{
    Options, install, is_luks, memory_mib, mount_pseudo_filesystems, opens, partition_on,
};

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
    // What each disk holds first, as Boot Camp Assistant shows it: the
    // person chooses knowing what is on it.
    println!("Disks:");
    for (i, disk) in disks.iter().enumerate() {
        println!(
            "  {}) {:<8} {:>9}  {:<24} {}",
            i + 1,
            disk.name,
            human(disk.bytes),
            disk.model,
            disk.holds.describe()
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
    let target = Path::new("/dev").join(&disk.name);

    // hideOS already there: a reinstall, unless the person says otherwise,
    // which keeps their files and replaces the rest.
    let existing_root =
        partition_on(&target, "hideos-esp").and(partition_on(&target, "hideos-root"));
    println!();
    let keep_home = match &existing_root {
        Some(_) => {
            println!("hideOS is already installed on {}.", disk.name);
            !matches!(
                ask("Reinstall it, keeping /home? Everything else is replaced. [Y/n]: ")?
                    .to_lowercase()
                    .as_str(),
                "n" | "no"
            )
        }
        None => false,
    };
    let beside = if keep_home {
        false
    } else if disk.holds.has_windows() {
        beside_windows(disk, minimum_bytes(&payload))?
    } else {
        match disk.holds {
            hide::disks::Holds::Empty => {
                println!("hideOS will use all of {}.", disk.name)
            }
            _ => println!(
                "Everything on {} — {} — will be erased.",
                disk.name,
                disk.holds.describe()
            ),
        }
        if ask("Type `erase` to continue: ")? != "erase" {
            bail!("nothing was changed");
        }
        false
    };
    let bitlocker = disks.iter().any(|d| d.holds.bitlocker());
    // Windows anywhere on this machine — this disk, or another — keeps the
    // hardware clock in local time, and so must hideOS beside it.
    let clock_local = disks.iter().any(|d| d.holds.has_windows());
    let extensions = extensions_for_this_machine();

    // The Workstation sets up at its first boot, as a Mac does: the person,
    // their passphrase and the recovery key are hidesetup's. Minimal has
    // no screen for that, and asks here.
    let edition = payload_edition(&payload).unwrap_or_else(|| "minimal".to_owned());
    if edition != "minimal" {
        return install_for_setup(
            payload,
            target,
            Placement {
                keep_home,
                beside,
                bitlocker,
                clock_local,
                extensions,
            },
            existing_root.as_deref(),
        );
    }

    println!();
    let (passphrase, encrypt) = match existing_root.as_ref().filter(|_| keep_home) {
        Some(root) if is_luks(root) => loop {
            let answer = ask_hidden("Disk passphrase or recovery key: ")?;
            if opens(root, &answer) {
                break (Some(answer), false);
            }
            println!("  That does not open the disk.");
        },
        Some(_) => (None, false),
        None => {
            let encrypt = !matches!(
                ask("Encrypt the disk? [Y/n]: ")?.to_lowercase().as_str(),
                "n" | "no"
            );
            if encrypt {
                (Some(new_secret("Disk passphrase")?), true)
            } else {
                (None, false)
            }
        }
    };
    // Only for a disk encrypted now: a reinstall keeps the key there is.
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
        disk: target,
        poweroff: false,
        user: Some((name, password)),
        encrypt: passphrase,
        recovery_key: recovery_key.clone(),
        swap_mib: Some(memory_mib()?),
        keep_home,
        setup_key: false,
        beside,
        clock_local,
        extensions,
    };
    install(&options)?;
    copy_recovery()?;
    enroll_secure_boot(bitlocker)?;

    println!();
    if keep_home {
        println!("hideOS is installed. /home is as it was.");
    } else {
        println!("hideOS is installed.");
    }
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

/// The Workstation's install: the disk, encrypted with a setup key unless
/// the person says not to, and nothing personal — the first boot asks for
/// the rest. A reinstall keeps the disk's passphrase and recovery key, so
/// it opens with them; setup then asks only for the account.
fn install_for_setup(
    payload: PathBuf,
    target: PathBuf,
    placement: Placement,
    existing_root: Option<&Path>,
) -> Result<()> {
    let Placement {
        keep_home,
        beside,
        bitlocker,
        clock_local,
        extensions,
    } = placement;
    println!();
    let (encrypt, setup_key) = match existing_root.filter(|_| keep_home) {
        Some(root) if is_luks(root) => loop {
            let answer = ask_hidden("Disk passphrase or recovery key: ")?;
            if opens(root, &answer) {
                break (Some(answer), false);
            }
            println!("  That does not open the disk.");
        },
        Some(_) => (None, false),
        None => {
            let encrypt = !matches!(
                ask("Encrypt the disk? [Y/n]: ")?.to_lowercase().as_str(),
                "n" | "no"
            );
            if encrypt {
                let mut random = [0u8; 32];
                fs::File::open("/dev/urandom")
                    .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut random))
                    .context("reading /dev/urandom")?;
                let key: String = random.iter().map(|b| format!("{b:02x}")).collect();
                (Some(key), true)
            } else {
                (None, false)
            }
        }
    };
    println!();
    let options = Options {
        payload,
        disk: target,
        poweroff: false,
        user: None,
        encrypt,
        recovery_key: None,
        swap_mib: Some(memory_mib()?),
        keep_home,
        setup_key,
        beside,
        clock_local,
        extensions,
    };
    install(&options)?;
    copy_recovery()?;
    enroll_secure_boot(bitlocker)?;
    println!();
    println!("hideOS is installed. Remove the installer and restart:");
    println!("the first start sets it up — language, network and your account.");
    Ok(())
}

/// The edition a payload installs, from the name of the kernel image it
/// carries for the ESP — `hideos-EDITION-VERSION-DIGEST.efi` — found by
/// reading the archive's headers and seeking past everything else.
fn payload_edition(payload: &Path) -> Option<String> {
    let file = fs::File::open(payload).ok()?;
    let mut archive = tar::Archive::new(file);
    for entry in archive.entries_with_seek().ok()? {
        let entry = entry.ok()?;
        let path = entry.path().ok()?.to_string_lossy().into_owned();
        let Some(name) = path
            .strip_prefix("esp/EFI/Linux/hideos-")
            .and_then(|rest| rest.strip_suffix(".efi"))
        else {
            continue;
        };
        // The edition is what comes before the version, the first part
        // that is all digits.
        let parts: Vec<&str> = name.split('-').collect();
        let version = parts
            .iter()
            .position(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))?;
        return Some(parts.get(..version)?.join("-"));
    }
    None
}

/// The recovery system, from the medium onto the new disk's ESP, where
/// hideBoot finds it: `\EFI\Recovery`.
fn copy_recovery() -> Result<()> {
    use rustix::mount::{MountFlags, UnmountFlags, mount, unmount};
    let medium = partition_named("hideos-installer").context("the medium's ESP")?;
    let target = partition_named("hideos-esp").context("the new disk's ESP")?;
    let (from, to) = (
        Path::new("/run/hide-installer/medium"),
        Path::new("/run/hide-installer/esp"),
    );
    fs::create_dir_all(from)?;
    fs::create_dir_all(to)?;
    mount(&medium, from, "vfat", MountFlags::RDONLY, None).context("mounting the medium")?;
    let result = (|| -> Result<()> {
        mount(&target, to, "vfat", MountFlags::empty(), Some(c"quiet"))
            .context("mounting the new ESP")?;
        let dir = to.join("EFI/Recovery");
        fs::create_dir_all(&dir)?;
        let copied = fs::copy(
            from.join("EFI/hideos/recovery.efi"),
            dir.join("hideos-recovery.efi"),
        )
        .map(|_| ())
        .context("copying the recovery system");
        rustix::fs::sync();
        let _ = unmount(to, UnmountFlags::empty());
        copied
    })();
    let _ = unmount(from, UnmountFlags::empty());
    result
}

struct Disk {
    name: String,
    bytes: u64,
    model: String,
    holds: hide::disks::Holds,
    /// The largest free space on it, in bytes: where hideOS goes beside
    /// Windows.
    free: u64,
}

/// Where the install goes, besides which disk.
struct Placement {
    /// A reinstall over hideOS, keeping /home.
    keep_home: bool,
    /// Into the free space beside Windows.
    beside: bool,
    /// A BitLocker volume on some disk of this machine.
    bitlocker: bool,
    /// Windows on this machine: the hardware clock in local time.
    clock_local: bool,
    /// The medium's extensions this machine needs.
    extensions: Option<(PathBuf, Vec<String>)>,
}

/// The extensions on the medium this machine needs: NVIDIA's driver where
/// there is an NVIDIA GPU its open modules drive.
fn extensions_for_this_machine() -> Option<(PathBuf, Vec<String>)> {
    let archive = partition_named("hideos-extensions")?;
    let mut names = Vec::new();
    let mut gpus = Vec::new();
    for device in fs::read_dir("/sys/bus/pci/devices")
        .into_iter()
        .flatten()
        .flatten()
    {
        let read = |file: &str| {
            fs::read_to_string(device.path().join(file))
                .ok()
                .and_then(|text| u32::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok())
        };
        if let (Some(vendor), Some(class), Some(id)) =
            (read("vendor"), read("class"), read("device"))
        {
            gpus.extend(hide::pci::nvidia_gpu(vendor, class, id));
        }
    }
    match gpus.iter().max() {
        Some(hide::pci::Nvidia::Open) => {
            println!("NVIDIA GPU found: its driver is installed with hideOS.");
            names.push("nvidia".to_owned());
        }
        Some(hide::pci::Nvidia::TooOld) => {
            println!("This NVIDIA GPU is older than NVIDIA's open driver supports (it");
            println!("starts with Turing: RTX 20, GTX 16); the desktop runs on the");
            println!("firmware's display instead.");
        }
        None => {}
    }
    Some((archive, names))
}

/// The space hideOS needs on a disk: the system twice over (the running
/// one and an update beside it), a swap file as large as memory for
/// hibernation, and room to work in.
fn minimum_bytes(payload: &Path) -> u64 {
    let payload = fs::metadata(payload)
        .map(|m| m.len())
        .ok()
        .filter(|n| *n > 0)
        .or_else(|| block_bytes(payload))
        .unwrap_or(4 << 30);
    let memory = memory_mib().unwrap_or(8 << 10) << 20;
    payload.saturating_mul(2) + memory + (16 << 30)
}

/// A block device's size, from sysfs: a raw payload partition's.
fn block_bytes(device: &Path) -> Option<u64> {
    let name = device.file_name()?.to_string_lossy().into_owned();
    let sectors: u64 = fs::read_to_string(format!("/sys/class/block/{name}/size"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(sectors * 512)
}

/// A disk with Windows: hideOS beside it in its free space, or the whole
/// disk erased once the person has typed `windows`. Returns whether it
/// goes beside.
fn beside_windows(disk: &Disk, minimum: u64) -> Result<bool> {
    println!("{} holds Windows.", disk.name);
    if disk.free >= minimum {
        println!(
            "hideOS can go in its free space, {}, beside Windows, which stays as it is.",
            human(disk.free)
        );
        println!("  1) Install beside Windows");
        println!("  2) Erase the whole disk, Windows included");
        loop {
            match ask("Which? [1-2]: ")?.as_str() {
                "" | "1" => return Ok(true),
                "2" => break,
                _ => {}
            }
        }
    } else {
        println!(
            "It has {} free, and hideOS needs {} beside it.",
            human(disk.free),
            human(minimum)
        );
        println!("To make room, start Windows, open Disk Management, right-click");
        println!("Windows's volume and choose Shrink Volume; then start this");
        println!("installer again. Windows moves its own files out of the way.");
        println!();
        println!("Or erase the whole disk, Windows included.");
    }
    println!(
        "Everything on {}, Windows included, will be erased.",
        disk.name
    );
    if ask("Type `windows` to erase it, or press Enter to stop: ")? != "windows" {
        bail!("nothing was changed");
    }
    Ok(false)
}

/// hideOS's Secure Boot keys, when the firmware will take them. With
/// BitLocker on this machine, changing them makes Windows ask for its
/// recovery key, so the person is told first and may leave it for later.
fn enroll_secure_boot(bitlocker: bool) -> Result<()> {
    println!();
    if bitlocker && crate::secureboot::setup_mode() {
        println!("Windows on this machine uses BitLocker. Turning Secure Boot on with");
        println!("hideOS's keys changes what the TPM measures, and Windows will then");
        println!("ask once for its BitLocker recovery key. Suspend BitLocker in");
        println!("Windows first (Control Panel, BitLocker, Suspend protection), or");
        println!("have the recovery key at hand: account.microsoft.com/devicekey.");
        let enroll = !matches!(
            ask("Turn Secure Boot on with hideOS's keys now? [Y/n]: ")?
                .to_lowercase()
                .as_str(),
            "n" | "no"
        );
        if !enroll {
            println!("Secure Boot left as it is; `hide secureboot enroll` turns it on later.");
            return Ok(());
        }
    }
    match crate::secureboot::run(&["enroll".to_owned()]) {
        Ok(()) => println!("Secure Boot is on with hideOS's keys from the next start."),
        Err(why) => println!("Secure Boot keys not enrolled: {why:#}"),
    }
    Ok(())
}

/// A partition's first sector, where NTFS and BitLocker say what they are.
fn boot_sector(node: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut sector = vec![0u8; 512];
    fs::File::open(node).ok()?.read_exact(&mut sector).ok()?;
    Some(sector)
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
        let device = Path::new("/dev").join(&name);
        let table = crate::install::table(&device).ok();
        let holds = table
            .as_ref()
            .map(|t| {
                hide::disks::holds(t, |p| {
                    boot_sector(&p.node)
                        .as_deref()
                        .and_then(hide::disks::volume)
                })
            })
            .unwrap_or(hide::disks::Holds::Empty);
        // A disk with no GPT at all is all free to an install that erases
        // it, and none beside.
        // hideOS's own partitions count as free: an install beside
        // Windows replaces them.
        let free = table
            .as_ref()
            .and_then(|t| {
                let mut without = t.clone();
                without
                    .partitions
                    .retain(|p| p.name != "hideos-esp" && p.name != "hideos-root");
                hide::disks::largest_free(&without).map(|f| f.bytes(&without))
            })
            .unwrap_or(0);
        found.push(Disk {
            name,
            bytes: sectors * 512,
            model,
            holds,
            free,
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

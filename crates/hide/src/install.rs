//! `hide install`: put a payload on a disk. See ARCHITECTURE.md, "Disk
//! layout" and "Disk images are installed, not assembled".
//!
//! The same code installs hideOS on a real machine (H7) and writes the disk
//! images the build produces, by running as PID 1 of hideOS Minimal in QEMU.
//! So it handles being PID 1: it mounts what it needs, and powers off when
//! asked rather than exiting, which would panic the kernel.

use std::fs;
use std::io::Read;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use composefs_ioctls::fsverity::{EnableVerityError, fs_ioc_enable_verity, fs_ioc_measure_verity};
use rustix::fs::{Mode, OFlags};
use rustix::mount::{MountFlags, UnmountFlags, mount, unmount};

/// GPT partition type GUIDs, from the Discoverable Partitions Specification.
const ESP_TYPE: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
fn root_type() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "B921B045-1DF0-41C3-AF44-4C6F280D3FAE"
    } else {
        "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709"
    }
}
/// Partition names hidestage looks for.
const ESP_NAME: &str = "hideos-esp";
const ROOT_NAME: &str = "hideos-root";
/// Subvolumes on the root partition. See ARCHITECTURE.md, "Disk layout".
const SUBVOLUMES: &[&str] = &["@store", "@etc", "@var", "@home"];
/// Where the payload's top-level directories go.
const PAYLOAD_DIRS: &[(&str, &str)] = &[("repo", "@store"), ("etc", "@etc")];
const WORK: &str = "/run/hide-install";
/// fs-verity: SHA-256 over 4 KiB blocks, what composefs repositories use.
const VERITY_SHA256: u8 = 1;
const VERITY_BLOCK: u32 = 4096;

struct Options {
    payload: PathBuf,
    disk: PathBuf,
    poweroff: bool,
}

pub fn run(args: &[String]) -> Result<()> {
    let options = parse(args)?;
    let pid1 = rustix::process::getpid().is_init();
    if pid1 {
        mount_pseudo_filesystems()?;
    }
    let result = install(&options);
    if let Err(error) = &result {
        eprintln!("hide install: FAILED: {error:#}");
    }
    if pid1 || options.poweroff {
        // PID 1 cannot exit. Whatever happened has been said; power off, and
        // whoever started the machine reads the console.
        rustix::fs::sync();
        let _ = rustix::system::reboot(rustix::system::RebootCommand::PowerOff);
    }
    result
}

fn parse(args: &[String]) -> Result<Options> {
    let mut payload = None;
    let mut disk = None;
    let mut poweroff = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--payload" => payload = iter.next().map(PathBuf::from),
            "--disk" => disk = iter.next().map(PathBuf::from),
            "--poweroff" => poweroff = true,
            other => bail!("unknown argument `{other}`"),
        }
    }
    Ok(Options {
        payload: payload.context("--payload FILE is required")?,
        disk: disk.context("--disk DEVICE is required")?,
        poweroff,
    })
}

fn install(options: &Options) -> Result<()> {
    let disk = &options.disk;
    say(&format!("partitioning {}", disk.display()));
    partition(disk)?;
    let esp = wait_for_partition(disk, 1)?;
    let root = wait_for_partition(disk, 2)?;

    say("creating filesystems");
    exec(
        Command::new("mkfs.vfat")
            .args(["-F", "32", "-n", "HIDEOS-ESP"])
            .arg(&esp),
    )?;
    exec(
        Command::new("mkfs.btrfs")
            .args(["-f", "-q", "-L", "hideos"])
            .arg(&root),
    )?;

    let root_mount = Path::new(WORK).join("root");
    let esp_mount = Path::new(WORK).join("esp");
    fs::create_dir_all(&root_mount)?;
    fs::create_dir_all(&esp_mount)?;
    mount(&root, &root_mount, "btrfs", MountFlags::NOATIME, None)
        .with_context(|| format!("mounting {}", root.display()))?;
    mount(&esp, &esp_mount, "vfat", MountFlags::empty(), None)
        .with_context(|| format!("mounting {}", esp.display()))?;
    for subvolume in SUBVOLUMES {
        exec(
            Command::new("btrfs")
                .args(["-q", "subvolume", "create"])
                .arg(root_mount.join(subvolume)),
        )?;
    }

    say(&format!("unpacking {}", options.payload.display()));
    let digest = unpack(&options.payload, &root_mount, &esp_mount)?;

    say("enabling fs-verity on every object");
    let objects = root_mount.join("@store/objects");
    let count = enable_verity(&objects)?;
    say(&format!("  {count} objects sealed"));

    // The check hidestage makes at every boot, made once here, so that a
    // payload that would not boot is a failed install, not a failed boot.
    let image = root_mount.join("@store/images").join(&digest);
    let file = rustix::fs::open(&image, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
        .with_context(|| format!("opening {}", image.display()))?;
    let measured: [u8; 32] = fs_ioc_measure_verity(file.as_fd(), VERITY_SHA256)
        .map_err(|e| anyhow::anyhow!("measuring the image: {e}"))?;
    ensure!(
        hex(&measured) == digest,
        "the image measures sha256:{}, but the payload says sha256:{digest}",
        hex(&measured)
    );
    say(&format!("image sealed: sha256:{digest}"));

    rustix::fs::sync();
    unmount(&esp_mount, UnmountFlags::empty()).context("unmounting the ESP")?;
    unmount(&root_mount, UnmountFlags::empty()).context("unmounting the root")?;
    say("installed");
    Ok(())
}

/// A GPT with the ESP first and the root after it, filling the disk. Names
/// and types are what hidestage and the firmware look for.
fn partition(disk: &Path) -> Result<()> {
    let script = format!(
        "label: gpt\n\
         size=512MiB, type={ESP_TYPE}, name=\"{ESP_NAME}\"\n\
         type={}, name=\"{ROOT_NAME}\"\n",
        root_type()
    );
    let mut child = Command::new("sfdisk")
        .args(["--quiet", "--wipe", "always", "--wipe-partitions", "always"])
        .arg(disk)
        .stdin(Stdio::piped())
        .spawn()
        .context("running sfdisk")?;
    use std::io::Write;
    child
        .stdin
        .take()
        .context("sfdisk's stdin")?
        .write_all(script.as_bytes())?;
    ensure!(
        child.wait()?.success(),
        "sfdisk failed on {}",
        disk.display()
    );
    Ok(())
}

/// The device node of partition `number` of `disk`, once the kernel has
/// created it: `/dev/vda` → `/dev/vda1`, `/dev/nvme0n1` → `/dev/nvme0n1p1`.
fn wait_for_partition(disk: &Path, number: u32) -> Result<PathBuf> {
    let name = disk.to_string_lossy();
    let separator = if name.ends_with(|c: char| c.is_ascii_digit()) {
        "p"
    } else {
        ""
    };
    let path = PathBuf::from(format!("{name}{separator}{number}"));
    let started = Instant::now();
    while !path.exists() {
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "{} did not appear after partitioning",
            path.display()
        );
        thread::sleep(Duration::from_millis(100));
    }
    Ok(path)
}

/// Extracts the payload: `repo/` into @store, `etc/` into @etc, `esp/` onto
/// the ESP. Returns the image digest from `image.digest`.
fn unpack(payload: &Path, root: &Path, esp: &Path) -> Result<String> {
    let file = fs::File::open(payload).with_context(|| format!("opening {}", payload.display()))?;
    let mut archive = tar::Archive::new(file);
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    archive.set_unpack_xattrs(true);
    let mut digest = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let mut components = path.components();
        let top = components
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        let rest: PathBuf = components.collect();
        if top == "image.digest" {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            digest = Some(
                text.trim()
                    .strip_prefix("sha256:")
                    .context("image.digest is not sha256:<hex>")?
                    .to_owned(),
            );
            continue;
        }
        let base = match top.as_str() {
            "esp" => esp.to_path_buf(),
            other => match PAYLOAD_DIRS.iter().find(|(dir, _)| *dir == other) {
                Some((_, subvolume)) => root.join(subvolume),
                None => bail!("unexpected `{}` in the payload", path.display()),
            },
        };
        if rest.as_os_str().is_empty() {
            continue;
        }
        let target = base.join(&rest);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        entry
            .unpack(&target)
            .with_context(|| format!("unpacking {}", path.display()))?;
    }
    digest.context("the payload has no image.digest")
}

/// Enables fs-verity on every regular file under `objects`. Each file must
/// be closed for writing, which, freshly unpacked and synced, they are.
fn enable_verity(objects: &Path) -> Result<usize> {
    rustix::fs::sync();
    let mut count = 0;
    let mut pending = vec![objects.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                let file = rustix::fs::open(
                    entry.path(),
                    OFlags::RDONLY | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .with_context(|| format!("opening {}", entry.path().display()))?;
                match fs_ioc_enable_verity(file.as_fd(), VERITY_SHA256, VERITY_BLOCK) {
                    Ok(()) | Err(EnableVerityError::AlreadyEnabled) => count += 1,
                    Err(error) => bail!("fs-verity on {}: {error}", entry.path().display()),
                }
            }
        }
    }
    Ok(count)
}

fn mount_pseudo_filesystems() -> Result<()> {
    for (source, target, fstype) in [
        ("proc", "/proc", "proc"),
        ("sysfs", "/sys", "sysfs"),
        ("devtmpfs", "/dev", "devtmpfs"),
        ("tmpfs", "/run", "tmpfs"),
    ] {
        fs::create_dir_all(target)?;
        match mount(source, target, fstype, MountFlags::empty(), None) {
            Ok(()) | Err(rustix::io::Errno::BUSY) => {}
            Err(error) => return Err(error).with_context(|| format!("mounting {target}")),
        }
    }
    Ok(())
}

fn exec(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("running {:?}", command.get_program()))?;
    ensure!(status.success(), "{command:?} failed: {status}");
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn say(line: &str) {
    eprintln!("hide install: {line}");
}

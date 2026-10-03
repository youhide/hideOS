//! Deployments on a running system: `hide update`, `boot-ok`, `status` and
//! `rollback`. See ARCHITECTURE.md, "Updates", and `hide::deployment` for
//! the names on the ESP.
//!
//! An update becomes bootable at one point: the rename of its UKI into
//! `EFI/Linux` under a name ending in `.efi`. Everything before writes what
//! that UKI will need, and nothing the running system or its way back uses;
//! everything after removes what no kept deployment needs. A power cut at
//! any instant leaves a machine that boots — the old deployment before the
//! rename, the new one, with its attempts, after it.
//!
//! The update's steps are on the boot path, so the boot path's rules hold
//! here: no panics, errors as values.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use composefs_ioctls::fsverity::{EnableVerityError, fs_ioc_enable_verity, fs_ioc_measure_verity};
use hide::deployment::{self, Uki};
use rustix::fs::{Mode, OFlags};
use rustix::mount::{MountFlags, UnmountFlags, mount, unmount};

/// The object store, as hidestage binds it into the running system.
const STORE: &str = "/hideos";
/// Where the ESP is mounted while an operation needs it, and only then: a
/// mounted ESP is one a crash can leave dirty.
const ESP_MOUNT: &str = "/run/hide/esp";
const ESP_NAME: &str = "hideos-esp";
/// systemd-boot's variables: which UKI it booted, with its counter.
const BOOT_COUNT_PATH: &str =
    "/sys/firmware/efi/efivars/LoaderBootCountPath-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
const VERITY_SHA256: u8 = 1;
const VERITY_BLOCK: u32 = 4096;

/// The steps of an update, in order, by the names `--crash-after` takes.
const STEPS: &[&str] = &["unpack", "seal", "stage", "commit", "prune"];

pub fn update(args: &[String]) -> Result<()> {
    let mut payload = None;
    let mut crash_after = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--payload" => payload = iter.next().map(PathBuf::from),
            // For tests: cut the power after a step, to show that it does
            // not matter where an update is interrupted.
            "--crash-after" => crash_after = iter.next().cloned(),
            other => bail!("unknown argument `{other}`"),
        }
    }
    let payload = payload.context("usage: hide update --payload FILE")?;
    if let Some(step) = &crash_after {
        ensure!(
            STEPS.contains(&step.as_str()),
            "--crash-after takes one of {}",
            STEPS.join(", ")
        );
    }
    let crash = |step: &str| -> Result<()> {
        if crash_after.as_deref() == Some(step) {
            say(&format!("cutting the power after `{step}`"));
            // Reboot now, without syncing: what a power cut leaves.
            fs::write("/proc/sysrq-trigger", "b").context("writing /proc/sysrq-trigger")?;
        }
        Ok(())
    };

    let booted = booted_digest()?;
    let edition = booted_edition()?;

    say(&format!("unpacking {}", payload.display()));
    let unpacked = unpack(&payload)?;
    ensure!(
        unpacked.digest != booted,
        "this payload is the system that is running, sha256:{booted}"
    );
    let uki = unpacked.uki.as_ref().context("the payload has no UKI")?;
    let version = Uki::parse(
        uki.file_name()
            .and_then(|n| n.to_str())
            .context("the payload's UKI has no name")?,
    )
    .context("the payload's UKI is not named as a hideOS deployment")?;
    ensure!(
        version.edition == edition,
        "the payload is hideOS {}, and this machine runs {edition}; rebasing is not done this way",
        version.edition
    );
    say(&format!(
        "  {} new objects, image sha256:{}",
        unpacked.new_objects.len(),
        unpacked.digest
    ));
    crash("unpack")?;

    say("sealing");
    sync();
    for object in &unpacked.new_objects {
        enable_verity(object)?;
    }
    let measured = measure(&Path::new(STORE).join("images").join(&unpacked.digest))?;
    ensure!(
        measured == unpacked.digest,
        "the new image measures sha256:{measured}, but its kernel expects sha256:{}",
        unpacked.digest
    );
    sync();
    crash("seal")?;

    let esp = Esp::mount()?;
    let new = Uki::new_deployment(&version.edition, version.version, &unpacked.digest);
    let linux = esp.path().join("EFI/Linux");
    let staged = linux.join(format!("{}.tmp", new.base_name()));
    say(&format!("staging {}", new.file_name()));
    copy_synced(uki, &staged)?;
    crash("stage")?;

    // The commit: from here the next boot tries the new deployment.
    rename_durably(&staged, &linux.join(new.file_name()))
        .context("putting the new kernel image in place")?;
    say(&format!(
        "committed: the next boot tries {} ({} attempts, then back)",
        new.file_name(),
        deployment::TRIES
    ));
    crash("commit")?;

    let ukis = list(&linux)?;
    let kept: Vec<String> = deployment::keep(&ukis, short(&booted), &new.digest)
        .iter()
        .map(|u| u.file_name())
        .collect();
    for uki in &ukis {
        if !kept.contains(&uki.file_name()) {
            fs::remove_file(linux.join(uki.file_name()))?;
            say(&format!("removed {}", uki.file_name()));
        }
    }
    sync_dir(&linux)?;
    crash("prune")?;
    esp.unmount()?;
    say("done; the new system starts at the next boot");
    Ok(())
}

/// Marks the deployment that booted as good: what makes an update stay. Run
/// once the system has come up; until then, the boot manager is counting.
pub fn boot_ok() -> Result<()> {
    let Ok(variable) = fs::read(BOOT_COUNT_PATH) else {
        // No counter: this deployment was already good.
        return Ok(());
    };
    let path = utf16_variable(&variable).context("LoaderBootCountPath is not a path")?;
    let name = path
        .rsplit('\\')
        .next()
        .context("LoaderBootCountPath names no file")?
        .to_owned();
    let uki = Uki::parse(&name).with_context(|| format!("`{name}` is not a hideOS deployment"))?;
    let esp = Esp::mount()?;
    let linux = esp.path().join("EFI/Linux");
    // The boot manager renamed it before booting: the name it has now is
    // the one in the variable.
    if linux.join(&name).exists() {
        rename_durably(&linux.join(&name), &linux.join(uki.good().file_name()))?;
        say(&format!("{} is good", uki.good().file_name()));
    }
    esp.unmount()
}

pub fn status() -> Result<()> {
    let booted = booted_digest()?;
    let esp = Esp::mount()?;
    let mut ukis = list(&esp.path().join("EFI/Linux"))?;
    esp.unmount()?;
    deployment::boot_order(&mut ukis);
    println!("{:<10} {:<8} image", "version", "state");
    for (i, uki) in ukis.iter().enumerate() {
        let mut notes = Vec::new();
        if booted.starts_with(&uki.digest) {
            notes.push("running");
        }
        if i == 0 {
            notes.push("boots next");
        }
        println!(
            "{:<10} {:<8} {:<14} {}",
            uki.version,
            uki.state(),
            uki.digest,
            notes.join(", ")
        );
    }
    Ok(())
}

/// Makes the next boot go back: the running deployment is marked out of
/// attempts, so the boot manager passes over it.
pub fn rollback() -> Result<()> {
    let booted = booted_digest()?;
    let esp = Esp::mount()?;
    let linux = esp.path().join("EFI/Linux");
    let ukis = list(&linux)?;
    let current = ukis
        .iter()
        .find(|u| booted.starts_with(&u.digest))
        .context("the running deployment is not on the ESP")?;
    ensure!(
        ukis.iter()
            .any(|u| !booted.starts_with(&u.digest) && !u.is_bad()),
        "there is no other deployment to go back to"
    );
    rename_durably(
        &linux.join(current.file_name()),
        &linux.join(current.bad().file_name()),
    )?;
    esp.unmount()?;
    say("the next boot goes back to the previous deployment");
    Ok(())
}

struct Unpacked {
    digest: String,
    uki: Option<PathBuf>,
    new_objects: Vec<PathBuf>,
}

/// Unpacks what the store does not have. Objects are named by their
/// content, so one that exists is the same object. /etc is the machine's:
/// only files it does not have yet are added. The UKI waits in /run.
fn unpack(payload: &Path) -> Result<Unpacked> {
    let file = fs::File::open(payload).with_context(|| format!("opening {}", payload.display()))?;
    let mut archive = tar::Archive::new(file);
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    let staging = Path::new("/run/hide/update");
    let _ = fs::remove_dir_all(staging);
    fs::create_dir_all(staging)?;
    let mut digest = None;
    let mut uki = None;
    let mut new_objects = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let mut parts = path.components();
        let top = parts
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        let rest: PathBuf = parts.collect();
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
        if rest.as_os_str().is_empty() {
            continue;
        }
        let kind = entry.header().entry_type();
        let target = match top.as_str() {
            "repo" => {
                let target = Path::new(STORE).join(&rest);
                if rest.starts_with("objects") && kind.is_file() {
                    if sealed_object(&target) {
                        continue;
                    }
                    // Missing, or left half-written by an update the power
                    // cut short: written again either way.
                    let _ = fs::remove_file(&target);
                    new_objects.push(target.clone());
                }
                // The image's name, and the ref naming the edition's latest:
                // replaced, they name the new image.
                if target.is_symlink() && !rest.starts_with("objects") {
                    fs::remove_file(&target)?;
                }
                target
            }
            "etc" => {
                let target = Path::new("/etc").join(&rest);
                // os-release is the image's, linked from /etc since images
                // carry IMAGE_VERSION; an old machine has a copy.
                let owned_by_image = rest == Path::new("os-release");
                if target.symlink_metadata().is_ok() && !owned_by_image {
                    continue;
                }
                if owned_by_image {
                    let _ = fs::remove_file(&target);
                }
                target
            }
            "esp" => {
                if rest.starts_with("EFI/Linux") && kind.is_file() {
                    let name = rest.file_name().context("a UKI without a name")?;
                    let target = staging.join(name);
                    uki = Some(target.clone());
                    target
                } else {
                    // The boot manager stays as installed: replacing it is
                    // hideBoot's business, with its own way back.
                    continue;
                }
            }
            other => bail!("unexpected `{other}` in the payload"),
        };
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        if kind.is_dir() && target.is_dir() {
            continue;
        }
        entry
            .unpack(&target)
            .with_context(|| format!("unpacking {}", path.display()))?;
    }
    Ok(Unpacked {
        digest: digest.context("the payload has no image.digest")?,
        uki,
        new_objects,
    })
}

/// The ESP, mounted for as long as this value lives.
struct Esp {
    mounted: bool,
}

impl Esp {
    fn mount() -> Result<Esp> {
        let device = partition(ESP_NAME)?;
        fs::create_dir_all(ESP_MOUNT)?;
        mount(
            &device,
            ESP_MOUNT,
            "vfat",
            MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
            Some(c"quiet"),
        )
        .with_context(|| format!("mounting the ESP, {}", device.display()))?;
        Ok(Esp { mounted: true })
    }

    fn path(&self) -> &Path {
        Path::new(ESP_MOUNT)
    }

    fn unmount(mut self) -> Result<()> {
        sync();
        unmount(ESP_MOUNT, UnmountFlags::empty()).context("unmounting the ESP")?;
        self.mounted = false;
        Ok(())
    }
}

impl Drop for Esp {
    fn drop(&mut self) {
        if self.mounted {
            sync();
            let _ = unmount(ESP_MOUNT, UnmountFlags::DETACH);
        }
    }
}

/// The device of the partition named `name`, from sysfs: no udev needed.
fn partition(name: &str) -> Result<PathBuf> {
    for entry in fs::read_dir("/sys/class/block")?.flatten() {
        let uevent = fs::read_to_string(entry.path().join("uevent")).unwrap_or_default();
        let field = |key: &str| {
            uevent
                .lines()
                .find_map(|l| l.strip_prefix(key))
                .map(str::to_owned)
        };
        if field("PARTNAME=").as_deref() == Some(name)
            && let Some(dev) = field("DEVNAME=")
        {
            return Ok(Path::new("/dev").join(dev));
        }
    }
    bail!("no partition named {name}")
}

fn list(linux: &Path) -> Result<Vec<Uki>> {
    let mut ukis = Vec::new();
    for entry in fs::read_dir(linux)?.flatten() {
        if let Some(uki) = Uki::parse(&entry.file_name().to_string_lossy()) {
            ukis.push(uki);
        }
    }
    Ok(ukis)
}

/// The running image's digest, from the command line the UKI carried.
fn booted_digest() -> Result<String> {
    let cmdline = fs::read_to_string("/proc/cmdline")?;
    cmdline
        .split_whitespace()
        .find_map(|w| w.strip_prefix("hideos.image=sha256:"))
        .map(str::to_owned)
        .context("not running from a sealed image: no hideos.image= on the command line")
}

fn booted_edition() -> Result<String> {
    let release = fs::read_to_string("/usr/lib/os-release")?;
    release
        .lines()
        .find_map(|l| l.strip_prefix("IMAGE_ID=hideos-"))
        .map(|v| v.trim_matches('"').to_owned())
        .context("os-release has no IMAGE_ID")
}

fn short(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}

fn utf16_variable(variable: &[u8]) -> Option<String> {
    let data = variable.get(4..)?;
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16(&units).ok()
}

fn copy_synced(from: &Path, to: &Path) -> Result<()> {
    let mut source = fs::File::open(from)?;
    let mut target = fs::File::create(to)?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        target.write_all(buffer.get(..n).unwrap_or_default())?;
    }
    target.sync_all()?;
    Ok(())
}

/// A rename on the ESP that a power cut cannot half-undo. On FAT a file's
/// size and first cluster live in its directory entry, and a rename writes a
/// new entry: until that entry is on disk, the file under its new name can
/// be empty — which is what a cut right after a plain rename left, a
/// zero-length UKI the firmware could not boot. So the file is synced under
/// its new name, then its directory, then the whole filesystem.
fn rename_durably(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)?;
    fs::File::open(to)?.sync_all()?;
    if let Some(dir) = to.parent() {
        sync_dir(dir)?;
    }
    sync();
    Ok(())
}

fn sync_dir(dir: &Path) -> Result<()> {
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

fn sync() {
    rustix::fs::sync();
}

fn enable_verity(path: &Path) -> Result<()> {
    let file = rustix::fs::open(path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
        .with_context(|| format!("opening {}", path.display()))?;
    match fs_ioc_enable_verity(file.as_fd(), VERITY_SHA256, VERITY_BLOCK) {
        Ok(()) | Err(EnableVerityError::AlreadyEnabled) => Ok(()),
        Err(error) => bail!("fs-verity on {}: {error}", path.display()),
    }
}

/// Whether an object in the store is complete: fs-verity on, with the
/// digest its name says — composefs names objects by that digest, as
/// `objects/ab/cdef…`. Anything else cannot be trusted to be what it is
/// named, and is replaced.
fn sealed_object(path: &Path) -> bool {
    let name = path
        .parent()
        .and_then(Path::file_name)
        .zip(path.file_name())
        .map(|(dir, file)| format!("{}{}", dir.to_string_lossy(), file.to_string_lossy()));
    match (name, measure(path)) {
        (Some(name), Ok(digest)) => name == digest,
        _ => false,
    }
}

fn measure(path: &Path) -> Result<String> {
    let file = rustix::fs::open(path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
        .with_context(|| format!("opening {}", path.display()))?;
    let digest: [u8; 32] = fs_ioc_measure_verity(file.as_fd(), VERITY_SHA256)
        .map_err(|e| anyhow::anyhow!("measuring {}: {e}", path.display()))?;
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

fn say(line: &str) {
    eprintln!("hide: {line}");
}

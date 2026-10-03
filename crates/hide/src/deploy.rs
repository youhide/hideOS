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
const STEPS: &[&str] = &["pull", "stage", "commit", "prune", "collect"];

pub fn update(args: &[String]) -> Result<()> {
    let mut image = None;
    let mut crash_after = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--image" => image = iter.next().cloned(),
            // For tests: cut the power after a step, to show that it does
            // not matter where an update is interrupted.
            "--crash-after" => crash_after = iter.next().cloned(),
            other => bail!("unknown argument `{other}`"),
        }
    }
    let image = image.context("usage: hide update --image oci-archive:PATH | oci:DIR[:TAG]")?;
    ensure!(
        image.starts_with("oci-archive:") || image.starts_with("oci:"),
        "hide update takes oci-archive:PATH or oci:DIR[:TAG]; pulling from a registry needs a network, which hideOS does not have yet"
    );
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

    say(&format!("pulling {image}"));
    let pulled = pull(&image, &edition)?;
    ensure!(
        pulled.digest != booted,
        "this image is the system that is running, sha256:{booted}"
    );
    let version = Uki::parse(&pulled.uki_name)
        .context("the image's UKI is not named as a hideOS deployment")?;
    ensure!(
        version.edition == edition,
        "the image is hideOS {}, and this machine runs {edition}; rebasing is not done this way",
        version.edition
    );
    ensure!(
        pulled.digest.starts_with(&version.digest),
        "the image's UKI is named for sha256:{}…, but boots sha256:{}",
        version.digest,
        pulled.digest
    );
    say(&format!("  image sha256:{}", pulled.digest));
    let uki = &pulled.uki;
    crash("pull")?;

    let esp = Esp::mount()?;
    let new = Uki::new_deployment(&version.edition, version.version, &pulled.digest);
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

    // The list was read after the commit: the new deployment is in it.
    let kept: Vec<Uki> = ukis
        .into_iter()
        .filter(|u| kept.contains(&u.file_name()))
        .collect();
    collect(&kept)?;
    crash("collect")?;
    say("done; the new system starts at the next boot");
    Ok(())
}

/// `hide gc`: removes from the store what no deployment on the ESP uses.
pub fn gc() -> Result<()> {
    let esp = Esp::mount()?;
    let ukis = list(&esp.path().join("EFI/Linux"))?;
    esp.unmount()?;
    collect(&ukis)
}

/// Deletes every object no kept deployment's image uses. composefs walks
/// the images; the roots are the images of the UKIs on the ESP — running,
/// the way back, the new one — so nothing a boot can reach is removed. A
/// power cut part-way leaves objects nobody uses, for the next collection.
fn collect(kept: &[Uki]) -> Result<()> {
    use composefs::fsverity::Sha256HashValue;
    use composefs::repository::Repository;

    let mut roots = Vec::new();
    for entry in fs::read_dir(Path::new(STORE).join("images"))?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() == 64 && kept.iter().any(|u| name.starts_with(&u.digest)) {
            roots.push(name);
        }
    }
    ensure!(
        !roots.is_empty(),
        "no image of a kept deployment found; not collecting anything"
    );
    let repo = Repository::<Sha256HashValue>::open_path(rustix::fs::CWD, STORE)
        .map_err(|e| anyhow::anyhow!("opening the store: {e}"))?;
    let roots: Vec<&str> = roots.iter().map(String::as_str).collect();
    let result = repo.gc(&roots)?;
    say(&format!(
        "collected {} objects, {} MiB",
        result.objects_removed,
        result.objects_bytes >> 20
    ));
    Ok(())
}

/// Marks the deployment that booted as good, and stops the watchdog
/// hidestage started: the system came up. Run once the edition's target is
/// reached; until then, the boot manager is counting and the watchdog is
/// running, and either can send the machine back.
pub fn boot_ok() -> Result<()> {
    let marked = mark_good();
    disarm_watchdog();
    marked
}

/// The magic close: writing `V` before closing tells the watchdog driver
/// the close is deliberate, and it stops. Opening it when nothing armed it
/// starts it, and the same close stops it again.
fn disarm_watchdog() {
    if let Ok(mut watchdog) = fs::OpenOptions::new().write(true).open("/dev/watchdog")
        && watchdog.write_all(b"V").is_ok()
    {
        say("watchdog stopped: the system is up");
    }
}

fn mark_good() -> Result<()> {
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

struct Pulled {
    /// The boot image's fs-verity digest: what the UKI boots.
    digest: String,
    /// The UKI, waiting in /run until the ESP takes it.
    uki: PathBuf,
    uki_name: String,
}

/// Pulls an OCI image into the store, and checks it against its own UKI.
///
/// composefs-oci writes the objects the store lacks, with fs-verity on, and
/// regenerates the boot image — the root with /boot emptied — from the
/// layers. The UKI, in the image's /boot, names the digest it boots; the
/// regenerated image has to have that digest, or nothing is staged. The
/// client never trusts an image it did not compute: the build signed the
/// UKI, and the UKI is what the firmware checks.
fn pull(image: &str, edition: &str) -> Result<Pulled> {
    use composefs::fsverity::{FsVerityHashValue, Sha256HashValue};
    use composefs::repository::Repository;
    use composefs_oci::{BootImageMatch, NullReporter, OciTransformOptions};
    use std::sync::Arc;

    // The store is fs-verity's: with meta.json sealed, composefs opens it
    // as a store that requires fs-verity, and seals each object it writes.
    // Installs made before this did not seal meta.json.
    enable_verity(&Path::new(STORE).join("meta.json"))?;
    let repo = Arc::new(
        Repository::<Sha256HashValue>::open_path(rustix::fs::CWD, STORE)
            .map_err(|e| anyhow::anyhow!("opening the store: {e}"))?,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime composefs needs")?;
    let options = OciTransformOptions::default();
    let (result, _) = runtime.block_on(composefs_oci::pull_image(
        &repo,
        image,
        Some(edition),
        None,
        Arc::new(NullReporter),
        Some(&options),
    ))?;

    let tree = composefs_oci::image::create_filesystem(
        &repo,
        &result.config_digest,
        Some(&result.config_verity),
        &options,
    )?;
    let staging = Path::new("/run/hide/update");
    let _ = fs::remove_dir_all(staging);
    fs::create_dir_all(staging)?;

    let linux = tree
        .root
        .get_directory(std::ffi::OsStr::new("boot/EFI/Linux"))
        .context("the image has no /boot/EFI/Linux, so no UKI")?;
    let mut ukis = linux
        .entries()
        .filter(|(name, _)| name.to_string_lossy().ends_with(".efi"));
    let (name, inode) = ukis.next().context("the image has no UKI")?;
    ensure!(ukis.next().is_none(), "the image has more than one UKI");
    let uki_name = name.to_string_lossy().into_owned();
    let composefs::tree::Inode::Leaf(id, _) = inode else {
        bail!("the image's {uki_name} is a directory");
    };
    let composefs::tree::LeafContent::Regular(file) = &tree.leaf(*id).content else {
        bail!("the image's {uki_name} is not a file");
    };
    let bytes = file_bytes(&repo, file)?;
    let cmdline = composefs_boot::uki::get_cmdline(&bytes)
        .map_err(|e| anyhow::anyhow!("reading {uki_name}'s command line: {e}"))?;
    let digest = cmdline
        .split_whitespace()
        .find_map(|w| w.strip_prefix("hideos.image=sha256:"))
        .context("the UKI boots no hideos.image=")?
        .to_owned();
    let uki = staging.join(&uki_name);
    fs::write(&uki, &bytes)?;

    let expected = Sha256HashValue::from_hex(&digest)
        .map_err(|e| anyhow::anyhow!("the UKI's hideos.image= is not a digest: {e}"))?;
    match composefs_oci::find_matching_boot_image(&repo, &result.manifest_digest, &expected)? {
        BootImageMatch::Found { .. } => {}
        BootImageMatch::NotFound(tried) => bail!(
            "the image does not regenerate to what its UKI boots, sha256:{digest} \
             ({tried} ways tried): refusing it"
        ),
    }
    // The kernel's word for it, as hidestage will ask: the image file is
    // sealed, with the digest the UKI names.
    let measured = measure(&Path::new(STORE).join("images").join(&digest))?;
    ensure!(
        measured == digest,
        "the new image measures sha256:{measured}, but its kernel expects sha256:{digest}"
    );

    if let Ok(etc) = tree.root.get_directory(std::ffi::OsStr::new("etc")) {
        merge_etc(&repo, &tree, etc, Path::new("/etc"), Path::new(""))?;
    }
    repo.sync().context("syncing the store")?;
    sync();
    Ok(Pulled {
        digest,
        uki,
        uki_name,
    })
}

type Repo = composefs::repository::Repository<composefs::fsverity::Sha256HashValue>;
type Tree = composefs::tree::FileSystem<composefs::fsverity::Sha256HashValue>;
type TreeFile = composefs::tree::RegularFile<composefs::fsverity::Sha256HashValue>;

fn file_bytes(repo: &Repo, file: &TreeFile) -> Result<Vec<u8>> {
    use composefs::tree::RegularFile;
    match file {
        RegularFile::Inline(data) => Ok(data.to_vec()),
        RegularFile::External(id, _) | RegularFile::ExternalNoVerity(id, _) => repo.read_object(id),
        RegularFile::Sparse(_) => bail!("a sparse file where a file was expected"),
    }
}

/// Adds to /etc what the new image's /etc has and the machine's does not.
/// /etc is the machine's: nothing in it is replaced — but os-release, the
/// image's, linked from /etc since images carry IMAGE_VERSION; an old
/// machine has a copy.
fn merge_etc(
    repo: &Repo,
    tree: &Tree,
    dir: &composefs::tree::Directory<composefs::fsverity::Sha256HashValue>,
    target: &Path,
    relative: &Path,
) -> Result<()> {
    use composefs::tree::{Inode, LeafContent};
    use std::os::unix::fs::{PermissionsExt, symlink};

    for (name, inode) in dir.entries() {
        let path = target.join(name);
        let relative = relative.join(name);
        let owned_by_image = relative == Path::new("os-release");
        let exists = path.symlink_metadata().is_ok();
        match inode {
            Inode::Directory(sub) => {
                if !exists {
                    fs::create_dir(&path)?;
                    fs::set_permissions(
                        &path,
                        PermissionsExt::from_mode(sub.stat.st_mode & 0o7777),
                    )?;
                    std::os::unix::fs::lchown(&path, Some(sub.stat.st_uid), Some(sub.stat.st_gid))?;
                }
                if path.is_dir() {
                    merge_etc(repo, tree, sub, &path, &relative)?;
                }
            }
            Inode::Leaf(id, _) => {
                if exists && !owned_by_image {
                    continue;
                }
                let leaf = tree.leaf(*id);
                if exists {
                    fs::remove_file(&path)?;
                }
                match &leaf.content {
                    LeafContent::Regular(file) => {
                        fs::write(&path, file_bytes(repo, file)?)?;
                        fs::set_permissions(
                            &path,
                            PermissionsExt::from_mode(leaf.stat.st_mode & 0o7777),
                        )?;
                    }
                    LeafContent::Symlink(to) => symlink(Path::new(to.as_ref()), &path)?,
                    // Devices, fifos and sockets have no place in /etc.
                    _ => continue,
                }
                std::os::unix::fs::lchown(&path, Some(leaf.stat.st_uid), Some(leaf.stat.st_gid))?;
            }
        }
    }
    Ok(())
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

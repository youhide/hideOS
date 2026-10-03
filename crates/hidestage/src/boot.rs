//! The boot sequence. Linux only.

use std::convert::Infallible;
use std::fs;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use composefs_ioctls::fsverity::fs_ioc_measure_verity;
use hidestage::{Config, hex, partition_named};
use rustix::fs::{CWD, Mode, OFlags};
use rustix::mount::{
    FsMountFlags, FsOpenFlags, MountAttrFlags, MountFlags, MoveMountFlags, fsconfig_create,
    fsconfig_set_fd, fsconfig_set_flag, fsconfig_set_string, fsmount, fsopen, mount, mount_bind,
    mount_move, move_mount,
};

/// Where the btrfs root is mounted, top level, before the system exists.
const DISK: &str = "/run/hidestage/disk";
/// Where the system is assembled before it becomes `/`.
const SYSROOT: &str = "/sysroot";
/// Subvolumes on the root partition. See ARCHITECTURE.md, "Disk layout".
const STORE: &str = "@store";
/// Writable subvolumes, and where they go in the system.
const BINDS: &[(&str, &str)] = &[
    ("@etc", "etc"),
    ("@var", "var"),
    ("@home", "home"),
    ("@store", "hideos"),
];
/// How long to wait for the root partition to appear.
const DEVICE_TIMEOUT: Duration = Duration::from_secs(30);
/// fs-verity's identifier for SHA-256.
const FS_VERITY_HASH_ALG_SHA256: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum BootError {
    #[error("{what}: {source}")]
    Os {
        what: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Config(#[from] hidestage::ConfigError),
    #[error("no partition named `{0}` appeared within 30 seconds")]
    NoRoot(String),
    #[error(
        "the system image does not match the one this kernel was signed with\n  \
         expected sha256:{expected}\n  found    sha256:{found}"
    )]
    Seal { expected: String, found: String },
    #[error("starting {0}: {1}")]
    Exec(PathBuf, std::io::Error),
}

fn os(what: impl Into<String>) -> impl FnOnce(rustix::io::Errno) -> BootError {
    let what = what.into();
    move |errno| BootError::Os {
        what,
        source: errno.into(),
    }
}

fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> BootError {
    let what = what.into();
    move |source| BootError::Os { what, source }
}

/// The whole boot. Returns only on failure: on success the process has
/// become oxinit.
pub fn run() -> Result<Infallible, BootError> {
    mount_pseudo_filesystems()?;
    say("hidestage: starting");

    let cmdline = fs::read_to_string("/proc/cmdline").map_err(io("reading /proc/cmdline"))?;
    let config = Config::from_cmdline(&cmdline)?;

    let device = wait_for_partition(&config.root_label)?;
    fs::create_dir_all(DISK).map_err(io(format!("creating {DISK}")))?;
    mount(device.as_path(), DISK, "btrfs", MountFlags::NOATIME, None)
        .map_err(os(format!("mounting {} on {DISK}", device.display())))?;

    let store = Path::new(DISK).join(STORE);
    let image = open_verified_image(&store, &config.image)?;
    say(&format!(
        "hidestage: seal ok, sha256:{}",
        hex(&config.image)
    ));

    mount_composefs(image, &store.join("objects"))?;
    for (subvolume, target) in BINDS {
        let from = Path::new(DISK).join(subvolume);
        let to = Path::new(SYSROOT).join(target);
        mount_bind(&from, &to).map_err(os(format!("binding {subvolume} to /{target}")))?;
    }
    // /tmp is a directory in the sealed image, so read-only until something
    // is mounted on it; programs that write there — dbus-run-session, for
    // the session bus — fail before the desktop starts.
    let tmp = Path::new(SYSROOT).join("tmp");
    mount(
        "tmpfs",
        &tmp,
        "tmpfs",
        MountFlags::NOSUID | MountFlags::NODEV,
        Some(c"mode=1777"),
    )
    .map_err(os("mounting /tmp"))?;

    switch_root(&config.init)
}

/// /proc, /sys, /dev and /run, which nothing works without. The kernel gives
/// an initramfs none of them.
fn mount_pseudo_filesystems() -> Result<(), BootError> {
    for (source, target, fstype, flags) in [
        (
            "proc",
            "/proc",
            "proc",
            MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
        ),
        (
            "sysfs",
            "/sys",
            "sysfs",
            MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
        ),
        ("devtmpfs", "/dev", "devtmpfs", MountFlags::NOSUID),
        (
            "tmpfs",
            "/run",
            "tmpfs",
            MountFlags::NOSUID | MountFlags::NODEV,
        ),
    ] {
        fs::create_dir_all(target).map_err(io(format!("creating {target}")))?;
        mount(source, target, fstype, flags, None).map_err(os(format!("mounting {target}")))?;
    }
    Ok(())
}

/// Polls sysfs for the partition named `label`, the way a person would wait
/// for a slow disk: the kernel has no event to wait on before userspace has a
/// device manager, and the initrd has none.
fn wait_for_partition(label: &str) -> Result<PathBuf, BootError> {
    let started = Instant::now();
    loop {
        if let Ok(entries) = fs::read_dir("/sys/class/block") {
            for entry in entries.flatten() {
                let uevent = fs::read_to_string(entry.path().join("uevent")).unwrap_or_default();
                if let Some(devname) = partition_named(&uevent, label) {
                    return Ok(Path::new("/dev").join(devname));
                }
            }
        }
        if started.elapsed() > DEVICE_TIMEOUT {
            return Err(BootError::NoRoot(label.to_owned()));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Opens the deployment's EROFS image and checks its fs-verity digest against
/// the one on the command line. This is the seal: the kernel was signed with
/// that digest, and a different image is refused here, before anything in it
/// runs.
fn open_verified_image(store: &Path, expected: &[u8; 32]) -> Result<OwnedFd, BootError> {
    let path = store.join("images").join(hex(expected));
    let image = rustix::fs::open(&path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
        .map_err(os(format!("opening {}", path.display())))?;
    let found: [u8; 32] =
        fs_ioc_measure_verity(image.as_fd(), FS_VERITY_HASH_ALG_SHA256).map_err(|e| {
            BootError::Os {
                what: format!("measuring fs-verity of {}", path.display()),
                source: std::io::Error::other(e.to_string()),
            }
        })?;
    if &found != expected {
        return Err(BootError::Seal {
            expected: hex(expected),
            found: hex(&found),
        });
    }
    Ok(image)
}

/// The image as `/sysroot`: EROFS metadata over overlayfs, file contents
/// from the object store, each one checked against the digest the image
/// records for it on every open. `verity=require` makes a file without
/// fs-verity, or with a different digest, unreadable rather than trusted.
fn mount_composefs(image: OwnedFd, objects: &Path) -> Result<(), BootError> {
    let erofs = fsopen("erofs", FsOpenFlags::FSOPEN_CLOEXEC).map_err(os("opening erofs"))?;
    fsconfig_set_flag(&erofs, "ro").map_err(os("erofs ro"))?;
    let source = format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&image));
    fsconfig_set_string(&erofs, "source", source.as_str()).map_err(os("erofs source"))?;
    fsconfig_create(&erofs).map_err(os("creating the erofs mount"))?;
    let erofs_mount = fsmount(
        &erofs,
        FsMountFlags::FSMOUNT_CLOEXEC,
        MountAttrFlags::empty(),
    )
    .map_err(os("mounting the erofs image"))?;

    let objects_dir = rustix::fs::open(
        objects,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(os(format!("opening {}", objects.display())))?;

    let overlay = fsopen("overlay", FsOpenFlags::FSOPEN_CLOEXEC).map_err(os("opening overlay"))?;
    for (key, value) in [
        ("source", "composefs:hideos"),
        ("metacopy", "on"),
        ("redirect_dir", "on"),
        ("verity", "require"),
    ] {
        fsconfig_set_string(&overlay, key, value).map_err(os(format!("overlay {key}={value}")))?;
    }
    fsconfig_set_fd(&overlay, "lowerdir+", &erofs_mount).map_err(os("overlay lowerdir+"))?;
    fsconfig_set_fd(&overlay, "datadir+", &objects_dir).map_err(os("overlay datadir+"))?;
    fsconfig_create(&overlay).map_err(os("creating the composefs mount"))?;
    let root = fsmount(
        &overlay,
        FsMountFlags::FSMOUNT_CLOEXEC,
        MountAttrFlags::MOUNT_ATTR_RDONLY,
    )
    .map_err(os("mounting composefs"))?;

    fs::create_dir_all(SYSROOT).map_err(io(format!("creating {SYSROOT}")))?;
    move_mount(
        &root,
        "",
        CWD,
        SYSROOT,
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
    )
    .map_err(os(format!("attaching the system at {SYSROOT}")))?;
    Ok(())
}

/// Moves the pseudo-filesystems into the system, makes it `/`, and becomes
/// its init. The initramfs is left behind in memory; it is a few megabytes.
fn switch_root(init: &Path) -> Result<Infallible, BootError> {
    for target in ["/dev", "/proc", "/sys", "/run"] {
        let to = Path::new(SYSROOT).join(target.trim_start_matches('/'));
        mount_move(target, &to).map_err(os(format!("moving {target} into the system")))?;
    }
    std::env::set_current_dir(SYSROOT).map_err(io(format!("entering {SYSROOT}")))?;
    mount_move(".", "/").map_err(os("moving the system to /"))?;
    rustix::process::chroot(".").map_err(os("chroot"))?;
    std::env::set_current_dir("/").map_err(io("entering /"))?;
    say(&format!("hidestage: starting {}", init.display()));
    let error = Command::new(init).exec();
    Err(BootError::Exec(init.to_path_buf(), error))
}

/// The last resort. There is no shell in the initrd to offer, so: say what
/// failed, give a person time to read it, and reboot. The boot manager counts
/// that as a failed boot of this deployment.
pub fn emergency(message: &str) -> ! {
    say("");
    say("hidestage: this deployment cannot boot");
    for line in message.lines() {
        say(&format!("  {line}"));
    }
    say("hidestage: rebooting in 30 seconds");
    thread::sleep(Duration::from_secs(30));
    let _ = rustix::system::reboot(rustix::system::RebootCommand::Restart);
    // reboot(2) does not return when it works. If it did not, the kernel
    // will not let PID 1 exit quietly either: wait for a person.
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

fn say(line: &str) {
    eprintln!("{line}");
}

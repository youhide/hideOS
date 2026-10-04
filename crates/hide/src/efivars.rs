//! Writing EFI variables. hidestage mounts efivarfs read-only, and the
//! kernel makes every variable outside its short list of known ones
//! immutable: a stray write to the wrong file can leave a firmware that
//! does not start. Both are lifted for one write and put back after it.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use rustix::fs::{IFlags, Mode, OFlags};
use rustix::mount::{MountFlags, mount_remount};

pub const EFIVARS: &str = "/sys/firmware/efi/efivars";

/// Runs `write` with efivarfs writable, and makes it read-only again
/// whatever `write` returned.
pub fn writable<T>(write: impl FnOnce() -> Result<T>) -> Result<T> {
    mount_remount(
        EFIVARS,
        MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
        "",
    )
    .context("remounting efivarfs writable")?;
    let result = write();
    let _ = mount_remount(
        EFIVARS,
        MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC | MountFlags::RDONLY,
        "",
    );
    result
}

/// Writes a variable whole — attributes, then data, as efivarfs takes it —
/// lifting the immutable flag of one that exists. Inside [`writable`].
pub fn write(path: &Path, value: &[u8]) -> Result<()> {
    if let Ok(fd) = rustix::fs::open(path, OFlags::RDONLY, Mode::empty()) {
        let flags = rustix::fs::ioctl_getflags(&fd)?;
        rustix::fs::ioctl_setflags(&fd, flags - IFlags::IMMUTABLE)?;
    }
    // One write: efivarfs takes a variable whole, or not at all.
    fs::write(path, value).with_context(|| format!("writing {}", path.display()))
}

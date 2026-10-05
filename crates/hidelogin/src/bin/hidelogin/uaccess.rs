//! The person at the screen's devices: each node udev tagged `uaccess`
//! gets an ACL entry for the active session's user, and loses it when
//! another session comes forward. hidelogin moves them all when the active
//! session changes; `hidelogin uaccess NODE`, run by udev for a device
//! that appears, grants that one to whoever is active then.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use hidelogin::{acl, uaccess};
use rustix::fs::XattrFlags;

const ACL_ACCESS: &str = "system.posix_acl_access";

/// `node`'s ACL with read and write for `uid` alone among named users.
pub fn set(node: &Path, uid: Option<u32>) -> Result<()> {
    let mut buffer = vec![0u8; 512];
    let entries = match rustix::fs::getxattr(node, ACL_ACCESS, &mut buffer) {
        Ok(n) => {
            let bytes = buffer.get(..n).unwrap_or_default();
            acl::parse(bytes).with_context(|| format!("{}'s ACL", node.display()))?
        }
        Err(rustix::io::Errno::NODATA) => {
            let mode = rustix::fs::stat(node)
                .with_context(|| format!("stat {}", node.display()))?
                .st_mode;
            acl::from_mode(mode)
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}'s ACL", node.display()));
        }
    };
    let bytes = acl::encode(&acl::grant(&entries, uid));
    rustix::fs::setxattr(node, ACL_ACCESS, &bytes, XattrFlags::empty())
        .with_context(|| format!("setting {}'s ACL", node.display()))
}

/// Every tagged device, to `uid`.
pub fn grant_all(uid: Option<u32>) {
    let Ok(entries) = fs::read_dir(uaccess::TAGGED) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(uevent) = uaccess::sysfs_uevent(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        let Some(node) = fs::read_to_string(&uevent)
            .ok()
            .as_deref()
            .and_then(uaccess::devnode)
        else {
            continue;
        };
        if let Err(error) = set(Path::new(&node), uid) {
            eprintln!("hidelogin: {error:#}");
        }
    }
}

/// udev's call for one device: to the active session's user, read from
/// hidelogin's seat file — the daemon may be busy, the file is current.
pub fn grant_one(node: &str) -> Result<()> {
    let seat = fs::read_to_string(Path::new(hidelogin::session::STATE).join("seats/seat0"))
        .unwrap_or_default();
    set(Path::new(node), uaccess::active_uid(&seat))
}

//! The only `unsafe` in hideforge.

use rustix::io;
use rustix::thread::UnshareFlags;

/// Moves the calling process into new mount, PID, network, UTS and IPC
/// namespaces. The PID namespace applies to children spawned afterwards, not
/// to the caller.
pub fn unshare_sandbox_namespaces() -> io::Result<()> {
    let flags = UnshareFlags::NEWNS
        | UnshareFlags::NEWPID
        | UnshareFlags::NEWNET
        | UnshareFlags::NEWUTS
        | UnshareFlags::NEWIPC;
    // SAFETY: `unshare_unsafe` is unsafe only because of `CLONE_FILES`: after
    // unsharing the file descriptor table, a descriptor opened on one thread
    // may be invalid on another. These flags do not include `FILES`, and none
    // of them changes what any thread's descriptors refer to.
    unsafe { rustix::thread::unshare_unsafe(flags) }
}

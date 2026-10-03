//! The one system call rustix has no safe wrapper for. Every `unsafe` in
//! hidestage is here.

#![allow(unsafe_code)]

use std::os::fd::AsFd;

use rustix::ioctl::{Updater, ioctl, opcode};

/// `WDIOC_SETTIMEOUT` from `<linux/watchdog.h>`: `_IOWR('W', 6, int)`.
const WDIOC_SETTIMEOUT: rustix::ioctl::Opcode = opcode::read_write::<i32>(b'W', 6);

/// Sets the watchdog's timeout, in seconds. The driver may round it; the
/// value it settled on is returned.
pub fn set_watchdog_timeout(watchdog: impl AsFd, seconds: i32) -> rustix::io::Result<i32> {
    let mut value = seconds;
    // SAFETY: WDIOC_SETTIMEOUT is the opcode the kernel defines for this
    // request, and its argument is a pointer to an `int`, which `value` is;
    // the kernel writes the timeout it applied back through the same
    // pointer, which lives until the call returns.
    unsafe {
        let updater = Updater::<WDIOC_SETTIMEOUT, i32>::new(&mut value);
        ioctl(watchdog, updater)?;
    }
    Ok(value)
}

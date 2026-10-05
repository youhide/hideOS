//! The ioctls rustix has no safe wrapper for: DRM master, revoking input
//! devices, and the console's VT switching. Every `unsafe` in hidelogin is
//! here.

#![allow(unsafe_code)]

use std::os::fd::AsFd;

use rustix::io;
use rustix::ioctl::{Getter, IntegerSetter, NoArg, Opcode, Setter, ioctl, opcode};

/// `DRM_IOCTL_SET_MASTER`, `_IO('d', 0x1e)`.
const DRM_SET_MASTER: Opcode = opcode::none(b'd', 0x1e);
/// `DRM_IOCTL_DROP_MASTER`, `_IO('d', 0x1f)`.
const DRM_DROP_MASTER: Opcode = opcode::none(b'd', 0x1f);
/// `EVIOCREVOKE`, `_IOW('E', 0x91, int)`: the argument must be 0.
const EVIOCREVOKE: Opcode = opcode::write::<i32>(b'E', 0x91);
/// `HIDIOCREVOKE`, `_IOW('H', 0x0D, int)`.
const HIDIOCREVOKE: Opcode = opcode::write::<i32>(b'H', 0x0D);

/// The console's ioctls, from `<linux/vt.h>` and `<linux/kd.h>`: numbers
/// from before `_IO` encoding, as the kernel still defines them.
const VT_SETMODE: Opcode = 0x5602;
const VT_GETSTATE: Opcode = 0x5603;
const VT_RELDISP: Opcode = 0x5605;
const VT_ACTIVATE: Opcode = 0x5606;
const KDSETMODE: Opcode = 0x4B3A;
const KDSKBMODE: Opcode = 0x4B45;

const KD_TEXT: usize = 0;
const KD_GRAPHICS: usize = 1;
const K_OFF: usize = 0x04;
const K_UNICODE: usize = 0x03;
/// `VT_RELDISP`'s argument to acknowledge an acquire.
const VT_ACKACQ: usize = 2;

const VT_AUTO: i8 = 0;
const VT_PROCESS: i8 = 1;

#[repr(C)]
struct VtMode {
    mode: i8,
    waitv: i8,
    relsig: i16,
    acqsig: i16,
    frsig: i16,
}

#[repr(C)]
#[derive(Default)]
struct VtStat {
    active: u16,
    signal: u16,
    state: u16,
}

pub fn drm_set_master(fd: impl AsFd) -> io::Result<()> {
    // SAFETY: DRM_IOCTL_SET_MASTER takes no argument.
    unsafe { ioctl(fd, NoArg::<DRM_SET_MASTER>::new()) }
}

pub fn drm_drop_master(fd: impl AsFd) -> io::Result<()> {
    // SAFETY: DRM_IOCTL_DROP_MASTER takes no argument.
    unsafe { ioctl(fd, NoArg::<DRM_DROP_MASTER>::new()) }
}

/// Every later read or write of this file description fails: the input
/// device is the client's no more.
pub fn evdev_revoke(fd: impl AsFd) -> io::Result<()> {
    // SAFETY: EVIOCREVOKE takes an integer that must be 0, passed by value.
    unsafe { ioctl(fd, IntegerSetter::<EVIOCREVOKE>::new_usize(0)) }
}

pub fn hidraw_revoke(fd: impl AsFd) -> io::Result<()> {
    // SAFETY: HIDIOCREVOKE takes an integer that must be 0, passed by value.
    unsafe { ioctl(fd, IntegerSetter::<HIDIOCREVOKE>::new_usize(0)) }
}

/// The VT the kernel shows, from any console descriptor.
pub fn current_vt(tty: impl AsFd) -> io::Result<i32> {
    // SAFETY: VT_GETSTATE fills a `struct vt_stat`, three u16s, which is
    // what `VtStat` is and what the getter allocates.
    let stat: VtStat = unsafe { ioctl(tty, Getter::<VT_GETSTATE, VtStat>::new())? };
    Ok(i32::from(stat.active))
}

/// Switching away from and to this VT asks this process first, with
/// SIGUSR1 to release and SIGUSR2 to acquire; or, with `process` false,
/// the kernel switches by itself again.
pub fn vt_set_process_switching(tty: impl AsFd, process: bool) -> io::Result<()> {
    let mode = VtMode {
        mode: if process { VT_PROCESS } else { VT_AUTO },
        waitv: 0,
        relsig: if process {
            rustix::process::Signal::USR1.as_raw() as i16
        } else {
            0
        },
        acqsig: if process {
            rustix::process::Signal::USR2.as_raw() as i16
        } else {
            0
        },
        frsig: 0,
    };
    // SAFETY: VT_SETMODE reads a `struct vt_mode`, which `VtMode` is laid
    // out as; the setter passes a pointer to it for the call's duration.
    unsafe { ioctl(tty, Setter::<VT_SETMODE, VtMode>::new(mode)) }
}

/// The keyboard on (Unicode) for the console, or off for a compositor
/// reading input devices itself.
pub fn vt_set_keyboard(tty: impl AsFd, on: bool) -> io::Result<()> {
    let mode = if on { K_UNICODE } else { K_OFF };
    // SAFETY: KDSKBMODE takes the mode as an integer, by value.
    unsafe { ioctl(tty, IntegerSetter::<KDSKBMODE>::new_usize(mode)) }
}

/// Graphics mode — the kernel draws nothing on this VT — or text.
pub fn vt_set_graphics(tty: impl AsFd, graphics: bool) -> io::Result<()> {
    let mode = if graphics { KD_GRAPHICS } else { KD_TEXT };
    // SAFETY: KDSETMODE takes the mode as an integer, by value.
    unsafe { ioctl(tty, IntegerSetter::<KDSETMODE>::new_usize(mode)) }
}

pub fn vt_activate(tty: impl AsFd, vt: i32) -> io::Result<()> {
    let vt = usize::try_from(vt).map_err(|_| io::Errno::INVAL)?;
    // SAFETY: VT_ACTIVATE takes the VT's number as an integer, by value.
    unsafe { ioctl(tty, IntegerSetter::<VT_ACTIVATE>::new_usize(vt)) }
}

/// Lets a switch away from (`release`) or to this VT go on.
pub fn vt_ack(tty: impl AsFd, release: bool) -> io::Result<()> {
    let value = if release { 1 } else { VT_ACKACQ };
    // SAFETY: VT_RELDISP takes 1 (release allowed) or VT_ACKACQ, by value.
    unsafe { ioctl(tty, IntegerSetter::<VT_RELDISP>::new_usize(value)) }
}

/// `EV_KEY` and `EV_SW`, from `<linux/input-event-codes.h>`.
pub const EV_KEY: u16 = 0x01;
pub const EV_SW: u16 = 0x05;
pub const KEY_POWER: u16 = 116;
pub const SW_LID: u16 = 0x00;

/// `EVIOCGBIT(EV_KEY, 96)`: which keys a device has, KEY_MAX + 1 bits.
const EVIOCGBIT_KEY: Opcode = opcode::read::<[u8; 96]>(b'E', 0x20 + EV_KEY as u8);
/// `EVIOCGBIT(EV_SW, 2)`: which switches, SW_MAX + 1 bits.
const EVIOCGBIT_SW: Opcode = opcode::read::<[u8; 2]>(b'E', 0x20 + EV_SW as u8);
/// `EVIOCGSW(2)`: the switches' states now.
const EVIOCGSW: Opcode = opcode::read::<[u8; 2]>(b'E', 0x1b);

fn bit(bits: &[u8], n: u16) -> bool {
    bits.get(usize::from(n / 8))
        .is_some_and(|byte| byte & (1 << (n % 8)) != 0)
}

/// Whether an input device has the power key.
pub fn has_power_key(fd: impl AsFd) -> bool {
    // SAFETY: EVIOCGBIT fills a buffer of the size its number encodes,
    // which is the getter's array.
    unsafe { ioctl(fd, Getter::<EVIOCGBIT_KEY, [u8; 96]>::new()) }
        .is_ok_and(|bits| bit(&bits, KEY_POWER))
}

/// Whether an input device is a lid switch, and then whether it is closed.
pub fn lid_switch(fd: impl AsFd) -> Option<bool> {
    // SAFETY: as above, for the switches' two bytes.
    let bits = unsafe { ioctl(&fd, Getter::<EVIOCGBIT_SW, [u8; 2]>::new()) }.ok()?;
    if !bit(&bits, SW_LID) {
        return None;
    }
    // SAFETY: EVIOCGSW fills the same two bytes with the states.
    let state = unsafe { ioctl(&fd, Getter::<EVIOCGSW, [u8; 2]>::new()) }.ok()?;
    Some(bit(&state, SW_LID))
}

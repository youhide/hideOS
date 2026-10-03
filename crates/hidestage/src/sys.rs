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

/// device-mapper's control interface, `<linux/dm-ioctl.h>`: every command
/// takes a `struct dm_ioctl` — 312 bytes — followed by its data, in one
/// buffer whose size the header carries.
pub mod dm {
    use std::fs::OpenOptions;
    use std::os::fd::AsFd;

    use rustix::ioctl::{Opcode, Updater, ioctl, opcode};

    const HEADER: usize = 312;
    const TARGET_SPEC: usize = 40;
    const BUFFER: usize = 16 * 1024;
    const DM_IOCTL: u8 = 0xfd;
    const DEV_CREATE: Opcode = opcode::read_write::<[u8; HEADER]>(DM_IOCTL, 3);
    const DEV_REMOVE: Opcode = opcode::read_write::<[u8; HEADER]>(DM_IOCTL, 4);
    const DEV_SUSPEND: Opcode = opcode::read_write::<[u8; HEADER]>(DM_IOCTL, 6);
    const TABLE_LOAD: Opcode = opcode::read_write::<[u8; HEADER]>(DM_IOCTL, 9);
    /// The kernel wipes the buffers it copied this command into: the table
    /// carries the volume key.
    const SECURE_DATA: u32 = 1 << 15;

    /// The buffer, aligned as the kernel's struct is.
    #[repr(C, align(8))]
    struct Buffer([u8; BUFFER]);

    impl Drop for Buffer {
        fn drop(&mut self) {
            // The key was in here; volatile so the clear is not elided.
            for byte in self.0.iter_mut() {
                // SAFETY: a valid, aligned, exclusive reference to a byte
                // of this buffer.
                unsafe { std::ptr::write_volatile(byte, 0) };
            }
        }
    }

    fn header(buffer: &mut Buffer, name: &str, uuid: &str, size: usize, flags: u32, targets: u32) {
        let b = &mut buffer.0;
        let mut put = |at: usize, bytes: &[u8]| {
            if let Some(field) = b.get_mut(at..at + bytes.len()) {
                field.copy_from_slice(bytes);
            }
        };
        put(0, &4u32.to_ne_bytes());
        put(4, &0u32.to_ne_bytes());
        put(8, &0u32.to_ne_bytes());
        put(12, &(size as u32).to_ne_bytes());
        put(16, &(HEADER as u32).to_ne_bytes());
        put(20, &targets.to_ne_bytes());
        put(28, &flags.to_ne_bytes());
        put(48, name.as_bytes().get(..127).unwrap_or(name.as_bytes()));
        put(176, uuid.as_bytes().get(..128).unwrap_or(uuid.as_bytes()));
    }

    macro_rules! command {
        ($name:ident, $opcode:expr) => {
            fn $name(buffer: &mut Buffer) -> std::io::Result<()> {
                let control = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/mapper/control")?;
                // SAFETY: the opcode is the kernel's for this command, and
                // its argument is a pointer to a dm_ioctl header followed by
                // `data_size` bytes, which `buffer` is: header() wrote a
                // data_size no larger than BUFFER. The kernel writes its
                // reply into the same buffer, which outlives the call.
                unsafe {
                    let updater = Updater::<{ $opcode }, Buffer>::new(buffer);
                    ioctl(control.as_fd(), updater)?;
                }
                Ok(())
            }
        };
    }
    command!(dev_create, DEV_CREATE);
    command!(dev_remove, DEV_REMOVE);
    command!(dev_suspend, DEV_SUSPEND);
    command!(table_load, TABLE_LOAD);

    /// Creates the device `name`, loads one target into it and makes it
    /// live. Returns its device number. On a failure after the create, the
    /// half-made device is removed.
    pub fn create(
        name: &str,
        uuid: &str,
        start: u64,
        length: u64,
        target: &str,
        params: &str,
    ) -> std::io::Result<u64> {
        let mut buffer = Buffer([0; BUFFER]);
        header(&mut buffer, name, uuid, HEADER, 0, 0);
        dev_create(&mut buffer)?;
        let dev = buffer
            .0
            .get(40..48)
            .and_then(|b| <[u8; 8]>::try_from(b).ok())
            .map(u64::from_ne_bytes)
            .unwrap_or_default();

        let loaded = (|| {
            let mut buffer = Buffer([0; BUFFER]);
            let spec_size = (TARGET_SPEC + params.len() + 1).div_ceil(8) * 8;
            let size = HEADER + spec_size;
            if size > BUFFER {
                return Err(std::io::Error::other("dm table too long"));
            }
            header(&mut buffer, name, "", size, SECURE_DATA, 1);
            let spec = &mut buffer.0;
            let mut put = |at: usize, bytes: &[u8]| {
                if let Some(field) = spec.get_mut(at..at + bytes.len()) {
                    field.copy_from_slice(bytes);
                }
            };
            put(HEADER, &start.to_ne_bytes());
            put(HEADER + 8, &length.to_ne_bytes());
            put(HEADER + 20, &(spec_size as u32).to_ne_bytes());
            put(
                HEADER + 24,
                target.as_bytes().get(..15).unwrap_or(target.as_bytes()),
            );
            put(HEADER + TARGET_SPEC, params.as_bytes());
            table_load(&mut buffer)?;

            let mut buffer = Buffer([0; BUFFER]);
            header(&mut buffer, name, "", HEADER, 0, 0);
            dev_suspend(&mut buffer)
        })();
        if let Err(error) = loaded {
            let mut buffer = Buffer([0; BUFFER]);
            header(&mut buffer, name, "", HEADER, 0, 0);
            let _ = dev_remove(&mut buffer);
            return Err(error);
        }
        Ok(dev)
    }
}

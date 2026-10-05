//! PCI devices the installer acts on, by their IDs as sysfs gives them.

/// NVIDIA's PCI vendor ID.
pub const NVIDIA: u32 = 0x10de;

/// An NVIDIA GPU, as far as hideOS's driver goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Nvidia {
    /// Older than Turing: NVIDIA's open kernel modules do not drive it.
    TooOld,
    /// Turing or newer: the nvidia extension drives it.
    Open,
}

/// Whether the device is an NVIDIA GPU, and which kind. `class` is the
/// 24-bit PCI class: display controllers are 0x03xxxx. Turing's device IDs
/// start at 0x1e00, and every later generation's are above them; Volta's
/// and Pascal's are below. NVIDIA's own list, supported-gpus.json in the
/// extension, is exact; this is what the installer can know before it has
/// the extension.
pub fn nvidia_gpu(vendor: u32, class: u32, device: u32) -> Option<Nvidia> {
    if vendor != NVIDIA || class >> 16 != 0x03 {
        return None;
    }
    Some(if device >= 0x1e00 {
        Nvidia::Open
    } else {
        Nvidia::TooOld
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turing_and_newer_are_open() {
        // RTX 4070, RTX 2080, GTX 1660.
        for id in [0x2786, 0x1e82, 0x2184] {
            assert_eq!(nvidia_gpu(NVIDIA, 0x030000, id), Some(Nvidia::Open));
        }
        // GTX 1080 (Pascal).
        assert_eq!(nvidia_gpu(NVIDIA, 0x030000, 0x1b80), Some(Nvidia::TooOld));
        // NVIDIA's HD audio function, and an Intel GPU.
        assert_eq!(nvidia_gpu(NVIDIA, 0x040300, 0x10f0), None);
        assert_eq!(nvidia_gpu(0x8086, 0x030000, 0x3e92), None);
    }
}

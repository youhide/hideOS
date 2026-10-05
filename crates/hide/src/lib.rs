//! What `hide` decides, apart from doing it: host-testable, no Linux-only
//! dependencies. The binary reads the files, calls these, and writes the
//! result.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

pub mod account;
pub mod bootentry;
pub mod channel;
pub mod clock;
pub mod config;
pub mod deployment;
pub mod disks;
pub mod firstboot;
pub mod pci;
pub mod recovery;
pub mod startup;
pub mod subid;
pub mod sysusers;
pub mod tmpfiles;

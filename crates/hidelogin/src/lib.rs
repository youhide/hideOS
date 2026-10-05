//! hidelogin's policy, tested on any host: seatd's protocol, the seat's
//! state machine, and the rules for sessions. The daemon in `main.rs` is
//! the Linux side, carrying out what these decide. See ARCHITECTURE.md,
//! "hidelogin".

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

pub mod conf;
pub mod mounts;
pub mod query;
pub mod seat;
pub mod seatd;
pub mod session;

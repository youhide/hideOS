//! hidestage: the `/init` of hideOS's initrd. See ARCHITECTURE.md, "Boot
//! chain".
//!
//! In order: mount the pseudo-filesystems, read the command line the signed
//! UKI carries, find the root partition, check the system image's fs-verity
//! digest against the one on the command line, mount it as composefs with
//! `verity=require`, bind the writable subvolumes, and hand the result to
//! oxinit with `switch_root`.
//!
//! A bug here is a machine that does not boot, so the rules are oxinit's:
//! no panics, errors as values, and when something cannot be done, say what
//! and why on the console and reboot — which the boot manager counts as a
//! failed boot of this deployment, and after three, falls back from.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(target_os = "linux")]
mod boot;

#[cfg(target_os = "linux")]
fn main() {
    // Unwinding reaches here and stops. A panic that escaped every Result
    // would otherwise end PID 1 of the initrd, and with it the kernel.
    let outcome = std::panic::catch_unwind(boot::run);
    let message = match outcome {
        Ok(Ok(never)) => match never {},
        Ok(Err(error)) => format!("{error}"),
        Err(_) => "hidestage panicked; this is a bug".to_owned(),
    };
    boot::emergency(&message);
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("hidestage runs only as the init of a Linux initrd");
    std::process::exit(1);
}

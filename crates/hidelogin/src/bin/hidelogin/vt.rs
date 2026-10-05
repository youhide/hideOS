//! VTs: the kernel's signals while a compositor holds one, and which VT is
//! shown, for which session is active.
//!
//! A VT held by a compositor switches only when hidelogin says so: the
//! kernel sends SIGUSR1 to ask for the release, SIGUSR2 once the switch to
//! one is done. Which VT is shown changes with or without a compositor;
//! `/sys/class/tty/tty0/active` says, and is polled for its change.

use std::time::Duration;

use anyhow::Result;
use tokio::signal::unix::{SignalKind, signal};

use crate::daemon::{Shared, current_vt, lock};
use crate::login1;

pub async fn signals(shared: Shared) -> Result<()> {
    let mut release = signal(SignalKind::user_defined1())?;
    let mut acquire = signal(SignalKind::user_defined2())?;
    loop {
        tokio::select! {
            _ = release.recv() => {
                let mut daemon = lock(&shared);
                let effects = daemon.seat.vt_release();
                daemon.apply(effects);
            }
            _ = acquire.recv() => {
                match current_vt() {
                    Ok(vt) => {
                        let mut daemon = lock(&shared);
                        let effects = daemon.seat.vt_acquire(vt);
                        daemon.apply(effects);
                    }
                    Err(error) => eprintln!("hidelogin: the VT acquired: {error}"),
                }
            }
        }
    }
}

/// The VT shown, from sysfs: `tty1` is 1.
fn shown() -> Option<u32> {
    std::fs::read_to_string("/sys/class/tty/tty0/active")
        .ok()?
        .trim()
        .strip_prefix("tty")?
        .parse()
        .ok()
}

/// Keeps the sessions' idea of the shown VT current. sysfs can wake a
/// poller on change, but a quarter-second look is simpler and costs
/// nothing a person would notice.
pub async fn follow(shared: Shared) -> Result<()> {
    let mut last = None;
    loop {
        let now = shown();
        if now != last {
            lock(&shared).set_current_vt(now);
            login1::active_changed(&shared).await;
            last = now;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

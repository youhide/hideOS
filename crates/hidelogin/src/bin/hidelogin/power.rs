//! Sleep and shutdown, as logind does them for its clients: announced
//! first — PrepareForSleep, PrepareForShutdown — so that a screen locker
//! locks and NetworkManager lets go, held back while a delay inhibitor is,
//! for at most `InhibitDelayMaxSec`, and refused while one blocks.
//!
//! Shutdown is oxinit's, through `hide`'s poweroff and reboot, as any other
//! shutdown on hideOS. Sleep is the kernel's, through /sys/power/state:
//! hibernation resumes from the swap file `hide swap` set up at boot.

use std::sync::LazyLock;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use tokio::sync::Notify;

use crate::daemon::{Shared, lock};
use crate::login1;

#[derive(Debug, Clone, Copy)]
pub enum Sleep {
    Suspend,
    Hibernate,
}

static INHIBITORS: LazyLock<Notify> = LazyLock::new(Notify::new);

/// An inhibitor went: whoever waits on one may stop waiting.
pub fn inhibitors_changed() {
    INHIBITORS.notify_waiters();
}

/// Until nothing holds `what` back with a delay, or the delay ran out.
async fn wait_for_delays(shared: &Shared, what: &str) {
    let max = lock(shared).config.inhibit_delay_max;
    let start = Instant::now();
    loop {
        if !lock(shared).inhibited(what, "delay") {
            return;
        }
        let left = max.saturating_sub(start.elapsed());
        if left.is_zero() {
            eprintln!("hidelogin: {what} goes on with a delay inhibitor still held");
            return;
        }
        let _ = tokio::time::timeout(left, INHIBITORS.notified()).await;
    }
}

pub async fn shutdown(shared: Shared, reboot: bool) {
    login1::announce(false, true).await;
    wait_for_delays(&shared, "shutdown").await;
    let program = if reboot {
        "/usr/bin/reboot"
    } else {
        "/usr/bin/poweroff"
    };
    println!("hidelogin: {program}");
    if let Err(error) = tokio::process::Command::new(program).status().await {
        eprintln!("hidelogin: {program}: {error}");
        // The machine stays up: say so to whoever got ready for it.
        login1::announce(false, false).await;
    }
}

pub async fn sleep(shared: Shared, how: Sleep) -> Result<()> {
    {
        let mut daemon = lock(&shared);
        if daemon.inhibited("sleep", "block") {
            let who: Vec<String> = daemon
                .inhibitors
                .values()
                .filter(|i| i.mode == "block" && i.what.iter().any(|w| w == "sleep"))
                .map(|i| format!("{} ({})", i.who, i.why))
                .collect();
            bail!("sleep is blocked by {}", who.join(", "));
        }
        if daemon.sleeping {
            bail!("already going to sleep");
        }
        daemon.sleeping = true;
    }
    login1::announce(true, true).await;
    wait_for_delays(&shared, "sleep").await;
    let state = match how {
        Sleep::Suspend => "mem",
        Sleep::Hibernate => "disk",
    };
    println!("hidelogin: to sleep, {state}");
    // The write returns once the machine is back.
    let slept = tokio::task::spawn_blocking(move || {
        std::fs::write("/sys/power/state", state)
            .with_context(|| format!("writing {state} to /sys/power/state"))
    })
    .await
    .context("the sleep's thread")?;
    lock(&shared).sleeping = false;
    login1::announce(true, false).await;
    println!("hidelogin: awake");
    slept
}

//! The machine's own buttons: the lid, and the power key when the
//! configuration gives it to hidelogin rather than to COSMIC. Read from
//! their input devices, one thread each blocked on its reads.
//!
//! Devices are found once, at start: the lid and the power button are
//! built in. A keyboard plugged in later with a power key is COSMIC's.

use std::fs::File;
use std::io::Read;

use anyhow::Result;
use hidelogin::conf::{Action, Button};
use tokio::sync::mpsc;

use crate::daemon::{Shared, lock};
use crate::{login1, power, sys};

/// `struct input_event` on a 64-bit kernel: a timeval, then type, code
/// and value.
const EVENT_SIZE: usize = 24;

/// Whether a display besides the built-in one is connected: the lid then
/// closes on a machine used as a desktop.
fn docked() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return false;
    };
    let connected = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains('-'))
        .filter(|e| {
            std::fs::read_to_string(e.path().join("status")).is_ok_and(|s| s.trim() == "connected")
        })
        .count();
    connected > 1
}

/// A press, or the lid's new state: `(Button::LidClosed, false)` is the lid
/// opening.
type Event = (Button, bool);

fn watch(file: File, power_key: bool, lid: bool, events: mpsc::UnboundedSender<Event>) {
    std::thread::spawn(move || {
        let mut file = file;
        let mut event = [0u8; EVENT_SIZE];
        while file.read_exact(&mut event).is_ok() {
            let field = |at: usize| {
                event
                    .get(at..at + 2)
                    .and_then(|b| b.try_into().ok())
                    .map(u16::from_ne_bytes)
            };
            let value = event
                .get(20..24)
                .and_then(|b| b.try_into().ok())
                .map(i32::from_ne_bytes);
            let event = match (field(16), field(18), value) {
                (Some(sys::EV_KEY), Some(sys::KEY_POWER), Some(1)) if power_key => {
                    (Button::PowerKey, true)
                }
                (Some(sys::EV_SW), Some(sys::SW_LID), Some(v)) if lid => {
                    (Button::LidClosed, v != 0)
                }
                _ => continue,
            };
            if events.send(event).is_err() {
                return;
            }
        }
    });
}

pub async fn serve(shared: Shared) -> Result<()> {
    let (sender, mut events) = mpsc::unbounded_channel();
    let want_power_key = lock(&shared).config.power_key != Action::Ignore;
    let mut watched = 0;
    for entry in std::fs::read_dir("/dev/input")?.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("event") {
            continue;
        }
        let Ok(file) = File::open(entry.path()) else {
            continue;
        };
        let power_key = want_power_key && sys::has_power_key(&file);
        let lid = sys::lid_switch(&file);
        if let Some(closed) = lid {
            lock(&shared).lid_closed = closed;
        }
        if power_key || lid.is_some() {
            watch(file, power_key, lid.is_some(), sender.clone());
            watched += 1;
        }
    }
    drop(sender);
    println!("hidelogin: watching {watched} button device(s)");
    while let Some((button, pressed)) = events.recv().await {
        let action = {
            let mut daemon = lock(&shared);
            if button == Button::LidClosed {
                daemon.lid_closed = pressed;
            }
            if !pressed {
                continue;
            }
            let inhibited = daemon.inhibited(button.inhibitor(), "block");
            daemon.config.action_for(button, docked(), inhibited)
        };
        println!("hidelogin: {button:?}: {action:?}");
        let shared = shared.clone();
        // Sleep returns at the wake: the next press is read meanwhile.
        tokio::spawn(async move {
            let done = match action {
                Action::Ignore => Ok(()),
                Action::Suspend => power::sleep(shared, power::Sleep::Suspend).await,
                Action::Hibernate => power::sleep(shared, power::Sleep::Hibernate).await,
                Action::PowerOff => {
                    power::shutdown(shared, false).await;
                    Ok(())
                }
                Action::Reboot => {
                    power::shutdown(shared, true).await;
                    Ok(())
                }
                Action::Lock => {
                    login1::lock_all(&shared).await;
                    Ok(())
                }
            };
            if let Err(error) = done {
                eprintln!("hidelogin: {error:#}");
            }
        });
    }
    // No buttons to watch: nothing more to do here, for good.
    std::future::pending::<()>().await;
    Ok(())
}

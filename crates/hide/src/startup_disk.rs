//! `hide startup-disk`: which system starts when no key is held, hideOS or
//! Windows, and starting the other one once — what Startup Disk is on a
//! Mac. The choice is hideBoot's to act on; see `hide::startup`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use hide::startup::{self, LOADER_GUID, System};

use crate::efivars::{self, EFIVARS};

/// Non-volatile, boot service and runtime access, as systemd-boot's own.
const ATTRIBUTES: [u8; 4] = [7, 0, 0, 0];

pub fn run(args: &[String]) -> Result<()> {
    match args {
        [] => show(),
        [system] => choose(system, false),
        [system, once] if once == "--once" => choose(system, true),
        _ => bail!("usage: hide startup-disk [hideos|windows [--once]]"),
    }
}

fn variable(name: &str) -> PathBuf {
    Path::new(EFIVARS).join(format!("{name}-{LOADER_GUID}"))
}

/// A variable's strings, without the attributes efivarfs puts first.
fn read(name: &str) -> Vec<String> {
    fs::read(variable(name))
        .ok()
        .and_then(|value| value.get(4..).map(startup::decode))
        .unwrap_or_default()
}

/// The systems hideBoot found at the last start, and the default.
fn show() -> Result<()> {
    let entries = read("LoaderEntries");
    let default = read("LoaderEntryDefault")
        .first()
        .map_or(System::HideOs, |id| startup::system_of(id));
    let windows = entries
        .iter()
        .any(|id| startup::system_of(id) == System::Windows);
    println!("starts: {}", default.name());
    println!(
        "Windows: {}",
        if windows {
            "found by hideBoot"
        } else {
            "not found"
        }
    );
    Ok(())
}

/// `system` as the default, or for the next start only.
fn choose(word: &str, once: bool) -> Result<()> {
    let system = System::parse(word).with_context(|| format!("`{word}` is hideos or windows"))?;
    let entries = read("LoaderEntries");
    let entry = startup::entry(system, &entries)?;
    let name = if once {
        "LoaderEntryOneShot"
    } else {
        "LoaderEntryDefault"
    };
    efivars::writable(|| match &entry {
        Some(id) => {
            let mut value = ATTRIBUTES.to_vec();
            value.extend(startup::encode(id));
            efivars::write(&variable(name), &value)
        }
        // hideOS: no entry named, and hideBoot chooses as it always does.
        None => efivars::remove(&variable(name)),
    })?;
    if once {
        println!("{} starts next, once; restart to start it.", system.name());
    } else {
        println!("{} starts from now on.", system.name());
    }
    Ok(())
}

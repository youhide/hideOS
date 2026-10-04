//! `hide setup`: what the writable parts of the system need before services
//! start, at every boot. A oneshot unit runs it first; see the
//! `hideos-units` recipe.
//!
//! In order: `/etc/machine-id`, then system users and groups from
//! `sysusers.d`, then paths from `tmpfiles.d` — which may name those users —
//! then a range of subordinate IDs for each person without one.
//! Each step leaves what already exists alone, so running it again is a
//! no-op.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hide::sysusers::{self, Database};
use hide::tmpfiles::{self, Action, Parsed};

pub fn run(args: &[String]) -> Result<()> {
    let root = match args {
        [] => PathBuf::from("/"),
        [flag, root] if flag == "--root" => PathBuf::from(root),
        _ => anyhow::bail!("usage: hide setup [--root DIR]"),
    };
    machine_id(&root)?;
    users(&root)?;
    paths(&root)?;
    subordinate_ids(&root)?;
    Ok(())
}

/// `/etc/subuid` and `/etc/subgid`: a range for each person, for rootless
/// containers. See hide::subid.
pub(crate) fn subordinate_ids(root: &Path) -> Result<()> {
    let passwd = fs::read_to_string(root.join("etc/passwd")).unwrap_or_default();
    for name in ["subuid", "subgid"] {
        let path = root.join("etc").join(name);
        let current = fs::read_to_string(&path).unwrap_or_default();
        let added = hide::subid::additions(&passwd, &current);
        if added.is_empty() {
            continue;
        }
        write_atomically(&path, &format!("{current}{added}"), 0o644)?;
        for line in added.lines() {
            say(&format!("{name} {line}"));
        }
    }
    Ok(())
}

/// The machine's identity, which D-Bus and elogind read. Made once, from
/// the kernel's random source, the first time the machine boots.
fn machine_id(root: &Path) -> Result<()> {
    let path = root.join("etc/machine-id");
    if fs::read_to_string(&path).is_ok_and(|id| id.trim().len() == 32) {
        return Ok(());
    }
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .context("reading /dev/urandom")?;
    let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    write_atomically(&path, &format!("{id}\n"), 0o444)?;
    say(&format!("machine-id {id}"));
    Ok(())
}

fn users(root: &Path) -> Result<()> {
    let mut entries = Vec::new();
    for file in config_files(root, "sysusers.d")? {
        let text = fs::read_to_string(&file)?;
        entries.extend(sysusers::parse(&text).with_context(|| format!("{}", file.display()))?);
    }
    let passwd_path = root.join("etc/passwd");
    let group_path = root.join("etc/group");
    let mut db = Database::parse(
        &fs::read_to_string(&passwd_path).unwrap_or_default(),
        &fs::read_to_string(&group_path).unwrap_or_default(),
    );
    let changes = db.apply(&entries).map_err(anyhow::Error::msg)?;
    if changes.is_empty() {
        return Ok(());
    }
    // group first: a passwd line naming a group that is not there yet is
    // the worse half-state.
    write_atomically(&group_path, &db.group_text(), 0o644)?;
    write_atomically(&passwd_path, &db.passwd_text(), 0o644)?;

    // New system users get a locked password: nobody logs in as them.
    let shadow_path = root.join("etc/shadow");
    let mut shadow = fs::read_to_string(&shadow_path).unwrap_or_default();
    for name in &db.new_users {
        if !shadow.lines().any(|l| l.split(':').next() == Some(name)) {
            shadow.push_str(&format!("{name}:!*:::::::\n"));
        }
    }
    write_atomically(&shadow_path, &shadow, 0o600)?;
    for change in changes {
        say(&format!("added {change}"));
    }
    Ok(())
}

fn paths(root: &Path) -> Result<()> {
    let passwd = fs::read_to_string(root.join("etc/passwd")).unwrap_or_default();
    let group = fs::read_to_string(root.join("etc/group")).unwrap_or_default();
    let id_of = |table: &str, name: &str| -> Option<u32> {
        table.lines().find_map(|l| {
            let mut f = l.split(':');
            (f.next() == Some(name))
                .then(|| f.nth(1).and_then(|id| id.parse().ok()))
                .flatten()
        })
    };
    for file in config_files(root, "tmpfiles.d")? {
        let text = fs::read_to_string(&file)?;
        for parsed in tmpfiles::parse(&text).with_context(|| format!("{}", file.display()))? {
            let line = match parsed {
                Parsed::Line(line) => line,
                Parsed::Skipped(text) => {
                    say(&format!("{}: skipped `{text}`", file.display()));
                    continue;
                }
            };
            let path = root.join(line.path.trim_start_matches('/'));
            if path.symlink_metadata().is_ok() {
                continue;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            match &line.action {
                Action::Directory => {
                    fs::create_dir(&path)?;
                    fs::set_permissions(
                        &path,
                        fs::Permissions::from_mode(line.mode.unwrap_or(0o755)),
                    )?;
                }
                Action::File { content } => {
                    let mut f = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(line.mode.unwrap_or(0o644))
                        .open(&path)?;
                    f.write_all(content.as_bytes())?;
                }
                Action::Symlink { target } => {
                    std::os::unix::fs::symlink(target, &path)?;
                    continue;
                }
            }
            let uid = line.user.as_deref().and_then(|u| id_of(&passwd, u));
            let gid = line.group.as_deref().and_then(|g| id_of(&group, g));
            if uid.is_some() || gid.is_some() {
                rustix::fs::chown(
                    &path,
                    uid.map(rustix::fs::Uid::from_raw),
                    gid.map(rustix::fs::Gid::from_raw),
                )
                .with_context(|| format!("chown {}", path.display()))?;
            }
        }
    }
    Ok(())
}

/// The `.conf` files of `/usr/lib/<dir>` and `/etc/<dir>`, overrides
/// replacing by name, in name order.
fn config_files(root: &Path, dir: &str) -> Result<Vec<PathBuf>> {
    let vendor = root.join("usr/lib").join(dir);
    let local = root.join("etc").join(dir);
    let names = |d: &Path| -> Vec<String> {
        fs::read_dir(d)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(hide::config::merge(&names(&vendor), &names(&local))
        .into_iter()
        .map(|(name, is_override)| {
            if is_override {
                local.join(name)
            } else {
                vendor.join(name)
            }
        })
        .collect())
}

/// Replaces `path` with `content` so that a power cut leaves the old file
/// or the new one, never half of either.
fn write_atomically(path: &Path, content: &str, mode: u32) -> Result<()> {
    let partial = path.with_extension("hide-new");
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&partial)
            .with_context(|| format!("writing {}", partial.display()))?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }
    fs::set_permissions(&partial, fs::Permissions::from_mode(mode))?;
    fs::rename(&partial, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn say(line: &str) {
    eprintln!("hide setup: {line}");
}

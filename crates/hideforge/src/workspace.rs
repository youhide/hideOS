//! A snapshot of this repository's Cargo workspace, for recipes that build
//! hideOS's own programs from it. See `Build::workspace` in hideforge-recipe.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::layout::Layout;

/// What a snapshot holds: enough to build every crate, and nothing whose
/// change should change a build. A new README must not move the seal.
const PATHS: &[&str] = &["Cargo.toml", "Cargo.lock", "crates"];

/// Archives the git-tracked files under [`PATHS`] — as they are in the
/// working tree, uncommitted changes included — into the source directory,
/// named by the archive's SHA-256, and returns the digest. `None` when
/// `root` is not a git checkout.
///
/// The archive is deterministic: sorted names, no owners, no timestamps. The
/// same files give the same digest on every machine.
pub fn snapshot(root: &Path, layout: &Layout) -> Result<Option<String>> {
    let listing = Command::new("git")
        .args(["-c", "safe.directory=*", "-C"])
        .arg(root)
        .args(["ls-files", "-z", "--"])
        .args(PATHS)
        .output()
        .context("running git")?;
    if !listing.status.success() {
        return Ok(None);
    }

    // Untracked files are left out, so that a scratch file cannot change a
    // build. A new source file is one too, until it is added; say so, or the
    // build fails much later on a missing module.
    let untracked = Command::new("git")
        .args(["-c", "safe.directory=*", "-C"])
        .arg(root)
        .args(["ls-files", "--others", "--exclude-standard", "--"])
        .args(PATHS)
        .output()
        .context("running git")?;
    for path in String::from_utf8_lossy(&untracked.stdout).lines() {
        eprintln!("  warning: {path} is untracked and not in the workspace snapshot; `git add` it");
    }

    fs::create_dir_all(layout.sources())?;
    let partial = layout
        .sources()
        .join(format!(".workspace-{}", std::process::id()));
    // --directory first: GNU tar applies it only to the names after it, and
    // the names come from --files-from.
    let mut tar = Command::new("tar")
        .arg("--directory")
        .arg(root)
        .args(["--create", "--sort=name"])
        .args(["--owner=0", "--group=0", "--numeric-owner", "--mtime=@0"])
        .args(["--null", "--files-from=-"])
        .arg("--file")
        .arg(&partial)
        .stdin(Stdio::piped())
        .spawn()
        .context("running tar")?;
    tar.stdin
        .take()
        .context("tar's stdin")?
        .write_all(&listing.stdout)?;
    if !tar.wait()?.success() {
        let _ = fs::remove_file(&partial);
        bail!("archiving the workspace failed");
    }

    let digest: String = Sha256::digest(fs::read(&partial)?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    fs::rename(&partial, layout.source(&digest))?;
    Ok(Some(digest))
}

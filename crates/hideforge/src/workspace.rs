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

/// The workspace's build tools, which never go into an image, and the one
/// target each has. A snapshot carries their manifests — cargo reads every
/// member's to load the workspace — and an empty target in place of their
/// code, so that a change to a tool does not rebuild, and re-seal, every
/// program built from the workspace.
const TOOLS: &[(&str, &str, &str)] = &[
    ("crates/xtask", "src/main.rs", "fn main() {}\n"),
    ("crates/hideforge", "src/main.rs", "fn main() {}\n"),
    ("crates/hideforge-recipe", "src/lib.rs", ""),
];

/// Whether `path`, from git, goes into a snapshot: everything but a tool's
/// files other than its manifest.
fn carried(path: &str) -> bool {
    TOOLS.iter().all(|(dir, _, _)| {
        path.strip_prefix(dir)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_none_or(|rest| rest == "Cargo.toml")
    })
}

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

    let listing: Vec<u8> = listing
        .stdout
        .split(|&b| b == 0)
        .filter(|path| !path.is_empty())
        .filter(|path| carried(&String::from_utf8_lossy(path)))
        .flat_map(|path| path.iter().copied().chain([0]))
        .collect();

    fs::create_dir_all(layout.sources())?;
    let partial = layout
        .sources()
        .join(format!(".workspace-{}", crate::layout::run_id()));
    let stubs = layout
        .sources()
        .join(format!(".workspace-stubs-{}", crate::layout::run_id()));
    let _ = fs::remove_dir_all(&stubs);
    let mut stub_paths = Vec::new();
    for (dir, target, text) in TOOLS {
        let path = format!("{dir}/{target}");
        let file = stubs.join(&path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&file, text)?;
        stub_paths.push(path);
    }
    // --directory first: GNU tar applies it only to the names after it, and
    // the names come from --files-from.
    let mut tar = Command::new("tar")
        .arg("--directory")
        .arg(root)
        .args(["--create", "--sort=name"])
        .args(["--owner=0", "--group=0", "--numeric-owner", "--mtime=@0"])
        .arg("--file")
        .arg(&partial)
        .args(["--null", "--files-from=-"])
        // The tools' stubs, after the checkout's files, from their own
        // directory: --directory applies to the names after it.
        .arg("--directory")
        .arg(&stubs)
        .args(&stub_paths)
        .stdin(Stdio::piped())
        .spawn()
        .context("running tar")?;
    tar.stdin
        .take()
        .context("tar's stdin")?
        .write_all(&listing)?;
    let archived = tar.wait()?.success();
    let _ = fs::remove_dir_all(&stubs);
    if !archived {
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

#[cfg(test)]
mod tests {
    use super::carried;

    #[test]
    fn a_tool_is_carried_as_its_manifest_alone() {
        assert!(carried("Cargo.toml"));
        assert!(carried("crates/hide/src/main.rs"));
        assert!(carried("crates/xtask/Cargo.toml"));
        assert!(!carried("crates/xtask/src/main.rs"));
        assert!(!carried("crates/hideforge/src/workspace.rs"));
        assert!(!carried("crates/hideforge-recipe/src/lib.rs"));
        assert!(carried("crates/hideforge-recipe/Cargo.toml"));
        // A crate whose name only starts like a tool's is not one.
        assert!(carried("crates/xtask-extra/src/main.rs"));
    }
}

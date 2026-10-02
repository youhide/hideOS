//! Sources: download, verify, unpack, patch. All of this happens outside the
//! sandbox, before it exists, because it is the only part of a build that is
//! allowed to touch the network.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use hideforge_recipe::{Arch, Entry, Vendor};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::layout::Layout;

/// Downloads every source of `entry` that is not already in the source
/// directory, and checks each against the recipe's digest.
pub fn fetch(layout: &Layout, entry: &Entry, arch: Arch) -> Result<()> {
    fs::create_dir_all(layout.sources())?;
    for source in entry.recipe.sources.iter().filter(|s| s.applies_to(arch)) {
        let path = layout.source(&source.sha256);
        if path.is_file() {
            continue;
        }
        download(&mirrors(&source.url), &path, &source.sha256)?;
    }
    Ok(())
}

/// Where to try downloading `url` from, in order. The recipe's URL first,
/// then mirrors of the same tree for hosts that have them. Safe because the
/// digest is checked whatever the source: a mirror can fail to deliver, but
/// it cannot deliver something else.
pub fn mirrors(url: &str) -> Vec<String> {
    let mut urls = vec![url.to_owned()];
    if let Some(path) = url.strip_prefix("https://ftp.gnu.org/gnu/") {
        urls.push(format!("https://ftpmirror.gnu.org/gnu/{path}"));
        urls.push(format!("https://mirrors.kernel.org/gnu/{path}"));
    }
    urls
}

/// Unpacks every source into `src` and applies the recipe's patches. Returns
/// the newest modification time in the unpacked tree, before patching, which
/// becomes `SOURCE_DATE_EPOCH`: the sources' own idea of when they were made,
/// and the same on every machine.
pub fn prepare(layout: &Layout, entry: &Entry, src: &Path, arch: Arch) -> Result<u64> {
    fs::create_dir_all(src)?;
    for source in entry.recipe.sources.iter().filter(|s| s.applies_to(arch)) {
        let archive = layout.source(&source.sha256);
        let dest = src.join(&source.dest);
        fs::create_dir_all(&dest)?;
        if source.extract {
            let status = Command::new("tar")
                .arg("--extract")
                .arg("--file")
                .arg(&archive)
                .arg("--directory")
                .arg(&dest)
                .arg(format!("--strip-components={}", source.strip))
                .arg("--no-same-owner")
                .status()
                .context("running tar")?;
            if !status.success() {
                bail!("unpacking {} failed", source.url);
            }
        } else {
            let name = source
                .url
                .rsplit('/')
                .next()
                .filter(|n| !n.is_empty())
                .unwrap_or("source");
            fs::copy(&archive, dest.join(name))?;
        }
    }

    let epoch = newest_mtime(src)?;

    // The recipe's own files, where the script finds them as $FILES. After
    // the epoch is taken: they are hideOS's, not the source's, and their
    // checkout times say nothing about when the source was made.
    if entry.files_dir.is_dir() {
        copy_tree(&entry.files_dir, &src.join(FILES_DIR))?;
    }

    if entry.recipe.build.vendor == Some(Vendor::Cargo) {
        vendor_cargo(layout, src)?;
    }

    for patch in &entry.recipe.build.patches {
        let file = entry.files_dir.join(patch);
        eprintln!("  patch {patch}");
        let status = Command::new("patch")
            .args(["--strip=1", "--forward", "--batch", "--input"])
            .arg(&file)
            .current_dir(src)
            .status()
            .context("running patch")?;
        if !status.success() {
            bail!("{} does not apply", file.display());
        }
    }
    Ok(epoch)
}

/// Where a recipe's files are, under `/build/src`.
pub const FILES_DIR: &str = ".hideforge-files";

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for item in fs::read_dir(from)? {
        let item = item?;
        let target = to.join(item.file_name());
        if item.file_type()?.is_dir() {
            copy_tree(&item.path(), &target)?;
        } else {
            fs::copy(item.path(), &target)?;
        }
    }
    Ok(())
}

/// The parts of a `Cargo.lock` vendoring needs.
#[derive(Deserialize)]
struct CargoLock {
    #[serde(default)]
    package: Vec<LockedPackage>,
}

#[derive(Deserialize)]
struct LockedPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

/// A crate the lockfile pins: name, version, SHA-256 of the `.crate` file.
#[derive(Debug, PartialEq, Eq)]
pub struct Crate {
    pub name: String,
    pub version: String,
    pub sha256: String,
}

const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// The crates.io packages a lockfile pins. Workspace members have no source
/// and are skipped; anything from another registry or from git is an error,
/// because nothing else gives a checksum to hold the download to.
pub fn locked_crates(lockfile: &str) -> Result<Vec<Crate>> {
    let lock: CargoLock = basic_toml::from_str(lockfile).context("parsing Cargo.lock")?;
    let mut crates = Vec::new();
    for package in lock.package {
        let Some(source) = package.source else {
            continue;
        };
        if source != CRATES_IO {
            bail!(
                "{} {} comes from {source}; only crates.io packages can be vendored",
                package.name,
                package.version
            );
        }
        let sha256 = package.checksum.ok_or_else(|| {
            anyhow::anyhow!(
                "{} {} has no checksum in Cargo.lock",
                package.name,
                package.version
            )
        })?;
        crates.push(Crate {
            name: package.name,
            version: package.version,
            sha256,
        });
    }
    Ok(crates)
}

/// Downloads and unpacks every crate in `src/Cargo.lock` into
/// `src/.hideforge-vendor`, and points cargo at it, offline. The lockfile is
/// part of the source archive, so its checksums are covered by the recipe's
/// own: the crates are as pinned as the source is.
fn vendor_cargo(layout: &Layout, src: &Path) -> Result<()> {
    let lockfile = fs::read_to_string(src.join("Cargo.lock"))
        .context("vendor = \"cargo\" needs a Cargo.lock at the top of the source")?;
    let crates = locked_crates(&lockfile)?;
    let vendor = src.join(".hideforge-vendor");
    fs::create_dir_all(&vendor)?;
    eprintln!("  vendor {} crates", crates.len());
    for krate in &crates {
        let archive = layout.source(&krate.sha256);
        if !archive.is_file() {
            let url = format!(
                "https://static.crates.io/crates/{0}/{0}-{1}.crate",
                krate.name, krate.version
            );
            download(&[url], &archive, &krate.sha256)?;
        }
        let dest = vendor.join(format!("{}-{}", krate.name, krate.version));
        fs::create_dir_all(&dest)?;
        let status = Command::new("tar")
            .arg("--extract")
            .arg("--file")
            .arg(&archive)
            .arg("--directory")
            .arg(&dest)
            .args(["--strip-components=1", "--no-same-owner"])
            .status()
            .context("running tar")?;
        if !status.success() {
            bail!("unpacking {} {} failed", krate.name, krate.version);
        }
        // Cargo checks vendored crates against this file. An empty file list
        // skips per-file checks, which the archive's checksum already covers.
        fs::write(
            dest.join(".cargo-checksum.json"),
            format!("{{\"files\":{{}},\"package\":\"{}\"}}", krate.sha256),
        )?;
    }
    fs::create_dir_all(src.join(".cargo"))?;
    fs::write(
        src.join(".cargo/config.toml"),
        "[source.crates-io]\nreplace-with = \"hideforge-vendor\"\n\n\
         [source.hideforge-vendor]\ndirectory = \"/build/src/.hideforge-vendor\"\n\n\
         [net]\noffline = true\n",
    )?;
    Ok(())
}

/// Downloads the first of `urls` that answers to `path`, keeping it only if
/// its SHA-256 is `sha256`.
fn download(urls: &[String], path: &Path, sha256: &str) -> Result<()> {
    let partial = path.with_extension("part");
    for url in urls {
        eprintln!("  fetch {url}");
        let status = Command::new("curl")
            .args(["--fail", "--location", "--silent", "--show-error"])
            .args(["--retry", "2", "--connect-timeout", "20"])
            .args(["--proto", "=https", "--tlsv1.2"])
            .arg("--output")
            .arg(&partial)
            .arg(url)
            .status()
            .context("running curl")?;
        if !status.success() {
            let _ = fs::remove_file(&partial);
            continue;
        }
        let actual = sha256_file(&partial)?;
        if actual != sha256 {
            let _ = fs::remove_file(&partial);
            bail!("{url}: SHA-256 mismatch\n  expected   {sha256}\n  download is {actual}");
        }
        fs::rename(&partial, path)?;
        return Ok(());
    }
    bail!("download failed from every URL: {}", urls.join(", "))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| path.display().to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(buffer.get(..read).unwrap_or_default());
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The newest mtime under `dir`, in seconds, not following symlinks. Zero for
/// an empty tree, which is a recipe with no sources: the epoch then says
/// nothing, and that is honest.
fn newest_mtime(dir: &Path) -> io::Result<u64> {
    let mut newest = 0;
    let mut pending: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for item in fs::read_dir(&next)? {
            let item = item?;
            let meta = fs::symlink_metadata(item.path())?;
            if let Ok(modified) = meta.modified() {
                let secs = modified
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                newest = newest.max(secs);
            }
            if meta.is_dir() {
                pending.push(item.path());
            }
        }
    }
    Ok(newest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfiles_yield_crates_io_packages_and_skip_workspace_members() {
        let lock = r#"
version = 4

[[package]]
name = "oxinit"
version = "0.1.0"
dependencies = ["rustix"]

[[package]]
name = "rustix"
version = "1.1.4"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaaa"
"#;
        assert_eq!(
            locked_crates(lock).unwrap(),
            [Crate {
                name: "rustix".to_owned(),
                version: "1.1.4".to_owned(),
                sha256: "aaaa".to_owned(),
            }]
        );
    }

    #[test]
    fn git_dependencies_cannot_be_vendored() {
        let lock = r#"
[[package]]
name = "thing"
version = "0.1.0"
source = "git+https://example.com/thing#abc"
"#;
        assert!(locked_crates(lock).is_err());
    }

    #[test]
    fn gnu_sources_have_mirrors_others_do_not() {
        assert_eq!(
            mirrors("https://ftp.gnu.org/gnu/gcc/gcc-16.2.0/gcc-16.2.0.tar.xz"),
            [
                "https://ftp.gnu.org/gnu/gcc/gcc-16.2.0/gcc-16.2.0.tar.xz",
                "https://ftpmirror.gnu.org/gnu/gcc/gcc-16.2.0/gcc-16.2.0.tar.xz",
                "https://mirrors.kernel.org/gnu/gcc/gcc-16.2.0/gcc-16.2.0.tar.xz",
            ]
        );
        assert_eq!(
            mirrors("https://astron.com/pub/file/file-5.48.tar.gz"),
            ["https://astron.com/pub/file/file-5.48.tar.gz"]
        );
    }
}

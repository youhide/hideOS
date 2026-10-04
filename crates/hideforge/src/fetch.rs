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
/// the newest modification time of a file in the unpacked tree, before
/// patching, which becomes `SOURCE_DATE_EPOCH`: the sources' own idea of when
/// they were made, and the same on every machine.
///
/// Files only, and only what came out of an archive: a directory made here
/// for a source's `dest`, or a source copied in whole (`extract = false`),
/// has the time of the build, and counting it made the epoch that — a
/// different one on every build. A copied source is given the epoch itself.
pub fn prepare(
    layout: &Layout,
    entry: &Entry,
    src: &Path,
    arch: Arch,
    workspace: Option<&str>,
) -> Result<u64> {
    fs::create_dir_all(src)?;
    let mut copied = Vec::new();
    if entry.recipe.build.workspace {
        let digest =
            workspace.context("this recipe builds from the workspace, and there is none")?;
        let status = Command::new("tar")
            .arg("--extract")
            .arg("--file")
            .arg(layout.source(digest))
            .arg("--directory")
            .arg(src)
            .arg("--no-same-owner")
            .status()
            .context("running tar")?;
        if !status.success() {
            bail!("unpacking the workspace failed");
        }
    }
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
            copied.push(dest.join(name));
        }
    }

    let epoch = newest_mtime(src, &copied)?;
    for file in &copied {
        let time = UNIX_EPOCH + std::time::Duration::from_secs(epoch);
        fs::File::options().write(true).open(file)?.set_modified(time)?;
    }

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
        // Absolute: patch runs inside the source tree, and the recipe's
        // directory is named relative to where hideforge was started.
        let file = std::path::absolute(entry.files_dir.join(patch))?;
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

/// What a lockfile pins, by where it comes from.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Locked {
    /// crates.io packages, each with the checksum of its `.crate` file.
    pub crates: Vec<Crate>,
    /// Packages from git, as `(name, source)`: the source ends in the full
    /// commit ID, which is what pins them.
    pub git: Vec<(String, String)>,
}

/// The packages a lockfile pins. Workspace members have no source and are
/// skipped. crates.io packages need a checksum, git packages a full commit
/// ID; anything else, or either without its pin, is an error, because then
/// nothing holds the download to what the lockfile meant.
pub fn locked_crates(lockfile: &str) -> Result<Locked> {
    let lock: CargoLock = basic_toml::from_str(lockfile).context("parsing Cargo.lock")?;
    let mut locked = Locked::default();
    for package in lock.package {
        let Some(source) = package.source else {
            continue;
        };
        if source.starts_with("git+https://") {
            let commit = source.rsplit_once('#').map(|(_, c)| c).unwrap_or("");
            if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!(
                    "{} {} comes from {source}, which names no full commit",
                    package.name,
                    package.version
                );
            }
            locked.git.push((package.name, source));
            continue;
        }
        if source != CRATES_IO {
            bail!(
                "{} {} comes from {source}; only crates.io and git over https can be vendored",
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
        locked.crates.push(Crate {
            name: package.name,
            version: package.version,
            sha256,
        });
    }
    Ok(locked)
}

/// Downloads and unpacks every crate in `src/Cargo.lock` into
/// `src/.hideforge-vendor`, and points cargo at it, offline. The lockfile is
/// part of the source archive, so its checksums are covered by the recipe's
/// own: the crates are as pinned as the source is.
fn vendor_cargo(layout: &Layout, src: &Path) -> Result<()> {
    let lockfile = fs::read_to_string(src.join("Cargo.lock"))
        .context("vendor = \"cargo\" needs a Cargo.lock at the top of the source")?;
    let locked = locked_crates(&lockfile)?;
    if !locked.git.is_empty() {
        return vendor_cargo_git(layout, src, locked.git.len());
    }
    let crates = locked.crates;
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

/// Vendoring for lockfiles with git dependencies, which is most of COSMIC:
/// `cargo vendor`, run here, where the network is allowed.
///
/// A crate in a git repository is not a directory that can be copied. Its
/// manifest inherits from the repository's workspace, its path dependencies
/// point at siblings, and cargo turns all that into a standalone package when
/// it vendors. Doing that by hand would be reimplementing cargo, so cargo
/// does it. What is vendored is still pinned: git packages by the full commit
/// in Cargo.lock, crates.io packages by checksum, which cargo verifies, and
/// Cargo.lock by the recipe's digest of the source. The builder's cargo
/// shapes only how the vendored manifests are written.
fn vendor_cargo_git(layout: &Layout, src: &Path, git: usize) -> Result<()> {
    eprintln!("  vendor with cargo ({git} packages from git)");
    let vendor = src.join(".hideforge-vendor");
    let output = Command::new("cargo")
        .args(["vendor", "--locked", "--versioned-dirs"])
        .arg(&vendor)
        .current_dir(src)
        // A cache that outlives the build, so a rebuild does not download
        // everything again. Not trusted: cargo checks every crate it takes
        // from it against the lockfile.
        .env("CARGO_HOME", layout.cargo_home())
        .output()
        .context("running cargo vendor")?;
    if !output.status.success() {
        bail!(
            "cargo vendor failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // What cargo prints is the configuration that uses what it vendored,
    // with the path as it was given; inside the sandbox the source is at
    // /build/src.
    let config = String::from_utf8(output.stdout)
        .context("cargo vendor printed something that is not UTF-8")?
        .replace(
            &vendor.display().to_string(),
            "/build/src/.hideforge-vendor",
        );
    // Without it, cargo would go to the network for the very sources just
    // vendored, and fail much later, offline, in the sandbox.
    if !config.contains("replace-with") {
        bail!("cargo vendor printed no source replacement:\n{config}");
    }
    fs::create_dir_all(src.join(".cargo"))?;
    fs::write(
        src.join(".cargo/config.toml"),
        format!("{config}\n[net]\noffline = true\n"),
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
/// The newest modification time of a file under `dir`, leaving out `skip`.
/// 1980-01-01 when there is none — a recipe whose sources are all copied
/// whole — rather than 1970: zip, and so Python's wheels, cannot store
/// anything older.
fn newest_mtime(dir: &Path, skip: &[PathBuf]) -> io::Result<u64> {
    const NO_FILES: u64 = 315_532_800;
    let mut newest = None;
    let mut pending: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for item in fs::read_dir(&next)? {
            let item = item?;
            let path = item.path();
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                pending.push(path);
                continue;
            }
            if skip.contains(&path) {
                continue;
            }
            if let Ok(modified) = meta.modified() {
                let secs = modified
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                newest = Some(newest.unwrap_or(0).max(secs));
            }
        }
    }
    Ok(newest.unwrap_or(NO_FILES))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_is_the_newest_file_not_a_directory_or_a_copy() {
        let dir = std::env::temp_dir().join(format!("hideforge-epoch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        let old = UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        for name in ["a", "sub/b"] {
            fs::write(dir.join(name), "x").unwrap();
            fs::File::options()
                .write(true)
                .open(dir.join(name))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        // Made now: a directory, and a copied source.
        fs::create_dir_all(dir.join("dest")).unwrap();
        fs::write(dir.join("dest/copied.run"), "x").unwrap();
        let copied = [dir.join("dest/copied.run")];
        assert_eq!(newest_mtime(&dir, &copied).unwrap(), 1_000_000_000);
        assert_eq!(newest_mtime(&dir.join("dest"), &copied).unwrap(), 315_532_800);
        fs::remove_dir_all(&dir).unwrap();
    }

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
            locked_crates(lock).unwrap().crates,
            [Crate {
                name: "rustix".to_owned(),
                version: "1.1.4".to_owned(),
                sha256: "aaaa".to_owned(),
            }]
        );
    }

    #[test]
    fn git_dependencies_must_name_a_full_commit() {
        let pinned = r#"
[[package]]
name = "thing"
version = "0.1.0"
source = "git+https://example.com/thing?rev=abc#0123456789abcdef0123456789abcdef01234567"
"#;
        assert_eq!(locked_crates(pinned).unwrap().git.len(), 1);
        let short = r#"
[[package]]
name = "thing"
version = "0.1.0"
source = "git+https://example.com/thing#abc"
"#;
        assert!(locked_crates(short).is_err());
        let insecure = r#"
[[package]]
name = "thing"
version = "0.1.0"
source = "git+http://example.com/thing#0123456789abcdef0123456789abcdef01234567"
"#;
        assert!(locked_crates(insecure).is_err());
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

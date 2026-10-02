//! Sources: download, verify, unpack, patch. All of this happens outside the
//! sandbox, before it exists, because it is the only part of a build that is
//! allowed to touch the network.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use hideforge_recipe::Entry;
use sha2::{Digest, Sha256};

use crate::layout::Layout;

/// Downloads every source of `entry` that is not already in the source
/// directory, and checks each against the recipe's digest.
pub fn fetch(layout: &Layout, entry: &Entry) -> Result<()> {
    fs::create_dir_all(layout.sources())?;
    for source in &entry.recipe.sources {
        let path = layout.source(&source.sha256);
        if path.is_file() {
            continue;
        }
        let partial = path.with_extension("part");
        let mut fetched = false;
        for url in mirrors(&source.url) {
            eprintln!("  fetch {url}");
            let status = Command::new("curl")
                .args(["--fail", "--location", "--silent", "--show-error"])
                .args(["--retry", "2", "--connect-timeout", "20"])
                .args(["--proto", "=https", "--tlsv1.2"])
                .arg("--output")
                .arg(&partial)
                .arg(&url)
                .status()
                .context("running curl")?;
            if status.success() {
                fetched = true;
                break;
            }
            let _ = fs::remove_file(&partial);
        }
        if !fetched {
            bail!("download failed from every mirror: {}", source.url);
        }
        let actual = sha256_file(&partial)?;
        if actual != source.sha256 {
            let _ = fs::remove_file(&partial);
            bail!(
                "{}: SHA-256 mismatch\n  recipe says {}\n  download is {actual}",
                source.url,
                source.sha256
            );
        }
        fs::rename(&partial, &path)?;
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
pub fn prepare(layout: &Layout, entry: &Entry, src: &Path) -> Result<u64> {
    fs::create_dir_all(src)?;
    for source in &entry.recipe.sources {
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

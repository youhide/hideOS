//! An image's output directory, left as it is when it was assembled from the
//! same inputs. Assembling copies every layer of the image's run closure —
//! gigabytes, for the Workstation — and every test asks for its image again;
//! when nothing it is made of has changed, the image is the one already
//! there.
//!
//! The stamp, `.hideforge-image` in the output directory, holds a key and
//! the files assembling wrote, each with its size and modification time. The
//! key is a digest of everything the image is a function of: the input hash
//! of every recipe in it, the command line, the signing keys, and this
//! hideforge, whose code is how it is assembled. A file changed or removed
//! since — a test that tampers with the kernel image in place — makes the
//! directory stale, and the image is assembled again.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use hideforge_recipe::InputHash;
use sha2::{Digest, Sha256};

const STAMP: &str = ".hideforge-image";

/// The digest of what an image assembled with `args` is made of.
pub fn key(
    hashes: &BTreeMap<String, InputHash>,
    args: &[String],
    sign: Option<&Path>,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(fs::read(std::env::current_exe()?)?);
    for (name, hash) in hashes {
        hasher.update(format!("{name}={hash}\n"));
    }
    for arg in args {
        hasher.update(arg.as_bytes());
        hasher.update([0]);
    }
    if let Some(dir) = sign {
        let mut keys: Vec<_> = fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        keys.sort();
        for key in keys {
            hasher.update(key.file_name().unwrap_or_default().as_encoded_bytes());
            hasher.update(fs::read(&key)?);
        }
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The regular files directly in `dir`, with their size and modification
/// time in nanoseconds.
pub fn files(dir: &Path) -> BTreeMap<String, (u64, u128)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return BTreeMap::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let meta = entry.metadata().ok()?;
            if !meta.is_file() || name == STAMP {
                return None;
            }
            let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
            Some((name, (meta.len(), modified.as_nanos())))
        })
        .collect()
}

/// Whether `dir` holds what assembling with `key` wrote, as it wrote it.
pub fn fresh(dir: &Path, key: &str) -> bool {
    let Ok(text) = fs::read_to_string(dir.join(STAMP)) else {
        return false;
    };
    let mut lines = text.lines();
    if lines.next() != Some(key) {
        return false;
    }
    let now = files(dir);
    let mut any = false;
    for line in lines {
        let mut fields = line.split('\t');
        let (Some(name), Some(size), Some(modified)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return false;
        };
        let (Ok(size), Ok(modified)) = (size.parse::<u64>(), modified.parse::<u128>()) else {
            return false;
        };
        if now.get(name) != Some(&(size, modified)) {
            return false;
        }
        any = true;
    }
    any
}

/// Forgets the stamp before assembling, so a run stopped halfway leaves no
/// claim behind, and removes the files it lists. Assembling writes its
/// files in place, and one of them may be a hard link another directory
/// shares — `cargo xtask round` gives each lane the image by linking — so it
/// writes new files instead.
pub fn clear(dir: &Path) {
    if let Ok(text) = fs::read_to_string(dir.join(STAMP)) {
        for line in text.lines().skip(1) {
            if let Some(name) = line.split('\t').next().filter(|n| !n.contains('/')) {
                let _ = fs::remove_file(dir.join(name));
            }
        }
    }
    let _ = fs::remove_file(dir.join(STAMP));
}

/// Records that `dir` was assembled with `key`: the files that are new or
/// changed since `before`, a listing taken just before assembling.
pub fn write(dir: &Path, key: &str, before: &BTreeMap<String, (u64, u128)>) -> Result<()> {
    let mut text = format!("{key}\n");
    for (name, (size, modified)) in files(dir) {
        if before.get(&name) != Some(&(size, modified)) {
            text.push_str(&format!("{name}\t{size}\t{modified}\n"));
        }
    }
    fs::write(dir.join(STAMP), text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_holds_until_a_file_it_lists_changes() {
        let dir = std::env::temp_dir().join(format!("hideforge-stamp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("disk.raw"), "a test's").unwrap();
        let before = files(&dir);
        fs::write(dir.join("image.efi"), "uki").unwrap();
        write(&dir, "k", &before).unwrap();
        assert!(fresh(&dir, "k"));
        assert!(!fresh(&dir, "other"));
        // A file the image does not own may change.
        fs::write(dir.join("disk.raw"), "another test's").unwrap();
        assert!(fresh(&dir, "k"));
        // One it owns may not.
        fs::write(dir.join("image.efi"), "tampered").unwrap();
        assert!(!fresh(&dir, "k"));
        fs::write(dir.join("image.efi"), "uki").unwrap();
        write(&dir, "k", &BTreeMap::new()).unwrap();
        fs::remove_file(dir.join("image.efi")).unwrap();
        assert!(!fresh(&dir, "k"));
        fs::write(dir.join("image.efi"), "uki").unwrap();
        write(&dir, "k", &BTreeMap::new()).unwrap();
        clear(&dir);
        assert!(!fresh(&dir, "k"));
        assert!(
            !dir.join("image.efi").exists(),
            "clear removes what the stamp listed"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

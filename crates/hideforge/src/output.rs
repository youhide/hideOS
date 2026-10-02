//! Checks on what a build produced, and on what it was given.
//!
//! Portable on purpose: these read directory trees with `std` and nothing
//! else, so they are tested on any host.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Something a finished build did that it may not.
#[derive(Debug, PartialEq, Eq)]
pub enum Violation {
    /// It deleted a file one of its inputs provided. Overlay records a
    /// deletion as a character device with device number 0/0.
    Deleted(PathBuf),
    /// It replaced or modified a file one of its inputs provided, so the file
    /// was copied up into the output.
    Replaced(PathBuf),
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Violation::Deleted(path) => write!(f, "deleted inherited /{}", path.display()),
            Violation::Replaced(path) => write!(f, "replaced inherited /{}", path.display()),
        }
    }
}

/// One input layered into a sandbox.
#[derive(Debug, Clone)]
pub struct Layer {
    pub name: String,
    pub path: PathBuf,
    pub stage: u8,
}

/// Walks the upper layer and reports every inherited file the build deleted or
/// replaced — if that file came from a layer of the build's own stage.
///
/// Replacing what an *earlier* stage provided is the point of a bootstrap:
/// stage 1's bash is built in a root where stage 0's bash is `/usr/bin/bash`,
/// and installs over it. Replacing what the *same* stage provided means two
/// recipes of one stage both claim a file, and that is still an error.
///
/// Directories are not reported: overlay copies a directory up whenever
/// something is created inside it, and that is the normal case.
pub fn check_upper(upper: &Path, layers: &[Layer], stage: u8) -> io::Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for (relative, meta) in walk(upper)? {
        let kind = meta.file_type();
        if kind.is_dir() {
            continue;
        }
        let same_stage = layers
            .iter()
            .filter(|layer| layer.stage == stage)
            .any(|layer| fs::symlink_metadata(layer.path.join(&relative)).is_ok());
        if !same_stage {
            continue;
        }
        if is_whiteout(&meta) {
            violations.push(Violation::Deleted(relative));
        } else {
            violations.push(Violation::Replaced(relative));
        }
    }
    Ok(violations)
}

/// Removes the whiteouts left in an upper layer. Once [`check_upper`] has
/// passed, each one records the deletion of an earlier stage's file, which
/// mattered inside that build's sandbox and means nothing outside it: the
/// output never contained the file in the first place.
pub fn remove_whiteouts(upper: &Path) -> io::Result<()> {
    for (relative, meta) in walk(upper)? {
        if is_whiteout(&meta) {
            fs::remove_file(upper.join(relative))?;
        }
    }
    Ok(())
}

/// Overlay records a deletion as a character device with device number 0/0.
fn is_whiteout(meta: &fs::Metadata) -> bool {
    meta.file_type().is_char_device() && meta.rdev() == 0
}

/// Files no output may contain, because they are indexes over *every*
/// package's files and so belong to the image, not to any one package. The
/// second package to install documentation would otherwise rewrite the first
/// one's copy and fail the build for it. They are generated when an image is
/// assembled.
///
/// - `share/info/dir`: the Info directory, rewritten by every `install-info`.
/// - `etc/ld.so.cache`: the dynamic linker's library cache, rewritten by every
///   `ldconfig` a `make install` runs. Without it the linker searches its
///   default paths, which on hideOS is everything.
///
/// Changing this list changes what outputs contain: bump
/// `hideforge_recipe::OUTPUT_POLICY` with it.
const IMAGE_INDEXES: &[&str] = &["share/info/dir", "etc/ld.so.cache"];

/// Removes [`IMAGE_INDEXES`] from an output, wherever they appear in it:
/// `usr/share/info/dir`, `tools/share/info/dir`, and so on.
pub fn remove_image_indexes(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    for (relative, meta) in walk(dir)? {
        if meta.is_file() && IMAGE_INDEXES.iter().any(|index| relative.ends_with(index)) {
            fs::remove_file(dir.join(&relative))?;
            removed.push(relative);
        }
    }
    Ok(removed)
}

/// Two layers of the same stage providing the same non-directory path. In an
/// overlay the upper one would silently win; here it is an error that names
/// both.
#[derive(Debug, PartialEq, Eq)]
pub struct Conflict {
    pub path: PathBuf,
    pub first: String,
    pub second: String,
}

/// Every path provided by more than one layer of the same stage. Directories
/// may be shared, and a later stage's file shadows an earlier stage's: see
/// [`check_upper`] for why.
pub fn conflicts(layers: &[Layer]) -> io::Result<Vec<Conflict>> {
    let mut owner: BTreeMap<(u8, PathBuf), &str> = BTreeMap::new();
    let mut found = Vec::new();
    for layer in layers {
        for (relative, meta) in walk(&layer.path)? {
            if meta.is_dir() {
                continue;
            }
            let key = (layer.stage, relative);
            match owner.get(&key) {
                Some(first) => found.push(Conflict {
                    path: key.1,
                    first: (*first).to_owned(),
                    second: layer.name.clone(),
                }),
                None => {
                    owner.insert(key, &layer.name);
                }
            }
        }
    }
    Ok(found)
}

/// The overlay's lower layers, top first: later stages above earlier ones,
/// so a later stage's file is the one a build sees. Within a stage the order
/// does not matter, because [`conflicts`] guarantees no overlap; it is by name
/// so that it is the same on every run.
pub fn overlay_order(layers: &mut [Layer]) {
    layers.sort_by(|a, b| b.stage.cmp(&a.stage).then_with(|| a.name.cmp(&b.name)));
}

/// Every entry under `root`, as (path relative to root, metadata), not
/// following symlinks. Sorted, so reports come out in the same order on every
/// run.
pub fn walk(root: &Path) -> io::Result<Vec<(PathBuf, fs::Metadata)>> {
    let mut out = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for item in fs::read_dir(root.join(&relative))? {
            let item = item?;
            let child = relative.join(item.file_name());
            let meta = fs::symlink_metadata(item.path())?;
            if meta.is_dir() {
                pending.push(child.clone());
            }
            out.push((child, meta));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Scratch {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "hideforge-output-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn file(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, relative).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn layer(s: &Scratch, name: &str, stage: u8) -> Layer {
        Layer {
            name: name.to_owned(),
            path: s.0.join(name),
            stage,
        }
    }

    #[test]
    fn new_files_and_shared_directories_are_fine() {
        let s = Scratch::new();
        s.file("lower/usr/lib/libc.so");
        s.file("upper/usr/lib/libz.so");
        let layers = [layer(&s, "lower", 2)];
        let violations = check_upper(&s.0.join("upper"), &layers, 2).unwrap();
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn replacing_a_same_stage_file_is_reported() {
        let s = Scratch::new();
        s.file("lower/usr/share/doc/index");
        s.file("upper/usr/share/doc/index");
        s.file("upper/usr/share/doc/zlib");
        let layers = [layer(&s, "lower", 2)];
        let violations = check_upper(&s.0.join("upper"), &layers, 2).unwrap();
        assert_eq!(
            violations,
            [Violation::Replaced(PathBuf::from("usr/share/doc/index"))]
        );
    }

    #[test]
    fn replacing_an_earlier_stage_file_is_the_bootstrap() {
        let s = Scratch::new();
        s.file("stage0-bash/usr/bin/bash");
        s.file("upper/usr/bin/bash");
        let layers = [layer(&s, "stage0-bash", 0)];
        let violations = check_upper(&s.0.join("upper"), &layers, 1).unwrap();
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn same_stage_layers_conflict_earlier_stages_are_shadowed() {
        let s = Scratch::new();
        s.file("stage0-bash/usr/bin/bash");
        s.file("stage1-bash/usr/bin/bash");
        s.file("stage1-gcc/usr/bin/gcc");
        s.file("stage1-other/usr/bin/gcc");
        let mut layers = vec![
            layer(&s, "stage0-bash", 0),
            layer(&s, "stage1-bash", 1),
            layer(&s, "stage1-gcc", 1),
            layer(&s, "stage1-other", 1),
        ];
        let found = conflicts(&layers).unwrap();
        assert_eq!(
            found,
            [Conflict {
                path: PathBuf::from("usr/bin/gcc"),
                first: "stage1-gcc".to_owned(),
                second: "stage1-other".to_owned(),
            }]
        );
        overlay_order(&mut layers);
        let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            ["stage1-bash", "stage1-gcc", "stage1-other", "stage0-bash"]
        );
    }

    #[test]
    fn image_indexes_are_removed_wherever_they_are() {
        let s = Scratch::new();
        s.file("out/usr/share/info/dir");
        s.file("out/tools/share/info/dir");
        s.file("out/usr/share/info/gcc.info");
        s.file("out/usr/share/dir");
        s.file("out/etc/ld.so.cache");
        let removed = remove_image_indexes(&s.0.join("out")).unwrap();
        assert_eq!(
            removed,
            [
                PathBuf::from("etc/ld.so.cache"),
                PathBuf::from("tools/share/info/dir"),
                PathBuf::from("usr/share/info/dir")
            ]
        );
        assert!(s.0.join("out/usr/share/info/gcc.info").exists());
        assert!(s.0.join("out/usr/share/dir").exists());
    }
}

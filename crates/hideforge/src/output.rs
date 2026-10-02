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

/// Walks the upper layer and reports every inherited file the build deleted or
/// replaced. Directories are not reported: overlay copies a directory up
/// whenever something is created inside it, and that is the normal case.
pub fn check_upper(upper: &Path, lowers: &[PathBuf]) -> io::Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for (relative, meta) in walk(upper)? {
        let kind = meta.file_type();
        if kind.is_dir() {
            continue;
        }
        if kind.is_char_device() && meta.rdev() == 0 {
            violations.push(Violation::Deleted(relative));
            continue;
        }
        let inherited = lowers
            .iter()
            .any(|lower| fs::symlink_metadata(lower.join(&relative)).is_ok());
        if inherited {
            violations.push(Violation::Replaced(relative));
        }
    }
    Ok(violations)
}

/// Two layers providing the same non-directory path. In an overlay the upper
/// one would silently win; here it is an error that names both.
#[derive(Debug, PartialEq, Eq)]
pub struct Conflict {
    pub path: PathBuf,
    pub first: String,
    pub second: String,
}

/// Every path provided by more than one of `layers`, each given as (name,
/// directory). Directories may be shared; nothing else may.
pub fn conflicts(layers: &[(String, PathBuf)]) -> io::Result<Vec<Conflict>> {
    let mut owner: BTreeMap<PathBuf, &str> = BTreeMap::new();
    let mut found = Vec::new();
    for (name, dir) in layers {
        for (relative, meta) in walk(dir)? {
            if meta.is_dir() {
                continue;
            }
            match owner.get(&relative) {
                Some(first) => found.push(Conflict {
                    path: relative,
                    first: (*first).to_owned(),
                    second: name.clone(),
                }),
                None => {
                    owner.insert(relative, name);
                }
            }
        }
    }
    Ok(found)
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

        fn dir(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(&path).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn new_files_and_shared_directories_are_fine() {
        let s = Scratch::new();
        s.file("lower/usr/lib/libc.so");
        s.file("upper/usr/lib/libz.so");
        let violations = check_upper(&s.0.join("upper"), &[s.0.join("lower")]).unwrap();
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn replacing_an_inherited_file_is_reported() {
        let s = Scratch::new();
        s.file("lower/usr/share/info/dir");
        s.file("upper/usr/share/info/dir");
        s.file("upper/usr/share/info/zlib.info");
        let violations = check_upper(&s.0.join("upper"), &[s.0.join("lower")]).unwrap();
        assert_eq!(
            violations,
            [Violation::Replaced(PathBuf::from("usr/share/info/dir"))]
        );
    }

    #[test]
    fn layers_may_share_directories_but_not_files() {
        let s = Scratch::new();
        s.file("a/usr/bin/gcc");
        s.file("b/usr/bin/ld");
        s.file("c/usr/bin/gcc");
        s.dir("c/usr/share");
        let layers = [
            ("a".to_owned(), s.0.join("a")),
            ("b".to_owned(), s.0.join("b")),
            ("c".to_owned(), s.0.join("c")),
        ];
        let found = conflicts(&layers).unwrap();
        assert_eq!(
            found,
            [Conflict {
                path: PathBuf::from("usr/bin/gcc"),
                first: "a".to_owned(),
                second: "c".to_owned(),
            }]
        );
    }
}

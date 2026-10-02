//! Where things live under the work directory. See `docs/HIDEFORGE.md`.

use std::path::{Path, PathBuf};

use hideforge_recipe::{InputHash, Recipe};

pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Layout {
        Layout { root: root.into() }
    }

    pub fn sources(&self) -> PathBuf {
        self.root.join("sources")
    }

    /// A downloaded archive, named by its content.
    pub fn source(&self, sha256: &str) -> PathBuf {
        self.sources().join(sha256)
    }

    pub fn store(&self) -> PathBuf {
        self.root.join("store")
    }

    pub fn output(&self, hash: &InputHash, recipe: &Recipe) -> PathBuf {
        self.store().join(store_name(hash, recipe))
    }

    pub fn build(&self, hash: &InputHash) -> BuildDirs {
        BuildDirs {
            base: self.root.join("build").join(hash.short()),
        }
    }

    /// Where this hideforge copies its own executable for the sandbox to
    /// re-execute. See `sandbox::run`.
    pub fn exe_copy(&self) -> PathBuf {
        // A directory per process, and the file still named `hideforge`, so
        // the sandbox's PID 1 is called that in `ps` and /proc/1/comm.
        self.root
            .join("build")
            .join(format!(".hideforge-{}", std::process::id()))
            .join("hideforge")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn log(&self, hash: &InputHash, recipe: &Recipe) -> PathBuf {
        self.logs()
            .join(format!("{}.log", store_name(hash, recipe)))
    }
}

pub fn store_name(hash: &InputHash, recipe: &Recipe) -> String {
    format!(
        "{}-{}-{}",
        hash.short(),
        recipe.package.name,
        recipe.package.version
    )
}

/// Scratch space for one build. Everything here is deleted afterwards except
/// `upper`, which is renamed into the store when the build succeeds.
pub struct BuildDirs {
    base: PathBuf,
}

impl BuildDirs {
    pub fn base(&self) -> &Path {
        &self.base
    }
    /// Unpacked sources; `/build/src` in the sandbox.
    pub fn src(&self) -> PathBuf {
        self.base.join("src")
    }
    /// `$HOME` in the sandbox. Holds the script, and whatever tools cache.
    pub fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    /// The overlay's upper layer: the output.
    pub fn upper(&self) -> PathBuf {
        self.base.join("upper")
    }
    /// The overlay's work directory, which must be on the same filesystem as
    /// `upper` and empty.
    pub fn overlay_work(&self) -> PathBuf {
        self.base.join("overlay-work")
    }
    /// Where the overlay is mounted before the sandbox pivots into it.
    pub fn root(&self) -> PathBuf {
        self.base.join("root")
    }
    /// The bottom lower layer: empty directories for every mount point the
    /// sandbox needs. Mount points have to exist, and creating them in the
    /// overlay would put them in the output.
    pub fn skeleton(&self) -> PathBuf {
        self.base.join("skeleton")
    }
    pub fn script(&self) -> PathBuf {
        self.home().join(".hideforge-build.sh")
    }
}

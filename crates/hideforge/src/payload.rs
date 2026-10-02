//! The payload an image installs from: its root as a composefs repository.
//! See ARCHITECTURE.md, "The sealed system" and "Disk images are installed,
//! not assembled".
//!
//! The repository is written without fs-verity — the builder's kernel may not
//! have it — but every object's fs-verity digest is computed here, in
//! userspace, and recorded in the EROFS image. That record is what
//! `verity=require` checks at boot; `hide install` enables fs-verity on each
//! object as it writes the disk, and the kernel then refuses any object whose
//! digest disagrees with the record.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use composefs::fsverity::{Algorithm, FsVerityHashValue, Sha256HashValue};
use composefs::repository::{Repository, RepositoryConfig};
use rustix::fs::{CWD, Mode, OFlags};

/// Writes `root` into a composefs repository at `repo`, and returns the image's
/// fs-verity digest in hex: the value the UKI's command line carries as
/// `hideos.image=sha256:<digest>`.
pub fn write(root: &Path, repo: &Path, name: &str) -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime composefs needs")?;
    runtime.block_on(async {
        let config = RepositoryConfig::new(Algorithm::SHA256).set_insecure();
        let (repository, _) = Repository::<Sha256HashValue>::init_path(CWD, repo, config)
            .with_context(|| format!("creating a composefs repository at {}", repo.display()))?;
        let repository = Arc::new(repository);
        let dirfd = rustix::fs::open("/", OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())
            .context("opening /")?;
        let filesystem = composefs::fs::read_filesystem(
            dirfd,
            root.to_path_buf(),
            Some(Arc::clone(&repository)),
        )
        .await
        .with_context(|| format!("reading {} into the repository", root.display()))?;
        let id = filesystem
            .commit_image(&repository, Some(name))
            .context("writing the EROFS image")?;
        repository.sync().context("syncing the repository")?;
        Ok(id.to_hex())
    })
}

//! The system as an OCI image, and as the composefs repository a payload
//! carries. See ARCHITECTURE.md, "Updates" and "Disk images are installed,
//! not assembled".
//!
//! The image has two layers: the root, and `/boot/EFI/Linux/<uki>`. The
//! digest the UKI carries on its command line is the one of the *boot*
//! composefs image — the root with `/boot` emptied — so the UKI can travel
//! inside the image it seals. That digest is computed here the way a client
//! computes it: by pulling the OCI image into a composefs repository with
//! composefs-oci. Whatever the client regenerates, the build regenerated
//! first, with the same code.
//!
//! The repository is written without fs-verity — the builder's kernel may not
//! have it — but every object's fs-verity digest is computed here, in
//! userspace, and recorded in the EROFS image. That record is what
//! `verity=require` checks at boot; `hide install` enables fs-verity on each
//! object as it writes the disk, and the kernel then refuses any object whose
//! digest disagrees with the record.

use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail, ensure};
use composefs::fsverity::{Algorithm, FsVerityHashValue, Sha256HashValue};
use composefs::repository::{Repository, RepositoryConfig};
use composefs_oci::{NullReporter, OciTransformOptions};
use ocidir::OciDir;
use ocidir::cap_std::{ambient_authority, fs::Dir};
use ocidir::oci_spec::image::{
    Arch, ConfigBuilder, HistoryBuilder, ImageConfiguration, ImageConfigurationBuilder,
    ImageManifest, Os, Platform, PlatformBuilder,
};
use rustix::fs::CWD;

type Repo = Repository<Sha256HashValue>;

/// An OCI image being written: its layout directory, and the manifest and
/// configuration the next layer is added to.
pub struct Image {
    oci: OciDir,
    manifest: ImageManifest,
    config: ImageConfiguration,
    platform: Platform,
    tag: String,
}

/// Writes `root` as the image's first layer into a new OCI layout at `oci`,
/// pulls it into a new composefs repository at `repo`, and returns the image
/// and its boot digest in hex: the value the UKI's command line carries as
/// `hideos.image=sha256:<digest>`.
pub fn write_root(
    root: &Path,
    oci: &Path,
    repo: &Path,
    tag: &str,
    arch: hideforge_recipe::Arch,
) -> Result<(Image, String)> {
    fs::create_dir_all(oci)?;
    let dir = Dir::open_ambient_dir(oci, ambient_authority())
        .with_context(|| format!("opening {}", oci.display()))?;
    let oci_dir = OciDir::ensure(dir).context("creating the OCI layout")?;
    let platform = PlatformBuilder::default()
        .architecture(match arch {
            hideforge_recipe::Arch::X86_64 => Arch::Amd64,
            hideforge_recipe::Arch::Aarch64 => Arch::ARM64,
        })
        .os(Os::Linux)
        .build()?;
    let config = ImageConfigurationBuilder::default()
        .architecture(platform.architecture().clone())
        .os(Os::Linux)
        .config(ConfigBuilder::default().build()?)
        .build()?;
    let manifest = oci_dir.new_empty_manifest()?.build()?;
    let mut image = Image {
        oci: oci_dir,
        manifest,
        config,
        platform,
        tag: tag.to_owned(),
    };

    // Every top-level entry by name, rather than ".": a layer's paths have
    // no "./", and / itself is not in it — composefs gives / the metadata
    // of /usr, which a layer does define.
    let mut entries: Vec<String> = fs::read_dir(root)?
        .map(|e| Ok(e?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_>>()?;
    entries.sort();
    image.push_layer(root, &entries, "the hideOS root")?;
    let digest = pull(oci, repo, tag, true)?;
    Ok((image, digest))
}

impl Image {
    /// Adds the UKI as `/boot/EFI/Linux/<name>`, and checks that the boot
    /// digest did not change: what a client computes from the finished
    /// image has to be what the UKI carries.
    pub fn add_uki(
        mut self,
        root: &Path,
        uki: &Path,
        oci: &Path,
        repo: &Path,
        digest: &str,
    ) -> Result<()> {
        let staging = oci.with_extension("boot");
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        let linux = staging.join("boot/EFI/Linux");
        fs::create_dir_all(&linux)?;
        let name = uki
            .file_name()
            .ok_or_else(|| anyhow!("{} has no file name", uki.display()))?;
        fs::copy(uki, linux.join(name))?;
        // /boot as the root layer has it: the later layer's directory
        // metadata wins, and the boot image keeps /boot's mode and owner.
        for dir in ["boot", "boot/EFI", "boot/EFI/Linux"] {
            run(Command::new("touch")
                .arg("--no-dereference")
                .arg("--reference")
                .arg(root.join("boot"))
                .arg(staging.join(dir)))?;
        }
        self.push_layer(&staging, &["boot".to_owned()], "the signed UKI")?;
        fs::remove_dir_all(&staging)?;

        let again = pull(oci, repo, &self.tag, false)?;
        ensure!(
            again == digest,
            "the finished image's boot digest is sha256:{again}, but its UKI carries \
             sha256:{digest}: the UKI layer changed what boots"
        );
        Ok(())
    }

    /// Archives `entries` of `dir` as the image's next layer, and makes the
    /// result the image `tag` names.
    fn push_layer(&mut self, dir: &Path, entries: &[String], what: &str) -> Result<()> {
        let mut layer = self.oci.create_uncompressed_layer()?;
        // GNU tar, reproducibly: sorted, PAX headers named without a PID,
        // no atime or ctime. Owners are kept, numerically; so are xattrs.
        let mut tar = Command::new("tar")
            .args([
                "--create",
                "--format=pax",
                "--sort=name",
                "--numeric-owner",
                "--xattrs",
                "--xattrs-include=*",
                "--pax-option=exthdr.name=%d/PaxHeaders/%f,delete=atime,delete=ctime",
                "--directory",
            ])
            .arg(dir)
            .arg("--")
            .args(entries)
            .stdout(Stdio::piped())
            .spawn()
            .context("running tar")?;
        let mut stdout = tar
            .stdout
            .take()
            .ok_or_else(|| anyhow!("tar has no stdout"))?;
        io::copy(&mut stdout, &mut layer).with_context(|| format!("writing {what}"))?;
        let status = tar.wait()?;
        if !status.success() {
            bail!("tar exited with {status} writing {what}");
        }
        let layer = layer.complete()?;
        // A fixed date: the same tree makes the same image.
        let history = HistoryBuilder::default()
            .created("1970-01-01T00:00:00Z")
            .created_by(what)
            .build()?;
        self.oci.push_layer_with_history(
            &mut self.manifest,
            &mut self.config,
            layer,
            Some(history),
        );
        let config = self.oci.write_config(self.config.clone())?;
        self.manifest.set_config(config);
        // Tagged: for `oci:DIR:TAG`, and for registries. The entry the tag
        // named before, the image without this layer, is replaced.
        self.oci.insert_manifest(
            self.manifest.clone(),
            Some(&self.tag),
            self.platform.clone(),
        )?;
        Ok(())
    }
}

/// Pulls the image `oci:DIR:TAG` into the repository at `repo` — creating it
/// with `create` — as a client would, and returns its boot digest in hex.
fn pull(oci: &Path, repo: &Path, tag: &str, create: bool) -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime composefs needs")?;
    runtime.block_on(async {
        let repository = if create {
            let config = RepositoryConfig::new(Algorithm::SHA256).set_insecure();
            Repo::init_path(CWD, repo, config)
                .with_context(|| format!("creating a composefs repository at {}", repo.display()))?
                .0
        } else {
            Repo::open_path(CWD, repo)?
        };
        let repository = Arc::new(repository);
        let (result, _) = composefs_oci::pull_image(
            &repository,
            &format!("oci:{}:{tag}", oci.display()),
            Some(tag),
            None,
            Arc::new(NullReporter),
            Some(&OciTransformOptions::default()),
        )
        .await
        .with_context(|| format!("pulling {}", oci.display()))?;
        let boot = composefs_oci::boot_image(&repository, &result.manifest_digest)?
            .ok_or_else(|| anyhow!("pulling {} made no boot image", oci.display()))?;
        repository.sync().context("syncing the repository")?;
        Ok(boot.to_hex())
    })
}

fn run(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        bail!("{command:?} exited with {status}");
    }
    Ok(())
}

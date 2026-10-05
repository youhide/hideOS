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
    let mut image = Image::create(oci, tag, arch)?;

    // Every top-level entry by name, rather than ".": a layer's paths have
    // no "./", and / itself is not in it — composefs gives / the metadata
    // of /usr, which a layer does define.
    let mut entries: Vec<String> = fs::read_dir(root)?
        .map(|e| Ok(e?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_>>()?;
    entries.sort();
    image.push_layer(root, &entries, "the hideOS root")?;
    let digest = pull(oci, repo, tag, true, true)?;
    Ok((image, digest))
}

/// A system extension: `root` — a tree of /usr only — as a one-layer OCI
/// image at `oci`, its composefs digest computed as `hide ext add` will,
/// and that digest signed with `sign`/db.key. The name and the signature
/// go on the manifest as annotations, which the digest does not cover.
/// Returns the digest in hex.
pub fn write_extension(
    root: &Path,
    oci: &Path,
    repo: &Path,
    name: &str,
    built_for: &str,
    arch: hideforge_recipe::Arch,
    sign: Option<&Path>,
) -> Result<String> {
    let mut image = Image::create(oci, name, arch)?;
    image.push_layer(root, &["usr".to_owned()], "the extension")?;
    let digest = pull(oci, repo, name, true, false)?;
    let mut annotations = std::collections::HashMap::new();
    annotations.insert("os.hide.extension.name".to_owned(), name.to_owned());
    // The system image it was built for, as its extension-release says:
    // where `hide` records it, without mounting it. Not signed, and not
    // trusted for more than that: hidestage checks the signed file.
    annotations.insert(
        "os.hide.extension.for".to_owned(),
        format!("sha256:{built_for}"),
    );
    if let Some(keys) = sign {
        let output = Command::new("openssl")
            .args(["dgst", "-sha256", "-sign"])
            .arg(keys.join("db.key"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(format!("sha256:{digest}").as_bytes())?;
                }
                child.wait_with_output()
            })
            .context("running openssl")?;
        if !output.status.success() {
            bail!("openssl could not sign the extension");
        }
        let signature: String = output.stdout.iter().map(|b| format!("{b:02x}")).collect();
        annotations.insert("os.hide.extension.signature".to_owned(), signature);
    }
    image.manifest.set_annotations(Some(annotations));
    image.oci.insert_manifest(
        image.manifest.clone(),
        Some(&image.tag),
        image.platform.clone(),
    )?;
    Ok(digest)
}

impl Image {
    fn create(oci: &Path, tag: &str, arch: hideforge_recipe::Arch) -> Result<Image> {
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
            // The repository the image is built from: what GitHub's registry
            // links the package to.
            .config(
                ConfigBuilder::default()
                    .labels(std::collections::HashMap::from([(
                        "org.opencontainers.image.source".to_owned(),
                        "https://github.com/youhide/hideOS".to_owned(),
                    )]))
                    .build()?,
            )
            .build()?;
        let manifest = oci_dir.new_empty_manifest()?.build()?;
        Ok(Image {
            oci: oci_dir,
            manifest,
            config,
            platform,
            tag: tag.to_owned(),
        })
    }
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

        let again = pull(oci, repo, &self.tag, false, true)?;
        ensure!(
            again == digest,
            "the finished image's boot digest is sha256:{again}, but its UKI carries \
             sha256:{digest}: the UKI layer changed what boots"
        );
        // What a client computes from the image, on its manifest, for what
        // has to name the image by it without pulling gigabytes: extensions
        // are published under it, and `cargo xtask promote` finds them so.
        // An annotation changes the manifest's digest, not what boots.
        let mut annotations = self.manifest.annotations().clone().unwrap_or_default();
        annotations.insert(
            "os.hide.image.system".to_owned(),
            format!("sha256:{digest}"),
        );
        self.manifest.set_annotations(Some(annotations));
        self.oci.insert_manifest(
            self.manifest.clone(),
            Some(&self.tag),
            self.platform.clone(),
        )?;
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
/// with `create` — as a client would, and returns the digest in hex of its
/// boot image, with /boot emptied, or with `boot` false its plain one.
fn pull(oci: &Path, repo: &Path, tag: &str, create: bool, boot: bool) -> Result<String> {
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
            boot.then(OciTransformOptions::default).as_ref(),
        )
        .await
        .with_context(|| format!("pulling {}", oci.display()))?;
        let image = if boot {
            composefs_oci::boot_image(&repository, &result.manifest_digest)?
        } else {
            // By digest alone: the pull rewrote the manifest's splitstream
            // to name its EROFS image, and the verity it returned is the
            // one from before.
            composefs_oci::composefs_erofs_for_manifest(
                &repository,
                &result.manifest_digest,
                None,
                repository.erofs_version(),
            )?
        }
        .ok_or_else(|| {
            let what = match composefs_oci::oci_image::OciImage::open(
                &repository,
                &result.manifest_digest,
                None,
            ) {
                Ok(img) => format!(
                    "container image: {}, config {:?}, EROFS v1 {:?}, v2 {:?}, by tag: {:?}",
                    img.is_container_image(),
                    img.manifest().config().media_type(),
                    img.image_ref_v1().map(|i| i.to_hex()),
                    img.image_ref_v2().map(|i| i.to_hex()),
                    composefs_oci::oci_image::OciImage::open_ref(&repository, tag).map(|i| (
                        i.image_ref_v1().map(|x| x.to_hex()),
                        i.image_ref_v2().map(|x| x.to_hex())
                    )),
                ),
                Err(e) => format!("the manifest does not open: {e:#}"),
            };
            anyhow!("pulling {} made no composefs image ({what})", oci.display())
        })?;
        repository.sync().context("syncing the repository")?;
        Ok(image.to_hex())
    })
}

fn run(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        bail!("{command:?} exited with {status}");
    }
    Ok(())
}

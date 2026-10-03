//! `hide ext`: system extensions — what hideOS publishes beyond the image,
//! merged over /usr at the next boot. See hidestage's sysext.rs for what is
//! merged and why, and ARCHITECTURE.md, "System extensions".
//!
//! An extension arrives as an OCI image, like an update: one layer of
//! `/usr`, and two annotations on its manifest — its name, and hideOS's
//! signature over its composefs image digest. `add` pulls it into the
//! store, checks the signature now rather than at boot, and records it
//! where hidestage looks.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use composefs::fsverity::{FsVerityHashValue, Sha256HashValue};
use composefs::repository::Repository;
use composefs_oci::NullReporter;
use hidecrypt::signature::PublicKey;

const STORE: &str = "/hideos";
const NAME_ANNOTATION: &str = "os.hide.extension.name";
const SIGNATURE_ANNOTATION: &str = "os.hide.extension.signature";

pub fn run(args: &[String]) -> Result<()> {
    match args {
        [cmd, image] if cmd == "add" => add(image),
        [cmd] if cmd == "list" => list(),
        [cmd, name] if cmd == "remove" => remove(name),
        _ => bail!("usage: hide ext add oci-archive:PATH | list | remove NAME"),
    }
}

fn add(image: &str) -> Result<()> {
    ensure!(
        image.starts_with("oci-archive:") || image.starts_with("oci:"),
        "hide ext add takes oci-archive:PATH or oci:DIR[:TAG]"
    );
    let repo = Arc::new(
        Repository::<Sha256HashValue>::open_path(rustix::fs::CWD, STORE)
            .map_err(|e| anyhow::anyhow!("opening the store: {e}"))?,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime composefs needs")?;
    let (result, _) = runtime.block_on(composefs_oci::pull_image(
        &repo,
        image,
        None,
        None,
        Arc::new(NullReporter),
        None,
    ))?;
    let opened = composefs_oci::oci_image::OciImage::open(
        &repo,
        &result.manifest_digest,
        Some(&result.manifest_verity),
    )?;
    let annotations = opened.manifest().annotations().clone().unwrap_or_default();
    let name = annotations
        .get(NAME_ANNOTATION)
        .context("the image does not name an extension")?
        .clone();
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "`{name}` is not an extension name"
    );
    let signature = annotations
        .get(SIGNATURE_ANNOTATION)
        .context("the image carries no signature")?;
    let erofs = composefs_oci::composefs_erofs_for_manifest(
        &repo,
        &result.manifest_digest,
        Some(&result.manifest_verity),
        repo.erofs_version(),
    )?
    .context("the image made no composefs image")?;
    let digest = format!("sha256:{}", erofs.to_hex());

    let key = PublicKey::hideos()?;
    let signature_bytes = decode_hex(signature).context("the signature is not hex")?;
    ensure!(
        key.verify(digest.as_bytes(), &signature_bytes),
        "hideOS did not sign this extension: it would not be merged, so it is not added"
    );
    // Tagged in the store, so that garbage collection keeps it.
    composefs_oci::oci_image::tag_image(
        &repo,
        &result.manifest_digest,
        &format!("extension-{name}"),
    )?;
    repo.sync()?;

    let dir = Path::new(STORE).join("extensions");
    fs::create_dir_all(&dir)?;
    let record = dir.join(&name);
    let staged = record.with_extension("tmp");
    fs::write(&staged, format!("image={digest}\nsignature={signature}\n"))?;
    fs::File::open(&staged)?.sync_all()?;
    fs::rename(&staged, &record)?;
    say(&format!(
        "{name} added ({digest}); it is merged from the next boot of the system it was built for"
    ));
    Ok(())
}

fn list() -> Result<()> {
    let Ok(entries) = fs::read_dir(Path::new(STORE).join("extensions")) else {
        println!("no extensions");
        return Ok(());
    };
    let merged = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let state = if merged.contains(&format!("/run/hidestage/extensions/{name} ")) {
            "merged"
        } else {
            "not merged"
        };
        println!("{name:<24} {state}");
    }
    Ok(())
}

fn remove(name: &str) -> Result<()> {
    let record = Path::new(STORE).join("extensions").join(name);
    ensure!(record.exists(), "no extension named {name}");
    fs::remove_file(&record)?;
    let repo = Repository::<Sha256HashValue>::open_path(rustix::fs::CWD, STORE)
        .map_err(|e| anyhow::anyhow!("opening the store: {e}"))?;
    let _ = composefs_oci::oci_image::untag_image(&repo, &format!("extension-{name}"));
    say(&format!("{name} removed; it is gone from the next boot"));
    Ok(())
}

/// The extensions' images, which garbage collection must keep.
pub fn images() -> Vec<String> {
    let Ok(entries) = fs::read_dir(Path::new(STORE).join("extensions")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|record| {
            record
                .lines()
                .find_map(|l| l.strip_prefix("image=sha256:"))
                .map(str::to_owned)
        })
        .collect()
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

fn say(line: &str) {
    eprintln!("hide ext: {line}");
}

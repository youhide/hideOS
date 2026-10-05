//! `hide ext`: system extensions — what hideOS publishes beyond the image,
//! merged over /usr at the next boot. See hidestage's sysext.rs for what is
//! merged and why, and ARCHITECTURE.md, "System extensions".
//!
//! An extension arrives as an OCI image, like an update: one layer of
//! `/usr`, and two annotations on its manifest — its name, and hideOS's
//! signature over its composefs image digest. `add` pulls it into the
//! store, checks the signature now rather than at boot, and records it
//! where hidestage looks: one build for each system image it was built
//! for (`hidestage::extension`). `hide update` fetches the builds for the
//! system it brings (`follow`), and garbage collection drops the builds
//! for systems no longer on the machine (`prune`).

use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use composefs::fsverity::{FsVerityHashValue, Sha256HashValue};
use composefs::repository::Repository;
use composefs_oci::NullReporter;
use hidecrypt::signature::PublicKey;
use hidestage::extension::{self, Entry};

const STORE: &str = "/hideos";
const NAME_ANNOTATION: &str = "os.hide.extension.name";
const SIGNATURE_ANNOTATION: &str = "os.hide.extension.signature";
const FOR_ANNOTATION: &str = "os.hide.extension.for";

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
    let (name, entry) = install(image, None)?;
    say(&format!(
        "{name} added ({}); it is merged from the next boot of the system it was built for",
        entry.image
    ));
    Ok(())
}

/// Pulls the extension `image` into the store, checks hideOS's signature,
/// and records it beside its builds for other systems. `expect`: the name
/// and the system (`sha256:<hex>`) the caller asked for, which the image
/// must say it is.
fn install(image: &str, expect: Option<(&str, &str)>) -> Result<(String, Entry)> {
    install_in(Path::new(STORE), image, expect)
}

/// [`install`], into the store at `store`: the running system's, or the
/// one the installer has just made.
pub(crate) fn install_in(
    store: &Path,
    image: &str,
    expect: Option<(&str, &str)>,
) -> Result<(String, Entry)> {
    let repo = Arc::new(
        Repository::<Sha256HashValue>::open_path(rustix::fs::CWD, store)
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
    ensure!(valid_name(&name), "`{name}` is not an extension name");
    let signature = annotations
        .get(SIGNATURE_ANNOTATION)
        .context("the image carries no signature")?
        .clone();
    let built_for = annotations
        .get(FOR_ANNOTATION)
        .filter(|system| valid_system(system))
        .cloned();
    if let Some((wanted, system)) = expect {
        ensure!(
            name == wanted && built_for.as_deref() == Some(system),
            "the image is {name} for {}, not {wanted} for {system}",
            built_for.as_deref().unwrap_or("an unnamed system")
        );
    }
    // By digest alone: the pull rewrote the manifest's splitstream to name
    // its EROFS image, and the verity it returned is the one from before.
    let erofs = composefs_oci::composefs_erofs_for_manifest(
        &repo,
        &result.manifest_digest,
        None,
        repo.erofs_version(),
    )?
    .context("the image made no composefs image")?;
    let digest = format!("sha256:{}", erofs.to_hex());

    let key = PublicKey::hideos()?;
    let signature_bytes = decode_hex(&signature).context("the signature is not hex")?;
    ensure!(
        key.verify(digest.as_bytes(), &signature_bytes),
        "hideOS did not sign this extension: it would not be merged, so it is not added"
    );
    let entry = Entry {
        image: digest,
        signature,
        built_for,
    };
    // Tagged in the store, so that garbage collection keeps it.
    composefs_oci::oci_image::tag_image(&repo, &result.manifest_digest, &tag(&name, &entry))?;
    repo.sync()?;

    let entries = read_in(store, &name);
    let kept = extension::with(&entries, entry.clone());
    untag_in(store, &name, &entries, &kept);
    write_in(store, &name, &kept)?;
    Ok((name, entry))
}

/// The builds of every extension this machine has, for the system image
/// an update brings (`system`, hex), fetched beside the ones for the
/// running system before the update commits. `source`: the repository the
/// update came from, where hideOS publishes each image's extensions as
/// `ext-<name>-<image digest>`. A build that is not there stops the
/// update: a machine whose display needs the NVIDIA extension must not
/// start a system without it.
pub(crate) fn follow(system: &str, source: Option<&str>) -> Result<()> {
    let wanted = format!("sha256:{system}");
    for name in names() {
        let entries = read(&name);
        if entries
            .iter()
            .any(|e| e.built_for.as_deref() == Some(wanted.as_str()))
        {
            continue;
        }
        let Some(repository) = source else {
            bail!(
                "this machine has the {name} extension, and an update from a local image \
                 brings no build of it: add {name}'s build for {wanted} first"
            );
        };
        let reference = format!("{repository}:ext-{name}-{system}");
        say(&format!("fetching {name} for the new system"));
        let local = crate::registry::fetch(&reference).with_context(|| {
            format!(
                "the update waits: there is no build of the {name} extension for this \
                 system yet ({reference})"
            )
        });
        let installed = local.and_then(|local| install(&local, Some((&name, &wanted))));
        crate::registry::clean();
        let (_, entry) = installed?;
        say(&format!("{name} for the new system: {}", entry.image));
    }
    Ok(())
}

/// Drops the builds for systems no longer on the machine; `kept` says,
/// for a system (`sha256:<hex>`), whether a deployment still boots it.
/// A record left with no build stays: the machine still wants the
/// extension, and the next update fetches it.
pub(crate) fn prune(kept: impl Fn(&str) -> bool) -> Result<()> {
    for name in names() {
        let entries = read(&name);
        let left = extension::keeping(&entries, &kept);
        if left.len() != entries.len() {
            untag(&name, &entries, &left);
            write(&name, &left)?;
        }
    }
    Ok(())
}

fn list() -> Result<()> {
    let names = names();
    if names.is_empty() {
        println!("no extensions");
        return Ok(());
    }
    let merged = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    for name in names {
        let state = if merged.contains(&format!("/run/hidestage/extensions/{name} ")) {
            "merged"
        } else {
            "not merged"
        };
        let builds = read(&name).len();
        let plural = if builds == 1 { "" } else { "s" };
        println!("{name:<24} {state}, {builds} build{plural}");
    }
    Ok(())
}

fn remove(name: &str) -> Result<()> {
    let record = Path::new(STORE).join("extensions").join(name);
    ensure!(record.exists(), "no extension named {name}");
    let entries = read(name);
    fs::remove_file(&record)?;
    untag(name, &entries, &[]);
    say(&format!("{name} removed; it is gone from the next boot"));
    Ok(())
}

/// The extensions' images, which garbage collection must keep.
pub fn images() -> Vec<String> {
    names()
        .iter()
        .flat_map(|name| read(name))
        .filter_map(|e| e.image.strip_prefix("sha256:").map(str::to_owned))
        .collect()
}

/// The extensions this machine has: the records' names.
fn names() -> Vec<String> {
    let Ok(entries) = fs::read_dir(Path::new(STORE).join("extensions")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| valid_name(name))
        .collect();
    names.sort();
    names
}

fn read(name: &str) -> Vec<Entry> {
    read_in(Path::new(STORE), name)
}

fn read_in(store: &Path, name: &str) -> Vec<Entry> {
    fs::read_to_string(store.join("extensions").join(name))
        .map(|record| extension::parse(&record))
        .unwrap_or_default()
}

/// The record, replaced whole: a rename, so a power cut leaves the old
/// one or the new one.
fn write(name: &str, entries: &[Entry]) -> Result<()> {
    write_in(Path::new(STORE), name, entries)
}

fn write_in(store: &Path, name: &str, entries: &[Entry]) -> Result<()> {
    let dir = store.join("extensions");
    fs::create_dir_all(&dir)?;
    let record = dir.join(name);
    let staged = dir.join(format!(".{name}.tmp"));
    fs::write(&staged, extension::render(entries))?;
    fs::File::open(&staged)?.sync_all()?;
    fs::rename(&staged, &record)?;
    Ok(())
}

/// The store's tag for one build: one per system it was built for, so
/// that each is kept while its system is.
fn tag(name: &str, entry: &Entry) -> String {
    match entry
        .built_for
        .as_deref()
        .and_then(|s| s.strip_prefix("sha256:"))
        .and_then(|hex| hex.get(..12))
    {
        Some(short) => format!("extension-{name}-{short}"),
        None => format!("extension-{name}"),
    }
}

/// Untags the builds in `before` that are not in `after`.
fn untag(name: &str, before: &[Entry], after: &[Entry]) {
    untag_in(Path::new(STORE), name, before, after);
}

fn untag_in(store: &Path, name: &str, before: &[Entry], after: &[Entry]) {
    let Ok(repo) = Repository::<Sha256HashValue>::open_path(rustix::fs::CWD, store) else {
        return;
    };
    for gone in before.iter().filter(|e| !after.contains(e)) {
        let _ = composefs_oci::oci_image::untag_image(&repo, &tag(name, gone));
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_system(system: &str) -> bool {
    system
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
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

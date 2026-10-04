//! Registry references for `hide update` and `hide ext add`: an image in an
//! OCI registry — `ghcr.io/youhide/hideos:workstation-stable` — fetched into
//! an OCI layout under `/var/cache/hide`, from where it is pulled as any
//! local image is. See ARCHITECTURE.md, "Updates".
//!
//! Nothing here decides whether the image is trusted: the client checks
//! every blob against the digest the manifest names, and the update then
//! checks the image it built against the digest in the signed UKI, as for
//! an image from a file. A reference starting with `http://` is fetched
//! without TLS: for a registry on the test machine's host, written out in
//! the reference so that it cannot happen by default.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};

const CACHE: &str = "/var/cache/hide/oci";
/// The tag the fetched image has in the cache's layout.
const TAG: &str = "fetched";

/// Whether `image` names a registry rather than a local OCI image.
pub fn is_registry(image: &str) -> bool {
    !(image.starts_with("oci-archive:") || image.starts_with("oci:"))
}

/// Fetches `image` into the cache and returns the `oci:` reference that
/// pulls it from there.
pub fn fetch(image: &str) -> Result<String> {
    let (plain, name) = match image.strip_prefix("http://") {
        Some(rest) => (true, rest),
        None => (false, image),
    };
    let reference: Reference = name
        .parse()
        .with_context(|| format!("`{image}` is not an image reference"))?;
    let cache = PathBuf::from(CACHE);
    // A fetch interrupted half-way is not resumed: started again, whole.
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(cache.join("blobs/sha256"))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(fetch_into(&reference, plain, &cache))?;
    say(&format!("fetched {image}"));
    Ok(format!("oci:{CACHE}:{TAG}"))
}

async fn fetch_into(reference: &Reference, plain: bool, cache: &Path) -> Result<()> {
    // rustls's cryptography: ring, chosen once for the process.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let protocol = if plain {
        ClientProtocol::HttpsExcept(vec![reference.registry().to_owned()])
    } else {
        ClientProtocol::Https
    };
    let client = Client::new(ClientConfig {
        protocol,
        ..Default::default()
    });
    // Public images: anonymous, with the token the registry hands out.
    let auth = RegistryAuth::Anonymous;
    let (raw, digest) = client
        .pull_manifest_raw(reference, &auth, &[OCI_IMAGE_MEDIA_TYPE])
        .await
        .with_context(|| format!("fetching the manifest of {reference}"))?;
    let manifest: OciImageManifest =
        serde_json::from_slice(&raw).context("the manifest is not an OCI image manifest")?;
    write_blob(cache, &digest, &raw).await?;

    let mut descriptors = vec![manifest.config.clone()];
    descriptors.extend(manifest.layers.iter().cloned());
    for descriptor in &descriptors {
        let path = blob_path(cache, &descriptor.digest)?;
        let file = tokio::fs::File::create(&path)
            .await
            .with_context(|| format!("creating {}", path.display()))?;
        say(&format!(
            "fetching {} ({} MiB)",
            descriptor.digest,
            descriptor.size / (1 << 20)
        ));
        // The client checks the blob against its digest as it arrives.
        client
            .pull_blob(reference, descriptor, file)
            .await
            .with_context(|| format!("fetching {}", descriptor.digest))?;
    }

    let index = serde_json::json!({
        "schemaVersion": 2,
        "manifests": [{
            "mediaType": OCI_IMAGE_MEDIA_TYPE,
            "digest": digest,
            "size": raw.len(),
            "annotations": { "org.opencontainers.image.ref.name": TAG },
        }],
    });
    tokio::fs::write(cache.join("index.json"), serde_json::to_vec(&index)?).await?;
    tokio::fs::write(
        cache.join("oci-layout"),
        br#"{"imageLayoutVersion":"1.0.0"}"#,
    )
    .await?;
    Ok(())
}

fn blob_path(cache: &Path, digest: &str) -> Result<PathBuf> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        bail!("{digest} is not a sha256 digest");
    };
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("{digest} is not a sha256 digest");
    }
    Ok(cache.join("blobs/sha256").join(hex))
}

async fn write_blob(cache: &Path, digest: &str, data: &[u8]) -> Result<()> {
    use sha2::{Digest, Sha256};
    let actual: String = Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest.strip_prefix("sha256:") != Some(actual.as_str()) {
        bail!("the manifest does not match its digest {digest}");
    }
    tokio::fs::write(blob_path(cache, digest)?, data).await?;
    Ok(())
}

/// Removes what a fetch left, once the image is in the store.
pub fn clean() {
    let _ = std::fs::remove_dir_all(CACHE);
}

fn say(line: &str) {
    eprintln!("hide update: {line}");
}

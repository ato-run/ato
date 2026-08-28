//! Deterministic producer for an immutable Static Web Bundle v1.
//!
//! It turns an explicitly selected, already-built output tree into the exact
//! JCS manifest and identity blobs consumed by `ato-edge`. It has no R2 client,
//! deployment record, route, database, or VM lifecycle dependency.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::manifest::{
    STATIC_WEB_MANIFEST_V1_SCHEMA, StaticWebFileV1, StaticWebManifestV1, StaticWebRoutingV1,
    validate_relative_path,
};
use crate::output::StaticWebOutputPlan;
pub use crate::receipt::{
    StaticWebBlobMetadataV1, StaticWebBlobReceiptV1, StaticWebBundleReceiptV1,
};
use crate::receipt::{blob_r2_key, host_label, manifest_r2_key};
use anyhow::{Context, Result, bail};
use sha2::{Digest as _, Sha256};

pub const MAX_FILE_COUNT: usize = 10_000;
pub const MAX_FILE_SIZE: u64 = 64 * 1024 * 1024;
pub const MAX_TOTAL_SIZE: u64 = 1024 * 1024 * 1024;
pub const MAX_DIRECTORY_COUNT: usize = 10_000;
pub const MAX_RECURSION_DEPTH: usize = 32;

fn blob_is_clean(blob: &[u8], secrets: &[&[u8]]) -> bool {
    !secrets
        .iter()
        .any(|secret| !secret.is_empty() && blob.windows(secret.len()).any(|part| part == *secret))
}

/// Output facts, not a deployment mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducedStaticWebBundle {
    pub bundle_root: PathBuf,
    pub manifest_bytes: Vec<u8>,
    pub receipt_bytes: Vec<u8>,
    pub receipt_digest: String,
    pub receipt: StaticWebBundleReceiptV1,
}

/// Produces `static-web-bundle-v1/` below `destination_parent`.
///
/// The final directory is never overwritten. All validation and runtime-secret
/// canary checks complete in a temporary sibling first, so failure leaves no
/// partial bundle. An empty canary list is *not* a generic secret scan claim.
pub fn produce_static_web_bundle(
    plan: &StaticWebOutputPlan,
    built_output_root: &Path,
    destination_parent: &Path,
    runtime_secret_canaries: &[&[u8]],
) -> Result<ProducedStaticWebBundle> {
    plan.validate()?;
    let source_meta = fs::symlink_metadata(built_output_root)
        .with_context(|| format!("read built static output {}", built_output_root.display()))?;
    if source_meta.file_type().is_symlink() || !source_meta.is_dir() {
        bail!("built static output must be a real directory");
    }
    if destination_parent.join("static-web-bundle-v1").exists() {
        bail!("static-web-bundle-v1 already exists; immutable output is never overwritten");
    }
    fs::create_dir_all(destination_parent)
        .with_context(|| format!("create bundle parent {}", destination_parent.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(".static-web-bundle-v1-")
        .tempdir_in(destination_parent)
        .context("create static web bundle staging directory")?;

    let mut input_files = Vec::new();
    let mut traversal = TraversalCounts::default();
    collect_files(built_output_root, &mut input_files, &mut traversal, 0)?;

    let mut files = BTreeMap::new();
    let mut receipts = BTreeMap::new();
    let mut total_bytes = 0_u64;
    let blobs_dir = staging.path().join("blobs/sha256");
    fs::create_dir_all(&blobs_dir).context("create static web blob directory")?;
    for source in input_files {
        let relative = source
            .strip_prefix(built_output_root)
            .expect("collected paths are descendants")
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("static web path is not UTF-8: {}", source.display()))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        validate_relative_path(&relative).map_err(anyhow::Error::from)?;
        if relative.ends_with(".map") {
            bail!("source maps are not publishable static web output: {relative}");
        }
        let metadata = fs::metadata(&source)
            .with_context(|| format!("read static web file metadata {}", source.display()))?;
        reject_hard_link(&metadata, &source)?;
        let size = metadata.len();
        if size > MAX_FILE_SIZE {
            bail!("static web file exceeds {MAX_FILE_SIZE} bytes: {relative}");
        }
        total_bytes = total_bytes
            .checked_add(size)
            .ok_or_else(|| anyhow::anyhow!("static web total size overflow"))?;
        if total_bytes > MAX_TOTAL_SIZE {
            bail!("static web output exceeds {MAX_TOTAL_SIZE} bytes");
        }
        let bytes = read_regular_file(&source)?;
        if !blob_is_clean(&bytes, runtime_secret_canaries) {
            bail!("static web output failed the runtime secret canary scan: {relative}");
        }
        let hex_digest = format!("{:x}", Sha256::digest(&bytes));
        let digest = format!("sha256:{hex_digest}");
        let blob_path = blobs_dir.join(&hex_digest);
        if !blob_path.exists() {
            fs::write(&blob_path, &bytes)
                .with_context(|| format!("write immutable blob {}", blob_path.display()))?;
        }
        let media_type = media_type_for(&relative)
            .ok_or_else(|| anyhow::anyhow!("unsupported static web media type: {relative}"))?;
        files.insert(
            relative,
            StaticWebFileV1 {
                blob: digest.clone(),
                size,
                media_type: media_type.to_owned(),
            },
        );
        receipts
            .entry(hex_digest.clone())
            .or_insert(StaticWebBlobReceiptV1 {
                digest: digest.clone(),
                size,
                r2_key: blob_r2_key(&digest).map_err(anyhow::Error::from)?,
                custom_metadata: StaticWebBlobMetadataV1 {
                    schema: "ato.static-blob/v1".to_owned(),
                    sha256: digest,
                },
            });
    }
    let manifest = StaticWebManifestV1 {
        schema: STATIC_WEB_MANIFEST_V1_SCHEMA.to_owned(),
        materialization_id: plan.materialization_id.clone(),
        entry_path: plan.entry_path.clone(),
        routing: StaticWebRoutingV1 {
            spa_fallback: plan.spa_fallback,
        },
        files,
        security: plan.security()?,
    };
    let manifest_bytes = manifest.canonical_bytes().map_err(anyhow::Error::from)?;
    if !blob_is_clean(&manifest_bytes, runtime_secret_canaries) {
        bail!("static web manifest failed the runtime secret canary scan");
    }
    let manifest_hex = format!("{:x}", Sha256::digest(&manifest_bytes));
    let manifest_digest = format!("sha256:{manifest_hex}");
    let receipt = StaticWebBundleReceiptV1 {
        schema: "ato.static-web-bundle-receipt/v1".to_owned(),
        materialization_id: plan.materialization_id.clone(),
        manifest_digest: manifest_digest.clone(),
        production_host_label: host_label('p', &manifest_digest).map_err(anyhow::Error::from)?,
        staging_host_label: host_label('s', &manifest_digest).map_err(anyhow::Error::from)?,
        manifest_r2_key: manifest_r2_key(&manifest_digest).map_err(anyhow::Error::from)?,
        entry_path: plan.entry_path.clone(),
        file_count: manifest.files.len() as u64,
        total_size: total_bytes,
        blobs: receipts.into_values().collect(),
    };
    receipt
        .validate_for_manifest(&manifest)
        .map_err(anyhow::Error::from)?;
    let receipt_bytes = receipt.canonical_bytes().map_err(anyhow::Error::from)?;
    let receipt_digest = receipt.digest().map_err(anyhow::Error::from)?;
    fs::write(staging.path().join("manifest.json"), &manifest_bytes)
        .context("write canonical static web manifest")?;
    fs::write(staging.path().join("receipt.json"), &receipt_bytes)
        .context("write static web receipt")?;

    let bundle_root = destination_parent.join("static-web-bundle-v1");
    fs::rename(staging.path(), &bundle_root).with_context(|| {
        format!(
            "atomically publish static web bundle {} to {}",
            staging.path().display(),
            bundle_root.display()
        )
    })?;
    Ok(ProducedStaticWebBundle {
        bundle_root,
        manifest_bytes,
        receipt_bytes,
        receipt_digest,
        receipt,
    })
}

#[derive(Default)]
struct TraversalCounts {
    directories: usize,
}

fn collect_files(
    current: &Path,
    output: &mut Vec<PathBuf>,
    traversal: &mut TraversalCounts,
    depth: usize,
) -> Result<()> {
    if depth > MAX_RECURSION_DEPTH {
        bail!("static web output exceeds recursion depth {MAX_RECURSION_DEPTH}");
    }
    let mut entries = fs::read_dir(current)
        .with_context(|| format!("read static web directory {}", current.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("enumerate static web directory {}", current.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("read static web file type {}", path.display()))?;
        if file_type.is_symlink() {
            bail!("static web output contains a symlink: {}", path.display());
        }
        if file_type.is_dir() {
            traversal.directories += 1;
            if traversal.directories > MAX_DIRECTORY_COUNT {
                bail!("static web output exceeds {MAX_DIRECTORY_COUNT} directories");
            }
            collect_files(&path, output, traversal, depth + 1)?;
        } else if file_type.is_file() {
            if output.len() == MAX_FILE_COUNT {
                bail!("static web output exceeds {MAX_FILE_COUNT} files");
            }
            output.push(path);
        } else {
            bail!(
                "static web output contains a non-regular file: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn read_regular_file(path: &Path) -> Result<Vec<u8>> {
    let mut file =
        fs::File::open(path).with_context(|| format!("open static web file {}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("read static web file {}", path.display()))?;
    Ok(bytes)
}

#[cfg(unix)]
fn reject_hard_link(metadata: &fs::Metadata, path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    if metadata.nlink() != 1 {
        bail!(
            "static web output contains a hard-linked file: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn reject_hard_link(_metadata: &fs::Metadata, _path: &Path) -> Result<()> {
    Ok(())
}

fn media_type_for(path: &str) -> Option<&'static str> {
    let extension = Path::new(path).extension()?.to_str()?;
    match extension {
        "js" | "mjs" => Some("application/javascript; charset=utf-8"),
        "json" => Some("application/json; charset=utf-8"),
        "wasm" => Some("application/wasm"),
        "bin" => Some("application/octet-stream"),
        "woff2" => Some("font/woff2"),
        "avif" => Some("image/avif"),
        "gif" => Some("image/gif"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "svg" => Some("image/svg+xml"),
        "webp" => Some("image/webp"),
        "css" => Some("text/css; charset=utf-8"),
        "html" | "htm" => Some("text/html; charset=utf-8"),
        "txt" => Some("text/plain; charset=utf-8"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{StaticWebOutputPlan, extract_static_web_output};

    fn plan() -> StaticWebOutputPlan {
        StaticWebOutputPlan {
            materialization_id: "mat_fixture".into(),
            image_output_root: PathBuf::from("dist"),
            entry_path: "index.html".into(),
            spa_fallback: true,
            connect_src: vec!["https://api.example.com:8443".into()],
        }
    }

    fn fixture_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("dist/assets")).unwrap();
        fs::write(root.path().join("dist/index.html"), "<main>ok</main>").unwrap();
        fs::write(root.path().join("dist/assets/app.js"), "console.log('ok')").unwrap();
        root
    }

    #[test]
    fn produces_deterministic_bundle_with_worker_keys_and_labels() {
        let image = fixture_root();
        let extracted = extract_static_web_output(image.path(), &plan()).unwrap();
        let one_parent = tempfile::tempdir().unwrap();
        let two_parent = tempfile::tempdir().unwrap();
        let one =
            produce_static_web_bundle(&plan(), extracted.output_root(), one_parent.path(), &[])
                .unwrap();
        let two =
            produce_static_web_bundle(&plan(), extracted.output_root(), two_parent.path(), &[])
                .unwrap();
        assert_eq!(one.manifest_bytes, two.manifest_bytes);
        assert_eq!(one.receipt.manifest_digest, two.receipt.manifest_digest);
        assert!(one.receipt.production_host_label.starts_with("p-"));
        assert!(one.receipt.staging_host_label.starts_with("s-"));
        assert_eq!(one.receipt.production_host_label.len(), 54);
        assert!(
            one.receipt
                .manifest_r2_key
                .starts_with("static/v1/manifests/sha256/")
        );
        assert!(one.receipt.blobs.iter().all(|blob| {
            blob.r2_key.starts_with("static/v1/blobs/sha256/")
                && blob.custom_metadata.schema == "ato.static-blob/v1"
                && blob.custom_metadata.sha256 == blob.digest
        }));
        assert!(
            !fs::read(one.bundle_root.join("manifest.json"))
                .unwrap()
                .ends_with(b"\n")
        );
    }

    #[test]
    fn rejects_source_maps_and_secret_canaries() {
        let image = fixture_root();
        fs::write(image.path().join("dist/assets/app.js.map"), "{}").unwrap();
        let extracted = extract_static_web_output(image.path(), &plan()).unwrap();
        let parent = tempfile::tempdir().unwrap();
        assert!(
            produce_static_web_bundle(&plan(), extracted.output_root(), parent.path(), &[])
                .unwrap_err()
                .to_string()
                .contains("source maps")
        );

        fs::remove_file(image.path().join("dist/assets/app.js.map")).unwrap();
        fs::write(image.path().join("dist/index.html"), "secret-value").unwrap();
        let extracted = extract_static_web_output(image.path(), &plan()).unwrap();
        let parent = tempfile::tempdir().unwrap();
        assert!(
            produce_static_web_bundle(
                &plan(),
                extracted.output_root(),
                parent.path(),
                &[b"secret-value"]
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_unsupported_media_types() {
        let image = fixture_root();
        fs::write(image.path().join("dist/installer.exe"), b"binary").unwrap();
        let extracted = extract_static_web_output(image.path(), &plan()).unwrap();
        let parent = tempfile::tempdir().unwrap();
        assert!(
            produce_static_web_bundle(&plan(), extracted.output_root(), parent.path(), &[])
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_unsafe_closure_members() {
        let image = fixture_root();
        std::os::unix::fs::symlink("index.html", image.path().join("dist/link.html")).unwrap();
        let extracted = extract_static_web_output(image.path(), &plan());
        assert!(extracted.is_err());
    }

    #[test]
    fn labels_match_the_normative_fixture() {
        assert_eq!(
            host_label(
                'p',
                "sha256:6d77d3da709a578e6d58f50d4b8f8cf5c54e2178200821769afb03449c8e6ba2"
            )
            .unwrap(),
            "p-nv35hwtqtjly43ky6uguxd4m6xcu4ilyeaecc5u27mbujheonora"
        );
    }
}

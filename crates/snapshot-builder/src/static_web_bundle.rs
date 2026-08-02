//! Deterministic producer for an immutable Static Web Bundle v1.
//!
//! It turns an explicitly selected, already-built output tree into the exact
//! JCS manifest and identity blobs consumed by `ato-edge`. It has no R2 client,
//! deployment record, route, database, or VM lifecycle dependency.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use capsule::contract::static_web_manifest::{
    STATIC_WEB_MANIFEST_V1_SCHEMA, StaticWebFileV1, StaticWebManifestV1, StaticWebRoutingV1,
    validate_relative_path,
};
use sha2::{Digest as _, Sha256};
use snapshot::no_secret_scan;

use crate::static_web_output::StaticWebOutputPlan;

pub const MAX_FILE_COUNT: usize = 10_000;
pub const MAX_FILE_SIZE: u64 = 64 * 1024 * 1024;
pub const MAX_TOTAL_SIZE: u64 = 1024 * 1024 * 1024;
const BLOB_METADATA_SCHEMA: &str = "ato.static-blob/v1";
const RECEIPT_SCHEMA: &str = "ato.static-web-bundle-receipt/v1";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticWebBundleReceiptV1 {
    pub schema: String,
    pub materialization_id: String,
    pub manifest_sha256: String,
    pub production_host_label: String,
    pub staging_host_label: String,
    pub manifest_r2_key: String,
    pub entry_path: String,
    pub file_count: u64,
    pub total_bytes: u64,
    pub blobs: BTreeMap<String, StaticWebBlobReceiptV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticWebBlobReceiptV1 {
    pub sha256: String,
    pub size: u64,
    pub r2_key: String,
    pub custom_metadata: StaticWebBlobMetadataV1,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticWebBlobMetadataV1 {
    pub schema: String,
    pub sha256: String,
}

/// Output facts, not a deployment mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducedStaticWebBundle {
    pub bundle_root: PathBuf,
    pub manifest_bytes: Vec<u8>,
    pub receipt: StaticWebBundleReceiptV1,
}

/// Produces `static-web-bundle-v1/` below `destination_parent`.
///
/// The final directory is never overwritten. All validation and secret checks
/// complete in a temporary sibling first, so failure leaves no partial bundle.
pub fn produce_static_web_bundle(
    plan: &StaticWebOutputPlan,
    built_output_root: &Path,
    destination_parent: &Path,
    secret_canaries: &[&[u8]],
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
    collect_files(built_output_root, &mut input_files)?;
    if input_files.len() > MAX_FILE_COUNT {
        bail!("static web output exceeds {MAX_FILE_COUNT} files");
    }

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
        if !no_secret_scan::blob_is_clean(&bytes, secret_canaries) {
            bail!("static web output failed the no-secret scan: {relative}");
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
                sha256: digest.clone(),
                size,
                r2_key: blob_r2_key(&hex_digest),
                custom_metadata: StaticWebBlobMetadataV1 {
                    schema: BLOB_METADATA_SCHEMA.to_owned(),
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
        security: plan.security(),
    };
    let manifest_bytes = manifest.canonical_bytes().map_err(anyhow::Error::from)?;
    if !no_secret_scan::blob_is_clean(&manifest_bytes, secret_canaries) {
        bail!("static web manifest failed the no-secret scan");
    }
    let manifest_hex = format!("{:x}", Sha256::digest(&manifest_bytes));
    let manifest_sha256 = format!("sha256:{manifest_hex}");
    let receipt = StaticWebBundleReceiptV1 {
        schema: RECEIPT_SCHEMA.to_owned(),
        materialization_id: plan.materialization_id.clone(),
        manifest_sha256: manifest_sha256.clone(),
        production_host_label: host_label('p', &manifest_hex),
        staging_host_label: host_label('s', &manifest_hex),
        manifest_r2_key: manifest_r2_key(&manifest_hex),
        entry_path: plan.entry_path.clone(),
        file_count: manifest.files.len() as u64,
        total_bytes,
        blobs: receipts,
    };
    let receipt_bytes = serde_jcs::to_vec(&receipt).context("canonicalize static web receipt")?;
    fs::write(staging.path().join("manifest.json"), &manifest_bytes)
        .context("write canonical static web manifest")?;
    fs::write(staging.path().join("receipt.json"), receipt_bytes)
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
        receipt,
    })
}

fn collect_files(current: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
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
            collect_files(&path, output)?;
        } else if file_type.is_file() {
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

fn blob_r2_key(hex_digest: &str) -> String {
    format!("static/v1/blobs/sha256/{hex_digest}")
}

fn manifest_r2_key(hex_digest: &str) -> String {
    format!("static/v1/manifests/sha256/{hex_digest}.json")
}

fn host_label(environment: char, hex_digest: &str) -> String {
    format!("{environment}-{}", base32_lower(hex_digest))
}

fn base32_lower(hex_digest: &str) -> String {
    let bytes = hex::decode(hex_digest).expect("SHA-256 hex is generated internally");
    let alphabet = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut buffer = 0_u16;
    let mut bits = 0_u8;
    let mut output = String::with_capacity(52);
    for byte in bytes {
        buffer = (buffer << 8) | u16::from(byte);
        bits += 8;
        while bits >= 5 {
            output.push(alphabet[((buffer >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        output.push(alphabet[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::static_web_output::{StaticWebOutputPlan, extract_static_web_output};

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
        assert_eq!(one.receipt.manifest_sha256, two.receipt.manifest_sha256);
        assert!(one.receipt.production_host_label.starts_with("p-"));
        assert!(one.receipt.staging_host_label.starts_with("s-"));
        assert_eq!(one.receipt.production_host_label.len(), 54);
        assert!(
            one.receipt
                .manifest_r2_key
                .starts_with("static/v1/manifests/sha256/")
        );
        assert!(one.receipt.blobs.values().all(|blob| {
            blob.r2_key.starts_with("static/v1/blobs/sha256/")
                && blob.custom_metadata.schema == BLOB_METADATA_SCHEMA
                && blob.custom_metadata.sha256 == blob.sha256
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
    fn rejects_unsafe_closure_members() {
        let image = fixture_root();
        #[cfg(unix)]
        std::os::unix::fs::symlink("index.html", image.path().join("dist/link.html")).unwrap();
        #[cfg(unix)]
        {
            let extracted = extract_static_web_output(image.path(), &plan());
            assert!(extracted.is_err());
        }
    }

    #[test]
    fn labels_match_the_normative_fixture() {
        assert_eq!(
            host_label(
                'p',
                "8a06c71db0519bb27f2dc92f88dcd8107f09e8cb52a1495f16cb1bac6a177abd"
            ),
            "p-ridmohnqkgn3e7znzexyrxgycb7qt2glkkqusxywzmn2y2qxpk6q"
        );
    }
}

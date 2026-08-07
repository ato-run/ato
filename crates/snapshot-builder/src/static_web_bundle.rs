//! Deterministic producer for an immutable Static Web Bundle v1.
//!
//! It turns an explicitly selected, already-built output tree into the exact
//! JCS manifest and identity blobs consumed by `ato-edge`. It has no R2 client,
//! deployment record, route, database, or VM lifecycle dependency.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use capsule::contract::static_web_manifest::{
    STATIC_WEB_MANIFEST_V1_SCHEMA, StaticWebFileV1, StaticWebManifestV1, StaticWebRoutingV1,
    validate_relative_path,
};
pub use capsule::contract::static_web_receipt::{
    StaticWebBlobMetadataV1, StaticWebBlobReceiptV1, StaticWebBundleReceiptV1,
};
use capsule::contract::static_web_receipt::{blob_r2_key, host_label, manifest_r2_key};
use sha2::{Digest as _, Sha256};
use snapshot::no_secret_scan;

use crate::static_web_output::StaticWebOutputPlan;

pub const MAX_FILE_COUNT: usize = 10_000;
pub const MAX_FILE_SIZE: u64 = 64 * 1024 * 1024;
pub const MAX_TOTAL_SIZE: u64 = 1024 * 1024 * 1024;
pub const MAX_DIRECTORY_COUNT: usize = 10_000;
pub const MAX_RECURSION_DEPTH: usize = 32;

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
        // A repository is not a build output. When the declared root is the
        // source root itself (`root = "."`, the dependency-free static case)
        // the tree also carries everything that produced the site — including
        // source maps, which are simply not publishable and are skipped like
        // every other non-web file.
        if relative.ends_with(".map") || !is_publishable_web_file(&relative) {
            continue;
        }
        // Open ONCE and derive size from the SAME descriptor that produces the
        // hashed bytes. `metadata.len()` before a separate read can disagree
        // with what was actually hashed if the file changes between the two
        // calls; a bounded read plus a post-read fstat makes the manifest size,
        // the receipt size, and the blob bytes one fact. `take(MAX_FILE_SIZE+1)`
        // also bounds builder memory and refuses growth past the limit.
        let (bytes, size) = read_regular_file_bounded(&source, MAX_FILE_SIZE)?;
        reject_hard_link(&fs::metadata(&source)?, &source)?;
        if size > MAX_FILE_SIZE {
            bail!("static web file exceeds {MAX_FILE_SIZE} bytes: {relative}");
        }
        total_bytes = total_bytes
            .checked_add(size)
            .ok_or_else(|| anyhow::anyhow!("static web total size overflow"))?;
        if total_bytes > MAX_TOTAL_SIZE {
            bail!("static web output exceeds {MAX_TOTAL_SIZE} bytes");
        }
        if !no_secret_scan::blob_is_clean(&bytes, runtime_secret_canaries) {
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
    if !no_secret_scan::blob_is_clean(&manifest_bytes, runtime_secret_canaries) {
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

/// Read a regular file with a hard size bound, returning the bytes AND the
/// size measured from the SAME descriptor that was read.
///
/// The read is capped at `max_size + 1` so an oversized or growing file cannot
/// consume unbounded builder memory. After the read, the file is re-stated and
/// must report the SAME size as the descriptor's original length — a file that
/// changed mid-read (TOCTOU) is refused rather than hashed inconsistently.
/// On Unix the post-read stat goes through the open descriptor (`fstat`), so
/// the check is immune to the path being replaced after open.
#[cfg(unix)]
fn read_regular_file_bounded(path: &Path, max_size: u64) -> Result<(Vec<u8>, u64)> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;

    let file =
        fs::File::open(path).with_context(|| format!("open static web file {}", path.display()))?;
    let before = file
        .metadata()
        .with_context(|| format!("fstat static web file {}", path.display()))?;
    let before_size = before.len();
    let mut bytes = Vec::with_capacity(usize::try_from(before_size.min(max_size)).unwrap_or(0));
    (&file)
        .take(max_size + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read static web file {}", path.display()))?;
    let after = file
        .metadata()
        .with_context(|| format!("fstat static web file after read {}", path.display()))?;
    if after.len() != before_size || after.ino() != before.ino() {
        bail!(
            "static web file changed while being read: {}",
            path.display()
        );
    }
    Ok((bytes, before_size))
}

#[cfg(not(unix))]
fn read_regular_file_bounded(path: &Path, max_size: u64) -> Result<(Vec<u8>, u64)> {
    use std::io::Read as _;

    let mut file =
        fs::File::open(path).with_context(|| format!("open static web file {}", path.display()))?;
    let before_size = file
        .metadata()
        .with_context(|| format!("stat static web file {}", path.display()))?
        .len();
    let mut bytes = Vec::with_capacity(usize::try_from(before_size.min(max_size)).unwrap_or(0));
    (&file)
        .take(max_size + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read static web file {}", path.display()))?;
    let after_size = file
        .metadata()
        .with_context(|| format!("stat static web file after read {}", path.display()))?
        .len();
    if after_size != before_size {
        bail!(
            "static web file changed while being read: {}",
            path.display()
        );
    }
    Ok((bytes, before_size))
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

/// The bundle carries exactly what the edge can serve.
///
/// The media-type table IS the definition of publishable web content: without
/// an entry there is no `Content-Type` to serve the file under, so the edge
/// could not deliver it even if it were included. Everything else — hidden
/// entries (`.gitignore`, `.github/`), extension-less tooling (`LICENSE`,
/// `Rakefile`, `Dockerfile`) and the sources a site is built FROM
/// (`style/helpers.scss`) — is excluded from the bundle.
///
/// This started as an enumerate-and-fail rule, which is what a build output
/// directory deserves. Three consecutive real repositories disproved it for
/// `root = "."`: 2048 alone failed on `.gitignore`, then `Rakefile`, then
/// `style/helpers.scss`. Refusing to publish a working site because it keeps
/// its Sass next to its CSS is the wrong trade; an unservable file simply is
/// not part of the site.
///
/// Source maps stay a hard failure (handled by the caller) — they are
/// servable, which is exactly why they must not be published by accident.
fn is_publishable_web_file(relative: &str) -> bool {
    if relative.split('/').any(|segment| segment.starts_with('.')) {
        return false;
    }
    media_type_for(relative).is_some()
}

fn media_type_for(path: &str) -> Option<&'static str> {
    let extension = Path::new(path).extension()?.to_str()?;
    match extension {
        "js" | "mjs" => Some("application/javascript; charset=utf-8"),
        "json" => Some("application/json; charset=utf-8"),
        "webmanifest" => Some("application/manifest+json; charset=utf-8"),
        "wasm" => Some("application/wasm"),
        "bin" => Some("application/octet-stream"),
        "eot" => Some("application/vnd.ms-fontobject"),
        "otf" => Some("font/otf"),
        "ttf" => Some("font/ttf"),
        "woff" => Some("font/woff"),
        "woff2" => Some("font/woff2"),
        "avif" => Some("image/avif"),
        "gif" => Some("image/gif"),
        "ico" => Some("image/x-icon"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "svg" => Some("image/svg+xml"),
        "webp" => Some("image/webp"),
        "css" => Some("text/css; charset=utf-8"),
        "html" | "htm" => Some("text/html; charset=utf-8"),
        "txt" => Some("text/plain; charset=utf-8"),
        // The v1 manifest allowlist (capsule::contract::static_web_manifest::
        // is_allowed_media_type) is a FROZEN contract shared with ato-api,
        // ato-edge and ato-usercontent-static — this table must stay a subset
        // of it. Audio, video, markdown and PDF are therefore NOT publishable
        // in v1: adding them here alone produces a bundle the manifest
        // validator rejects. They belong in a coordinated v2.
        _ => None,
    }
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
        // Source maps are simply not publishable: skipped, never bundled.
        let bundle =
            produce_static_web_bundle(&plan(), extracted.output_root(), parent.path(), &[])
                .unwrap();
        assert!(!bundle.receipt.blobs.is_empty());
        assert!(
            String::from_utf8(bundle.manifest_bytes.clone())
                .unwrap()
                .find("app.js.map")
                .is_none()
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
    fn publishes_common_manifest_icon_and_font_media_types() {
        let image = fixture_root();
        for (path, bytes) in [
            ("favicon.ico", b"icon".as_slice()),
            ("manifest.webmanifest", b"{}".as_slice()),
            ("font.eot", b"eot".as_slice()),
            ("font.otf", b"otf".as_slice()),
            ("font.ttf", b"ttf".as_slice()),
            ("font.woff", b"woff".as_slice()),
        ] {
            fs::write(image.path().join("dist").join(path), bytes).unwrap();
        }
        let extracted = extract_static_web_output(image.path(), &plan()).unwrap();
        let parent = tempfile::tempdir().unwrap();
        let produced =
            produce_static_web_bundle(&plan(), extracted.output_root(), parent.path(), &[])
                .unwrap();
        let manifest: capsule::contract::static_web_manifest::StaticWebManifestV1 =
            serde_json::from_slice(&produced.manifest_bytes).unwrap();
        assert_eq!(manifest.files["favicon.ico"].media_type, "image/x-icon");
        assert_eq!(
            manifest.files["manifest.webmanifest"].media_type,
            "application/manifest+json; charset=utf-8"
        );
        assert_eq!(
            manifest.files["font.eot"].media_type,
            "application/vnd.ms-fontobject"
        );
        assert_eq!(manifest.files["font.otf"].media_type, "font/otf");
        assert_eq!(manifest.files["font.ttf"].media_type, "font/ttf");
        assert_eq!(manifest.files["font.woff"].media_type, "font/woff");
    }

    #[test]
    fn refuses_a_file_that_exceeds_the_single_file_limit() {
        let image = fixture_root();
        fs::write(
            image.path().join("dist/index.html"),
            vec![b'x'; (MAX_FILE_SIZE + 1) as usize],
        )
        .unwrap();
        let extracted = extract_static_web_output(image.path(), &plan()).unwrap();
        let parent = tempfile::tempdir().unwrap();
        assert!(
            produce_static_web_bundle(&plan(), extracted.output_root(), parent.path(), &[])
                .unwrap_err()
                .to_string()
                .contains("exceeds")
        );
    }

    #[test]
    fn rejects_a_manifest_with_the_same_blob_at_two_sizes() {
        let image = fixture_root();
        fs::write(image.path().join("dist/index.html"), "built").unwrap();
        fs::write(image.path().join("dist/a.js"), "aaaa").unwrap();
        fs::write(image.path().join("dist/b.js"), "bbbbbb").unwrap();
        let extracted = extract_static_web_output(image.path(), &plan()).unwrap();
        let parent = tempfile::tempdir().unwrap();
        let produced =
            produce_static_web_bundle(&plan(), extracted.output_root(), parent.path(), &[])
                .unwrap();
        let mut manifest: capsule::contract::static_web_manifest::StaticWebManifestV1 =
            serde_json::from_slice(&produced.manifest_bytes).unwrap();
        // Point both JS files at the SAME blob with DIFFERENT sizes — the R2
        // object has one size, so the manifest must refuse it.
        let blob = manifest.files["a.js"].blob.clone();
        manifest.files.get_mut("b.js").unwrap().blob = blob;
        manifest.files.get_mut("b.js").unwrap().size = manifest.files["a.js"].size + 1;
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn rejects_unsafe_closure_members() {
        // An INTERNAL link is materialized (not a closure risk); only a link
        // whose target escapes the image root stays refused.
        let image = fixture_root();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("index.html", image.path().join("dist/link.html"))
                .unwrap();
            let extracted = extract_static_web_output(image.path(), &plan());
            assert!(extracted.is_ok(), "internal link must be materialized");

            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("secret.txt"), "host-secret").unwrap();
            std::os::unix::fs::symlink(
                outside.path().join("secret.txt"),
                image.path().join("dist/leak.html"),
            )
            .unwrap();
            let escaping = extract_static_web_output(image.path(), &plan());
            assert!(escaping.is_err(), "escaping link must be refused");
        }
    }

    #[test]
    fn labels_match_the_normative_fixture() {
        assert_eq!(
            host_label(
                'p',
                "sha256:c61c17155f2594c1c32fda225bb5c552d611f5c916b95e904f55afa6b7b69543"
            )
            .unwrap(),
            "p-yyobofk7ewkmdqzp3irfxnofkllbd5ojc24v5ecpkwx2nn5wsvbq"
        );
    }
}

#[cfg(test)]
mod publishable_web_file_tests {
    use super::is_publishable_web_file;

    #[test]
    fn real_site_content_is_published() {
        for path in [
            "index.html",
            "style/main.css",
            "js/application.js",
            "favicon.ico",
            "meta/apple-touch-icon.png",
        ] {
            assert!(is_publishable_web_file(path), "must publish {path}");
        }
    }

    #[test]
    fn hidden_entries_never_reach_the_edge() {
        for path in [".gitignore", ".github/workflows/ci.yml", "assets/.keep"] {
            assert!(!is_publishable_web_file(path), "must exclude {path}");
        }
    }

    #[test]
    fn tooling_and_sources_are_not_web_content() {
        for path in [
            "LICENSE",
            "Rakefile",
            "Makefile",
            "Dockerfile",
            "style/helpers.scss",
            "src/app.ts",
            "Gemfile.lock",
            "README.md",
            "audio/hit.mp3",
        ] {
            assert!(!is_publishable_web_file(path), "must exclude {path}");
        }
    }
}

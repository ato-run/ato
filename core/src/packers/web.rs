use std::fs;
use std::io::BufReader;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use tar::{Builder, EntryType, Header};
use tracing::debug;
use zstd::stream::write::Encoder as ZstdEncoder;

use crate::error::{CapsuleError, Result};
use crate::lockfile;
use crate::lockfile::{CAPSULE_LOCK_FILE_NAME, LEGACY_CAPSULE_LOCK_FILE_NAME};
use crate::manifest;
use crate::packers::payload::{
    build_distribution_manifest, normalize_relative_utf8_path, reconstruct_from_chunks,
};
use crate::packers::sbom::{generate_embedded_sbom, SBOM_PATH};
use crate::router::ManifestData;

#[derive(Debug, Clone)]
pub struct WebPackOptions {
    pub manifest_path: PathBuf,
    pub manifest_dir: PathBuf,
    pub output: Option<PathBuf>,
}

const ZSTD_COMPRESSION_LEVEL: i32 = 19;
const DEFAULT_REPRO_MTIME: u64 = 0;

pub fn pack(
    plan: &ManifestData,
    opts: WebPackOptions,
    reporter: Arc<dyn crate::reporter::CapsuleReporter + 'static>,
) -> Result<PathBuf> {
    let runtime = plan
        .execution_runtime()
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_default();
    if runtime != "web" {
        return Err(CapsuleError::Pack(
            "web packer requires runtime=web target".to_string(),
        ));
    }

    let driver = plan
        .execution_driver()
        .map(|v| v.trim().to_ascii_lowercase())
        .ok_or_else(|| CapsuleError::Pack("runtime=web target requires driver".to_string()))?;
    if driver != "static" {
        return Err(CapsuleError::Pack(format!(
            "web packer only supports driver=static (got '{}')",
            driver
        )));
    }

    let loaded = manifest::load_manifest(&opts.manifest_path)?;
    let entrypoint = plan
        .execution_entrypoint()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| CapsuleError::Pack("runtime=web target requires entrypoint".to_string()))?;
    let (entrypoint_dir, entrypoint_prefix) =
        resolve_static_entrypoint(&opts.manifest_dir, &entrypoint)?;

    let temp_dir = tempfile::tempdir().map_err(CapsuleError::Io)?;
    let payload_tar_path = temp_dir.path().join("payload.tar");
    let payload_zst_path = temp_dir.path().join("payload.tar.zst");

    debug!("Packing runtime=web static payload");
    let mut payload_file = fs::File::create(&payload_tar_path).map_err(CapsuleError::Io)?;
    let mut payload_builder = Builder::new(&mut payload_file);
    let mut sbom_files = Vec::new();
    append_directory_tree(
        &mut payload_builder,
        &entrypoint_dir,
        &entrypoint_prefix,
        &mut sbom_files,
        reproducible_mtime_epoch(),
    )?;
    payload_builder.finish().map_err(CapsuleError::Io)?;
    drop(payload_builder);
    let payload_tar_bytes = fs::read(&payload_tar_path).map_err(CapsuleError::Io)?;
    let (distribution_manifest, manifest_toml_bytes) =
        build_distribution_manifest(&loaded.model, &payload_tar_bytes)?;
    let rebuilt_payload = reconstruct_from_chunks(
        &payload_tar_bytes,
        &distribution_manifest
            .distribution
            .as_ref()
            .expect("distribution metadata")
            .chunk_list,
    )?;
    if rebuilt_payload != payload_tar_bytes {
        return Err(CapsuleError::Pack(
            "failed to reconstruct payload.tar from chunk_list".to_string(),
        ));
    }

    let mut zst_encoder = ZstdEncoder::new(
        fs::File::create(&payload_zst_path).map_err(CapsuleError::Io)?,
        ZSTD_COMPRESSION_LEVEL,
    )
    .map_err(CapsuleError::Io)?;
    let mut payload_reader =
        BufReader::new(fs::File::open(&payload_tar_path).map_err(CapsuleError::Io)?);
    std::io::copy(&mut payload_reader, &mut zst_encoder).map_err(CapsuleError::Io)?;
    let _ = zst_encoder.finish().map_err(CapsuleError::Io)?;

    let output_path = opts.output.unwrap_or_else(|| {
        let name = loaded.model.name.replace('\"', "-");
        opts.manifest_dir.join(format!("{}.capsule", name))
    });

    let mut capsule_file = fs::File::create(&output_path).map_err(CapsuleError::Io)?;
    let mut outer = Builder::new(&mut capsule_file);
    let manifest_tmp = temp_dir.path().join("capsule.toml");
    fs::write(&manifest_tmp, &manifest_toml_bytes).map_err(CapsuleError::Io)?;
    let lockfile_path = ensure_lockfile(
        &opts.manifest_path,
        &loaded.raw,
        &loaded.raw_text,
        reporter.clone(),
    )?;
    append_regular_file_normalized(
        &mut outer,
        &manifest_tmp,
        "capsule.toml",
        reproducible_mtime_epoch(),
    )?;
    let packaged_lockfile_bytes =
        crate::lockfile::render_lockfile_for_manifest(&lockfile_path, &distribution_manifest)?;
    let packaged_lockfile_path = temp_dir.path().join(CAPSULE_LOCK_FILE_NAME);
    fs::write(&packaged_lockfile_path, packaged_lockfile_bytes).map_err(CapsuleError::Io)?;
    append_regular_file_normalized(
        &mut outer,
        &packaged_lockfile_path,
        CAPSULE_LOCK_FILE_NAME,
        reproducible_mtime_epoch(),
    )?;

    let sbom = generate_embedded_sbom(&loaded.model.name, &sbom_files)?;
    let sbom_tmp = temp_dir.path().join(SBOM_PATH);
    fs::write(&sbom_tmp, sbom.document).map_err(CapsuleError::Io)?;
    append_regular_file_normalized(&mut outer, &sbom_tmp, SBOM_PATH, reproducible_mtime_epoch())?;

    let signature_tmp = temp_dir.path().join("signature.json");
    let signature = serde_json::json!({
        "signed": false,
        "note": "To be signed",
        "sbom": {
            "path": SBOM_PATH,
            "sha256": sbom.sha256,
            "format": "spdx-json",
        }
    });
    let signature_bytes = serde_jcs::to_vec(&signature).map_err(|e| {
        CapsuleError::Pack(format!("Failed to serialize signature metadata (JCS): {e}"))
    })?;
    fs::write(&signature_tmp, signature_bytes).map_err(CapsuleError::Io)?;
    append_regular_file_normalized(
        &mut outer,
        &signature_tmp,
        "signature.json",
        reproducible_mtime_epoch(),
    )?;
    append_regular_file_normalized(
        &mut outer,
        &payload_zst_path,
        "payload.tar.zst",
        reproducible_mtime_epoch(),
    )?;
    if let Some((readme_path, archive_name)) =
        crate::packers::capsule::find_nearest_readme_candidate(&opts.manifest_dir)
    {
        append_regular_file_normalized(
            &mut outer,
            &readme_path,
            &archive_name,
            reproducible_mtime_epoch(),
        )?;
    }
    outer.finish().map_err(CapsuleError::Io)?;

    Ok(output_path)
}

fn ensure_lockfile(
    manifest_path: &Path,
    manifest_raw: &toml::Value,
    manifest_text: &str,
    reporter: Arc<dyn crate::reporter::CapsuleReporter + 'static>,
) -> Result<PathBuf> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        return tokio::task::block_in_place(|| {
            handle.block_on(lockfile::ensure_lockfile(
                manifest_path,
                manifest_raw,
                manifest_text,
                reporter,
                false,
            ))
        });
    }

    let rt = tokio::runtime::Runtime::new().map_err(CapsuleError::Io)?;
    rt.block_on(lockfile::ensure_lockfile(
        manifest_path,
        manifest_raw,
        manifest_text,
        reporter,
        false,
    ))
}

fn resolve_static_entrypoint(manifest_dir: &Path, entrypoint: &str) -> Result<(PathBuf, PathBuf)> {
    let trimmed = entrypoint.trim();
    let raw = PathBuf::from(trimmed);
    if raw.is_absolute() {
        return Err(CapsuleError::Pack(format!(
            "runtime=web static entrypoint '{}' must be a relative directory path",
            entrypoint
        )));
    }

    let mut cleaned = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => cleaned.push(part),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(CapsuleError::Pack(format!(
                    "runtime=web static entrypoint '{}' is unsafe",
                    entrypoint
                )));
            }
        }
    }

    let entrypoint_dir = manifest_dir.join(&cleaned);
    if !entrypoint_dir.exists() || !entrypoint_dir.is_dir() {
        return Err(CapsuleError::Pack(format!(
            "runtime=web static entrypoint '{}' must be an existing directory",
            entrypoint
        )));
    }

    let root = manifest_dir
        .canonicalize()
        .unwrap_or_else(|_| manifest_dir.to_path_buf());
    let canonical_entrypoint = entrypoint_dir.canonicalize().map_err(CapsuleError::Io)?;
    if !canonical_entrypoint.starts_with(&root) {
        return Err(CapsuleError::Pack(format!(
            "runtime=web static entrypoint '{}' resolves outside manifest directory",
            entrypoint
        )));
    }

    Ok((canonical_entrypoint, cleaned))
}

fn append_directory_tree(
    builder: &mut Builder<&mut fs::File>,
    source_root: &Path,
    tar_prefix: &Path,
    sbom_files: &mut Vec<(String, PathBuf)>,
    mtime: u64,
) -> Result<()> {
    if !tar_prefix.as_os_str().is_empty() {
        let prefix = normalize_relative_utf8_path(tar_prefix)?;
        append_directory_normalized(builder, &prefix, 0o755, mtime)?;
    }
    append_directory_tree_recursive(
        builder,
        source_root,
        source_root,
        tar_prefix,
        sbom_files,
        mtime,
    )
}

fn append_directory_tree_recursive(
    builder: &mut Builder<&mut fs::File>,
    source_root: &Path,
    current_dir: &Path,
    tar_prefix: &Path,
    sbom_files: &mut Vec<(String, PathBuf)>,
    mtime: u64,
) -> Result<()> {
    let mut entries = fs::read_dir(current_dir)
        .map_err(CapsuleError::Io)?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(CapsuleError::Io)?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let rel = path
            .strip_prefix(source_root)
            .map_err(|e| CapsuleError::Pack(format!("Failed to compute archive path: {}", e)))?;
        if should_skip_entry(rel, entry.file_type().map_err(CapsuleError::Io)?.is_dir()) {
            continue;
        }

        let metadata = fs::symlink_metadata(&path).map_err(CapsuleError::Io)?;
        if metadata.file_type().is_symlink() {
            return Err(CapsuleError::Pack(format!(
                "symlink is not allowed in static web payload: {}",
                path.display()
            )));
        }

        let archive_path = if tar_prefix.as_os_str().is_empty() {
            rel.to_path_buf()
        } else {
            tar_prefix.join(rel)
        };
        let archive_path_normalized = normalize_relative_utf8_path(&archive_path)?;

        if metadata.is_dir() {
            append_directory_normalized(builder, &archive_path_normalized, 0o755, mtime)?;
            append_directory_tree_recursive(
                builder,
                source_root,
                &path,
                tar_prefix,
                sbom_files,
                mtime,
            )?;
            continue;
        }

        if metadata.is_file() {
            append_regular_file_normalized(builder, &path, &archive_path_normalized, mtime)?;
            sbom_files.push((archive_path_normalized, path.clone()));
        }
    }

    Ok(())
}

fn should_skip_entry(rel: &Path, is_dir: bool) -> bool {
    let rel_text = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/");
    if rel_text == ".next/cache" || rel_text.starts_with(".next/cache/") {
        return true;
    }
    if rel_text.contains("/.next/cache/") {
        return true;
    }

    for component in rel.components() {
        let part = component.as_os_str().to_string_lossy();
        if matches!(
            part.as_ref(),
            ".git"
                | ".capsule"
                | "target"
                | "node_modules"
                | ".venv"
                | "venv"
                | "__pycache__"
                | ".pytest_cache"
        ) {
            return true;
        }
    }

    if is_dir {
        return false;
    }

    let file_name = rel
        .file_name()
        .map(|v| v.to_string_lossy().to_string())
        .unwrap_or_default();
    if matches!(
        file_name.as_str(),
        "capsule.toml"
            | CAPSULE_LOCK_FILE_NAME
            | LEGACY_CAPSULE_LOCK_FILE_NAME
            | "config.json"
            | "signature.json"
            | "sbom.spdx.json"
            | "payload.tar"
            | "payload.tar.zst"
    ) {
        return true;
    }
    file_name.ends_with(".capsule") || file_name.ends_with(".sig")
}

fn append_regular_file_normalized(
    builder: &mut Builder<&mut fs::File>,
    source_path: &Path,
    archive_path: &str,
    mtime: u64,
) -> Result<()> {
    let mut file = fs::File::open(source_path).map_err(CapsuleError::Io)?;
    let metadata = file.metadata().map_err(CapsuleError::Io)?;
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_size(metadata.len());
    header.set_mode(normalize_file_mode(metadata_mode(&metadata)));
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(mtime);
    header.set_cksum();
    builder
        .append_data(&mut header, archive_path, &mut file)
        .map_err(CapsuleError::Io)
}

fn append_directory_normalized(
    builder: &mut Builder<&mut fs::File>,
    archive_path: &str,
    mode: u32,
    mtime: u64,
) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Directory);
    header.set_size(0);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(mtime);
    header.set_cksum();
    builder
        .append_data(&mut header, archive_path, std::io::empty())
        .map_err(CapsuleError::Io)
}

#[cfg(unix)]
fn metadata_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn metadata_mode(_: &fs::Metadata) -> u32 {
    0o644
}

fn normalize_file_mode(mode: u32) -> u32 {
    if mode & 0o111 != 0 {
        0o755
    } else {
        0o644
    }
}

fn reproducible_mtime_epoch() -> u64 {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_REPRO_MTIME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporter::NoOpReporter;
    use crate::router::ExecutionProfile;
    use crate::types::CapsuleManifest;
    use sha2::{Digest, Sha256};
    use std::io::Read;

    fn sha256_hex(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hex::encode(hasher.finalize())
    }

    #[test]
    fn pack_static_emits_capsule_with_lock_and_without_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::create_dir_all(tmp.path().join("dist/assets")).expect("mkdir");
        std::fs::write(tmp.path().join("dist/index.html"), "<h1>hello</h1>").expect("write html");
        std::fs::write(tmp.path().join("dist/assets/app.js"), "console.log('ok')")
            .expect("write js");
        std::fs::write(
            tmp.path().join("capsule.lock"),
            r#"version = "1"

[meta]
created_at = "2026-01-01T00:00:00Z"
manifest_hash = "sha256:dummy"
"#,
        )
        .expect("write lock");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.2"
name = "web-static-pack"
version = "0.1.0"
type = "app"
default_target = "static"

[targets.static]
runtime = "web"
driver = "static"
entrypoint = "dist"
port = 8080
"#,
        )
        .expect("write manifest");

        let decision =
            crate::router::route_manifest(&manifest_path, ExecutionProfile::Release, None)
                .expect("route");
        let output_path = tmp.path().join("web-static-pack.capsule");
        let out = pack(
            &decision.plan,
            WebPackOptions {
                manifest_path: manifest_path.clone(),
                manifest_dir: tmp.path().to_path_buf(),
                output: Some(output_path.clone()),
            },
            Arc::new(NoOpReporter),
        )
        .expect("pack");
        assert_eq!(out, output_path);

        let mut outer = tar::Archive::new(fs::File::open(&out).expect("open capsule"));
        let mut has_capsule_toml = false;
        let mut has_lock = false;
        let mut has_payload = false;
        let mut has_signature = false;
        let mut has_sbom = false;
        let mut has_manifest_toml = false;
        let mut payload_bytes = Vec::new();
        let mut manifest_toml_bytes = Vec::new();
        for entry in outer.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let path = entry.path().expect("path").to_string_lossy().to_string();
            if path == "capsule.toml" {
                has_capsule_toml = true;
                has_manifest_toml = true;
                entry
                    .read_to_end(&mut manifest_toml_bytes)
                    .expect("read manifest TOML");
            } else if path == CAPSULE_LOCK_FILE_NAME || path == LEGACY_CAPSULE_LOCK_FILE_NAME {
                has_lock = true;
            } else if path == "signature.json" {
                has_signature = true;
            } else if path == SBOM_PATH {
                has_sbom = true;
            } else if path == "payload.tar.zst" {
                has_payload = true;
                entry.read_to_end(&mut payload_bytes).expect("read payload");
            }
        }

        assert!(has_capsule_toml);
        assert!(has_lock);
        assert!(has_signature);
        assert!(has_sbom);
        assert!(has_manifest_toml);
        assert!(has_payload);

        let embedded = crate::packers::sbom::extract_and_verify_embedded_sbom(&out)
            .expect("verify embedded sbom");
        assert!(embedded.contains("\"fileName\": \"dist/index.html\""));

        let decoder = zstd::stream::Decoder::new(std::io::Cursor::new(payload_bytes.clone()))
            .expect("decoder");
        let mut payload = tar::Archive::new(decoder);
        let mut files = Vec::new();
        for entry in payload.entries().expect("payload entries") {
            let entry = entry.expect("payload entry");
            files.push(
                entry
                    .path()
                    .expect("payload path")
                    .to_string_lossy()
                    .to_string(),
            );
        }
        files.sort();

        assert!(files.iter().any(|p| p == "dist/index.html"));
        assert!(files.iter().any(|p| p == "dist/assets/app.js"));
        assert!(!files.iter().any(|p| p == CAPSULE_LOCK_FILE_NAME));
        assert!(!files.iter().any(|p| p == LEGACY_CAPSULE_LOCK_FILE_NAME));
        assert!(!files.iter().any(|p| p == "config.json"));

        let manifest_toml: CapsuleManifest =
            toml::from_str(std::str::from_utf8(&manifest_toml_bytes).expect("manifest utf8"))
                .expect("manifest parse");
        let mut payload_decoder = zstd::stream::Decoder::new(std::io::Cursor::new(payload_bytes))
            .expect("payload decoder");
        let mut payload_tar_bytes = Vec::new();
        payload_decoder
            .read_to_end(&mut payload_tar_bytes)
            .expect("payload tar bytes");
        let reconstructed = crate::packers::payload::reconstruct_from_chunks(
            &payload_tar_bytes,
            &manifest_toml
                .distribution
                .as_ref()
                .expect("distribution metadata")
                .chunk_list,
        )
        .expect("reconstruct");
        assert_eq!(reconstructed, payload_tar_bytes);
    }

    #[cfg(unix)]
    #[test]
    fn pack_static_rejects_symlink_escape() {
        use std::os::unix::fs as unix_fs;

        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::create_dir_all(tmp.path().join("dist")).expect("mkdir");
        std::fs::write(tmp.path().join("outside.txt"), "secret").expect("write outside");
        unix_fs::symlink("../outside.txt", tmp.path().join("dist/link.txt")).expect("symlink");

        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.2"
name = "web-static-pack"
version = "0.1.0"
type = "app"
default_target = "static"

[targets.static]
runtime = "web"
driver = "static"
entrypoint = "dist"
port = 8080
"#,
        )
        .expect("write manifest");

        let decision =
            crate::router::route_manifest(&manifest_path, ExecutionProfile::Release, None)
                .expect("route");
        let err = pack(
            &decision.plan,
            WebPackOptions {
                manifest_path,
                manifest_dir: tmp.path().to_path_buf(),
                output: Some(tmp.path().join("web-static-pack.capsule")),
            },
            Arc::new(NoOpReporter),
        )
        .expect_err("must fail");

        assert!(err.to_string().contains("symlink is not allowed"));
    }

    #[test]
    fn pack_static_is_reproducible_for_identical_inputs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::create_dir_all(tmp.path().join("dist/assets")).expect("mkdir");
        std::fs::write(
            tmp.path().join("dist/index.html"),
            "<h1>hello reproducible</h1>",
        )
        .expect("write html");
        std::fs::write(
            tmp.path().join("dist/assets/app.js"),
            "console.log('repro')",
        )
        .expect("write js");

        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.2"
name = "web-static-repro"
version = "0.1.0"
type = "app"
default_target = "static"

[targets.static]
runtime = "web"
driver = "static"
entrypoint = "dist"
port = 8080
"#,
        )
        .expect("write manifest");

        let decision =
            crate::router::route_manifest(&manifest_path, ExecutionProfile::Release, None)
                .expect("route");
        let out1 = tmp.path().join("web-static-repro-1.capsule");
        let out2 = tmp.path().join("web-static-repro-2.capsule");

        pack(
            &decision.plan,
            WebPackOptions {
                manifest_path: manifest_path.clone(),
                manifest_dir: tmp.path().to_path_buf(),
                output: Some(out1.clone()),
            },
            Arc::new(NoOpReporter),
        )
        .expect("first pack");

        pack(
            &decision.plan,
            WebPackOptions {
                manifest_path,
                manifest_dir: tmp.path().to_path_buf(),
                output: Some(out2.clone()),
            },
            Arc::new(NoOpReporter),
        )
        .expect("second pack");

        let first = std::fs::read(out1).expect("read first artifact");
        let second = std::fs::read(out2).expect("read second artifact");
        assert_eq!(sha256_hex(&first), sha256_hex(&second));
    }
}

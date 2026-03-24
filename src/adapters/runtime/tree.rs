use anyhow::{bail, Context, Result};
use rand::RngCore;
use serde::Deserialize;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs as unix_fs;
#[cfg(windows)]
use std::os::windows::fs as windows_fs;

use capsule_core::packers::payload as manifest_payload;
use capsule_core::types::CapsuleManifest;

const STORE_DIR: &str = ".ato/store";
const RUNTIMES_DIR: &str = ".ato/runtimes";
const CURRENT_SYMLINK: &str = "current";
const PROMOTED_NAMESPACE: &str = "promoted";

#[derive(Debug, Clone, Deserialize)]
struct StoredPromotionMetadata {
    performed: bool,
    content_hash: Option<String>,
}

pub fn prepare_runtime_tree(
    publisher: &str,
    slug: &str,
    version: &str,
    capsule_bytes: &[u8],
) -> Result<PathBuf> {
    prepare_runtime_tree_at(&runtime_root()?, publisher, slug, version, capsule_bytes)
}

fn prepare_runtime_tree_at(
    runtime_root: &Path,
    publisher: &str,
    slug: &str,
    version: &str,
    capsule_bytes: &[u8],
) -> Result<PathBuf> {
    let manifest_toml = extract_capsule_entry(capsule_bytes, "capsule.toml")
        .with_context(|| "capsule.toml is required for runtime extraction")?;
    let manifest: CapsuleManifest =
        toml::from_str(&manifest_toml).with_context(|| "Invalid capsule.toml in artifact")?;
    let manifest_hash = manifest_payload::compute_manifest_hash_without_signatures(&manifest)?;
    let manifest_prefix = manifest_hash
        .trim_start_matches("blake3:")
        .chars()
        .take(12)
        .collect::<String>();

    let base_dir = runtime_root.join(publisher).join(slug);
    fs::create_dir_all(&base_dir).with_context(|| {
        format!(
            "Failed to create runtime base directory: {}",
            base_dir.display()
        )
    })?;
    let runtime_dir = base_dir.join(format!("{}_{}", version, manifest_prefix));

    if !runtime_dir.join("capsule.toml").exists() {
        extract_capsule_to_runtime_dir(capsule_bytes, &runtime_dir)?;
    }
    switch_current_symlink(&base_dir, &runtime_dir)?;
    Ok(runtime_dir.join("capsule.toml"))
}

pub fn prepare_store_runtime_for_capsule(capsule_path: &Path) -> Result<Option<PathBuf>> {
    prepare_store_runtime_for_capsule_at(capsule_path, &store_root()?, &runtime_root()?)
}

pub fn prepare_promoted_runtime_for_capsule(capsule_path: &Path) -> Result<Option<PathBuf>> {
    let store_root = store_root()?;
    let runtime_root = runtime_root()?;
    let Some((publisher, slug, version)) =
        parse_store_capsule_identity_at(capsule_path, &store_root)?
    else {
        return Ok(None);
    };
    let install_dir = capsule_path
        .parent()
        .context("Installed capsule must have parent directory")?;
    let promotion = load_promotion_metadata(&install_dir.join("promotion.json"))?;
    let Some(promotion) = promotion else {
        return Ok(None);
    };
    if !promotion.performed {
        return Ok(None);
    }

    let bytes = fs::read(capsule_path).with_context(|| {
        format!(
            "Failed to read installed capsule: {}",
            capsule_path.display()
        )
    })?;
    validate_promotion_metadata(capsule_path, &promotion, &bytes)?;
    let manifest_path = prepare_runtime_tree_at(
        &runtime_root.join(PROMOTED_NAMESPACE),
        &publisher,
        &slug,
        &version,
        &bytes,
    )?;
    Ok(Some(manifest_path))
}

fn prepare_store_runtime_for_capsule_at(
    capsule_path: &Path,
    store_root: &Path,
    runtime_root: &Path,
) -> Result<Option<PathBuf>> {
    if let Some(manifest_path) =
        prepare_promoted_runtime_for_capsule_at(capsule_path, store_root, runtime_root)?
    {
        return Ok(Some(manifest_path));
    }

    let Some((publisher, slug, version)) =
        parse_store_capsule_identity_at(capsule_path, store_root)?
    else {
        return Ok(None);
    };
    let bytes = fs::read(capsule_path).with_context(|| {
        format!(
            "Failed to read installed capsule: {}",
            capsule_path.display()
        )
    })?;
    let manifest_path = prepare_runtime_tree_at(runtime_root, &publisher, &slug, &version, &bytes)?;
    Ok(Some(manifest_path))
}

fn prepare_promoted_runtime_for_capsule_at(
    capsule_path: &Path,
    store_root: &Path,
    runtime_root: &Path,
) -> Result<Option<PathBuf>> {
    let Some((publisher, slug, version)) =
        parse_store_capsule_identity_at(capsule_path, store_root)?
    else {
        return Ok(None);
    };
    let install_dir = capsule_path
        .parent()
        .context("Installed capsule must have parent directory")?;
    let promotion = load_promotion_metadata(&install_dir.join("promotion.json"))?;
    let Some(promotion) = promotion else {
        return Ok(None);
    };
    if !promotion.performed {
        return Ok(None);
    }

    let bytes = fs::read(capsule_path).with_context(|| {
        format!(
            "Failed to read installed capsule: {}",
            capsule_path.display()
        )
    })?;
    validate_promotion_metadata(capsule_path, &promotion, &bytes)?;
    let manifest_path = prepare_runtime_tree_at(
        &runtime_root.join(PROMOTED_NAMESPACE),
        &publisher,
        &slug,
        &version,
        &bytes,
    )?;
    Ok(Some(manifest_path))
}

fn runtime_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("ATO_RUNTIME_ROOT") {
        return Ok(PathBuf::from(path));
    }
    let home = dirs::home_dir().context("Failed to determine home directory")?;
    Ok(home.join(RUNTIMES_DIR))
}

fn store_root() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to determine home directory")?;
    Ok(home.join(STORE_DIR))
}

fn parse_store_capsule_identity_at(
    capsule_path: &Path,
    store_root: &Path,
) -> Result<Option<(String, String, String)>> {
    let Ok(relative) = capsule_path.strip_prefix(store_root) else {
        return Ok(None);
    };
    let components = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    if components.len() != 4 {
        return Ok(None);
    }
    Ok(Some((
        components[0].clone(),
        components[1].clone(),
        components[2].clone(),
    )))
}

fn load_promotion_metadata(path: &Path) -> Result<Option<StoredPromotionMetadata>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read(path)
        .with_context(|| format!("Failed to read promotion metadata: {}", path.display()))?;
    let metadata = serde_json::from_slice(&raw)
        .with_context(|| format!("Failed to parse promotion metadata: {}", path.display()))?;
    Ok(Some(metadata))
}

fn validate_promotion_metadata(
    capsule_path: &Path,
    promotion: &StoredPromotionMetadata,
    capsule_bytes: &[u8],
) -> Result<()> {
    let Some(expected_hash) = promotion.content_hash.as_deref() else {
        return Ok(());
    };
    let actual_hash = format!("blake3:{}", blake3::hash(capsule_bytes).to_hex());
    if actual_hash != expected_hash {
        bail!(
            "Promotion metadata hash mismatch for {}: expected {}, got {}",
            capsule_path.display(),
            expected_hash,
            actual_hash
        );
    }
    Ok(())
}

fn extract_capsule_to_runtime_dir(capsule_bytes: &[u8], runtime_dir: &Path) -> Result<()> {
    let parent = runtime_dir
        .parent()
        .context("Runtime directory must have a parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create runtime parent: {}", parent.display()))?;

    let tmp_dir = parent.join(format!(
        ".{}.tmp-{}",
        runtime_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("runtime"),
        random_suffix()
    ));
    if tmp_dir.exists() {
        fs::remove_dir_all(&tmp_dir).ok();
    }
    fs::create_dir_all(&tmp_dir)
        .with_context(|| format!("Failed to create temp runtime dir: {}", tmp_dir.display()))?;

    let result = (|| -> Result<()> {
        let mut archive = tar::Archive::new(Cursor::new(capsule_bytes));
        archive
            .unpack(&tmp_dir)
            .with_context(|| format!("Failed to unpack capsule into {}", tmp_dir.display()))?;

        let payload_zst_path = tmp_dir.join("payload.tar.zst");
        if payload_zst_path.exists() {
            let mut decoder = zstd::stream::Decoder::new(
                fs::File::open(&payload_zst_path)
                    .with_context(|| format!("Failed to open {}", payload_zst_path.display()))?,
            )
            .with_context(|| "Failed to create zstd decoder")?;
            let mut payload_tar = Vec::new();
            decoder
                .read_to_end(&mut payload_tar)
                .with_context(|| "Failed to decode payload.tar.zst")?;
            let mut payload_archive = tar::Archive::new(Cursor::new(payload_tar));
            payload_archive
                .unpack(&tmp_dir)
                .with_context(|| format!("Failed to expand payload into {}", tmp_dir.display()))?;
            fs::remove_file(&payload_zst_path).ok();
        }

        match fs::rename(&tmp_dir, runtime_dir) {
            Ok(()) => Ok(()),
            Err(_err) if runtime_dir.exists() => {
                fs::remove_dir_all(&tmp_dir).ok();
                Ok(())
            }
            Err(err) => Err(err).with_context(|| {
                format!(
                    "Failed to atomically move runtime {} -> {}",
                    tmp_dir.display(),
                    runtime_dir.display()
                )
            }),
        }
    })();

    if result.is_err() {
        fs::remove_dir_all(&tmp_dir).ok();
    }
    result
}

fn switch_current_symlink(base_dir: &Path, runtime_dir: &Path) -> Result<()> {
    let current_path = base_dir.join(CURRENT_SYMLINK);
    if let Ok(meta) = fs::symlink_metadata(&current_path) {
        if meta.file_type().is_dir() && !meta.file_type().is_symlink() {
            bail!(
                "Refusing to replace runtime current directory that is not a symlink: {}",
                current_path.display()
            );
        }
    }

    let tmp_link = base_dir.join(format!(".{}.tmp-{}", CURRENT_SYMLINK, random_suffix()));
    if tmp_link.exists() {
        fs::remove_file(&tmp_link).ok();
    }

    create_directory_symlink(runtime_dir, &tmp_link)?;
    let result = fs::rename(&tmp_link, &current_path).with_context(|| {
        format!(
            "Failed to atomically switch runtime symlink {} -> {}",
            tmp_link.display(),
            current_path.display()
        )
    });
    if result.is_err() {
        fs::remove_file(&tmp_link).ok();
    }
    result
}

#[cfg(unix)]
fn create_directory_symlink(target: &Path, link: &Path) -> Result<()> {
    unix_fs::symlink(target, link).with_context(|| {
        format!(
            "Failed to create symlink {} -> {}",
            link.display(),
            target.display()
        )
    })
}

#[cfg(windows)]
fn create_directory_symlink(target: &Path, link: &Path) -> Result<()> {
    windows_fs::symlink_dir(target, link).with_context(|| {
        format!(
            "Failed to create symlink {} -> {}",
            link.display(),
            target.display()
        )
    })
}

#[cfg(not(any(unix, windows)))]
fn create_directory_symlink(_target: &Path, _link: &Path) -> Result<()> {
    bail!("Runtime symlink switching is not supported on this platform")
}

fn extract_capsule_entry(capsule_bytes: &[u8], expected_path: &str) -> Result<String> {
    let mut archive = tar::Archive::new(Cursor::new(capsule_bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;
    for entry in entries {
        let mut entry = entry.context("Invalid .capsule archive entry")?;
        let path = entry.path().context("Failed to read archive entry path")?;
        if path.to_string_lossy() == expected_path {
            let mut raw = Vec::new();
            entry
                .read_to_end(&mut raw)
                .with_context(|| format!("Failed to read {}", expected_path))?;
            return String::from_utf8(raw)
                .with_context(|| format!("{} must be valid UTF-8", expected_path));
        }
    }
    bail!("{} not found in .capsule archive", expected_path)
}

fn random_suffix() -> String {
    let mut nonce = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut nonce);
    hex::encode(nonce)
}

#[cfg(test)]
mod tests {
    use super::{prepare_runtime_tree_at, prepare_store_runtime_for_capsule_at, random_suffix};
    use std::fs;
    use std::io::Cursor;

    fn build_test_capsule(version: &str, payload_name: &str, payload_body: &[u8]) -> Vec<u8> {
        let manifest = format!(
            "schema_version = \"1\"\nname = \"sample\"\nversion = \"{}\"\ntype = \"app\"\ndefault_target = \"cli\"\n[distribution]\nmanifest_hash = \"blake3:placeholder\"\nmerkle_root = \"blake3:placeholder\"\nchunk_list = []\nsignatures = []\n",
            version
        );
        let mut payload = Vec::new();
        {
            let mut payload_builder = tar::Builder::new(&mut payload);
            let mut header = tar::Header::new_gnu();
            header.set_size(payload_body.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            payload_builder
                .append_data(&mut header, payload_name, Cursor::new(payload_body))
                .expect("payload append");
            payload_builder.finish().expect("payload finish");
        }
        let payload_zst =
            zstd::stream::encode_all(Cursor::new(payload), 1).expect("payload encode");

        let mut capsule = Vec::new();
        {
            let mut capsule_builder = tar::Builder::new(&mut capsule);

            let mut manifest_header = tar::Header::new_gnu();
            manifest_header.set_size(manifest.len() as u64);
            manifest_header.set_mode(0o644);
            manifest_header.set_mtime(0);
            manifest_header.set_cksum();
            capsule_builder
                .append_data(&mut manifest_header, "capsule.toml", Cursor::new(manifest))
                .expect("manifest append");

            let mut payload_header = tar::Header::new_gnu();
            payload_header.set_size(payload_zst.len() as u64);
            payload_header.set_mode(0o644);
            payload_header.set_mtime(0);
            payload_header.set_cksum();
            capsule_builder
                .append_data(
                    &mut payload_header,
                    "payload.tar.zst",
                    Cursor::new(payload_zst),
                )
                .expect("payload zst append");
            capsule_builder.finish().expect("capsule finish");
        }
        capsule
    }

    #[test]
    fn prepare_runtime_tree_extracts_and_switches_current_symlink() {
        let root = tempfile::tempdir().expect("tempdir");

        let first = build_test_capsule("1.0.0", "source/main.txt", b"alpha");
        let second = build_test_capsule("1.1.0", "source/main.txt", b"beta");

        let first_manifest =
            prepare_runtime_tree_at(root.path(), "koh0920", "sample", "1.0.0", &first)
                .expect("prepare first");
        let second_manifest =
            prepare_runtime_tree_at(root.path(), "koh0920", "sample", "1.1.0", &second)
                .expect("prepare second");

        assert!(first_manifest.exists());
        assert!(second_manifest.exists());

        let current = root.path().join("koh0920").join("sample").join("current");
        let target = fs::read_link(&current).expect("current symlink");
        let file_name = target
            .file_name()
            .and_then(|value| value.to_str())
            .expect("target name");
        assert!(file_name.starts_with("1.1.0_"));
        let resolved = current
            .parent()
            .expect("parent")
            .join(target)
            .join("source")
            .join("main.txt");
        assert_eq!(fs::read(resolved).expect("payload"), b"beta");
    }

    #[test]
    fn random_suffix_produces_hex() {
        let value = random_suffix();
        assert_eq!(value.len(), 16);
        assert!(value.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn prepare_store_runtime_prefers_promoted_namespace_when_promotion_exists() {
        let store_root = tempfile::tempdir().expect("store");
        let runtime_root = tempfile::tempdir().expect("runtime");

        let version = "1.0.0";
        let capsule = build_test_capsule(version, "source/main.txt", b"promoted");
        let artifact_hash = format!("blake3:{}", blake3::hash(&capsule).to_hex());

        let install_dir = store_root
            .path()
            .join("koh0920")
            .join("sample")
            .join(version);
        fs::create_dir_all(&install_dir).expect("install dir");
        let capsule_path = install_dir.join("sample-1.0.0.capsule");
        fs::write(&capsule_path, &capsule).expect("write capsule");
        fs::write(
            install_dir.join("promotion.json"),
            format!(
                "{{\n  \"performed\": true,\n  \"content_hash\": \"{}\"\n}}",
                artifact_hash
            ),
        )
        .expect("write promotion");

        let manifest_path = prepare_store_runtime_for_capsule_at(
            &capsule_path,
            store_root.path(),
            runtime_root.path(),
        )
        .expect("prepare")
        .expect("manifest path");

        assert!(manifest_path.starts_with(runtime_root.path().join("promoted")));
        let current = runtime_root
            .path()
            .join("promoted")
            .join("koh0920")
            .join("sample")
            .join("current");
        let target = fs::read_link(&current).expect("current symlink");
        let resolved = current
            .parent()
            .expect("parent")
            .join(target)
            .join("source")
            .join("main.txt");
        assert_eq!(fs::read(resolved).expect("payload"), b"promoted");
    }
}

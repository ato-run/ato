use anyhow::{bail, Context, Result};
use rand::RngCore;
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs as unix_fs;
#[cfg(windows)]
use std::os::windows::fs as windows_fs;

use capsule_core::packers::payload as manifest_payload;
use capsule_core::types::CapsuleManifest;

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
        if runtime_dir.exists() {
            fs::remove_dir_all(&runtime_dir).with_context(|| {
                format!(
                    "Failed to remove incomplete runtime directory: {}",
                    runtime_dir.display()
                )
            })?;
        }
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
    capsule_core::common::paths::runtime_cache_dir().map_err(Into::into)
}

fn store_root() -> Result<PathBuf> {
    Ok(capsule_core::common::paths::ato_store_dir())
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

        rewrite_runtime_manifest_for_desktop_delivery(&tmp_dir)?;

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

fn rewrite_runtime_manifest_for_desktop_delivery(runtime_dir: &Path) -> Result<()> {
    let lock_path = runtime_dir.join("capsule.lock.json");
    if !lock_path.is_file() {
        return Ok(());
    }

    let lock: Value = serde_json::from_slice(
        &fs::read(&lock_path).with_context(|| format!("Failed to read {}", lock_path.display()))?,
    )
    .with_context(|| format!("Failed to parse {}", lock_path.display()))?;

    let Some(artifact_path) = lock
        .get("contract")
        .and_then(|value| value.get("delivery"))
        .and_then(|value| value.get("artifact"))
        .and_then(Value::as_object)
        .filter(|artifact| artifact.get("kind").and_then(Value::as_str) == Some("desktop-native"))
        .and_then(|artifact| artifact.get("path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let entrypoint = resolve_desktop_delivery_entrypoint(runtime_dir, artifact_path);

    let metadata = lock.get("contract").and_then(|value| value.get("metadata"));
    let name = metadata
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("capsule");
    let version = metadata
        .and_then(|value| value.get("version"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("0.0.0");
    let default_target = lock
        .get("resolution")
        .and_then(|value| value.get("target_selection"))
        .and_then(|value| value.get("default_target"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            lock.get("resolution")
                .and_then(|value| value.get("runtime"))
                .and_then(|value| value.get("selected_target"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .or_else(|| {
            metadata
                .and_then(|value| value.get("default_target"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("desktop");

    let mut root = toml::map::Map::new();
    root.insert(
        "schema_version".to_string(),
        toml::Value::String("0.2".to_string()),
    );
    root.insert("name".to_string(), toml::Value::String(name.to_string()));
    root.insert(
        "version".to_string(),
        toml::Value::String(version.to_string()),
    );
    root.insert("type".to_string(), toml::Value::String("app".to_string()));
    root.insert(
        "default_target".to_string(),
        toml::Value::String(default_target.to_string()),
    );

    let mut target = toml::map::Map::new();
    target.insert(
        "runtime".to_string(),
        toml::Value::String("source".to_string()),
    );
    target.insert(
        "driver".to_string(),
        toml::Value::String("native".to_string()),
    );
    target.insert("entrypoint".to_string(), toml::Value::String(entrypoint));

    let mut targets = toml::map::Map::new();
    targets.insert(default_target.to_string(), toml::Value::Table(target));
    root.insert("targets".to_string(), toml::Value::Table(targets));

    let manifest_path = runtime_dir.join("capsule.toml");
    fs::write(&manifest_path, toml::to_string(&toml::Value::Table(root))?)
        .with_context(|| format!("Failed to rewrite {}", manifest_path.display()))?;

    Ok(())
}

fn random_suffix() -> String {
    let mut nonce = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut nonce);
    hex::encode(nonce)
}

fn resolve_desktop_delivery_entrypoint(runtime_dir: &Path, artifact_path: &str) -> String {
    let artifact_relative = Path::new(artifact_path.trim());
    let resolved = if artifact_relative
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
    {
        resolve_macos_bundle_binary(runtime_dir, artifact_relative)
            .unwrap_or_else(|| artifact_relative.to_path_buf())
    } else {
        artifact_relative.to_path_buf()
    };

    if resolved.is_absolute() {
        return resolved.to_string_lossy().into_owned();
    }

    let value = resolved.to_string_lossy();
    if value.starts_with("./") || value.starts_with("../") {
        value.into_owned()
    } else {
        format!("./{}", value)
    }
}

fn resolve_macos_bundle_binary(runtime_dir: &Path, artifact_relative: &Path) -> Option<PathBuf> {
    let bundle_dir = runtime_dir.join(artifact_relative);
    let macos_dir = bundle_dir.join("Contents").join("MacOS");
    if !macos_dir.is_dir() {
        return None;
    }

    let preferred_name = artifact_relative.file_stem()?;
    let preferred = macos_dir.join(preferred_name);
    if preferred.is_file() {
        return preferred.strip_prefix(runtime_dir).ok().map(PathBuf::from);
    }

    let mut entries = fs::read_dir(&macos_dir)
        .ok()?
        .filter_map(|entry| entry.ok());
    let candidate = entries.find_map(|entry| {
        let path = entry.path();
        path.is_file().then_some(path)
    })?;
    candidate.strip_prefix(runtime_dir).ok().map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::{prepare_runtime_tree_at, prepare_store_runtime_for_capsule_at, random_suffix};
    use std::fs;
    use std::io::Cursor;
    use std::path::Path;

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

    fn build_desktop_native_test_capsule(version: &str, artifact_path: &str) -> Vec<u8> {
        let manifest = format!(
            "schema_version = \"0.2\"\nname = \"sample\"\nversion = \"{}\"\ntype = \"app\"\ndefault_target = \"desktop\"\n[targets.desktop]\nruntime = \"source\"\ndriver = \"native\"\nentrypoint = \"npm\"\ncmd = [\"run\", \"dev\"]\n[distribution]\nmanifest_hash = \"blake3:placeholder\"\nmerkle_root = \"blake3:placeholder\"\nchunk_list = []\nsignatures = []\n",
            version
        );
        let lock = format!(
            "{{\n  \"schema_version\": 1,\n  \"resolution\": {{\n    \"runtime\": {{\"kind\": \"native\", \"selected_target\": \"desktop\"}},\n    \"target_selection\": {{\"default_target\": \"desktop\"}},\n    \"closure\": {{\"kind\": \"build_closure\", \"status\": \"complete\"}}\n  }},\n  \"contract\": {{\n    \"metadata\": {{\"name\": \"sample\", \"version\": \"{}\", \"default_target\": \"desktop\"}},\n    \"delivery\": {{\n      \"mode\": \"source-derivation\",\n      \"artifact\": {{\"kind\": \"desktop-native\", \"path\": \"{}\"}}\n    }}\n  }}\n}}",
            version, artifact_path
        );

        let payload_entry = if artifact_path.ends_with(".app") {
            let bundle_name = Path::new(artifact_path)
                .file_stem()
                .and_then(|value| value.to_str())
                .expect("bundle name");
            format!("{artifact_path}/Contents/MacOS/{bundle_name}")
        } else {
            artifact_path.to_string()
        };

        let mut payload = Vec::new();
        {
            let mut payload_builder = tar::Builder::new(&mut payload);
            let mut header = tar::Header::new_gnu();
            header.set_size(b"binary".len() as u64);
            header.set_mode(0o755);
            header.set_mtime(0);
            header.set_cksum();
            payload_builder
                .append_data(&mut header, payload_entry, Cursor::new(b"binary"))
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

            let mut lock_header = tar::Header::new_gnu();
            lock_header.set_size(lock.len() as u64);
            lock_header.set_mode(0o644);
            lock_header.set_mtime(0);
            lock_header.set_cksum();
            capsule_builder
                .append_data(&mut lock_header, "capsule.lock.json", Cursor::new(lock))
                .expect("lock append");

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
    fn prepare_runtime_tree_rewrites_desktop_native_manifest_from_lock() {
        let root = tempfile::tempdir().expect("tempdir");
        let capsule = build_desktop_native_test_capsule("1.0.0", "MyApp.app");

        let manifest_path =
            prepare_runtime_tree_at(root.path(), "koh0920", "sample", "1.0.0", &capsule)
                .expect("prepare runtime tree");
        let manifest = fs::read_to_string(&manifest_path).expect("read manifest");

        assert!(manifest.contains("default_target = \"desktop\""));
        assert!(manifest.contains("runtime = \"source\""));
        assert!(manifest.contains("driver = \"native\""));
        assert!(manifest.contains("entrypoint = \"./MyApp.app/Contents/MacOS/MyApp\""));
        assert!(!manifest.contains("cmd = [\"run\", \"dev\"]"));
    }

    #[test]
    fn prepare_runtime_tree_recovers_incomplete_runtime_directory() {
        let root = tempfile::tempdir().expect("tempdir");
        let capsule = build_desktop_native_test_capsule("1.0.0", "MyApp.app");

        let manifest_path =
            prepare_runtime_tree_at(root.path(), "koh0920", "sample", "1.0.0", &capsule)
                .expect("prepare runtime tree");
        fs::remove_file(&manifest_path).expect("remove manifest");

        let recovered_manifest =
            prepare_runtime_tree_at(root.path(), "koh0920", "sample", "1.0.0", &capsule)
                .expect("recover runtime tree");
        let manifest = fs::read_to_string(&recovered_manifest).expect("read manifest");

        assert!(recovered_manifest.exists());
        assert!(manifest.contains("runtime = \"source\""));
        assert!(manifest.contains("driver = \"native\""));
        assert!(manifest.contains("entrypoint = \"./MyApp.app/Contents/MacOS/MyApp\""));
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

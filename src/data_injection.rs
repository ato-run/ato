use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::lockfile::{
    parse_lockfile_text, resolve_existing_lockfile_path, verify_lockfile_manifest,
    LockedInjectedData,
};
use capsule_core::router::ManifestData;
use capsule_core::types::ExternalInjectionSpec;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::executors::launch_context::InjectedMount;
use crate::local_input;

const ENV_INJECTED_DATA_CACHE_DIR: &str = "ATO_INJECTED_DATA_CACHE_DIR";

#[derive(Debug, Clone, Default)]
pub struct ResolvedDataInjection {
    pub env: HashMap<String, String>,
    pub mounts: Vec<InjectedMount>,
}

#[derive(Debug, Clone)]
struct MaterializedInjection {
    env_value: String,
    mount: Option<InjectedMount>,
    locked: LockedInjectedData,
}

pub async fn resolve_and_record(
    plan: &ManifestData,
    raw_bindings: &[String],
) -> Result<ResolvedDataInjection> {
    let contract = plan.selected_target_external_injection();
    if contract.is_empty() {
        if raw_bindings.is_empty() {
            return Ok(ResolvedDataInjection::default());
        }
        anyhow::bail!(
            "target '{}' does not declare [external_injection], but --inject was provided",
            plan.selected_target_label()
        );
    }

    let bindings = parse_cli_bindings(raw_bindings)?;
    for key in bindings.keys() {
        if !contract.contains_key(key) {
            anyhow::bail!(
                "target '{}' does not declare external_injection.{}",
                plan.selected_target_label(),
                key
            );
        }
    }

    let mut env = HashMap::new();
    let mut mounts = Vec::new();
    let mut locked = HashMap::new();
    for (key, spec) in &contract {
        let source = bindings.get(key).cloned().or_else(|| spec.default.clone());
        let Some(source) = source else {
            if spec.required {
                anyhow::bail!(
                    "target '{}' requires injection for {}",
                    plan.selected_target_label(),
                    key
                );
            }
            continue;
        };

        let materialized = materialize_injection(plan, key, spec, &source)
            .await
            .with_context(|| {
                format!(
                    "failed to resolve external injection {} for target '{}'",
                    key,
                    plan.selected_target_label()
                )
            })?;
        env.insert(key.clone(), materialized.env_value);
        if let Some(mount) = materialized.mount {
            mounts.push(mount);
        }
        locked.insert(key.clone(), materialized.locked);
    }

    persist_lockfile_injected_data(&plan.manifest_path, &locked)?;
    Ok(ResolvedDataInjection { env, mounts })
}

fn parse_cli_bindings(raw_bindings: &[String]) -> Result<BTreeMap<String, String>> {
    let mut bindings = BTreeMap::new();
    for raw_binding in raw_bindings {
        let (key, value) = raw_binding.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("--inject must use KEY=VALUE syntax, got '{}'", raw_binding)
        })?;
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            anyhow::bail!(
                "--inject must use non-empty KEY=VALUE syntax, got '{}'",
                raw_binding
            );
        }
        if bindings
            .insert(key.to_string(), value.to_string())
            .is_some()
        {
            anyhow::bail!("duplicate --inject key '{}'", key);
        }
    }
    Ok(bindings)
}

async fn materialize_injection(
    plan: &ManifestData,
    key: &str,
    spec: &ExternalInjectionSpec,
    source: &str,
) -> Result<MaterializedInjection> {
    match spec.injection_type.as_str() {
        "string" => Ok(materialize_string_injection(source)),
        "file" => materialize_file_injection(plan, key, source).await,
        "directory" => materialize_directory_injection(plan, key, source).await,
        other => anyhow::bail!("unsupported external injection type '{}'", other),
    }
}

fn materialize_string_injection(source: &str) -> MaterializedInjection {
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    let digest = format!("sha256:{}", hex::encode(hasher.finalize()));
    MaterializedInjection {
        env_value: source.to_string(),
        mount: None,
        locked: LockedInjectedData {
            source: source.to_string(),
            digest,
            bytes: source.len() as u64,
        },
    }
}

async fn materialize_file_injection(
    plan: &ManifestData,
    key: &str,
    source: &str,
) -> Result<MaterializedInjection> {
    let base_dir = &plan.manifest_dir;
    if is_http_source(source) {
        let downloaded = download_http_source(source).await?;
        let digest = sha256_bytes(&downloaded.bytes);
        let file_name = downloaded
            .file_name
            .clone()
            .unwrap_or_else(|| "payload.bin".to_string());
        let target_path = injected_cache_root()?
            .join("files")
            .join(digest.strip_prefix("sha256:").unwrap_or(&digest))
            .join(file_name);
        if !target_path.exists() {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target_path, &downloaded.bytes)?;
            set_read_only_recursive(&target_path)?;
        }
        let (env_value, mount) = external_injection_path(plan, key, &target_path);
        return Ok(MaterializedInjection {
            env_value,
            mount,
            locked: LockedInjectedData {
                source: source.to_string(),
                digest,
                bytes: downloaded.bytes.len() as u64,
            },
        });
    }

    let local_path = resolve_local_source(base_dir, source)?;
    if !local_path.is_file() {
        anyhow::bail!("'{}' does not resolve to a file", source);
    }
    let bytes = fs::read(&local_path)
        .with_context(|| format!("failed to read {}", local_path.display()))?;
    let digest = sha256_bytes(&bytes);
    let file_name = local_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("payload.bin");
    let target_path = injected_cache_root()?
        .join("files")
        .join(digest.strip_prefix("sha256:").unwrap_or(&digest))
        .join(file_name);
    if !target_path.exists() {
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target_path, &bytes)?;
        set_read_only_recursive(&target_path)?;
    }
    let (env_value, mount) = external_injection_path(plan, key, &target_path);
    Ok(MaterializedInjection {
        env_value,
        mount,
        locked: LockedInjectedData {
            source: source.to_string(),
            digest,
            bytes: bytes.len() as u64,
        },
    })
}

async fn materialize_directory_injection(
    plan: &ManifestData,
    key: &str,
    source: &str,
) -> Result<MaterializedInjection> {
    let base_dir = &plan.manifest_dir;
    if is_http_source(source) {
        let downloaded = download_http_source(source).await?;
        let digest = sha256_bytes(&downloaded.bytes);
        let dir_path = injected_cache_root()?
            .join("dirs")
            .join(digest.strip_prefix("sha256:").unwrap_or(&digest));
        if !dir_path.exists() {
            fs::create_dir_all(&dir_path)?;
            let archive_name = downloaded
                .file_name
                .unwrap_or_else(|| "payload.tar".to_string());
            let archive_path = dir_path.join(&archive_name);
            fs::write(&archive_path, &downloaded.bytes)?;
            extract_archive_if_needed(&archive_path, &dir_path)?;
            let _ = fs::remove_file(&archive_path);
            set_read_only_recursive(&dir_path)?;
        }
        let (env_value, mount) = external_injection_path(plan, key, &dir_path);
        return Ok(MaterializedInjection {
            env_value,
            mount,
            locked: LockedInjectedData {
                source: source.to_string(),
                digest,
                bytes: downloaded.bytes.len() as u64,
            },
        });
    }

    let local_path = resolve_local_source(base_dir, source)?;
    if local_path.is_dir() {
        let (digest, bytes) = sha256_dir(&local_path)?;
        let dir_path = injected_cache_root()?
            .join("dirs")
            .join(digest.strip_prefix("sha256:").unwrap_or(&digest));
        if !dir_path.exists() {
            crate::fs_copy::copy_path_recursive(&local_path, &dir_path)?;
            set_read_only_recursive(&dir_path)?;
        }
        let (env_value, mount) = external_injection_path(plan, key, &dir_path);
        return Ok(MaterializedInjection {
            env_value,
            mount,
            locked: LockedInjectedData {
                source: source.to_string(),
                digest,
                bytes,
            },
        });
    }
    if !local_path.is_file() {
        anyhow::bail!("'{}' does not resolve to a directory or archive", source);
    }

    let bytes = fs::read(&local_path)
        .with_context(|| format!("failed to read {}", local_path.display()))?;
    let digest = sha256_bytes(&bytes);
    let dir_path = injected_cache_root()?
        .join("dirs")
        .join(digest.strip_prefix("sha256:").unwrap_or(&digest));
    if !dir_path.exists() {
        fs::create_dir_all(&dir_path)?;
        let archive_name = local_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("payload.tar");
        let archive_path = dir_path.join(archive_name);
        fs::write(&archive_path, &bytes)?;
        extract_archive_if_needed(&archive_path, &dir_path)?;
        let _ = fs::remove_file(&archive_path);
        set_read_only_recursive(&dir_path)?;
    }
    let (env_value, mount) = external_injection_path(plan, key, &dir_path);
    Ok(MaterializedInjection {
        env_value,
        mount,
        locked: LockedInjectedData {
            source: source.to_string(),
            digest,
            bytes: bytes.len() as u64,
        },
    })
}

fn external_injection_path(
    plan: &ManifestData,
    key: &str,
    resolved_host_path: &Path,
) -> (String, Option<InjectedMount>) {
    if plan
        .execution_runtime()
        .map(|runtime| runtime.eq_ignore_ascii_case("oci"))
        .unwrap_or(false)
    {
        let target = format!("/var/run/ato/injected/{}", key);
        return (
            target.clone(),
            Some(InjectedMount {
                source: resolved_host_path.to_path_buf(),
                target,
                readonly: true,
            }),
        );
    }

    (resolved_host_path.to_string_lossy().to_string(), None)
}

fn persist_lockfile_injected_data(
    manifest_path: &Path,
    injected_data: &HashMap<String, LockedInjectedData>,
) -> Result<()> {
    if injected_data.is_empty() {
        return Ok(());
    }

    let Some(lockfile_path) = manifest_path
        .parent()
        .and_then(resolve_existing_lockfile_path)
    else {
        return Ok(());
    };

    verify_lockfile_manifest(manifest_path, &lockfile_path)?;
    let raw = fs::read_to_string(&lockfile_path)?;
    let mut lockfile = parse_lockfile_text(&raw, &lockfile_path)?;
    let mut changed = false;
    for (key, value) in injected_data {
        match lockfile.injected_data.get(key) {
            Some(existing) if existing == value => {}
            Some(existing) => {
                anyhow::bail!(
                    "{} injected_data.{} does not match runtime-resolved data (expected {} / {}, got {} / {})",
                    lockfile_path.display(),
                    key,
                    existing.source,
                    existing.digest,
                    value.source,
                    value.digest
                );
            }
            None => {
                lockfile.injected_data.insert(key.clone(), value.clone());
                changed = true;
            }
        }
    }

    if changed {
        let bytes = serde_json::to_vec_pretty(&lockfile)?;
        fs::write(&lockfile_path, bytes)?;
    }

    Ok(())
}

fn resolve_local_source(base_dir: &Path, source: &str) -> Result<PathBuf> {
    let local = source.strip_prefix("file://").unwrap_or(source);
    let expanded = local_input::expand_local_path(local);
    let resolved = if expanded.is_absolute() {
        expanded
    } else {
        base_dir.join(expanded)
    };
    if !resolved.exists() {
        anyhow::bail!("'{}' does not exist", resolved.display());
    }
    Ok(resolved)
}

fn is_http_source(source: &str) -> bool {
    source.starts_with("https://") || source.starts_with("http://")
}

fn injected_cache_root() -> Result<PathBuf> {
    if let Ok(path) = std::env::var(ENV_INJECTED_DATA_CACHE_DIR) {
        let path = PathBuf::from(path);
        fs::create_dir_all(&path)?;
        return Ok(path);
    }
    let home = dirs::home_dir().context("failed to determine home directory")?;
    let path = home.join(".ato").join("injected-data");
    fs::create_dir_all(&path)?;
    Ok(path)
}

struct DownloadedSource {
    bytes: Vec<u8>,
    file_name: Option<String>,
}

async fn download_http_source(source: &str) -> Result<DownloadedSource> {
    let response = reqwest::Client::new()
        .get(source)
        .send()
        .await
        .with_context(|| format!("failed to download {}", source))?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("download {} returned {}", source, status);
    }
    let file_name = source
        .split('?')
        .next()
        .and_then(|value| value.rsplit('/').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let bytes = response.bytes().await?.to_vec();
    Ok(DownloadedSource { bytes, file_name })
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn sha256_dir(root: &Path) -> Result<(String, u64)> {
    let mut entries = Vec::new();
    for entry in WalkDir::new(root).into_iter().flatten() {
        if entry.file_type().is_file() {
            entries.push(entry.path().to_path_buf());
        }
    }
    entries.sort();

    let mut total_bytes = 0u64;
    let mut hasher = Sha256::new();
    for path in entries {
        let rel = path.strip_prefix(root).unwrap_or(&path);
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update([0]);
        let bytes = fs::read(&path)?;
        total_bytes += bytes.len() as u64;
        hasher.update(&bytes);
    }
    Ok((
        format!("sha256:{}", hex::encode(hasher.finalize())),
        total_bytes,
    ))
}

fn extract_archive_if_needed(archive_path: &Path, dest: &Path) -> Result<()> {
    let name = archive_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".zip") {
        let file = fs::File::open(archive_path)?;
        let mut zip = zip::ZipArchive::new(file)?;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index)?;
            let Some(safe_name) = entry.enclosed_name().map(|value| value.to_path_buf()) else {
                continue;
            };
            let out_path = dest.join(safe_name);
            if entry.name().ends_with('/') {
                fs::create_dir_all(&out_path)?;
                continue;
            }
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out)?;
        }
        return Ok(());
    }
    if name.ends_with(".tar") || name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let file = fs::File::open(archive_path)?;
        if name.ends_with(".tar") {
            let mut archive = tar::Archive::new(file);
            archive.unpack(dest)?;
        } else {
            let decoder = flate2::read::GzDecoder::new(file);
            let mut archive = tar::Archive::new(decoder);
            archive.unpack(dest)?;
        }
        return Ok(());
    }
    anyhow::bail!(
        "unsupported directory injection archive: {}",
        archive_path.display()
    )
}

fn set_read_only_recursive(path: &Path) -> Result<()> {
    if path.is_dir() {
        for entry in WalkDir::new(path).into_iter().flatten() {
            let metadata = entry.metadata()?;
            let mut permissions = metadata.permissions();
            permissions.set_readonly(true);
            fs::set_permissions(entry.path(), permissions)?;
        }
    } else if path.exists() {
        let metadata = fs::metadata(path)?;
        let mut permissions = metadata.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_cache_dir(test_name: &str) -> (PathBuf, String) {
        let base = std::env::current_dir()
            .unwrap()
            .join(".tmp")
            .join(test_name);
        if base.exists() {
            let _ = fs::remove_dir_all(&base);
        }
        fs::create_dir_all(&base).unwrap();
        (base.clone(), base.to_string_lossy().to_string())
    }

    #[tokio::test]
    async fn resolves_string_injection_from_cli_binding() {
        let mut target = toml::map::Map::new();
        target.insert(
            "runtime".to_string(),
            toml::Value::String("source".to_string()),
        );
        target.insert(
            "driver".to_string(),
            toml::Value::String("native".to_string()),
        );
        target.insert(
            "entrypoint".to_string(),
            toml::Value::String("main.py".to_string()),
        );
        let mut external_injection = toml::map::Map::new();
        external_injection.insert(
            "API_KEY".to_string(),
            toml::Value::Table(toml::map::Map::from_iter([(
                "type".to_string(),
                toml::Value::String("string".to_string()),
            )])),
        );
        target.insert(
            "external_injection".to_string(),
            toml::Value::Table(external_injection),
        );
        let manifest = toml::Value::Table(toml::map::Map::from_iter([
            ("name".to_string(), toml::Value::String("demo".to_string())),
            (
                "default_target".to_string(),
                toml::Value::String("default".to_string()),
            ),
            (
                "targets".to_string(),
                toml::Value::Table(toml::map::Map::from_iter([(
                    "default".to_string(),
                    toml::Value::Table(target),
                )])),
            ),
        ]));
        let plan = ManifestData {
            manifest,
            manifest_path: PathBuf::from("capsule.toml"),
            manifest_dir: PathBuf::from("."),
            profile: capsule_core::router::ExecutionProfile::Dev,
            selected_target: "default".to_string(),
            state_source_overrides: HashMap::new(),
        };

        let resolved = resolve_and_record(&plan, &["API_KEY=test-token".to_string()])
            .await
            .expect("resolve injection");
        assert_eq!(resolved.env["API_KEY"], "test-token");
    }

    #[tokio::test]
    async fn resolves_directory_injection_from_file_uri() {
        let (cache_dir, cache_dir_string) = with_cache_dir("data-injection-dir");
        std::env::set_var(ENV_INJECTED_DATA_CACHE_DIR, &cache_dir_string);
        let fixture_root = cache_dir.join("fixture");
        fs::create_dir_all(&fixture_root).unwrap();
        fs::write(fixture_root.join("weights.bin"), b"abc").unwrap();

        let mut target = toml::map::Map::new();
        target.insert(
            "runtime".to_string(),
            toml::Value::String("source".to_string()),
        );
        target.insert(
            "driver".to_string(),
            toml::Value::String("native".to_string()),
        );
        target.insert(
            "entrypoint".to_string(),
            toml::Value::String("main.py".to_string()),
        );
        let mut external_injection = toml::map::Map::new();
        external_injection.insert(
            "MODEL_DIR".to_string(),
            toml::Value::Table(toml::map::Map::from_iter([(
                "type".to_string(),
                toml::Value::String("directory".to_string()),
            )])),
        );
        target.insert(
            "external_injection".to_string(),
            toml::Value::Table(external_injection),
        );
        let manifest = toml::Value::Table(toml::map::Map::from_iter([
            ("name".to_string(), toml::Value::String("demo".to_string())),
            (
                "default_target".to_string(),
                toml::Value::String("default".to_string()),
            ),
            (
                "targets".to_string(),
                toml::Value::Table(toml::map::Map::from_iter([(
                    "default".to_string(),
                    toml::Value::Table(target),
                )])),
            ),
        ]));
        let plan = ManifestData {
            manifest,
            manifest_path: cache_dir.join("capsule.toml"),
            manifest_dir: cache_dir.clone(),
            profile: capsule_core::router::ExecutionProfile::Dev,
            selected_target: "default".to_string(),
            state_source_overrides: HashMap::new(),
        };

        let resolved = resolve_and_record(
            &plan,
            &[format!("MODEL_DIR=file://{}", fixture_root.display())],
        )
        .await
        .expect("resolve injection");

        let injected_path = PathBuf::from(&resolved.env["MODEL_DIR"]);
        assert!(injected_path.exists());
        assert!(injected_path.join("weights.bin").exists());
        std::env::remove_var(ENV_INJECTED_DATA_CACHE_DIR);
    }

    #[tokio::test]
    async fn resolves_oci_file_injection_as_mount() {
        let (cache_dir, cache_dir_string) = with_cache_dir("data-injection-oci-file");
        std::env::set_var(ENV_INJECTED_DATA_CACHE_DIR, &cache_dir_string);
        let fixture = cache_dir.join("config.json");
        fs::write(&fixture, b"{}\n").unwrap();

        let mut target = toml::map::Map::new();
        target.insert(
            "runtime".to_string(),
            toml::Value::String("oci".to_string()),
        );
        target.insert(
            "image".to_string(),
            toml::Value::String("ghcr.io/example/demo:latest".to_string()),
        );
        target.insert(
            "external_injection".to_string(),
            toml::Value::Table(toml::map::Map::from_iter([(
                "CONFIG_FILE".to_string(),
                toml::Value::Table(toml::map::Map::from_iter([(
                    "type".to_string(),
                    toml::Value::String("file".to_string()),
                )])),
            )])),
        );
        let manifest = toml::Value::Table(toml::map::Map::from_iter([
            ("name".to_string(), toml::Value::String("demo".to_string())),
            (
                "default_target".to_string(),
                toml::Value::String("default".to_string()),
            ),
            (
                "targets".to_string(),
                toml::Value::Table(toml::map::Map::from_iter([(
                    "default".to_string(),
                    toml::Value::Table(target),
                )])),
            ),
        ]));
        let plan = ManifestData {
            manifest,
            manifest_path: cache_dir.join("capsule.toml"),
            manifest_dir: cache_dir.clone(),
            profile: capsule_core::router::ExecutionProfile::Dev,
            selected_target: "default".to_string(),
            state_source_overrides: HashMap::new(),
        };

        let resolved = resolve_and_record(
            &plan,
            &[format!("CONFIG_FILE=file://{}", fixture.display())],
        )
        .await
        .expect("resolve injection");

        assert_eq!(
            resolved.env["CONFIG_FILE"],
            "/var/run/ato/injected/CONFIG_FILE"
        );
        assert_eq!(resolved.mounts.len(), 1);
        assert_eq!(
            resolved.mounts[0].target,
            "/var/run/ato/injected/CONFIG_FILE"
        );
        assert!(resolved.mounts[0].readonly);
        std::env::remove_var(ENV_INJECTED_DATA_CACHE_DIR);
    }
}

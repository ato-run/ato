use std::path::Path;

use anyhow::{Context, Result};
use capsule_core::lockfile::LockedInjectedData;
use capsule_core::router::ManifestData;
use capsule_core::types::ExternalInjectionSpec;
use sha2::{Digest, Sha256};

use crate::executors::launch_context::InjectedMount;

use super::archive::{extract_archive_if_needed, set_read_only_recursive};
use super::cache::{
    download_http_source, injected_cache_root, is_http_source, resolve_local_source, sha256_bytes,
    sha256_dir,
};

#[derive(Debug, Clone)]
pub(super) struct MaterializedInjection {
    pub env_value: String,
    pub mount: Option<InjectedMount>,
    pub locked: LockedInjectedData,
}

pub(super) async fn materialize_injection(
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
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target_path, &downloaded.bytes)?;
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
    let bytes = std::fs::read(&local_path)
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
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target_path, &bytes)?;
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
            std::fs::create_dir_all(&dir_path)?;
            let archive_name = downloaded
                .file_name
                .unwrap_or_else(|| "payload.tar".to_string());
            let archive_path = dir_path.join(&archive_name);
            std::fs::write(&archive_path, &downloaded.bytes)?;
            extract_archive_if_needed(&archive_path, &dir_path)?;
            let _ = std::fs::remove_file(&archive_path);
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

    let bytes = std::fs::read(&local_path)
        .with_context(|| format!("failed to read {}", local_path.display()))?;
    let digest = sha256_bytes(&bytes);
    let dir_path = injected_cache_root()?
        .join("dirs")
        .join(digest.strip_prefix("sha256:").unwrap_or(&digest));
    if !dir_path.exists() {
        std::fs::create_dir_all(&dir_path)?;
        let archive_name = local_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("payload.tar");
        let archive_path = dir_path.join(archive_name);
        std::fs::write(&archive_path, &bytes)?;
        extract_archive_if_needed(&archive_path, &dir_path)?;
        let _ = std::fs::remove_file(&archive_path);
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

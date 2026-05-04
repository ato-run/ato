use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use capsule_core::lockfile::LockedCapsuleDependency;
use sha2::{Digest, Sha256};

use crate::runtime::tree as runtime_tree;

use super::EXTERNAL_CAPSULE_CACHE_DIR_ENV;

pub(crate) async fn ensure_runtime_tree_for_dependency(
    locked: &LockedCapsuleDependency,
) -> Result<PathBuf> {
    if locked.source_type != "store" {
        anyhow::bail!(
            "external capsule dependency '{}' uses unsupported source_type '{}'",
            locked.name,
            locked.source_type
        );
    }

    let artifact_url = locked.artifact_url.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "{} capsule dependency '{}' is missing artifact_url",
            capsule_core::lockfile::CAPSULE_LOCK_FILE_NAME,
            locked.name
        )
    })?;
    let cache_path = external_capsule_cache_path(locked)?;

    let bytes = if cache_path.exists() {
        let bytes = fs::read(&cache_path)
            .with_context(|| format!("failed to read {}", cache_path.display()))?;
        verify_artifact_bytes(locked, &bytes)?;
        bytes
    } else {
        let bytes = reqwest::Client::new()
            .get(artifact_url)
            .send()
            .await
            .with_context(|| format!("failed to download {}", artifact_url))?
            .error_for_status()
            .with_context(|| format!("failed to download {}", artifact_url))?
            .bytes()
            .await
            .with_context(|| format!("failed to read artifact body {}", artifact_url))?
            .to_vec();
        verify_artifact_bytes(locked, &bytes)?;
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&cache_path, &bytes)
            .with_context(|| format!("failed to write {}", cache_path.display()))?;
        bytes
    };

    let (publisher, slug) = parse_store_source_identity(&locked.source)?;
    let version = locked.resolved_version.as_deref().unwrap_or("resolved");
    runtime_tree::prepare_runtime_tree(&publisher, &slug, version, &bytes)
}

fn external_capsule_cache_path(locked: &LockedCapsuleDependency) -> Result<PathBuf> {
    let base = if let Ok(path) = std::env::var(EXTERNAL_CAPSULE_CACHE_DIR_ENV) {
        PathBuf::from(path)
    } else {
        dirs::home_dir()
            .context("failed to determine home directory")?
            .join(".ato")
            .join("external-capsules")
    };
    let key = locked
        .sha256
        .as_deref()
        .map(|value| value.trim_start_matches("sha256:"))
        .or_else(|| {
            locked
                .digest
                .as_deref()
                .map(|value| value.trim_start_matches("blake3:"))
        })
        .unwrap_or(locked.name.as_str());
    Ok(base.join(format!("{}.capsule", key)))
}

fn verify_artifact_bytes(locked: &LockedCapsuleDependency, bytes: &[u8]) -> Result<()> {
    if let Some(expected) = locked.sha256.as_deref() {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let actual = hex::encode(hasher.finalize());
        let expected = expected.trim_start_matches("sha256:");
        if actual != expected {
            anyhow::bail!(
                "artifact sha256 mismatch for '{}': expected {} got {}",
                locked.name,
                expected,
                actual
            );
        }
    }

    if let Some(expected) = locked.digest.as_deref() {
        if let Some(expected) = expected.strip_prefix("blake3:") {
            let actual = blake3::hash(bytes).to_hex().to_string();
            if actual != expected {
                anyhow::bail!(
                    "artifact blake3 mismatch for '{}': expected {} got {}",
                    locked.name,
                    expected,
                    actual
                );
            }
        }
    }

    Ok(())
}

fn parse_store_source_identity(source: &str) -> Result<(String, String)> {
    let raw = source.trim();
    let raw = raw
        .strip_prefix("capsule://store/")
        .or_else(|| raw.strip_prefix("capsule://ato.run/"))
        .ok_or_else(|| anyhow::anyhow!("unsupported store source '{}'", source))?;
    let raw = raw.split_once('?').map(|(path, _)| path).unwrap_or(raw);
    let raw = raw.split_once('@').map(|(path, _)| path).unwrap_or(raw);
    let mut segments = raw.split('/').filter(|segment| !segment.trim().is_empty());
    let publisher = segments
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid store source '{}'", source))?;
    let slug = segments
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid store source '{}'", source))?;
    if segments.next().is_some() {
        anyhow::bail!("invalid store source '{}'", source);
    }
    Ok((publisher.to_string(), slug.to_string()))
}

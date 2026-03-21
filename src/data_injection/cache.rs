use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::local_input;

use super::ENV_INJECTED_DATA_CACHE_DIR;

pub(super) fn resolve_local_source(base_dir: &Path, source: &str) -> Result<PathBuf> {
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

pub(super) fn is_http_source(source: &str) -> bool {
    source.starts_with("https://") || source.starts_with("http://")
}

pub(super) fn injected_cache_root() -> Result<PathBuf> {
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

pub(super) struct DownloadedSource {
    pub bytes: Vec<u8>,
    pub file_name: Option<String>,
}

pub(super) async fn download_http_source(source: &str) -> Result<DownloadedSource> {
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

pub(super) fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

pub(super) fn sha256_dir(root: &Path) -> Result<(String, u64)> {
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

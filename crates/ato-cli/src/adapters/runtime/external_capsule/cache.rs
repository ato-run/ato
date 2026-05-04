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
    match locked.source_type.as_str() {
        "store" => ensure_store_runtime_tree_for_dependency(locked).await,
        "github" => ensure_github_runtime_tree_for_dependency(locked).await,
        other => anyhow::bail!(
            "external capsule dependency '{}' uses unsupported source_type '{}'",
            locked.name,
            other
        ),
    }
}

async fn ensure_store_runtime_tree_for_dependency(
    locked: &LockedCapsuleDependency,
) -> Result<PathBuf> {
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

/// Materialize a `capsule://github.com/<owner>/<repo>@<commit>` dependency
/// by checking out the repo at the pinned commit into a stable cache
/// directory. Returns the path to the capsule's `capsule.toml`.
///
/// The cache layout is `~/.ato/external-capsules/github/<owner>/<repo>/<commit>/`
/// (or `$ATO_EXTERNAL_CAPSULE_CACHE_DIR/github/...` if the env override is
/// set). The directory is content-addressed by the commit SHA — no
/// invalidation logic needed since SHAs are immutable.
async fn ensure_github_runtime_tree_for_dependency(
    locked: &LockedCapsuleDependency,
) -> Result<PathBuf> {
    let parsed = capsule_core::lockfile::parse_github_capsule_source(&locked.source)
        .with_context(|| format!("invalid github source '{}'", locked.source))?;
    // Defense-in-depth: lock generation pins the commit; verify the lock
    // entry's resolved_version matches the URL we're about to fetch.
    if let Some(resolved) = locked.resolved_version.as_deref() {
        if resolved.to_lowercase() != parsed.commit {
            anyhow::bail!(
                "github capsule dependency '{}' resolved_version mismatch: lock={} url={}",
                locked.name,
                resolved,
                parsed.commit
            );
        }
    }

    let cache_dir = github_capsule_cache_dir(&parsed)?;
    let manifest_path = cache_dir.join("capsule.toml");
    if !manifest_path.exists() {
        // Not cached yet — download the tarball at the pinned commit and
        // copy the checkout into the persistent cache. The downloader
        // uses `tempfile::TempDir` which auto-deletes; we copy *before*
        // returning so the cache survives.
        let repository = format!("{}/{}", parsed.owner, parsed.repo);
        // `checkout` is held in scope so its internal TempDir handle is
        // not dropped before we finish copying the contents into the
        // persistent cache.
        let checkout = crate::application::engine::install::download_github_repository_at_ref(
            &repository,
            Some(&parsed.commit),
        )
        .await
        .with_context(|| {
            format!(
                "failed to fetch github capsule dependency '{}' at {}",
                locked.name, parsed.commit
            )
        })?;
        if let Some(parent) = cache_dir.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create cache parent {}", parent.display()))?;
        }
        copy_dir_recursive(&checkout.checkout_dir, &cache_dir).with_context(|| {
            format!(
                "failed to copy github checkout into cache {}",
                cache_dir.display()
            )
        })?;
    }
    if !manifest_path.exists() {
        anyhow::bail!(
            "github capsule dependency '{}' did not contain a capsule.toml at the repo root (commit {})",
            locked.name,
            parsed.commit
        );
    }
    Ok(manifest_path)
}

fn github_capsule_cache_dir(
    parsed: &capsule_core::lockfile::GitHubCapsuleSource,
) -> Result<PathBuf> {
    let base = if let Ok(path) = std::env::var(EXTERNAL_CAPSULE_CACHE_DIR_ENV) {
        PathBuf::from(path)
    } else {
        dirs::home_dir()
            .context("failed to determine home directory")?
            .join(".ato")
            .join("external-capsules")
    };
    Ok(base
        .join("github")
        .join(&parsed.owner)
        .join(&parsed.repo)
        .join(&parsed.commit))
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else if file_type.is_symlink() {
            // Skip symlinks; they may point outside the checkout.
            continue;
        } else {
            fs::copy(entry.path(), &dst_path)?;
            // Preserve executable bit (bootstrap.sh etc.) on Unix.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = fs::metadata(entry.path())?.permissions();
                fs::set_permissions(&dst_path, fs::Permissions::from_mode(perms.mode()))?;
            }
        }
    }
    Ok(())
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

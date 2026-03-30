use super::*;

pub(crate) fn github_checkout_root() -> Result<PathBuf> {
    let root = std::env::current_dir()
        .with_context(|| "Failed to resolve current directory for temporary checkout")?
        .join(".ato")
        .join("tmp")
        .join("gh-install");
    std::fs::create_dir_all(&root).with_context(|| {
        format!(
            "Failed to create temporary checkout root: {}",
            root.display()
        )
    })?;
    Ok(root)
}

/// Returns the GitHub API base URL for repository archive downloads.
///
/// `ATO_GITHUB_API_BASE_URL` is intended for local/mock CLI tests so the
/// `--from-gh-repo` flow can be exercised without real GitHub network access.
pub(crate) fn github_api_base_url() -> String {
    std::env::var("ATO_GITHUB_API_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.github.com".to_string())
}

pub(crate) fn unpack_github_tarball(bytes: &[u8], destination: &Path) -> Result<PathBuf> {
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut root_dir: Option<PathBuf> = None;
    for entry in archive
        .entries()
        .context("Failed to read GitHub repository archive")?
    {
        let mut entry = entry.context("Invalid GitHub repository archive entry")?;
        if !matches!(
            entry.header().entry_type(),
            tar::EntryType::Regular
                | tar::EntryType::Directory
                | tar::EntryType::Symlink
                | tar::EntryType::Link
        ) {
            // Ignore tar metadata entries like PAX/GNU headers so valid GitHub
            // archives with a single repository root are not rejected.
            continue;
        }
        let path = entry
            .path()
            .context("Failed to read GitHub archive entry path")?;
        let mut components = path.components();
        let first = components
            .next()
            .ok_or_else(|| anyhow::anyhow!("GitHub archive entry path is empty or invalid"))?;
        let Component::Normal(root_component) = first else {
            bail!(
                "GitHub archive entry must start with a top-level directory before repository files; found non-standard leading path component"
            );
        };
        // The first component is the expected top-level repository directory. Remaining
        // components must stay within that directory and must not traverse outward.
        if components.any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            bail!(
                "GitHub archive entry contains unsafe path traversal components (`..`, absolute paths, or prefixes)"
            );
        }
        let root_path = PathBuf::from(root_component);
        match &root_dir {
            Some(existing) if existing != &root_path => {
                bail!("GitHub archive contains multiple top-level directories")
            }
            None => root_dir = Some(root_path),
            _ => {}
        }
        entry
            .unpack_in(destination)
            .context("Failed to unpack GitHub repository archive")?;
    }
    let root_dir = root_dir.ok_or_else(|| anyhow::anyhow!("GitHub archive is empty"))?;
    Ok(destination.join(root_dir))
}

pub(crate) fn normalize_github_checkout_dir(
    extracted_root: PathBuf,
    repo: &str,
) -> Result<PathBuf> {
    let parent = extracted_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("GitHub checkout root is missing a parent directory"))?;
    let normalized = parent.join(repo.trim());
    if normalized == extracted_root {
        return Ok(extracted_root);
    }
    if normalized.exists() {
        bail!(
            "GitHub checkout directory already exists: {}",
            normalized.display()
        );
    }
    std::fs::rename(&extracted_root, &normalized).with_context(|| {
        format!(
            "Failed to normalize GitHub checkout directory {} -> {}",
            extracted_root.display(),
            normalized.display()
        )
    })?;
    Ok(normalized)
}

pub(crate) fn extract_payload_v3_manifest_from_capsule(
    bytes: &[u8],
) -> Result<Option<capsule_core::capsule_v3::CapsuleManifestV3>> {
    let mut archive = tar::Archive::new(std::io::Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;

    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let entry_path = entry
            .path()
            .context("Failed to read archive entry path")?
            .to_string_lossy()
            .to_string();
        if entry_path != capsule_core::capsule_v3::V3_PAYLOAD_MANIFEST_PATH {
            continue;
        }

        let mut manifest_bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut manifest_bytes)
            .context("Failed to read payload.v3.manifest.json from artifact")?;
        let manifest: capsule_core::capsule_v3::CapsuleManifestV3 =
            serde_json::from_slice(&manifest_bytes)
                .context("Failed to parse payload.v3.manifest.json from artifact")?;
        capsule_core::capsule_v3::verify_artifact_hash(&manifest)
            .context("Invalid payload.v3.manifest.json artifact_hash")?;
        return Ok(Some(manifest));
    }

    Ok(None)
}

pub(crate) async fn sync_v3_chunks_from_manifest(
    client: &reqwest::Client,
    registry: &str,
    manifest: &capsule_core::capsule_v3::CapsuleManifestV3,
) -> Result<V3SyncOutcome> {
    let cas = match capsule_core::capsule_v3::CasProvider::from_env() {
        capsule_core::capsule_v3::CasProvider::Enabled(store) => store,
        capsule_core::capsule_v3::CasProvider::Disabled(reason) => {
            capsule_core::capsule_v3::CasProvider::log_disabled_once(
                "install_v3_chunk_sync",
                &reason,
            );
            return Ok(V3SyncOutcome::SkippedDisabledCas(reason));
        }
    };
    let token = crate::registry::http::current_ato_token();
    let concurrency = sync_concurrency_limit();
    sync_v3_chunks_from_manifest_with_options(client, registry, manifest, cas, token, concurrency)
        .await
}

pub(crate) fn emit_cas_disabled_performance_warning_once(
    reason: &capsule_core::capsule_v3::CasDisableReason,
    json_output: bool,
) {
    if json_output {
        return;
    }
    static STDERR_WARN_ONCE: Once = Once::new();
    STDERR_WARN_ONCE.call_once(|| {
        eprintln!(
            "⚠️  Performance warning: CAS is disabled (reason: {}). Falling back to v2 legacy mode.",
            reason
        );
    });
}

pub(crate) async fn sync_v3_chunks_from_manifest_with_options(
    client: &reqwest::Client,
    registry: &str,
    manifest: &capsule_core::capsule_v3::CapsuleManifestV3,
    cas: capsule_core::capsule_v3::CasStore,
    token: Option<String>,
    concurrency: usize,
) -> Result<V3SyncOutcome> {
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut downloads = FuturesUnordered::new();

    for chunk in &manifest.chunks {
        if cas
            .has_chunk(&chunk.raw_hash)
            .with_context(|| format!("Failed to check local CAS chunk {}", chunk.raw_hash))?
        {
            continue;
        }
        let client = client.clone();
        let cas = cas.clone();
        let registry = registry.to_string();
        let token = token.clone();
        let raw_hash = chunk.raw_hash.clone();
        let raw_size = chunk.raw_size;
        let semaphore = Arc::clone(&semaphore);

        downloads.push(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .map_err(|_| anyhow::anyhow!("v3 pull semaphore was closed"))?;
            download_chunk_to_cas_with_retry(
                &client,
                &registry,
                &cas,
                &raw_hash,
                raw_size,
                token.as_deref(),
            )
            .await
        });
    }

    while let Some(result) = downloads.next().await {
        match result? {
            ChunkDownloadOutcome::Stored => {}
            ChunkDownloadOutcome::UnsupportedRegistry => {
                return Ok(V3SyncOutcome::SkippedUnsupportedRegistry);
            }
        }
    }

    Ok(V3SyncOutcome::Synced)
}

pub(crate) async fn download_chunk_to_cas_with_retry(
    client: &reqwest::Client,
    registry: &str,
    cas: &capsule_core::capsule_v3::CasStore,
    raw_hash: &str,
    raw_size: u32,
    token: Option<&str>,
) -> Result<ChunkDownloadOutcome> {
    let endpoint = format!("{}/v1/chunks/{}", registry, urlencoding::encode(raw_hash));
    const MAX_RETRIES: usize = 4;

    for attempt in 0..=MAX_RETRIES {
        let mut req = client.get(&endpoint);
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                let bytes = resp.bytes().await.with_context(|| {
                    format!("Failed to read downloaded chunk body {}", raw_hash)
                })?;
                verify_downloaded_chunk(raw_hash, raw_size, bytes.as_ref())?;
                cas.put_chunk_zstd(raw_hash, bytes.as_ref())
                    .with_context(|| format!("Failed to store downloaded chunk {}", raw_hash))?;
                return Ok(ChunkDownloadOutcome::Stored);
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if is_sync_not_supported_status(status) {
                    return Ok(ChunkDownloadOutcome::UnsupportedRegistry);
                }
                if is_transient_status(status) && attempt < MAX_RETRIES {
                    tokio::time::sleep(backoff_duration(attempt)).await;
                    continue;
                }
                bail!(
                    "v3 chunk download failed for {} ({}): {}",
                    raw_hash,
                    status.as_u16(),
                    body.trim()
                );
            }
            Err(err) => {
                if is_transient_reqwest_error(&err) && attempt < MAX_RETRIES {
                    tokio::time::sleep(backoff_duration(attempt)).await;
                    continue;
                }
                return Err(err).with_context(|| {
                    format!(
                        "v3 chunk download request failed for {} via {}",
                        raw_hash, endpoint
                    )
                });
            }
        }
    }

    bail!("v3 chunk download exhausted retries for {}", raw_hash)
}

pub(crate) fn verify_downloaded_chunk(
    raw_hash: &str,
    raw_size: u32,
    zstd_bytes: &[u8],
) -> Result<()> {
    let cursor = std::io::Cursor::new(zstd_bytes);
    let mut decoder = zstd::Decoder::new(cursor)
        .with_context(|| format!("Failed to decode downloaded chunk {}", raw_hash))?;
    let mut hasher = blake3::Hasher::new();
    let mut total: u64 = 0;
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = std::io::Read::read(&mut decoder, &mut buf)
            .with_context(|| format!("Failed to read decoded bytes for {}", raw_hash))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total = total.saturating_add(n as u64);
    }

    if total != raw_size as u64 {
        bail!(
            "downloaded chunk raw_size mismatch for {}: expected {} got {}",
            raw_hash,
            raw_size,
            total
        );
    }

    let got = format!("blake3:{}", hex::encode(hasher.finalize().as_bytes()));
    if !equals_hash(raw_hash, &got) {
        bail!(
            "downloaded chunk hash mismatch for {}: expected {} got {}",
            raw_hash,
            raw_hash,
            got
        );
    }

    Ok(())
}

pub(crate) fn sync_concurrency_limit() -> usize {
    std::env::var("ATO_SYNC_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|v| v.clamp(1, 128))
        .unwrap_or(8)
}

pub(crate) fn is_sync_not_supported_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::NOT_FOUND
            | reqwest::StatusCode::METHOD_NOT_ALLOWED
            | reqwest::StatusCode::NOT_IMPLEMENTED
    )
}

pub(crate) fn is_transient_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

pub(crate) fn is_transient_reqwest_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

pub(crate) fn backoff_duration(attempt: usize) -> Duration {
    let base_ms = 200u64.saturating_mul(1u64 << attempt.min(4));
    Duration::from_millis(base_ms.min(2_000))
}

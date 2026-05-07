use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use super::*;

const DEFAULT_GITHUB_RUN_CHECKOUT_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const OWNER_MARKER_FILE: &str = ".ato-owner.json";

#[derive(Debug, Serialize, Deserialize)]
struct GithubRunCheckoutOwner {
    owner_pid: u32,
    #[serde(default)]
    owner_start_time_unix_ms: Option<u64>,
}

pub(crate) fn github_checkout_root() -> Result<PathBuf> {
    // Use the global ato home directory rather than a CWD-relative .ato/tmp path.
    // Paths under .ato/tmp are blocked by the workspace-internal-state guard in the
    // capsule packer (core/src/packers/capsule.rs::ensure_payload_source_root), which
    // prevents packing from within the ato internal scratch space.  Using
    // ~/.ato/gh-install/ instead places checkouts alongside the global ato state but
    // outside the blocked WORKSPACE_INTERNAL_SUBDIRS list.
    let root = capsule_core::common::paths::ato_path("gh-install")
        .context("Cannot determine ato home directory for GitHub checkout")?;
    std::fs::create_dir_all(&root).with_context(|| {
        format!(
            "Failed to create temporary checkout root: {}",
            root.display()
        )
    })?;
    Ok(root)
}

pub(crate) fn github_run_checkout_root() -> Result<PathBuf> {
    let root = capsule_core::common::paths::nacelle_home_dir()
        .context("Failed to resolve Ato home directory for GitHub run")?
        .join("tmp")
        .join("gh-run");
    std::fs::create_dir_all(&root).with_context(|| {
        format!(
            "Failed to create transient GitHub run root: {}",
            root.display()
        )
    })?;
    Ok(root)
}

pub(crate) fn remove_github_run_checkout(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "Failed to remove transient GitHub run checkout: {}",
                path.display()
            )
        }),
    }
}

pub(crate) fn write_github_run_checkout_owner_marker(path: &Path) -> Result<()> {
    let marker_path = path.join(OWNER_MARKER_FILE);
    // This marker is transient runtime metadata for GC safety only.
    // It lives inside the relocated checkout so the sweep can preserve
    // live workspaces even when process records are briefly unavailable.
    let owner = GithubRunCheckoutOwner {
        owner_pid: std::process::id(),
        owner_start_time_unix_ms: ato_session_core::process::process_start_time_unix_ms(
            std::process::id(),
        ),
    };
    let bytes = serde_json::to_vec_pretty(&owner)
        .context("Failed to serialize transient GitHub run checkout owner marker")?;
    std::fs::write(&marker_path, bytes).with_context(|| {
        format!(
            "Failed to write transient GitHub run checkout owner marker: {}",
            marker_path.display()
        )
    })?;
    Ok(())
}

pub(crate) fn sweep_stale_github_run_checkouts_best_effort() {
    let root = match github_run_checkout_root() {
        Ok(root) => root,
        Err(error) => {
            debug!(error = %error, "skipping transient GitHub run checkout sweep");
            return;
        }
    };

    if let Err(error) = sweep_stale_github_run_checkouts_in(
        &root,
        SystemTime::now(),
        DEFAULT_GITHUB_RUN_CHECKOUT_TTL,
    ) {
        debug!(
            root = %root.display(),
            error = %error,
            "transient GitHub run checkout sweep failed"
        );
    }
}

pub(crate) fn sweep_stale_github_run_checkouts_in(
    root: &Path,
    now: SystemTime,
    ttl: Duration,
) -> Result<usize> {
    if !root.exists() {
        return Ok(0);
    }

    let active_roots = active_github_run_checkout_roots(root);
    let mut removed = 0;
    for entry in std::fs::read_dir(root).with_context(|| {
        format!(
            "Failed to read GitHub run checkout root: {}",
            root.display()
        )
    })? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                debug!(error = %error, "skipping unreadable GitHub run checkout entry");
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                debug!(path = %path.display(), error = %error, "skipping GitHub run checkout with unreadable type");
                continue;
            }
        };
        if !file_type.is_dir() {
            continue;
        }
        if active_roots.contains(&path) || github_run_checkout_owner_is_alive(&path) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                debug!(path = %path.display(), error = %error, "skipping GitHub run checkout with unreadable metadata");
                continue;
            }
        };
        if !github_run_checkout_is_stale(&metadata, now, ttl) {
            continue;
        }
        match remove_github_run_checkout(&path) {
            Ok(()) => removed += 1,
            Err(error) => {
                debug!(path = %path.display(), error = %error, "failed to sweep stale GitHub run checkout")
            }
        }
    }
    Ok(removed)
}

fn github_run_checkout_is_stale(
    metadata: &std::fs::Metadata,
    now: SystemTime,
    ttl: Duration,
) -> bool {
    let Some(timestamp) = metadata.modified().ok() else {
        return false;
    };
    now.duration_since(timestamp)
        .map(|age| age >= ttl)
        .unwrap_or(false)
}

fn active_github_run_checkout_roots(root: &Path) -> HashSet<PathBuf> {
    let process_manager = match crate::runtime::process::ProcessManager::new() {
        Ok(process_manager) => process_manager,
        Err(error) => {
            debug!(error = %error, "failed to open process manager for GitHub run checkout sweep");
            return HashSet::new();
        }
    };

    let processes = match process_manager.list_processes() {
        Ok(processes) => processes,
        Err(error) => {
            debug!(error = %error, "failed to list processes for GitHub run checkout sweep");
            return HashSet::new();
        }
    };

    processes
        .into_iter()
        .filter(|process| process.status.is_active())
        .filter_map(|process| process.manifest_path)
        .filter_map(|manifest_path| github_run_checkout_root_for_manifest(root, &manifest_path))
        .collect()
}

fn github_run_checkout_root_for_manifest(root: &Path, manifest_path: &Path) -> Option<PathBuf> {
    manifest_path
        .ancestors()
        .skip(1)
        .find(|ancestor| ancestor.parent() == Some(root))
        .map(Path::to_path_buf)
}

pub(crate) fn github_run_checkout_owner_is_alive(path: &Path) -> bool {
    let marker_path = path.join(OWNER_MARKER_FILE);
    let bytes = match std::fs::read(&marker_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(error) => {
            debug!(path = %marker_path.display(), error = %error, "failed to read GitHub run checkout owner marker");
            return false;
        }
    };
    let owner: GithubRunCheckoutOwner = match serde_json::from_slice(&bytes) {
        Ok(owner) => owner,
        Err(error) => {
            debug!(path = %marker_path.display(), error = %error, "failed to parse GitHub run checkout owner marker");
            return false;
        }
    };
    if !pid_is_alive(owner.owner_pid) {
        return false;
    }

    let Some(expected_start_time) = owner.owner_start_time_unix_ms else {
        return false;
    };

    ato_session_core::process::process_start_time_unix_ms(owner.owner_pid)
        .is_some_and(|live_start_time| live_start_time == expected_start_time)
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error()
        .raw_os_error()
        .is_some_and(|errno| errno == libc::EPERM)
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    // On unsupported platforms we fall back to TTL + active-process
    // reverse lookup rather than preserving every marker forever.
    false
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

pub(crate) fn github_api_bearer_token() -> Option<String> {
    ["ATO_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]
        .into_iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
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

pub(crate) fn extract_payload_payload_manifest_from_capsule(
    bytes: &[u8],
) -> Result<Option<capsule_core::capsule::PayloadManifest>> {
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
        if entry_path != capsule_core::capsule::PAYLOAD_MANIFEST_PATH {
            continue;
        }

        let mut manifest_bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut manifest_bytes)
            .context("Failed to read payload.v3.manifest.json from artifact")?;
        let manifest: capsule_core::capsule::PayloadManifest =
            serde_json::from_slice(&manifest_bytes)
                .context("Failed to parse payload.v3.manifest.json from artifact")?;
        capsule_core::capsule::verify_artifact_hash(&manifest)
            .context("Invalid payload.v3.manifest.json artifact_hash")?;
        return Ok(Some(manifest));
    }

    Ok(None)
}

pub(crate) async fn sync_v3_chunks_from_manifest(
    client: &reqwest::Client,
    registry: &str,
    manifest: &capsule_core::capsule::PayloadManifest,
) -> Result<V3SyncOutcome> {
    let cas = match capsule_core::capsule::CasProvider::from_env() {
        capsule_core::capsule::CasProvider::Enabled(store) => store,
        capsule_core::capsule::CasProvider::Disabled(reason) => {
            capsule_core::capsule::CasProvider::log_disabled_once("install_v3_chunk_sync", &reason);
            return Ok(V3SyncOutcome::SkippedDisabledCas(reason));
        }
    };
    let token = crate::registry::http::current_ato_token();
    let concurrency = sync_concurrency_limit();
    sync_v3_chunks_from_manifest_with_options(client, registry, manifest, cas, token, concurrency)
        .await
}

pub(crate) fn emit_cas_disabled_performance_warning_once(
    reason: &capsule_core::capsule::CasDisableReason,
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
    manifest: &capsule_core::capsule::PayloadManifest,
    cas: capsule_core::capsule::CasStore,
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
    cas: &capsule_core::capsule::CasStore,
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

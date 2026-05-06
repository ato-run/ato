//! Lockfile support helpers for tool bootstrap, downloads, and atomic filesystem writes.

use std::fs;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs2::FileExt;

use crate::bootstrap::{BootstrapBoundary, BootstrapVerificationKind};
use crate::common::paths::{nacelle_home_dir, toolchain_cache_dir};
use crate::error::{CapsuleError, Result};
use crate::packers::runtime_fetcher::RuntimeFetcher;
use crate::reporter::CapsuleReporter;

use super::{platform_triple, METADATA_CACHE_DIR_NAME, PNPM_VERSION, UV_VERSION};

pub(super) struct PnpmCommand {
    pub(super) program: PathBuf,
    pub(super) args_prefix: Vec<String>,
}

pub(super) async fn ensure_uv(reporter: Arc<dyn CapsuleReporter + 'static>) -> Result<PathBuf> {
    if let Ok(found) = which::which("uv") {
        return Ok(found);
    }

    let boundary =
        BootstrapBoundary::network_tool("uv", BootstrapVerificationKind::ChecksumUnavailable);
    let version = UV_VERSION;
    reporter
        .notify(format!("⬇️  Downloading uv {}", version))
        .await?;
    let target_triple = platform_triple()?;
    let tools_dir = toolchain_cache_dir()?
        .join("tools")
        .join(boundary.subject_name.as_str())
        .join(version);
    std::fs::create_dir_all(&tools_dir)
        .map_err(|e| CapsuleError::Pack(format!("Failed to create uv tools directory: {}", e)))?;
    let archive_path = tools_dir.join(format!("uv-{}.tar.gz", target_triple));
    let url = format!(
        "https://github.com/astral-sh/uv/releases/download/{}/uv-{}.tar.gz",
        version, target_triple
    );
    download_file(&url, &archive_path).await?;
    extract_tgz(&archive_path, &tools_dir)?;
    let uv_bin = find_binary_recursive(&tools_dir, &["uv", "uv.exe"])
        .ok_or_else(|| CapsuleError::Pack("uv binary not found after extraction".to_string()))?;
    Ok(uv_bin)
}

pub(super) async fn ensure_node(
    version: &str,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<PathBuf> {
    if let Ok(found) = which::which("node") {
        return Ok(found);
    }
    let fetcher = RuntimeFetcher::new_with_reporter(reporter)?;
    fetcher.ensure_node(version).await
}

// TODO(tools-registry, ato-run/ato#29): the lockfile-generation path still
// fetches pnpm via this private helper. The preflight (ato run) path now goes
// through `crate::tools::ensure_runtime_tool(&PNPM, ...)`, so for the moment
// pnpm has two parallel fetch sites with shared `PNPM_VERSION` constants. The
// next slice migrates this caller to the registry and deletes the duplicate.
pub(super) async fn ensure_pnpm(
    node_path: &Path,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<PnpmCommand> {
    if let Ok(found) = which::which("pnpm") {
        return Ok(PnpmCommand {
            program: found,
            args_prefix: Vec::new(),
        });
    }
    let boundary =
        BootstrapBoundary::network_tool("pnpm", BootstrapVerificationKind::ChecksumUnavailable);
    let version = PNPM_VERSION;
    reporter
        .notify(format!("⬇️  Downloading pnpm {}", version))
        .await?;
    let tools_dir = toolchain_cache_dir()?
        .join("tools")
        .join(boundary.subject_name.as_str())
        .join(version);
    std::fs::create_dir_all(&tools_dir)
        .map_err(|e| CapsuleError::Pack(format!("Failed to create pnpm tools directory: {}", e)))?;
    let archive_path = tools_dir.join(format!("pnpm-{}.tgz", version));
    let url = format!("https://registry.npmjs.org/pnpm/-/pnpm-{}.tgz", version);
    download_file(&url, &archive_path).await?;
    extract_tgz(&archive_path, &tools_dir)?;

    let script = tools_dir.join("package").join("bin").join("pnpm.cjs");
    if !script.exists() {
        return Err(CapsuleError::Pack(
            "pnpm.cjs not found after extraction".to_string(),
        ));
    }

    Ok(PnpmCommand {
        program: node_path.to_path_buf(),
        args_prefix: vec![script.to_string_lossy().to_string()],
    })
}

pub(super) async fn download_file(url: &str, dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(CapsuleError::Network)?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(CapsuleError::Network)?;
    if !response.status().is_success() {
        return Err(CapsuleError::Network(
            response.error_for_status().unwrap_err(),
        ));
    }
    let bytes = response.bytes().await.map_err(CapsuleError::Network)?;
    write_atomic_bytes_with_os_lock(
        dest,
        &bytes,
        &format!("download cache for {url}"),
        capsule_error_pack,
    )?;
    Ok(())
}

pub(super) fn capsule_error_pack(message: String) -> CapsuleError {
    CapsuleError::Pack(message)
}

pub(super) fn capsule_error_config(message: String) -> CapsuleError {
    CapsuleError::Config(message)
}

pub(super) fn write_atomic_bytes_with_os_lock<E>(
    path: &Path,
    bytes: &[u8],
    label: &str,
    to_error: E,
) -> Result<()>
where
    E: Fn(String) -> CapsuleError,
{
    let parent = path.parent().ok_or_else(|| {
        to_error(format!(
            "Failed to resolve parent directory for {} ({})",
            path.display(),
            label
        ))
    })?;
    fs::create_dir_all(parent).map_err(|e| {
        to_error(format!(
            "Failed to create parent directory {} for {}: {}",
            parent.display(),
            label,
            e
        ))
    })?;

    with_path_lock(path, label, &to_error, || {
        atomic_write_in_place(path, bytes, label, &to_error)
    })
}

fn with_path_lock<T, E, F>(path: &Path, label: &str, to_error: &E, op: F) -> Result<T>
where
    E: Fn(String) -> CapsuleError,
    F: FnOnce() -> Result<T>,
{
    #[cfg(unix)]
    let lock_target = path.parent().ok_or_else(|| {
        to_error(format!(
            "Failed to locate lock parent for {}",
            path.display()
        ))
    })?;
    #[cfg(not(unix))]
    let lock_target = path;

    #[cfg(unix)]
    let lock_file = fs::OpenOptions::new()
        .read(true)
        .open(lock_target)
        .map_err(|e| {
            to_error(format!(
                "Failed to open lock directory {} for {}: {}",
                lock_target.display(),
                label,
                e
            ))
        })?;
    #[cfg(not(unix))]
    let lock_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_target)
        .map_err(|e| {
            to_error(format!(
                "Failed to open lock target {} for {}: {}",
                lock_target.display(),
                label,
                e
            ))
        })?;

    lock_file.lock_exclusive().map_err(|e| {
        to_error(format!(
            "Failed to acquire exclusive lock on {} for {}: {}",
            lock_target.display(),
            label,
            e
        ))
    })?;

    let op_result = op();
    let unlock_result = lock_file.unlock();
    match (op_result, unlock_result) {
        (Ok(v), Ok(())) => Ok(v),
        (Err(err), Ok(())) => Err(err),
        (Ok(_), Err(e)) => Err(to_error(format!(
            "Failed to release lock on {} for {}: {}",
            lock_target.display(),
            label,
            e
        ))),
        (Err(err), Err(_)) => Err(err),
    }
}

fn atomic_write_in_place<E>(path: &Path, bytes: &[u8], label: &str, to_error: &E) -> Result<()>
where
    E: Fn(String) -> CapsuleError,
{
    let parent = path.parent().ok_or_else(|| {
        to_error(format!(
            "Failed to resolve parent directory for {} ({})",
            path.display(),
            label
        ))
    })?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "tmp".to_string());

    let tmp_path = create_atomic_temp_file(parent, &file_name, label, to_error)?;
    let write_result = (|| -> Result<()> {
        let mut tmp_file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|e| {
                to_error(format!(
                    "Failed to reopen temp file {} for {}: {}",
                    tmp_path.display(),
                    label,
                    e
                ))
            })?;
        tmp_file.write_all(bytes).map_err(|e| {
            to_error(format!(
                "Failed to write temp file {} for {}: {}",
                tmp_path.display(),
                label,
                e
            ))
        })?;
        tmp_file.sync_all().map_err(|e| {
            to_error(format!(
                "Failed to sync temp file {} for {}: {}",
                tmp_path.display(),
                label,
                e
            ))
        })?;
        drop(tmp_file);

        fs::rename(&tmp_path, path).map_err(|e| {
            to_error(format!(
                "Failed to atomically rename {} -> {} for {}: {}",
                tmp_path.display(),
                path.display(),
                label,
                e
            ))
        })?;
        sync_parent_directory(parent, label, to_error)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_result
}

pub(super) fn create_atomic_temp_file<E>(
    parent: &Path,
    file_name: &str,
    label: &str,
    to_error: &E,
) -> Result<PathBuf>
where
    E: Fn(String) -> CapsuleError,
{
    let pid = std::process::id();
    for attempt in 0..256u32 {
        let tmp_name = format!(".{}.tmp-{}-{}", file_name, pid, attempt);
        let tmp_path = parent.join(tmp_name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(file) => {
                drop(file);
                return Ok(tmp_path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(to_error(format!(
                    "Failed to create temp file in {} for {}: {}",
                    parent.display(),
                    label,
                    e
                )));
            }
        }
    }
    Err(to_error(format!(
        "Failed to allocate unique temp file in {} for {}",
        parent.display(),
        label
    )))
}

fn sync_parent_directory<E>(parent: &Path, label: &str, to_error: &E) -> Result<()>
where
    E: Fn(String) -> CapsuleError,
{
    #[cfg(unix)]
    {
        let dir = fs::File::open(parent).map_err(|e| {
            to_error(format!(
                "Failed to open parent directory {} for {} sync: {}",
                parent.display(),
                label,
                e
            ))
        })?;
        dir.sync_all().map_err(|e| {
            to_error(format!(
                "Failed to sync parent directory {} for {}: {}",
                parent.display(),
                label,
                e
            ))
        })?;
    }
    #[cfg(not(unix))]
    {
        let _ = (parent, label, to_error);
    }
    Ok(())
}

fn extract_tgz(archive_path: &Path, dest: &Path) -> Result<()> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let file = std::fs::File::open(archive_path)
        .map_err(|e| CapsuleError::Pack(format!("Failed to open archive: {}", e)))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    archive
        .unpack(dest)
        .map_err(|e| CapsuleError::Pack(format!("Failed to extract archive: {}", e)))?;
    Ok(())
}

fn find_binary_recursive(root: &Path, candidates: &[&str]) -> Option<PathBuf> {
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if candidates.iter().any(|c| *c == name) {
            return Some(entry.path().to_path_buf());
        }
    }
    None
}

fn metadata_cache_dir() -> Result<PathBuf> {
    Ok(nacelle_home_dir()?.join(METADATA_CACHE_DIR_NAME))
}

pub(super) fn metadata_cache_path(
    scope: &str,
    name: &str,
    version: &str,
    target_triple: &str,
) -> Result<PathBuf> {
    Ok(metadata_cache_dir()?
        .join(scope)
        .join(name)
        .join(version)
        .join(format!("{}.sha256", target_triple)))
}

fn read_cached_sha256(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(path).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to read metadata cache {}: {}",
            path.display(),
            e
        ))
    })?;
    let value = raw.trim();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value.to_string()))
    }
}

pub(super) async fn cached_sha256<F, Fut>(cache_path: PathBuf, fetch: F) -> Result<String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String>>,
{
    if let Some(value) = read_cached_sha256(&cache_path)? {
        return Ok(value);
    }

    let value = fetch().await?;
    write_atomic_bytes_with_os_lock(
        &cache_path,
        value.as_bytes(),
        "metadata cache",
        capsule_error_pack,
    )?;
    Ok(value)
}

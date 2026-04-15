//! Lockfile runtime/tool resolution, generation, and artifact prefetch helpers.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::try_join_all;
use tempfile::TempDir;
use tracing::debug;

use crate::error::{CapsuleError, Result};
use crate::importer::{
    probe_required_deno_lockfile, probe_required_node_lockfile, probe_required_python_lockfile,
    ImporterId, ProbeResult,
};
use crate::packers::runtime_fetcher::RuntimeFetcher;
use crate::reporter::CapsuleReporter;

use super::lockfile_support::{
    cached_sha256, ensure_node, ensure_pnpm, ensure_uv, metadata_cache_path,
};
use super::{
    artifact_root, read_dependencies_path, read_target_entrypoint, reset_dir, sha256_dir,
    sha256_hex, ArtifactEntry, RuntimeArtifact, RuntimeEntry, RuntimePlatform, ToolArtifact,
    ToolTargets, BUN_VERSION, PNPM_VERSION, UV_VERSION, YARN_CLASSIC_VERSION,
};

pub(super) async fn generate_uv_lock(
    manifest_dir: &Path,
    manifest: &toml::Value,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<Option<PathBuf>> {
    let uv_lock = manifest_dir.join("uv.lock");
    let deps_path = read_dependencies_path(manifest, "python", manifest_dir)
        .or_else(|| {
            let candidate = manifest_dir.join("pyproject.toml");
            if candidate.exists() {
                Some(candidate)
            } else {
                None
            }
        })
        .or_else(|| {
            let candidate = manifest_dir.join("requirements.txt");
            if candidate.exists() {
                Some(candidate)
            } else {
                None
            }
        });
    debug!(
        manifest_dir = %manifest_dir.display(),
        execution_working_directory = %manifest_dir.display(),
        lockfile_check_paths = ?vec![("uv.lock", uv_lock.clone(), uv_lock.exists())],
        dependency_check_paths = ?deps_path.as_ref().map(|path| vec![path.clone()]).unwrap_or_default(),
        "Lockfile generation path diagnostics"
    );

    let Some(deps_path) = deps_path else {
        return Ok(None);
    };

    match probe_required_python_lockfile(manifest_dir)? {
        ProbeResult::Found(evidence) => {
            return Ok(evidence.first().map(|value| value.primary_path.clone()));
        }
        ProbeResult::Missing(_) => {}
        ProbeResult::Ambiguous(ambiguity) => {
            return Err(CapsuleError::Pack(ambiguity.message));
        }
        ProbeResult::NotApplicable => return Ok(None),
    }

    reporter
        .notify("ℹ️  uv.lock is required but will not be generated automatically".to_string())
        .await?;
    Err(CapsuleError::Pack(format!(
        "uv.lock is missing for '{}'. Generate it with `uv lock` (or `uv pip compile requirements.txt -o uv.lock`) and rerun `ato generate-lockfile`.",
        deps_path.display()
    )))
}

pub(super) async fn generate_node_lockfile(
    manifest_dir: &Path,
    manifest: &toml::Value,
    _node_version: &str,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<Option<PathBuf>> {
    let package_lock = manifest_dir.join("package-lock.json");
    let pnpm_lock = manifest_dir.join("pnpm-lock.yaml");
    let bun_lock = manifest_dir.join("bun.lock");
    let bun_lockb = manifest_dir.join("bun.lockb");
    let deps_path = read_dependencies_path(manifest, "node", manifest_dir).or_else(|| {
        let candidate = manifest_dir.join("package.json");
        if candidate.exists() {
            Some(candidate)
        } else {
            None
        }
    });
    debug!(
        manifest_dir = %manifest_dir.display(),
        execution_working_directory = %manifest_dir.display(),
        lockfile_check_paths = ?vec![
            ("package-lock.json", package_lock.clone(), package_lock.exists()),
            ("pnpm-lock.yaml", pnpm_lock.clone(), pnpm_lock.exists()),
            ("bun.lock", bun_lock.clone(), bun_lock.exists()),
            ("bun.lockb", bun_lockb.clone(), bun_lockb.exists()),
        ],
        dependency_check_paths = ?deps_path.as_ref().map(|path| vec![path.clone()]).unwrap_or_default(),
        "Lockfile generation path diagnostics"
    );
    let Some(_) = deps_path else {
        return Ok(None);
    };

    match probe_required_node_lockfile(manifest_dir)? {
        ProbeResult::Found(evidence) => {
            let Some(primary) = evidence.first() else {
                return Ok(None);
            };
            match primary.importer_id {
                ImporterId::Npm => {
                    reporter
                        .notify(
                            "ℹ️  package-lock.json detected; skipping pnpm-lock.yaml generation"
                                .to_string(),
                        )
                        .await?;
                    return Ok(None);
                }
                ImporterId::Pnpm => return Ok(Some(primary.primary_path.clone())),
                ImporterId::Yarn => {
                    return Ok(Some(primary.primary_path.clone()));
                }
                ImporterId::Bun => {
                    return Ok(Some(primary.primary_path.clone()));
                }
                _ => return Ok(None),
            }
        }
        ProbeResult::Missing(_) => {}
        ProbeResult::Ambiguous(ambiguity) => {
            return Err(CapsuleError::Pack(ambiguity.message));
        }
        ProbeResult::NotApplicable => return Ok(None),
    }

    reporter
        .notify(
            "ℹ️  pnpm-lock.yaml is required but will not be generated automatically".to_string(),
        )
        .await?;
    Err(CapsuleError::Pack(
        "pnpm-lock.yaml is missing. Generate it with `pnpm install --lockfile-only` and rerun `ato generate-lockfile`.".to_string(),
    ))
}

pub(super) async fn generate_deno_lock(
    manifest_dir: &Path,
    manifest: &toml::Value,
    _deno_version: &str,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<Option<PathBuf>> {
    let entrypoint = read_target_entrypoint(manifest).or_else(|| {
        manifest
            .get("execution")
            .and_then(|e| e.get("entrypoint"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });
    let Some(entrypoint) = entrypoint else {
        debug!(
            manifest_dir = %manifest_dir.display(),
            execution_working_directory = %manifest_dir.display(),
            lockfile_check_paths = ?vec![("deno.lock", manifest_dir.join("deno.lock"), manifest_dir.join("deno.lock").exists())],
            dependency_check_paths = ?Vec::<std::path::PathBuf>::new(),
            "Lockfile generation path diagnostics"
        );
        return Ok(None);
    };

    let entrypoint_path = manifest_dir.join(&entrypoint);
    debug!(
        manifest_dir = %manifest_dir.display(),
        execution_working_directory = %manifest_dir.display(),
        lockfile_check_paths = ?vec![("deno.lock", manifest_dir.join("deno.lock"), manifest_dir.join("deno.lock").exists())],
        dependency_check_paths = ?vec![entrypoint_path.clone()],
        "Lockfile generation path diagnostics"
    );
    if !entrypoint_path.exists() || entrypoint_path.is_dir() {
        return Ok(None);
    }

    match probe_required_deno_lockfile(manifest_dir)? {
        ProbeResult::Found(evidence) => {
            return Ok(evidence.first().map(|value| value.primary_path.clone()));
        }
        ProbeResult::Missing(_) => {}
        ProbeResult::Ambiguous(ambiguity) => {
            return Err(CapsuleError::Pack(ambiguity.message));
        }
        ProbeResult::NotApplicable => return Ok(None),
    }

    reporter
        .notify("ℹ️  deno.lock is required but will not be generated automatically".to_string())
        .await?;
    Err(CapsuleError::Pack(format!(
        "deno.lock is missing for '{}'. Generate it with `deno cache --lock=deno.lock --frozen=false {}` and rerun `ato generate-lockfile`.",
        manifest_dir.display(),
        entrypoint
    )))
}

pub(super) async fn prepare_python_artifacts(
    manifest: &toml::Value,
    manifest_dir: &Path,
    target_key: &str,
    python_version: &str,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<Vec<ArtifactEntry>> {
    let lock_path = manifest_dir.join("uv.lock");
    if !lock_path.exists() {
        return Ok(Vec::new());
    }

    let uv_path = ensure_uv(reporter.clone()).await?;
    let fetcher = RuntimeFetcher::new_with_reporter(reporter.clone())?;
    let python_path = fetcher.ensure_python(python_version).await?;
    reporter
        .notify("⬇️  Prefetching Python cache".to_string())
        .await?;

    let cache_dir = artifact_root(manifest_dir, target_key).join("uv-cache");
    reset_dir(&cache_dir)?;
    let install_dir = artifact_root(manifest_dir, target_key).join("uv-install");
    reset_dir(&install_dir)?;

    let mut cmd = std::process::Command::new(&uv_path);
    cmd.args([
        "pip",
        "sync",
        lock_path.to_string_lossy().as_ref(),
        "--python",
        python_path.to_string_lossy().as_ref(),
        "--cache-dir",
        cache_dir.to_string_lossy().as_ref(),
        "--target",
        install_dir.to_string_lossy().as_ref(),
    ])
    .current_dir(manifest_dir);

    let status = run_command_inner_with_manifest_env(cmd, Some(manifest)).await?;
    if !status.success() {
        return Err(CapsuleError::Pack("uv pip sync failed".to_string()));
    }

    if install_dir.exists() {
        std::fs::remove_dir_all(&install_dir)?;
    }

    let cache_hash = sha256_dir(&cache_dir)?;
    Ok(vec![ArtifactEntry {
        filename: "uv-cache".to_string(),
        url: "https://files.pythonhosted.org/".to_string(),
        sha256: cache_hash,
        artifact_type: "uv-cache".to_string(),
    }])
}

pub(super) async fn prepare_node_artifacts(
    manifest: &toml::Value,
    manifest_dir: &Path,
    target_key: &str,
    node_version: &str,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<Vec<ArtifactEntry>> {
    let deps_path = read_dependencies_path(manifest, "node", manifest_dir).or_else(|| {
        let candidate = manifest_dir.join("package.json");
        if candidate.exists() {
            Some(candidate)
        } else {
            None
        }
    });
    let Some(_) = deps_path else {
        return Ok(Vec::new());
    };
    let lock_path = manifest_dir.join("pnpm-lock.yaml");
    if !lock_path.exists() {
        return Ok(Vec::new());
    }

    let node_path = ensure_node(node_version, reporter.clone()).await?;
    let pnpm_cmd = ensure_pnpm(&node_path, reporter.clone()).await?;
    reporter
        .notify("⬇️  Fetching pnpm store".to_string())
        .await?;

    let store_dir = artifact_root(manifest_dir, target_key).join("pnpm-store");
    reset_dir(&store_dir)?;

    let temp_dir = TempDir::new()
        .map_err(|e| CapsuleError::Pack(format!("Failed to create pnpm temp dir: {}", e)))?;
    let temp_path = temp_dir.path();
    if let Some(path) = deps_path.as_ref() {
        let dest = temp_path.join(path.file_name().unwrap_or_else(|| path.as_os_str()));
        std::fs::copy(path, &dest)
            .map_err(|e| CapsuleError::Pack(format!("Failed to copy {}: {}", path.display(), e)))?;
    }
    let temp_lock = temp_path.join("pnpm-lock.yaml");
    std::fs::copy(&lock_path, &temp_lock)
        .map_err(|e| CapsuleError::Pack(format!("Failed to copy pnpm-lock.yaml: {}", e)))?;

    let mut cmd = std::process::Command::new(&pnpm_cmd.program);
    cmd.args(&pnpm_cmd.args_prefix)
        .args([
            "fetch",
            "--ignore-scripts",
            "--silent",
            "--store-dir",
            store_dir.to_string_lossy().as_ref(),
        ])
        .current_dir(temp_path);
    let status = run_command_inner_with_manifest_env(cmd, Some(manifest)).await?;
    if !status.success() {
        return Err(CapsuleError::Pack("pnpm fetch failed".to_string()));
    }

    let store_hash = sha256_dir(&store_dir)?;
    Ok(vec![ArtifactEntry {
        filename: "pnpm-store".to_string(),
        url: "https://registry.npmjs.org/".to_string(),
        sha256: store_hash,
        artifact_type: "pnpm-store".to_string(),
    }])
}

#[cfg(test)]
pub(super) async fn run_command_inner(
    cmd: std::process::Command,
) -> Result<std::process::ExitStatus> {
    run_command_inner_with_manifest_env(cmd, None).await
}

async fn run_command_inner_with_manifest_env(
    mut cmd: std::process::Command,
    manifest: Option<&toml::Value>,
) -> Result<std::process::ExitStatus> {
    let program = Path::new(cmd.get_program());
    if !program.is_absolute() {
        return Err(CapsuleError::Pack(format!(
            "Refusing to execute non-absolute command path: {}",
            program.to_string_lossy()
        )));
    }
    let required_env = manifest
        .map(read_required_env_pairs)
        .transpose()?
        .unwrap_or_default();
    apply_sanitized_command_env(&mut cmd, &required_env);
    tokio::task::spawn_blocking(move || cmd.status())
        .await
        .map_err(|e| CapsuleError::Pack(format!("Failed to run command: {}", e)))?
        .map_err(|e| CapsuleError::Pack(format!("Failed to run command: {}", e)))
}

fn apply_sanitized_command_env(
    cmd: &mut std::process::Command,
    required_env: &[(OsString, OsString)],
) {
    const ALLOWED_ENV_KEYS: &[&str] = &[
        "PATH",
        "HOME",
        "TMPDIR",
        "TMP",
        "TEMP",
        "SYSTEMROOT",
        "WINDIR",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
    ];

    let mut allowed_envs: HashMap<OsString, OsString> = HashMap::new();
    for key in ALLOWED_ENV_KEYS {
        if let Some(value) = std::env::var_os(key) {
            allowed_envs.insert(OsString::from(key), value);
        }
    }

    for (key, value) in std::env::vars_os() {
        let key_text = key.to_string_lossy();
        if key_text.starts_with("ATO_") || key_text.starts_with("CAPSULE_") {
            allowed_envs.insert(key, value);
        }
    }
    for (key, value) in required_env {
        allowed_envs.insert(key.clone(), value.clone());
    }

    cmd.env_clear();
    for (key, value) in allowed_envs {
        cmd.env(key, value);
    }
}

fn read_required_env_pairs(manifest: &toml::Value) -> Result<Vec<(OsString, OsString)>> {
    let keys = required_env_keys_from_manifest(manifest);
    let mut required_env = Vec::new();
    for key in keys {
        if let Some(value) = std::env::var_os(&key) {
            required_env.push((OsString::from(key), value));
        }
    }

    Ok(required_env)
}

pub(super) fn required_env_keys_from_manifest(manifest: &toml::Value) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    if let Some(targets) = manifest.get("targets").and_then(|v| v.as_table()) {
        for target in targets.values() {
            if let Some(required_env) = target.get("required_env").and_then(|v| v.as_array()) {
                for value in required_env {
                    if let Some(key) = value.as_str() {
                        let trimmed = key.trim();
                        if !trimmed.is_empty() {
                            keys.push(trimmed.to_string());
                        }
                    }
                }
            }

            if let Some(legacy_required) = target
                .get("env")
                .and_then(|v| v.as_table())
                .and_then(|env| env.get("ATO_ORCH_REQUIRED_ENVS"))
                .and_then(|v| v.as_str())
            {
                for item in legacy_required.split(',') {
                    let trimmed = item.trim();
                    if !trimmed.is_empty() {
                        keys.push(trimmed.to_string());
                    }
                }
            }
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

pub(super) async fn resolve_python_runtime(
    version: &str,
    platforms: &[RuntimePlatform],
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<RuntimeEntry> {
    let fetcher = Arc::new(RuntimeFetcher::new_with_reporter(reporter)?);
    let targets = try_join_all(platforms.iter().copied().filter_map(|platform| {
        let Ok(url) = RuntimeFetcher::get_python_download_url(version, platform.os, platform.arch)
        else {
            return None;
        };
        let fetcher = Arc::clone(&fetcher);
        let version = version.to_string();
        Some(async move {
            let sha256 = resolve_python_sha256_cached(
                fetcher.as_ref(),
                &version,
                platform.target_triple,
                &url,
            )
            .await?;
            Ok::<(String, RuntimeArtifact), CapsuleError>((
                platform.target_triple.to_string(),
                RuntimeArtifact { url, sha256 },
            ))
        })
    }))
    .await?
    .into_iter()
    .collect();

    Ok(RuntimeEntry {
        provider: "python-build-standalone".to_string(),
        version: version.to_string(),
        targets,
    })
}

async fn resolve_python_sha256(fetcher: &RuntimeFetcher, artifact_url: &str) -> Result<String> {
    let mut candidates: Vec<(String, Option<String>)> = vec![
        (format!("{}.sha256", artifact_url), None),
        (format!("{}.sha256sum", artifact_url), None),
    ];

    if let Some((release_base, filename)) = split_release_base_and_filename(artifact_url) {
        candidates.push((format!("{release_base}/SHA256SUMS"), Some(filename.clone())));
        candidates.push((format!("{release_base}/SHA256SUMS.txt"), Some(filename)));
    }

    let mut last_not_found = None;
    for (checksum_url, hint) in candidates {
        match fetcher
            .fetch_expected_sha256(&checksum_url, hint.as_deref())
            .await
        {
            Ok(sum) => return Ok(sum),
            Err(CapsuleError::NotFound(_)) => {
                last_not_found = Some(checksum_url);
            }
            Err(err) => return Err(err),
        }
    }

    match download_and_sha256(artifact_url).await {
        Ok(sum) => Ok(sum),
        Err(_) => Err(CapsuleError::NotFound(
            last_not_found.unwrap_or_else(|| artifact_url.to_string()),
        )),
    }
}

pub(super) async fn resolve_node_runtime(
    version: &str,
    platforms: &[RuntimePlatform],
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<RuntimeEntry> {
    let fetcher = Arc::new(RuntimeFetcher::new_with_reporter(reporter)?);
    let full_version = RuntimeFetcher::resolve_node_full_version(version).await?;
    let checksum_url = format!("https://nodejs.org/dist/v{}/SHASUMS256.txt", full_version);
    let targets = try_join_all(platforms.iter().copied().map(|platform| {
        let fetcher = Arc::clone(&fetcher);
        let checksum_url = checksum_url.clone();
        let full_version = full_version.clone();
        async move {
            let (filename, _is_zip) =
                RuntimeFetcher::node_artifact_filename(&full_version, platform.os, platform.arch)?;
            let url = format!("https://nodejs.org/dist/v{}/{}", full_version, filename);
            let sha256 = cached_sha256(
                metadata_cache_path("runtime", "node", &full_version, platform.target_triple)?,
                || async {
                    fetcher
                        .fetch_expected_sha256(&checksum_url, Some(&filename))
                        .await
                },
            )
            .await?;
            Ok::<(String, RuntimeArtifact), CapsuleError>((
                platform.target_triple.to_string(),
                RuntimeArtifact { url, sha256 },
            ))
        }
    }))
    .await?
    .into_iter()
    .collect();

    Ok(RuntimeEntry {
        provider: "official".to_string(),
        version: full_version,
        targets,
    })
}

pub(super) async fn resolve_deno_runtime(
    version: &str,
    platforms: &[RuntimePlatform],
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<RuntimeEntry> {
    let fetcher = Arc::new(RuntimeFetcher::new_with_reporter(reporter)?);
    let version = version.to_string();
    let targets = try_join_all(platforms.iter().copied().filter_map(|platform| {
        let Ok(filename) = deno_artifact_filename(platform.os, platform.arch) else {
            return None;
        };
        let fetcher = Arc::clone(&fetcher);
        let version = version.clone();
        Some(async move {
            let url = format!(
                "https://github.com/denoland/deno/releases/download/v{}/{}",
                version, filename
            );
            let sha256 = resolve_deno_sha256_cached(
                fetcher.as_ref(),
                &version,
                platform.target_triple,
                &filename,
            )
            .await?;
            Ok::<(String, RuntimeArtifact), CapsuleError>((
                platform.target_triple.to_string(),
                RuntimeArtifact { url, sha256 },
            ))
        })
    }))
    .await?
    .into_iter()
    .collect();

    Ok(RuntimeEntry {
        provider: "official".to_string(),
        version,
        targets,
    })
}

pub(super) fn deno_artifact_filename(os: &str, arch: &str) -> Result<String> {
    let target = match (os, arch) {
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => {
            return Err(CapsuleError::Pack(format!(
                "Unsupported Deno platform: {} {}",
                os, arch
            )))
        }
    };
    Ok(format!("deno-{}.zip", target))
}

async fn resolve_deno_sha256(
    fetcher: &RuntimeFetcher,
    version: &str,
    filename: &str,
) -> Result<String> {
    let candidates = [
        (
            format!(
                "https://github.com/denoland/deno/releases/download/v{}/{}.sha256sum",
                version, filename
            ),
            None,
        ),
        (
            format!(
                "https://github.com/denoland/deno/releases/download/v{}/{}.sha256",
                version, filename
            ),
            None,
        ),
        (
            format!(
                "https://github.com/denoland/deno/releases/download/v{}/SHASUMS256.txt",
                version
            ),
            Some(filename),
        ),
    ];

    let mut last_not_found = None;
    for (checksum_url, hint) in candidates {
        match fetcher.fetch_expected_sha256(&checksum_url, hint).await {
            Ok(sum) => return Ok(sum),
            Err(CapsuleError::NotFound(_)) => {
                last_not_found = Some(checksum_url);
            }
            Err(err) => return Err(err),
        }
    }

    let artifact_url = format!(
        "https://github.com/denoland/deno/releases/download/v{}/{}",
        version, filename
    );
    match download_and_sha256(&artifact_url).await {
        Ok(sum) => Ok(sum),
        Err(_) => {
            let detail = last_not_found.unwrap_or_else(|| "Deno checksum".to_string());
            Err(CapsuleError::NotFound(detail))
        }
    }
}

async fn resolve_python_sha256_cached(
    fetcher: &RuntimeFetcher,
    version: &str,
    target_triple: &str,
    artifact_url: &str,
) -> Result<String> {
    cached_sha256(
        metadata_cache_path("runtime", "python", version, target_triple)?,
        || async { resolve_python_sha256(fetcher, artifact_url).await },
    )
    .await
}

async fn resolve_deno_sha256_cached(
    fetcher: &RuntimeFetcher,
    version: &str,
    target_triple: &str,
    filename: &str,
) -> Result<String> {
    cached_sha256(
        metadata_cache_path("runtime", "deno", version, target_triple)?,
        || async { resolve_deno_sha256(fetcher, version, filename).await },
    )
    .await
}

fn split_release_base_and_filename(url: &str) -> Option<(String, String)> {
    let idx = url.rfind('/')?;
    let base = url[..idx].to_string();
    let filename = url[idx + 1..].to_string();
    if filename.is_empty() {
        None
    } else {
        Some((base, filename))
    }
}

async fn download_and_sha256(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(CapsuleError::Network)?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(CapsuleError::Network)?;
    if !response.status().is_success() {
        return Err(CapsuleError::NotFound(url.to_string()));
    }
    let bytes = response.bytes().await.map_err(CapsuleError::Network)?;
    Ok(sha256_hex(&bytes))
}

pub(super) fn resolve_pnpm_tool_targets(platforms: &[RuntimePlatform]) -> ToolTargets {
    let targets = platforms
        .iter()
        .map(|platform| {
            (
                platform.target_triple.to_string(),
                ToolArtifact {
                    url: format!(
                        "https://registry.npmjs.org/pnpm/-/pnpm-{}.tgz",
                        PNPM_VERSION
                    ),
                    sha256: None,
                    version: Some(PNPM_VERSION.to_string()),
                },
            )
        })
        .collect();
    ToolTargets { targets }
}

pub(super) async fn resolve_uv_tool_targets(
    platforms: &[RuntimePlatform],
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<ToolTargets> {
    let fetcher = Arc::new(RuntimeFetcher::new_with_reporter(reporter)?);
    let targets = try_join_all(platforms.iter().copied().filter_map(|platform| {
        let url = uv_artifact_url(platform.target_triple)?;
        let fetcher = Arc::clone(&fetcher);
        Some(async move {
            let sha256 = cached_sha256(
                metadata_cache_path("tool", "uv", UV_VERSION, platform.target_triple)?,
                || async {
                    fetcher
                        .fetch_expected_sha256(&(url.clone() + ".sha256"), None)
                        .await
                },
            )
            .await?;
            Ok::<(String, ToolArtifact), CapsuleError>((
                platform.target_triple.to_string(),
                ToolArtifact {
                    url,
                    sha256: Some(sha256),
                    version: Some(UV_VERSION.to_string()),
                },
            ))
        })
    }))
    .await?
    .into_iter()
    .collect();

    Ok(ToolTargets { targets })
}

pub(super) fn uv_artifact_url(target_triple: &str) -> Option<String> {
    let extension = match target_triple {
        "x86_64-pc-windows-msvc" => "zip",
        "aarch64-pc-windows-msvc" => return None,
        _ => "tar.gz",
    };
    Some(format!(
        "https://github.com/astral-sh/uv/releases/download/{0}/uv-{1}.{2}",
        UV_VERSION, target_triple, extension
    ))
}

/// Yarn Classic is a plain Node.js package — same URL for all platforms.
pub(super) fn resolve_yarn_tool_targets(platforms: &[RuntimePlatform]) -> ToolTargets {
    let targets = platforms
        .iter()
        .map(|platform| {
            (
                platform.target_triple.to_string(),
                ToolArtifact {
                    url: format!(
                        "https://registry.npmjs.org/yarn/-/yarn-{}.tgz",
                        YARN_CLASSIC_VERSION
                    ),
                    sha256: None,
                    version: Some(YARN_CLASSIC_VERSION.to_string()),
                },
            )
        })
        .collect();
    ToolTargets { targets }
}

/// Bun is a native binary; the download URL is platform-specific.
pub(super) fn resolve_bun_tool_targets(platforms: &[RuntimePlatform]) -> ToolTargets {
    let targets = platforms
        .iter()
        .filter_map(|platform| {
            let bun_triple = bun_platform_triple(platform.target_triple)?;
            Some((
                platform.target_triple.to_string(),
                ToolArtifact {
                    url: format!(
                        "https://github.com/oven-sh/bun/releases/download/bun-v{}/bun-{}.zip",
                        BUN_VERSION, bun_triple
                    ),
                    sha256: None,
                    version: Some(BUN_VERSION.to_string()),
                },
            ))
        })
        .collect();
    ToolTargets { targets }
}

fn bun_platform_triple(rust_triple: &str) -> Option<&'static str> {
    match rust_triple {
        "aarch64-apple-darwin" => Some("darwin-aarch64"),
        "x86_64-apple-darwin" => Some("darwin-x86_64"),
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => Some("linux-x64"),
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => Some("linux-aarch64"),
        "x86_64-pc-windows-msvc" => Some("windows-x64.exe"),
        _ => None,
    }
}

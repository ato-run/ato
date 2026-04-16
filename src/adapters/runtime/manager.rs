#![allow(dead_code)]

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::{env, ffi::OsString};

use anyhow::{bail, Context, Result};
use capsule_core::bootstrap::{BootstrapBoundary, BootstrapSubjectKind};
use capsule_core::common::paths::runtime_cache_dir;
use capsule_core::lockfile::{
    parse_lockfile_text, resolve_existing_lockfile_path, CapsuleLock, RuntimeArtifact,
    RuntimeEntry, ToolArtifact, CAPSULE_LOCK_FILE_NAME,
};
use capsule_core::packers::runtime_fetcher::RuntimeFetcher;
use capsule_core::router::ManifestData;
use fs2::FileExt;
use sha2::{Digest, Sha256};

pub struct RuntimeManager {
    lockfile: CapsuleLock,
    target_triple: String,
    platform_key: String,
    cache_root: PathBuf,
}

impl RuntimeManager {
    pub fn for_plan(plan: &ManifestData) -> Result<Self> {
        let lockfile_path = resolve_existing_lockfile_path(&plan.manifest_dir)
            .unwrap_or_else(|| plan.manifest_dir.join(CAPSULE_LOCK_FILE_NAME));
        let lockfile_raw = match fs::read_to_string(&lockfile_path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                bail!(
                    "Missing {} in capsule payload ({}). Rebuild and republish this capsule with the latest ato-cli.",
                    CAPSULE_LOCK_FILE_NAME,
                    lockfile_path.display()
                );
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("Failed to read {}", lockfile_path.display()));
            }
        };
        let lockfile: CapsuleLock = parse_lockfile_text(&lockfile_raw, &lockfile_path)
            .with_context(|| format!("Failed to parse {}", lockfile_path.display()))?;
        let (target_triple, platform_key) = current_platform_keys()?;
        let cache_root = runtime_cache_dir()?;
        Ok(Self {
            lockfile,
            target_triple,
            platform_key,
            cache_root,
        })
    }

    pub fn ensure_deno_binary_for_plan(&self, plan: &ManifestData) -> Result<PathBuf> {
        let runtime = self
            .lockfile
            .runtimes
            .as_ref()
            .and_then(|r| r.deno.as_ref())
            .ok_or_else(|| anyhow::anyhow!("capsule.lock.json is missing runtimes.deno entry"))?;
        self.ensure_runtime_entry(
            "deno",
            runtime,
            required_runtime_version(plan, &["deno"])?,
            &["deno", "deno.exe"],
        )
    }

    pub fn ensure_node_binary_for_plan(&self, plan: &ManifestData) -> Result<PathBuf> {
        let runtime = self
            .lockfile
            .runtimes
            .as_ref()
            .and_then(|r| r.node.as_ref())
            .ok_or_else(|| anyhow::anyhow!("capsule.lock.json is missing runtimes.node entry"))?;
        self.ensure_runtime_entry(
            "node",
            runtime,
            required_runtime_tool_version(plan, "node")?
                .or(required_runtime_version(plan, &["node"])?),
            &["node", "node.exe"],
        )
    }

    pub fn ensure_python_binary_for_plan(&self, plan: &ManifestData) -> Result<PathBuf> {
        let runtime = self
            .lockfile
            .runtimes
            .as_ref()
            .and_then(|r| r.python.as_ref())
            .ok_or_else(|| anyhow::anyhow!("capsule.lock.json is missing runtimes.python entry"))?;
        self.ensure_runtime_entry(
            "python",
            runtime,
            required_runtime_tool_version(plan, "python")?
                .or(required_runtime_version(plan, &["python"])?),
            &["python", "python3", "python.exe"],
        )
    }

    pub fn ensure_uv_binary_for_plan(&self, _plan: &ManifestData) -> Result<PathBuf> {
        let artifact = self
            .lockfile
            .tools
            .as_ref()
            .and_then(|tools| tools.uv.as_ref())
            .and_then(|uv| {
                select_tool_artifact(&uv.targets, &self.target_triple, &self.platform_key)
            })
            .ok_or_else(|| {
                anyhow::anyhow!("capsule.lock.json is missing tools.uv entry for this platform")
            })?;
        self.ensure_tool_artifact("uv", artifact, &["uv", "uv.exe"])
    }

    fn ensure_runtime_entry(
        &self,
        runtime_name: &str,
        runtime: &RuntimeEntry,
        required_version: Option<String>,
        candidates: &[&str],
    ) -> Result<PathBuf> {
        if let Some(required) = required_version {
            if required != runtime.version {
                bail!(
                    "capsule.lock.json {} version mismatch (manifest={}, lock={})",
                    runtime_name,
                    required,
                    runtime.version
                );
            }
        }
        let artifact =
            select_runtime_artifact(&runtime.targets, &self.target_triple, &self.platform_key)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "capsule.lock.json is missing {} runtime artifact for this platform",
                        runtime_name
                    )
                })?;
        self.install_artifact(runtime_name, &runtime.version, artifact, candidates)
    }

    fn ensure_tool_artifact(
        &self,
        tool_name: &str,
        artifact: &ToolArtifact,
        candidates: &[&str],
    ) -> Result<PathBuf> {
        let version = artifact.version.as_deref().ok_or_else(|| {
            anyhow::anyhow!("capsule.lock.json {} tool version is missing", tool_name)
        })?;
        let sha256 = artifact.sha256.as_deref().ok_or_else(|| {
            anyhow::anyhow!("capsule.lock.json {} tool sha256 is missing", tool_name)
        })?;
        self.install_archive(tool_name, version, &artifact.url, sha256, candidates)
    }

    fn install_artifact(
        &self,
        name: &str,
        version: &str,
        artifact: &RuntimeArtifact,
        candidates: &[&str],
    ) -> Result<PathBuf> {
        self.install_archive(name, version, &artifact.url, &artifact.sha256, candidates)
    }

    fn install_archive(
        &self,
        name: &str,
        version: &str,
        url: &str,
        sha256: &str,
        candidates: &[&str],
    ) -> Result<PathBuf> {
        let install_dir = self
            .cache_root
            .join(name)
            .join(version)
            .join(&self.target_triple);
        if let Some(existing) = find_binary_recursive(&install_dir, candidates)? {
            return Ok(existing);
        }

        fs::create_dir_all(&self.cache_root)?;
        let lock_file_path = self
            .cache_root
            .join(".locks")
            .join(format!("{}-{}-{}.lock", name, version, self.target_triple));
        if let Some(parent) = lock_file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_file_path)
            .with_context(|| format!("Failed to open runtime lock {}", lock_file_path.display()))?;
        lock_file.lock_exclusive().with_context(|| {
            format!(
                "Failed to lock runtime install {}",
                lock_file_path.display()
            )
        })?;

        let result = (|| -> Result<PathBuf> {
            if let Some(existing) = find_binary_recursive(&install_dir, candidates)? {
                return Ok(existing);
            }

            let download_dir = self.cache_root.join(".downloads");
            fs::create_dir_all(&download_dir)?;
            let archive_ext = archive_extension_from_url(url)?;
            let archive_path = download_dir.join(format!(
                "{}-{}-{}.{}",
                name, version, self.target_triple, archive_ext
            ));

            download_to_file(url, &archive_path)?;
            verify_sha256(&archive_path, sha256)?;

            let temp_dir = self
                .cache_root
                .join(".tmp")
                .join(format!("{}-{}-{}", name, version, self.target_triple));
            if temp_dir.exists() {
                fs::remove_dir_all(&temp_dir)?;
            }
            fs::create_dir_all(&temp_dir)?;
            extract_archive(&archive_path, &temp_dir, archive_ext)?;

            if install_dir.exists() {
                fs::remove_dir_all(&install_dir)?;
            }
            if let Some(parent) = install_dir.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&temp_dir, &install_dir).with_context(|| {
                format!(
                    "Failed to move extracted runtime into cache: {}",
                    install_dir.display()
                )
            })?;
            let _ = fs::remove_file(&archive_path);

            find_binary_recursive(&install_dir, candidates)?
                .ok_or_else(|| anyhow::anyhow!("Installed runtime is missing expected binary"))
        })();

        lock_file.unlock().ok();
        result
    }
}

pub fn ensure_deno_binary(plan: &ManifestData) -> Result<PathBuf> {
    RuntimeManager::for_plan(plan)?.ensure_deno_binary_for_plan(plan)
}

/// Like `ensure_deno_binary`, but falls back to the default Deno version when
/// `capsule.lock.json` is absent (e.g., decap'd shared workspaces that have no
/// published lock file). Used by the static file-server path where any Deno version
/// is acceptable.
pub fn ensure_deno_binary_for_file_server(plan: &ManifestData) -> Result<PathBuf> {
    match RuntimeManager::for_plan(plan) {
        Ok(manager) => manager.ensure_deno_binary_for_plan(plan),
        Err(e) => {
            if e.to_string().contains(CAPSULE_LOCK_FILE_NAME) {
                let version = capsule_core::packers::lockfile::DEFAULT_DENO_VERSION;
                block_on_runtime_fetch(
                    async move { RuntimeFetcher::new()?.ensure_deno(version).await },
                )
            } else {
                Err(e)
            }
        }
    }
}

pub fn ensure_deno_binary_with_authority(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
) -> Result<PathBuf> {
    let version = required_runtime_tool_version(plan, "deno")?;
    if authoritative_lock.is_some() {
        // For source runs (authoritative AtoLock present), use RuntimeFetcher directly
        // since there is no capsule.lock.json. Deno is used as the Node.js compat runtime,
        // so no explicit version is required — fall back to the well-known default.
        let version_str = version
            .unwrap_or_else(|| capsule_core::packers::lockfile::DEFAULT_DENO_VERSION.to_string());
        return block_on_runtime_fetch(async move {
            RuntimeFetcher::new()?.ensure_deno(&version_str).await
        });
    }
    if let Some(version) = version {
        return block_on_runtime_fetch(async move {
            RuntimeFetcher::new()?.ensure_deno(&version).await
        });
    }
    ensure_deno_binary(plan)
}

pub fn ensure_python_binary(plan: &ManifestData) -> Result<PathBuf> {
    RuntimeManager::for_plan(plan)?.ensure_python_binary_for_plan(plan)
}

pub fn ensure_python_binary_with_authority(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
) -> Result<PathBuf> {
    let version = required_runtime_tool_version(plan, "python")?
        .or(required_runtime_version(plan, &["python"])?);
    if authoritative_lock.is_some() {
        let version = version.ok_or_else(|| {
            anyhow::anyhow!(
                "targets.{}.runtime_version or runtime_tools.python is required for authoritative python execution",
                plan.selected_target_label()
            )
        })?;
        return block_on_runtime_fetch(async move {
            RuntimeFetcher::new()?.ensure_python(&version).await
        });
    }
    // For local source runs (no authoritative_lock), use runtime_version from the manifest
    // if available, before falling back to capsule.lock.json.
    if let Some(version) = version {
        return block_on_runtime_fetch(async move {
            RuntimeFetcher::new()?.ensure_python(&version).await
        });
    }
    ensure_python_binary(plan)
}

pub fn ensure_node_binary(plan: &ManifestData) -> Result<PathBuf> {
    RuntimeManager::for_plan(plan)?.ensure_node_binary_for_plan(plan)
}

pub fn ensure_node_binary_with_authority(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
) -> Result<PathBuf> {
    let version =
        required_runtime_tool_version(plan, "node")?.or(required_runtime_version(plan, &["node"])?);
    if authoritative_lock.is_some() {
        let version = version.ok_or_else(|| {
            anyhow::anyhow!(
                "targets.{}.runtime_version or runtime_tools.node is required for authoritative node execution",
                plan.selected_target_label()
            )
        })?;
        return block_on_runtime_fetch(async move {
            RuntimeFetcher::new()?.ensure_node(&version).await
        });
    }
    // For local source runs (no authoritative_lock), use runtime_version from the manifest
    // if available (e.g., auto-provisioned capsule.toml), before falling back to capsule.lock.json.
    if let Some(version) = version {
        return block_on_runtime_fetch(async move {
            RuntimeFetcher::new()?.ensure_node(&version).await
        });
    }
    ensure_node_binary(plan)
}

pub fn ensure_uv_binary(plan: &ManifestData) -> Result<PathBuf> {
    RuntimeManager::for_plan(plan)?.ensure_uv_binary_for_plan(plan)
}

pub fn ensure_uv_binary_with_authority(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
) -> Result<PathBuf> {
    if authoritative_lock.is_some() {
        let version = required_runtime_tool_version(plan, "uv")?;
        return block_on_runtime_fetch(async move {
            RuntimeFetcher::new()?.ensure_uv(version.as_deref()).await
        });
    }
    ensure_uv_binary(plan)
}

fn block_on_runtime_fetch<F>(future: F) -> Result<PathBuf>
where
    F: std::future::Future<Output = capsule_core::Result<PathBuf>> + Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        return tokio::task::block_in_place(|| {
            let _ = handle;
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(anyhow::Error::from)?;
                runtime
                    .block_on(future)
                    .map_err(|error| anyhow::anyhow!(error.to_string()))
            })
            .join()
            .map_err(|_| anyhow::anyhow!("runtime fetch thread panicked"))?
        });
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime
        .block_on(future)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn ensure_host_capability_for_authoritative_lock(
    boundary: BootstrapBoundary,
    candidates: &[&str],
) -> Result<PathBuf> {
    debug_assert!(matches!(
        boundary.subject_kind,
        BootstrapSubjectKind::Runtime | BootstrapSubjectKind::Tool
    ));
    find_runtime_on_path(candidates)
        .ok_or_else(|| anyhow::anyhow!(boundary.missing_on_path_message()))
}

fn find_runtime_on_path(candidates: &[&str]) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let path_exts = executable_extensions();

    for directory in env::split_paths(&path) {
        for candidate in candidates {
            let direct = directory.join(candidate);
            if direct.is_file() {
                return Some(direct);
            }
            for extension in &path_exts {
                let with_extension = directory.join(format!("{}{}", candidate, extension));
                if with_extension.is_file() {
                    return Some(with_extension);
                }
            }
        }
    }

    None
}

fn executable_extensions() -> Vec<String> {
    if cfg!(windows) {
        env::var_os("PATHEXT")
            .map(split_windows_path_exts)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| vec![".exe".to_string(), ".cmd".to_string(), ".bat".to_string()])
    } else {
        Vec::new()
    }
}

fn split_windows_path_exts(value: OsString) -> Vec<String> {
    value
        .to_string_lossy()
        .split(';')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn required_runtime_version(plan: &ManifestData, drivers: &[&str]) -> Result<Option<String>> {
    let runtime = match plan.execution_runtime() {
        Some(runtime) => runtime,
        None => return Ok(None),
    };
    let runtime_is_source = runtime.eq_ignore_ascii_case("source");
    let runtime_is_web = runtime.eq_ignore_ascii_case("web");
    if !runtime_is_source && !runtime_is_web {
        return Ok(None);
    }
    let driver = match plan.execution_driver() {
        Some(driver) => driver,
        None => return Ok(None),
    };
    let supports_driver = drivers.iter().any(|d| driver.eq_ignore_ascii_case(d));
    if !supports_driver {
        return Ok(None);
    }
    // web/deno orchestrator also pins runtime_version deterministically.
    if runtime_is_web && !driver.eq_ignore_ascii_case("deno") {
        return Ok(None);
    }
    let value = plan.execution_runtime_version().ok_or_else(|| {
        anyhow::anyhow!(
            "targets.{}.runtime_version is required for runtime '{}' driver '{}'",
            plan.selected_target_label(),
            runtime,
            driver
        )
    })?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!(
            "targets.{}.runtime_version is required for runtime '{}' driver '{}'",
            plan.selected_target_label(),
            runtime,
            driver
        );
    }
    Ok(Some(trimmed.to_string()))
}

fn required_runtime_tool_version(plan: &ManifestData, tool: &str) -> Result<Option<String>> {
    let value = match plan.execution_runtime_tool_version(tool) {
        Some(value) => value,
        None => return Ok(None),
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!(
            "targets.{}.runtime_tools.{} must be a non-empty string",
            plan.selected_target_label(),
            tool
        );
    }
    Ok(Some(trimmed.to_string()))
}

fn select_runtime_artifact<'a>(
    targets: &'a std::collections::HashMap<String, RuntimeArtifact>,
    target_triple: &str,
    platform_key: &str,
) -> Option<&'a RuntimeArtifact> {
    targets
        .get(target_triple)
        .or_else(|| targets.get(platform_key))
}

fn select_tool_artifact<'a>(
    targets: &'a std::collections::HashMap<String, ToolArtifact>,
    target_triple: &str,
    platform_key: &str,
) -> Option<&'a ToolArtifact> {
    targets
        .get(target_triple)
        .or_else(|| targets.get(platform_key))
}

fn current_platform_keys() -> Result<(String, String)> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        bail!("Unsupported OS");
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        bail!("Unsupported architecture");
    };

    let target_triple = match (os, arch) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => bail!("Unsupported platform"),
    };

    Ok((target_triple.to_string(), format!("{}-{}", os, arch)))
}

fn archive_extension_from_url(url: &str) -> Result<&'static str> {
    if url.ends_with(".tar.gz") {
        return Ok("tar.gz");
    }
    if url.ends_with(".tgz") {
        return Ok("tgz");
    }
    if url.ends_with(".zip") {
        return Ok("zip");
    }
    bail!("Unsupported runtime archive format: {}", url);
}

fn download_to_file(url: &str, dest: &Path) -> Result<()> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(download_to_file_async(url, dest)))
    } else {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(download_to_file_async(url, dest))
    }
}

async fn download_to_file_async(url: &str, dest: &Path) -> Result<()> {
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to download runtime from {}", url))?;

    if !response.status().is_success() {
        bail!(
            "Failed to fetch runtime artifact {} ({})",
            url,
            response.status()
        );
    }
    let bytes = response.bytes().await?;
    fs::write(dest, &bytes).with_context(|| format!("Failed to write {}", dest.display()))?;
    Ok(())
}

fn verify_sha256(path: &Path, expected_hex: &str) -> Result<()> {
    let mut file =
        File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 64];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = hex::encode(hasher.finalize());
    let expected = expected_hex.strip_prefix("sha256:").unwrap_or(expected_hex);
    if !actual.eq_ignore_ascii_case(expected) {
        bail!(
            "Runtime checksum mismatch for {} (expected {}, got {})",
            path.display(),
            expected,
            actual
        );
    }
    Ok(())
}

fn extract_archive(archive_path: &Path, dest: &Path, extension: &str) -> Result<()> {
    match extension {
        "tar.gz" | "tgz" => {
            let file = File::open(archive_path)?;
            let decoder = flate2::read::GzDecoder::new(file);
            let mut archive = tar::Archive::new(decoder);
            archive.unpack(dest)?;
        }
        "zip" => {
            let file = File::open(archive_path)?;
            let mut zip = zip::ZipArchive::new(file)?;
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i)?;
                let Some(rel) = entry.enclosed_name().map(|p| p.to_owned()) else {
                    continue;
                };
                let out_path = dest.join(rel);
                if entry.is_dir() {
                    fs::create_dir_all(&out_path)?;
                    continue;
                }
                if let Some(parent) = out_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut out = File::create(&out_path)?;
                std::io::copy(&mut entry, &mut out)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Some(name) = out_path.file_name().and_then(|n| n.to_str()) {
                        if matches!(name, "deno" | "python" | "python3" | "uv" | "node") {
                            let mut perms = fs::metadata(&out_path)?.permissions();
                            perms.set_mode(0o755);
                            fs::set_permissions(&out_path, perms)?;
                        }
                    }
                }
            }
        }
        _ => bail!("Unsupported runtime archive extension {}", extension),
    }
    Ok(())
}

fn find_binary_recursive(root: &Path, candidates: &[&str]) -> Result<Option<PathBuf>> {
    if !root.exists() {
        return Ok(None);
    }
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        if !entry.file_type().is_file() && !entry.file_type().is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if candidates
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&name))
        {
            return Ok(Some(entry.path().to_path_buf()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::bootstrap::{BootstrapAuthorityKind, BootstrapClosureRole};
    use std::collections::HashMap;

    #[test]
    fn consumer_paths_do_not_spawn_git_commands() {
        let files = [
            "src/cli/commands/run.rs",
            "src/application/engine/install/mod.rs",
            "src/application/search/mod.rs",
            "src/adapters/runtime/executors/deno.rs",
            "src/adapters/runtime/executors/node_compat.rs",
            "src/adapters/runtime/executors/open_web.rs",
            "src/adapters/runtime/executors/source.rs",
        ];

        for rel in files {
            let absolute = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
            let raw = fs::read_to_string(&absolute)
                .unwrap_or_else(|_| panic!("failed to read {}", absolute.display()));
            assert!(
                !raw.contains("Command::new(\"git\")"),
                "consumer path contains git command: {}",
                absolute.display()
            );
            assert!(
                !raw.contains(" run_git("),
                "consumer path references git helper: {}",
                absolute.display()
            );
        }
    }

    #[test]
    fn select_runtime_artifact_returns_matching_linux_entry() {
        let targets = HashMap::from([
            (
                "aarch64-apple-darwin".to_string(),
                RuntimeArtifact {
                    url: "https://example.invalid/deno-aarch64-apple-darwin.zip".to_string(),
                    sha256: "sha256:mac".to_string(),
                },
            ),
            (
                "x86_64-unknown-linux-gnu".to_string(),
                RuntimeArtifact {
                    url: "https://example.invalid/deno-x86_64-unknown-linux-gnu.zip".to_string(),
                    sha256: "sha256:linux".to_string(),
                },
            ),
        ]);

        let artifact =
            select_runtime_artifact(&targets, "x86_64-unknown-linux-gnu", "linux-x86_64")
                .expect("linux runtime artifact");
        assert!(artifact.url.contains("x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn authoritative_lock_requires_host_runtime_boundary_for_node() {
        let boundary = BootstrapBoundary::host_runtime("node");
        assert_eq!(
            boundary.authority_kind,
            BootstrapAuthorityKind::HostCapability
        );
        assert_eq!(boundary.closure_role, BootstrapClosureRole::HostCapability);
        assert!(boundary
            .missing_on_path_message()
            .contains("host-local 'node' runtime on PATH"));
    }

    #[test]
    fn authoritative_lock_requires_host_tool_boundary_for_uv() {
        let boundary = BootstrapBoundary::host_tool("uv");
        assert_eq!(
            boundary.authority_kind,
            BootstrapAuthorityKind::HostCapability
        );
        assert_eq!(boundary.closure_role, BootstrapClosureRole::HostCapability);
        assert!(boundary
            .missing_on_path_message()
            .contains("host-local 'uv' tool on PATH"));
    }
}

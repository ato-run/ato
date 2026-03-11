use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, UNIX_EPOCH};

use chrono::Utc;
use fs2::FileExt;
use futures::future::try_join_all;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::common::paths::{nacelle_home_dir, toolchain_cache_dir};
use crate::error::{CapsuleError, Result};
use crate::packers::payload;
use crate::packers::runtime_fetcher::RuntimeFetcher;
use crate::reporter::CapsuleReporter;
use crate::types::CapsuleManifest;

const UV_VERSION: &str = "0.4.19";
const PNPM_VERSION: &str = "9.9.0";
const LOCKFILE_INPUT_SNAPSHOT_VERSION: u32 = 1;
const LOCKFILE_INPUT_SNAPSHOT_NAME: &str = ".capsule.lock.inputs.json";
const METADATA_CACHE_DIR_NAME: &str = "metadata-cache";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapsuleLock {
    pub version: String,
    pub meta: LockMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowlist: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtimes: Option<RuntimeSection>,
    #[serde(default)]
    pub targets: HashMap<String, TargetEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockMeta {
    pub created_at: String,
    pub manifest_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uv: Option<ToolTargets>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pnpm: Option<ToolTargets>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTargets {
    pub targets: HashMap<String, ToolArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolArtifact {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python: Option<RuntimeEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deno: Option<RuntimeEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<RuntimeEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub java: Option<RuntimeEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dotnet: Option<RuntimeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEntry {
    pub provider: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub targets: HashMap<String, RuntimeArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeArtifact {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TargetEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_lockfile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_lockfile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constraints: Option<TargetConstraints>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compiled: Option<CompiledEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetConstraints {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glibc: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledEntry {
    pub entrypoint: String,
    pub artifacts: CompiledArtifact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledArtifact {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactEntry {
    pub filename: String,
    pub url: String,
    pub sha256: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LockfileInputSnapshot {
    version: u32,
    manifest_hash: String,
    files: Vec<LockfileInputState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LockfileInputState {
    path: String,
    size: u64,
    modified_ns: u128,
    inode: Option<u64>,
}

struct OpenLockfileInput {
    path: PathBuf,
    state: LockfileInputState,
    #[allow(dead_code)]
    file: fs::File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimePlatform {
    os: &'static str,
    arch: &'static str,
    target_triple: &'static str,
}

const SUPPORTED_RUNTIME_PLATFORMS: &[RuntimePlatform] = &[
    RuntimePlatform {
        os: "macos",
        arch: "x86_64",
        target_triple: "x86_64-apple-darwin",
    },
    RuntimePlatform {
        os: "macos",
        arch: "aarch64",
        target_triple: "aarch64-apple-darwin",
    },
    RuntimePlatform {
        os: "linux",
        arch: "x86_64",
        target_triple: "x86_64-unknown-linux-gnu",
    },
    RuntimePlatform {
        os: "linux",
        arch: "aarch64",
        target_triple: "aarch64-unknown-linux-gnu",
    },
    RuntimePlatform {
        os: "windows",
        arch: "x86_64",
        target_triple: "x86_64-pc-windows-msvc",
    },
    RuntimePlatform {
        os: "windows",
        arch: "aarch64",
        target_triple: "aarch64-pc-windows-msvc",
    },
];

pub async fn generate_and_write_lockfile(
    manifest_path: &Path,
    manifest_raw: &toml::Value,
    manifest_text: &str,
    reporter: Arc<dyn CapsuleReporter + 'static>,
    timings: bool,
) -> Result<PathBuf> {
    let manifest_dir = manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let lockfile = generate_lockfile(
        manifest_raw,
        manifest_text,
        &manifest_dir,
        reporter,
        timings,
    )
    .await?;
    let output_path = manifest_dir.join("capsule.lock");
    let content = toml::to_string_pretty(&lockfile)
        .map_err(|e| CapsuleError::Pack(format!("Failed to serialize capsule.lock: {}", e)))?;
    write_atomic_bytes_with_os_lock(
        &output_path,
        content.as_bytes(),
        "capsule.lock",
        capsule_error_pack,
    )?;
    Ok(output_path)
}

pub async fn ensure_lockfile(
    manifest_path: &Path,
    manifest_raw: &toml::Value,
    manifest_text: &str,
    reporter: Arc<dyn CapsuleReporter + 'static>,
    timings: bool,
) -> Result<PathBuf> {
    let ensure_started = Instant::now();
    let manifest_dir = manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let lock_path = manifest_dir.join("capsule.lock");
    let mut inputs = open_lockfile_inputs(manifest_path, &manifest_dir, manifest_raw)?;
    let manifest_hash = manifest_hash_from_open_inputs(&mut inputs, manifest_path)?;

    if lock_path.exists()
        && verify_lockfile_manifest_hash(&lock_path, &manifest_hash).is_ok()
        && lockfile_inputs_snapshot_matches(&manifest_dir, &manifest_hash, &inputs)?
        && existing_lockfile_has_required_platform_coverage(&lock_path, manifest_raw)?
    {
        maybe_report_timing(
            &reporter,
            timings,
            "lockfile.reuse_hit",
            ensure_started.elapsed(),
        )
        .await?;
        return Ok(lock_path);
    }

    let generated = generate_and_write_lockfile(
        manifest_path,
        manifest_raw,
        manifest_text,
        reporter.clone(),
        timings,
    )
    .await?;
    write_lockfile_inputs_snapshot(&manifest_dir, &manifest_hash, &inputs)?;
    maybe_report_timing(
        &reporter,
        timings,
        "lockfile.ensure_total",
        ensure_started.elapsed(),
    )
    .await?;
    Ok(generated)
}

pub fn verify_lockfile_manifest(manifest_path: &Path, lockfile_path: &Path) -> Result<()> {
    let mut manifest_file = fs::File::open(manifest_path)
        .map_err(|e| CapsuleError::Config(format!("Failed to read manifest: {}", e)))?;
    verify_lockfile_manifest_with_open_manifest(&mut manifest_file, lockfile_path)
}

pub fn render_lockfile_for_manifest(
    lockfile_path: &Path,
    manifest: &CapsuleManifest,
) -> Result<Vec<u8>> {
    let mut lockfile = read_lockfile(lockfile_path)?;
    lockfile.meta.manifest_hash = semantic_manifest_hash(manifest)?;
    toml::to_string_pretty(&lockfile)
        .map(|toml| toml.into_bytes())
        .map_err(|e| CapsuleError::Pack(format!("Failed to serialize capsule.lock: {}", e)))
}

fn verify_lockfile_manifest_with_open_manifest(
    manifest_file: &mut fs::File,
    lockfile_path: &Path,
) -> Result<()> {
    let expected_hash = manifest_hash_from_open_file(manifest_file)?;
    verify_lockfile_manifest_hash(lockfile_path, &expected_hash)
}

fn verify_lockfile_manifest_hash(lockfile_path: &Path, expected_hash: &str) -> Result<()> {
    let lockfile = read_lockfile(lockfile_path)?;
    if lockfile.meta.manifest_hash != expected_hash {
        return Err(CapsuleError::Config(format!(
            "capsule.lock manifest hash mismatch (expected {}, got {})",
            expected_hash, lockfile.meta.manifest_hash
        )));
    }
    Ok(())
}

fn read_lockfile(path: &Path) -> Result<CapsuleLock> {
    let raw = fs::read_to_string(path)
        .map_err(|e| CapsuleError::Config(format!("Failed to read capsule.lock: {}", e)))?;
    toml::from_str(&raw)
        .map_err(|e| CapsuleError::Config(format!("Failed to parse capsule.lock: {}", e)))
}

fn existing_lockfile_has_required_platform_coverage(
    lockfile_path: &Path,
    manifest: &toml::Value,
) -> Result<bool> {
    let lockfile = read_lockfile(lockfile_path)?;
    lockfile_has_required_platform_coverage(&lockfile, manifest)
}

fn lockfile_has_required_platform_coverage(
    lockfile: &CapsuleLock,
    manifest: &toml::Value,
) -> Result<bool> {
    let required_platforms = lockfile_runtime_platforms(manifest)?;
    if required_platforms.len() <= 1 {
        return Ok(true);
    }

    let runtime_targets = lockfile.runtimes.as_ref();

    if let Some(targets) = runtime_targets
        .and_then(|r| r.deno.as_ref())
        .map(|entry| &entry.targets)
    {
        let deno_platforms = supported_deno_platforms(&required_platforms);
        if deno_platforms
            .iter()
            .any(|platform| !targets.contains_key(platform.target_triple))
        {
            return Ok(false);
        }
    }

    if let Some(targets) = runtime_targets
        .and_then(|r| r.python.as_ref())
        .map(|entry| &entry.targets)
    {
        let python_platforms = supported_python_platforms(&required_platforms, &lockfile.runtimes);
        if python_platforms
            .iter()
            .any(|platform| !targets.contains_key(platform.target_triple))
        {
            return Ok(false);
        }
    }

    let runtime_target_sets = [runtime_targets
        .and_then(|r| r.node.as_ref())
        .map(|entry| &entry.targets)];

    for targets in runtime_target_sets.into_iter().flatten() {
        if required_platforms
            .iter()
            .any(|platform| !targets.contains_key(platform.target_triple))
        {
            return Ok(false);
        }
    }

    if let Some(targets) = lockfile
        .tools
        .as_ref()
        .and_then(|tools| tools.uv.as_ref())
        .map(|entry| &entry.targets)
    {
        let uv_platforms = supported_uv_platforms(&required_platforms);
        if uv_platforms
            .iter()
            .any(|platform| !targets.contains_key(platform.target_triple))
        {
            return Ok(false);
        }
    }

    Ok(true)
}

fn supported_deno_platforms(platforms: &[RuntimePlatform]) -> Vec<RuntimePlatform> {
    platforms
        .iter()
        .copied()
        .filter(|platform| deno_artifact_filename(platform.os, platform.arch).is_ok())
        .collect()
}

fn supported_python_platforms(
    platforms: &[RuntimePlatform],
    runtimes: &Option<RuntimeSection>,
) -> Vec<RuntimePlatform> {
    let version = runtimes
        .as_ref()
        .and_then(|runtime| runtime.python.as_ref())
        .map(|python| python.version.as_str())
        .unwrap_or("3.11.10");
    platforms
        .iter()
        .copied()
        .filter(|platform| {
            RuntimeFetcher::get_python_download_url(version, platform.os, platform.arch).is_ok()
        })
        .collect()
}

fn supported_uv_platforms(platforms: &[RuntimePlatform]) -> Vec<RuntimePlatform> {
    platforms
        .iter()
        .copied()
        .filter(|platform| uv_artifact_url(platform.target_triple).is_some())
        .collect()
}

fn manifest_hash_from_open_inputs(
    inputs: &mut [OpenLockfileInput],
    manifest_path: &Path,
) -> Result<String> {
    let manifest = inputs
        .iter_mut()
        .find(|input| input.path == manifest_path)
        .ok_or_else(|| {
            CapsuleError::Config(format!(
                "Failed to locate opened manifest input: {}",
                manifest_path.display()
            ))
        })?;
    manifest_hash_from_open_file(&mut manifest.file)
}

fn manifest_hash_from_open_file(file: &mut fs::File) -> Result<String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|e| CapsuleError::Config(format!("Failed to seek manifest: {}", e)))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|e| CapsuleError::Config(format!("Failed to read manifest: {}", e)))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| CapsuleError::Config(format!("Failed to rewind manifest: {}", e)))?;
    semantic_manifest_hash_from_text(&text)
}

fn lockfile_inputs_snapshot_path(manifest_dir: &Path) -> PathBuf {
    manifest_dir.join(LOCKFILE_INPUT_SNAPSHOT_NAME)
}

fn write_lockfile_inputs_snapshot(
    manifest_dir: &Path,
    manifest_hash: &str,
    inputs: &[OpenLockfileInput],
) -> Result<()> {
    let snapshot = LockfileInputSnapshot {
        version: LOCKFILE_INPUT_SNAPSHOT_VERSION,
        manifest_hash: manifest_hash.to_string(),
        files: inputs.iter().map(|i| i.state.clone()).collect(),
    };
    let raw = serde_json::to_vec_pretty(&snapshot).map_err(|e| {
        CapsuleError::Config(format!("Failed to serialize lockfile input snapshot: {e}"))
    })?;
    let snapshot_path = lockfile_inputs_snapshot_path(manifest_dir);
    write_atomic_bytes_with_os_lock(
        &snapshot_path,
        &raw,
        "lockfile input snapshot",
        capsule_error_config,
    )?;
    Ok(())
}

fn lockfile_inputs_snapshot_matches(
    manifest_dir: &Path,
    manifest_hash: &str,
    inputs: &[OpenLockfileInput],
) -> Result<bool> {
    let snapshot_path = lockfile_inputs_snapshot_path(manifest_dir);
    if !snapshot_path.exists() {
        return Ok(false);
    }
    let raw = fs::read_to_string(&snapshot_path).map_err(|e| {
        CapsuleError::Config(format!("Failed to read lockfile input snapshot: {}", e))
    })?;
    let snapshot: LockfileInputSnapshot = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };

    if snapshot.version != LOCKFILE_INPUT_SNAPSHOT_VERSION {
        return Ok(false);
    }
    if snapshot.manifest_hash != manifest_hash {
        return Ok(false);
    }

    let current: Vec<LockfileInputState> = inputs.iter().map(|i| i.state.clone()).collect();
    Ok(snapshot.files == current)
}

fn open_lockfile_inputs(
    manifest_path: &Path,
    manifest_dir: &Path,
    manifest_raw: &toml::Value,
) -> Result<Vec<OpenLockfileInput>> {
    let mut paths = collect_lockfile_input_paths(manifest_path, manifest_dir, manifest_raw);
    paths.sort();
    paths.dedup();

    let mut inputs = Vec::new();
    for path in paths {
        if !path.exists() || !path.is_file() {
            continue;
        }
        let file = fs::File::open(&path).map_err(|e| {
            CapsuleError::Config(format!(
                "Failed to open lockfile input {}: {}",
                path.display(),
                e
            ))
        })?;
        let metadata = file.metadata().map_err(|e| {
            CapsuleError::Config(format!(
                "Failed to read metadata for {}: {}",
                path.display(),
                e
            ))
        })?;
        let state = lockfile_input_state(manifest_dir, &path, &metadata)?;
        inputs.push(OpenLockfileInput { path, state, file });
    }
    inputs.sort_by(|a, b| a.state.path.cmp(&b.state.path));
    Ok(inputs)
}

fn collect_lockfile_input_paths(
    manifest_path: &Path,
    manifest_dir: &Path,
    manifest_raw: &toml::Value,
) -> Vec<PathBuf> {
    let mut paths = vec![manifest_path.to_path_buf()];
    for name in [
        "package.json",
        "package-lock.json",
        "pnpm-lock.yaml",
        "pyproject.toml",
        "requirements.txt",
        "deno.json",
        "deno.jsonc",
        "uv.lock",
    ] {
        paths.push(manifest_dir.join(name));
    }

    for language in ["python", "node"] {
        if let Some(path) = read_dependencies_path(manifest_raw, language, manifest_dir) {
            paths.push(path);
        }
    }

    paths
}

fn lockfile_input_state(
    manifest_dir: &Path,
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<LockfileInputState> {
    let rel = path
        .strip_prefix(manifest_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|ts| ts.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    Ok(LockfileInputState {
        path: rel,
        size: metadata.len(),
        modified_ns,
        inode: metadata_inode(metadata),
    })
}

#[cfg(unix)]
fn metadata_inode(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.ino())
}

#[cfg(not(unix))]
fn metadata_inode(_: &fs::Metadata) -> Option<u64> {
    None
}

async fn generate_lockfile(
    manifest_raw: &toml::Value,
    manifest_text: &str,
    manifest_dir: &Path,
    reporter: Arc<dyn CapsuleReporter + 'static>,
    timings: bool,
) -> Result<CapsuleLock> {
    let allowlist = read_allowlist(manifest_raw);
    let target_key = platform_target_key()?;
    let runtime_platforms = lockfile_runtime_platforms(manifest_raw)?;
    let required_runtime_version = required_runtime_version(manifest_raw)?;
    let runtime_tools = read_runtime_tools(manifest_raw);

    let mut targets: HashMap<String, TargetEntry> = HashMap::new();
    let mut tools = ToolSection {
        uv: None,
        pnpm: None,
    };
    let mut runtimes = RuntimeSection {
        python: None,
        deno: None,
        node: None,
        java: None,
        dotnet: None,
    };

    let language = detect_language(manifest_raw);
    if let Some(lang) = language.as_deref() {
        if lang == "python" {
            let version = required_runtime_version
                .clone()
                .or_else(|| read_runtime_version(manifest_raw))
                .unwrap_or_else(|| read_language_version(manifest_raw, "python", "3.11"));
            let step_started = Instant::now();
            let python_lockfile =
                generate_uv_lock(manifest_dir, manifest_raw, reporter.clone()).await?;
            maybe_report_timing(
                &reporter,
                timings,
                "lockfile.generate_uv_lock",
                step_started.elapsed(),
            )
            .await?;
            let step_started = Instant::now();
            let runtime =
                resolve_python_runtime(&version, &runtime_platforms, reporter.clone()).await?;
            maybe_report_timing(
                &reporter,
                timings,
                "lockfile.resolve_python_runtime",
                step_started.elapsed(),
            )
            .await?;
            runtimes.python = Some(runtime);
            if python_lockfile.is_some() {
                let python_artifacts = match prepare_python_artifacts(
                    manifest_raw,
                    manifest_dir,
                    &target_key,
                    &version,
                    reporter.clone(),
                )
                .await
                {
                    Ok(artifacts) if !artifacts.is_empty() => Some(artifacts),
                    Ok(_) => None,
                    Err(err) => {
                        reporter
                            .warn(format!("⚠️  Failed to prefetch Python artifacts: {}", err))
                            .await?;
                        None
                    }
                };
                let target_entry = targets.entry(target_key.clone()).or_default();
                target_entry.python_lockfile = Some("uv.lock".to_string());
                if let Some(artifacts) = python_artifacts {
                    target_entry.artifacts.extend(artifacts);
                }
                let step_started = Instant::now();
                tools.uv =
                    Some(resolve_uv_tool_targets(&runtime_platforms, reporter.clone()).await?);
                maybe_report_timing(
                    &reporter,
                    timings,
                    "lockfile.resolve_uv_tool_targets",
                    step_started.elapsed(),
                )
                .await?;
            }
        } else if lang == "node" {
            let version = required_runtime_version
                .clone()
                .or_else(|| read_runtime_version(manifest_raw))
                .unwrap_or_else(|| read_language_version(manifest_raw, "node", "20"));
            let step_started = Instant::now();
            let node_lockfile =
                generate_pnpm_lock(manifest_dir, manifest_raw, &version, reporter.clone()).await?;
            maybe_report_timing(
                &reporter,
                timings,
                "lockfile.generate_pnpm_lock",
                step_started.elapsed(),
            )
            .await?;
            let step_started = Instant::now();
            let runtime =
                resolve_node_runtime(&version, &runtime_platforms, reporter.clone()).await?;
            maybe_report_timing(
                &reporter,
                timings,
                "lockfile.resolve_node_runtime",
                step_started.elapsed(),
            )
            .await?;
            runtimes.node = Some(runtime);
            if runtimes.deno.is_none() {
                let deno_version = read_language_version(manifest_raw, "deno", "1.46.3");
                let step_started = Instant::now();
                let deno_runtime =
                    resolve_deno_runtime(&deno_version, &runtime_platforms, reporter.clone())
                        .await?;
                maybe_report_timing(
                    &reporter,
                    timings,
                    "lockfile.resolve_deno_runtime",
                    step_started.elapsed(),
                )
                .await?;
                runtimes.deno = Some(deno_runtime);
            }
            if node_lockfile.is_some() {
                let node_artifacts = match prepare_node_artifacts(
                    manifest_raw,
                    manifest_dir,
                    &target_key,
                    &version,
                    reporter.clone(),
                )
                .await
                {
                    Ok(artifacts) if !artifacts.is_empty() => Some(artifacts),
                    Ok(_) => None,
                    Err(err) => {
                        reporter
                            .warn(format!("⚠️  Failed to prefetch Node artifacts: {}", err))
                            .await?;
                        None
                    }
                };
                let target_entry = targets.entry(target_key.clone()).or_default();
                target_entry.node_lockfile = Some(format!("locks/{}/pnpm-lock.yaml", target_key));
                if let Some(artifacts) = node_artifacts {
                    target_entry.artifacts.extend(artifacts);
                }
                tools.pnpm = Some(resolve_pnpm_tool_targets(&runtime_platforms));
            }
        } else if lang == "deno" {
            let version = required_runtime_version
                .clone()
                .or_else(|| read_runtime_version(manifest_raw))
                .unwrap_or_else(|| read_language_version(manifest_raw, "deno", "1.46.3"));
            let step_started = Instant::now();
            let runtime =
                resolve_deno_runtime(&version, &runtime_platforms, reporter.clone()).await?;
            maybe_report_timing(
                &reporter,
                timings,
                "lockfile.resolve_deno_runtime",
                step_started.elapsed(),
            )
            .await?;
            runtimes.deno = Some(runtime);

            if let Some(node_version) = runtime_tools.get("node") {
                let step_started = Instant::now();
                let runtime =
                    resolve_node_runtime(node_version, &runtime_platforms, reporter.clone())
                        .await?;
                maybe_report_timing(
                    &reporter,
                    timings,
                    "lockfile.resolve_node_runtime",
                    step_started.elapsed(),
                )
                .await?;
                runtimes.node = Some(runtime);
            }
            if let Some(python_version) = runtime_tools.get("python") {
                let step_started = Instant::now();
                let runtime =
                    resolve_python_runtime(python_version, &runtime_platforms, reporter.clone())
                        .await?;
                maybe_report_timing(
                    &reporter,
                    timings,
                    "lockfile.resolve_python_runtime",
                    step_started.elapsed(),
                )
                .await?;
                runtimes.python = Some(runtime);

                let step_started = Instant::now();
                tools.uv =
                    Some(resolve_uv_tool_targets(&runtime_platforms, reporter.clone()).await?);
                maybe_report_timing(
                    &reporter,
                    timings,
                    "lockfile.resolve_uv_tool_targets",
                    step_started.elapsed(),
                )
                .await?;
            }

            // runtime=web/static は静的配信用途であり、Deno runtime 自体は必要だが
            // プロジェクト依存の deno.lock 生成は不要（かつ monorepo で誤検出しやすい）。
            let is_web_static = selected_target_runtime(manifest_raw).as_deref() == Some("web")
                && selected_target_driver(manifest_raw).as_deref() == Some("static");
            if !is_web_static {
                let step_started = Instant::now();
                let _ = generate_deno_lock(manifest_dir, manifest_raw, &version, reporter.clone())
                    .await?;
                maybe_report_timing(
                    &reporter,
                    timings,
                    "lockfile.generate_deno_lock",
                    step_started.elapsed(),
                )
                .await?;
            }
        }
    }

    for target_label in orchestration_service_target_labels(manifest_raw) {
        if selected_target_label(manifest_raw)
            .as_deref()
            .map(|selected| selected == target_label)
            .unwrap_or(false)
        {
            continue;
        }

        let Some(target) = named_target_table(manifest_raw, &target_label) else {
            continue;
        };

        let runtime = target
            .get("runtime")
            .and_then(|v| v.as_str())
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty());
        let driver = target
            .get("driver")
            .and_then(|v| v.as_str())
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty());
        let runtime_version = target
            .get("runtime_version")
            .and_then(|v| v.as_str())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let runtime_tools = read_runtime_tools_from_target(target);

        if matches!(driver.as_deref(), Some("node")) && runtimes.node.is_none() {
            let version = runtime_version.clone().unwrap_or_else(|| "20".to_string());
            runtimes.node =
                Some(resolve_node_runtime(&version, &runtime_platforms, reporter.clone()).await?);
        }

        if matches!(driver.as_deref(), Some("python")) && runtimes.python.is_none() {
            let version = runtime_version
                .clone()
                .unwrap_or_else(|| "3.11".to_string());
            runtimes.python =
                Some(resolve_python_runtime(&version, &runtime_platforms, reporter.clone()).await?);
            if tools.uv.is_none() {
                tools.uv =
                    Some(resolve_uv_tool_targets(&runtime_platforms, reporter.clone()).await?);
            }
        }

        if matches!(driver.as_deref(), Some("deno")) && runtimes.deno.is_none() {
            let version = runtime_version
                .clone()
                .unwrap_or_else(|| "1.46.3".to_string());
            runtimes.deno =
                Some(resolve_deno_runtime(&version, &runtime_platforms, reporter.clone()).await?);
        }

        if runtime.as_deref() == Some("web")
            && matches!(driver.as_deref(), Some("static"))
            && runtimes.deno.is_none()
        {
            let version = runtime_version
                .clone()
                .unwrap_or_else(|| "1.46.3".to_string());
            runtimes.deno =
                Some(resolve_deno_runtime(&version, &runtime_platforms, reporter.clone()).await?);
        }

        if let Some(node_version) = runtime_tools.get("node") {
            if runtimes.node.is_none() {
                runtimes.node = Some(
                    resolve_node_runtime(node_version, &runtime_platforms, reporter.clone())
                        .await?,
                );
            }
        }
        if let Some(python_version) = runtime_tools.get("python") {
            if runtimes.python.is_none() {
                runtimes.python = Some(
                    resolve_python_runtime(python_version, &runtime_platforms, reporter.clone())
                        .await?,
                );
            }
            if tools.uv.is_none() {
                tools.uv =
                    Some(resolve_uv_tool_targets(&runtime_platforms, reporter.clone()).await?);
            }
        }
    }

    let tools = if tools.uv.is_none() && tools.pnpm.is_none() {
        None
    } else {
        Some(tools)
    };

    Ok(CapsuleLock {
        version: "1".to_string(),
        meta: LockMeta {
            created_at: Utc::now().to_rfc3339(),
            manifest_hash: semantic_manifest_hash_from_text(manifest_text)?,
        },
        allowlist,
        tools,
        runtimes: if runtimes.python.is_none() && runtimes.node.is_none() && runtimes.deno.is_none()
        {
            None
        } else {
            Some(runtimes)
        },
        targets,
    })
}

fn semantic_manifest_hash(manifest: &CapsuleManifest) -> Result<String> {
    payload::compute_manifest_hash_without_signatures(manifest)
        .map_err(|e| CapsuleError::Config(format!("Failed to compute manifest hash: {}", e)))
}

fn semantic_manifest_hash_from_text(text: &str) -> Result<String> {
    let manifest = CapsuleManifest::from_toml(text)
        .map_err(|e| CapsuleError::Config(format!("Failed to parse manifest schema: {}", e)))?;
    semantic_manifest_hash(&manifest)
}

async fn generate_uv_lock(
    manifest_dir: &Path,
    manifest: &toml::Value,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<Option<PathBuf>> {
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
    let Some(deps_path) = deps_path else {
        return Ok(None);
    };

    let uv_path = ensure_uv(reporter.clone()).await?;
    reporter
        .notify("⚙️  Generating uv.lock".to_string())
        .await?;

    let status = if deps_path.file_name().is_some_and(|n| n == "pyproject.toml") {
        run_command(
            &uv_path,
            &["lock"],
            deps_path.parent().unwrap_or(manifest_dir),
            Some(manifest),
        )
        .await?
    } else if deps_path.extension().and_then(|e| e.to_str()) == Some("txt") {
        let dep_string = deps_path.to_string_lossy().to_string();
        run_command(
            &uv_path,
            &["pip", "compile", dep_string.as_str(), "-o", "uv.lock"],
            manifest_dir,
            Some(manifest),
        )
        .await?
    } else {
        run_command(&uv_path, &["lock"], manifest_dir, Some(manifest)).await?
    };

    if !status.success() {
        return Err(CapsuleError::Pack("uv lock failed".to_string()));
    }

    let lock_path = manifest_dir.join("uv.lock");
    if lock_path.exists() {
        Ok(Some(lock_path))
    } else {
        Ok(None)
    }
}

async fn generate_pnpm_lock(
    manifest_dir: &Path,
    manifest: &toml::Value,
    node_version: &str,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<Option<PathBuf>> {
    // npm プロジェクト（package-lock.json）では pnpm lock 生成を強制しない。
    // source/node 実行側は package-lock.json を Tier1 要件として扱うため、
    // ここでの pnpm 固定生成は不要かつ実運用で失敗要因になる。
    if manifest_dir.join("package-lock.json").exists() {
        reporter
            .notify(
                "ℹ️  package-lock.json detected; skipping pnpm-lock.yaml generation".to_string(),
            )
            .await?;
        return Ok(None);
    }

    let deps_path = read_dependencies_path(manifest, "node", manifest_dir).or_else(|| {
        let candidate = manifest_dir.join("package.json");
        if candidate.exists() {
            Some(candidate)
        } else {
            None
        }
    });
    let Some(_) = deps_path else {
        return Ok(None);
    };

    let node_path = ensure_node(node_version, reporter.clone()).await?;
    let pnpm_cmd = ensure_pnpm(&node_path, reporter.clone()).await?;

    reporter
        .notify("⚙️  Generating pnpm-lock.yaml".to_string())
        .await?;

    let mut cmd = std::process::Command::new(&pnpm_cmd.program);
    cmd.args(&pnpm_cmd.args_prefix)
        .args(["install", "--lockfile-only", "--ignore-scripts", "--silent"])
        .current_dir(manifest_dir);
    let status = run_command_inner_with_manifest_env(cmd, Some(manifest)).await?;
    if !status.success() {
        return Err(CapsuleError::Pack(
            "pnpm lock generation failed".to_string(),
        ));
    }

    let lock_path = manifest_dir.join("pnpm-lock.yaml");
    if !lock_path.exists() {
        return Ok(None);
    }
    let target_dir = manifest_dir.join("locks").join(platform_target_key()?);
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| CapsuleError::Pack(format!("Failed to create locks directory: {}", e)))?;
    let target_lock = target_dir.join("pnpm-lock.yaml");
    std::fs::copy(&lock_path, &target_lock)
        .map_err(|e| CapsuleError::Pack(format!("Failed to copy pnpm-lock.yaml: {}", e)))?;
    Ok(Some(target_lock))
}

async fn generate_deno_lock(
    manifest_dir: &Path,
    manifest: &toml::Value,
    deno_version: &str,
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
        return Ok(None);
    };

    let entrypoint_path = manifest_dir.join(&entrypoint);
    if !entrypoint_path.exists() {
        return Ok(None);
    }
    if entrypoint_path.is_dir() {
        // runtime=web/static など、ディレクトリを payload ルートにするケースでは
        // deno cache 対象が曖昧になり不要な解決失敗を引き起こすため lock 生成を行わない。
        return Ok(None);
    }

    reporter
        .notify("⚙️  Generating deno.lock".to_string())
        .await?;

    let deno_path = ensure_deno(deno_version, reporter.clone()).await?;
    let mut cmd = std::process::Command::new(&deno_path);
    cmd.args([
        "cache",
        entrypoint.as_str(),
        "--lock=deno.lock",
        "--lock-write",
    ])
    .current_dir(manifest_dir);

    let status = run_command_inner_with_manifest_env(cmd, Some(manifest)).await?;
    if !status.success() {
        return Err(CapsuleError::Pack(
            "deno lock generation failed".to_string(),
        ));
    }

    let lock_path = manifest_dir.join("deno.lock");
    if lock_path.exists() {
        Ok(Some(lock_path))
    } else {
        Ok(None)
    }
}

async fn prepare_python_artifacts(
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

async fn prepare_node_artifacts(
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

struct PnpmCommand {
    program: PathBuf,
    args_prefix: Vec<String>,
}

async fn ensure_uv(reporter: Arc<dyn CapsuleReporter + 'static>) -> Result<PathBuf> {
    if let Ok(found) = which::which("uv") {
        return Ok(found);
    }

    let version = UV_VERSION;
    reporter
        .notify(format!("⬇️  Downloading uv {}", version))
        .await?;
    let target_triple = platform_triple()?;
    let tools_dir = toolchain_cache_dir()?
        .join("tools")
        .join("uv")
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

async fn ensure_node(
    version: &str,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<PathBuf> {
    if let Ok(found) = which::which("node") {
        return Ok(found);
    }
    let fetcher = RuntimeFetcher::new_with_reporter(reporter)?;
    fetcher.ensure_node(version).await
}

async fn ensure_deno(
    version: &str,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<PathBuf> {
    let fetcher = RuntimeFetcher::new_with_reporter(reporter)?;
    fetcher.ensure_deno(version).await
}

async fn ensure_pnpm(
    node_path: &Path,
    reporter: Arc<dyn CapsuleReporter + 'static>,
) -> Result<PnpmCommand> {
    if let Ok(found) = which::which("pnpm") {
        return Ok(PnpmCommand {
            program: found,
            args_prefix: Vec::new(),
        });
    }

    let version = PNPM_VERSION;
    reporter
        .notify(format!("⬇️  Downloading pnpm {}", version))
        .await?;
    let tools_dir = toolchain_cache_dir()?
        .join("tools")
        .join("pnpm")
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

async fn download_file(url: &str, dest: &Path) -> Result<()> {
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

fn capsule_error_pack(message: String) -> CapsuleError {
    CapsuleError::Pack(message)
}

fn capsule_error_config(message: String) -> CapsuleError {
    CapsuleError::Config(message)
}

fn write_atomic_bytes_with_os_lock<E>(
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

fn create_atomic_temp_file<E>(
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

async fn run_command(
    program: &Path,
    args: &[&str],
    cwd: &Path,
    manifest: Option<&toml::Value>,
) -> Result<std::process::ExitStatus> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).current_dir(cwd);
    run_command_inner_with_manifest_env(cmd, manifest).await
}

#[cfg(test)]
async fn run_command_inner(cmd: std::process::Command) -> Result<std::process::ExitStatus> {
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
    for (key, value) in allowed_envs.into_iter() {
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

fn required_env_keys_from_manifest(manifest: &toml::Value) -> Vec<String> {
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

async fn resolve_python_runtime(
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

async fn resolve_node_runtime(
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

async fn resolve_deno_runtime(
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

fn deno_artifact_filename(os: &str, arch: &str) -> Result<String> {
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

fn resolve_pnpm_tool_targets(platforms: &[RuntimePlatform]) -> ToolTargets {
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

async fn resolve_uv_tool_targets(
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

fn uv_artifact_url(target_triple: &str) -> Option<String> {
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

fn metadata_cache_dir() -> Result<PathBuf> {
    Ok(nacelle_home_dir()?.join(METADATA_CACHE_DIR_NAME))
}

fn metadata_cache_path(
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

async fn cached_sha256<F, Fut>(cache_path: PathBuf, fetch: F) -> Result<String>
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

async fn maybe_report_timing(
    reporter: &Arc<dyn CapsuleReporter + 'static>,
    enabled: bool,
    label: &str,
    elapsed: std::time::Duration,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }

    reporter
        .notify(format!("⏱ [timings] {label}: {} ms", elapsed.as_millis()))
        .await?;
    Ok(())
}

fn read_allowlist(manifest: &toml::Value) -> Option<Vec<String>> {
    manifest
        .get("runtime")
        .and_then(|v| v.get("allowlist"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .filter(|list| !list.is_empty())
}

fn read_dependencies_path(
    manifest: &toml::Value,
    language: &str,
    manifest_dir: &Path,
) -> Option<PathBuf> {
    let from_targets = selected_target_table(manifest)
        .and_then(|t| t.get("dependencies"))
        .and_then(|v| v.as_str())
        .map(|s| manifest_dir.join(s));
    if from_targets.as_ref().is_some_and(|p| p.exists()) {
        return from_targets;
    }

    let from_language = manifest
        .get("language")
        .and_then(|v| v.get(language))
        .and_then(|v| v.get("manifest"))
        .and_then(|v| v.as_str())
        .map(|s| manifest_dir.join(s));
    if from_language.as_ref().is_some_and(|p| p.exists()) {
        return from_language;
    }

    None
}

fn detect_language(manifest: &toml::Value) -> Option<String> {
    if let Some(driver) = selected_target_table(manifest)
        .and_then(|t| t.get("driver"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_ascii_lowercase())
    {
        if matches!(driver.as_str(), "python" | "node" | "deno") {
            return Some(driver);
        }
    }

    if selected_target_runtime(manifest)
        .map(|r| r == "web")
        .unwrap_or(false)
        && selected_target_driver(manifest)
            .map(|d| d == "static")
            .unwrap_or(false)
    {
        return Some("deno".to_string());
    }

    if manifest
        .get("language")
        .and_then(|v| v.get("python"))
        .is_some()
    {
        return Some("python".to_string());
    }
    if manifest
        .get("language")
        .and_then(|v| v.get("node"))
        .is_some()
    {
        return Some("node".to_string());
    }

    let target_lang = selected_target_table(manifest)
        .and_then(|t| t.get("language"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if target_lang.is_some() {
        return target_lang;
    }

    let entrypoint = manifest
        .get("execution")
        .and_then(|e| e.get("entrypoint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let ext = Path::new(entrypoint)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "py" {
        return Some("python".to_string());
    }
    if matches!(ext.as_str(), "js" | "mjs" | "cjs" | "ts") {
        return Some("node".to_string());
    }
    None
}

fn read_language_version(manifest: &toml::Value, language: &str, fallback: &str) -> String {
    let version = manifest
        .get("language")
        .and_then(|v| v.get(language))
        .and_then(|v| v.get("version"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            selected_target_table(manifest)
                .and_then(|t| t.get("version"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.to_string());

    version.unwrap_or_else(|| fallback.to_string())
}

fn read_runtime_version(manifest: &toml::Value) -> Option<String> {
    selected_target_table(manifest)
        .and_then(|t| t.get("runtime_version"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_runtime_tools(manifest: &toml::Value) -> HashMap<String, String> {
    selected_target_table(manifest)
        .map(read_runtime_tools_from_target)
        .unwrap_or_default()
}

fn read_runtime_tools_from_target(target: &toml::Value) -> HashMap<String, String> {
    let mut tools = HashMap::new();
    let Some(table) = target.get("runtime_tools").and_then(|v| v.as_table()) else {
        return tools;
    };

    for (key, value) in table {
        let Some(raw) = value.as_str() else {
            continue;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        tools.insert(key.to_ascii_lowercase(), trimmed.to_string());
    }

    tools
}

fn selected_target_label(manifest: &toml::Value) -> Option<String> {
    let default_target = manifest
        .get("default_target")
        .and_then(|v| v.as_str())
        .unwrap_or("source")
        .trim();
    if default_target.is_empty() {
        None
    } else {
        Some(default_target.to_string())
    }
}

fn named_target_table<'a>(
    manifest: &'a toml::Value,
    target_label: &str,
) -> Option<&'a toml::Value> {
    manifest
        .get("targets")
        .and_then(|targets| targets.get(target_label))
}

fn orchestration_service_target_labels(manifest: &toml::Value) -> Vec<String> {
    let mut labels = Vec::new();
    let Some(services) = manifest.get("services").and_then(|value| value.as_table()) else {
        return labels;
    };

    for (name, service) in services {
        let target = service
            .get("target")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                (name == "main")
                    .then(|| selected_target_label(manifest))
                    .flatten()
            });

        if let Some(target) = target {
            if !labels.iter().any(|existing| existing == &target) {
                labels.push(target);
            }
        }
    }

    labels
}

fn selected_target_runtime(manifest: &toml::Value) -> Option<String> {
    selected_target_table(manifest)
        .and_then(|t| t.get("runtime"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
}

fn selected_target_driver(manifest: &toml::Value) -> Option<String> {
    selected_target_table(manifest)
        .and_then(|t| t.get("driver"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
}

fn required_runtime_version(manifest: &toml::Value) -> Result<Option<String>> {
    let runtime = selected_target_runtime(manifest);
    let driver = selected_target_driver(manifest);
    let requires_source = runtime.as_deref() == Some("source")
        && matches!(
            driver.as_deref(),
            Some("python") | Some("node") | Some("deno")
        );
    let requires_web_deno = runtime.as_deref() == Some("web") && driver.as_deref() == Some("deno");
    let requires = requires_source || requires_web_deno;
    if !requires {
        return Ok(None);
    }

    read_runtime_version(manifest).map(Some).ok_or_else(|| {
        CapsuleError::Config(
            "targets.<default_target>.runtime_version is required for source driver deno/node/python and web driver deno".to_string(),
        )
    })
}

fn selected_target_table(manifest: &toml::Value) -> Option<&toml::Value> {
    let targets = manifest.get("targets")?;
    let default_target = manifest
        .get("default_target")
        .and_then(|v| v.as_str())
        .unwrap_or("source");

    targets
        .get(default_target)
        .or_else(|| targets.get("source"))
}

fn lockfile_runtime_platforms(manifest: &toml::Value) -> Result<Vec<RuntimePlatform>> {
    let runtime = selected_target_runtime(manifest);
    let driver = selected_target_driver(manifest).or_else(|| detect_language(manifest));
    let runtime_tools = read_runtime_tools(manifest);
    let needs_universal_lock = runtime.as_deref() == Some("web")
        || (runtime.as_deref() == Some("source")
            && (matches!(
                driver.as_deref(),
                Some("python") | Some("node") | Some("deno")
            ) || !runtime_tools.is_empty()));

    if needs_universal_lock {
        return Ok(SUPPORTED_RUNTIME_PLATFORMS.to_vec());
    }
    Ok(vec![current_runtime_platform()?])
}

fn current_runtime_platform() -> Result<RuntimePlatform> {
    let (os, arch) = RuntimeFetcher::detect_platform()?;
    runtime_platform(&os, &arch)
}

fn runtime_platform(os: &str, arch: &str) -> Result<RuntimePlatform> {
    SUPPORTED_RUNTIME_PLATFORMS
        .iter()
        .copied()
        .find(|platform| platform.os == os && platform.arch == arch)
        .ok_or_else(|| CapsuleError::Pack(format!("Unsupported platform: {} {}", os, arch)))
}

fn read_target_entrypoint(manifest: &toml::Value) -> Option<String> {
    selected_target_table(manifest)
        .and_then(|t| t.get("entrypoint"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn platform_target_key() -> Result<String> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return Err(CapsuleError::Pack("Unsupported OS".to_string()));
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        return Err(CapsuleError::Pack("Unsupported architecture".to_string()));
    };
    Ok(format!("{}-{}", os, arch))
}

fn platform_triple() -> Result<String> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return Err(CapsuleError::Pack("Unsupported OS".to_string()));
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return Err(CapsuleError::Pack("Unsupported architecture".to_string()));
    };

    let triple = match (os, arch) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => {
            return Err(CapsuleError::Pack(format!(
                "Unsupported platform: {} {}",
                os, arch
            )))
        }
    };

    Ok(triple.to_string())
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    hex::encode(digest)
}

fn sha256_dir(root: &Path) -> Result<String> {
    let mut entries = Vec::new();
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        if entry.file_type().is_file() {
            entries.push(entry.path().to_path_buf());
        }
    }
    entries.sort();

    let mut hasher = Sha256::new();
    for path in entries {
        let rel = path.strip_prefix(root).unwrap_or(&path);
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update([0]);
        let bytes = std::fs::read(&path)?;
        hasher.update(bytes);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn artifact_root(manifest_dir: &Path, target_key: &str) -> PathBuf {
    manifest_dir.join("artifacts").join(target_key)
}

fn reset_dir(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    std::fs::create_dir_all(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn serialize_lockfile_with_allowlist() {
        let lockfile = CapsuleLock {
            version: "1".to_string(),
            meta: LockMeta {
                created_at: "2026-01-20T00:00:00Z".to_string(),
                manifest_hash: "sha256:deadbeef".to_string(),
            },
            allowlist: Some(vec!["nodejs.org".to_string()]),
            tools: None,
            runtimes: None,
            targets: HashMap::new(),
        };

        let toml = toml::to_string(&lockfile).unwrap();
        let parsed: CapsuleLock = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.allowlist.unwrap()[0], "nodejs.org");
    }

    #[test]
    fn verify_lockfile_manifest_hash() {
        let temp = TempDir::new().unwrap();
        let manifest_path = temp.path().join("capsule.toml");
        let lockfile_path = temp.path().join("capsule.lock");
        let manifest_text = r#"schema_version = "0.2"
name = "demo"
version = "1.0.0"
type = "app"
default_target = "cli"
"#;
        fs::write(&manifest_path, manifest_text).unwrap();

        let lockfile = CapsuleLock {
            version: "1".to_string(),
            meta: LockMeta {
                created_at: "2026-01-20T00:00:00Z".to_string(),
                manifest_hash: semantic_manifest_hash_from_text(manifest_text).unwrap(),
            },
            allowlist: None,
            tools: None,
            runtimes: None,
            targets: HashMap::new(),
        };

        let toml = toml::to_string(&lockfile).unwrap();
        fs::write(&lockfile_path, toml).unwrap();

        verify_lockfile_manifest(&manifest_path, &lockfile_path).unwrap();
    }

    #[test]
    fn deno_artifact_filename_uses_release_target_triplets() {
        assert_eq!(
            deno_artifact_filename("macos", "aarch64").unwrap(),
            "deno-aarch64-apple-darwin.zip"
        );
        assert_eq!(
            deno_artifact_filename("linux", "x86_64").unwrap(),
            "deno-x86_64-unknown-linux-gnu.zip"
        );
        assert_eq!(
            deno_artifact_filename("windows", "x86_64").unwrap(),
            "deno-x86_64-pc-windows-msvc.zip"
        );
        assert!(deno_artifact_filename("windows", "aarch64").is_err());
    }

    #[test]
    fn runtime_tools_are_read_from_selected_target() {
        let manifest: toml::Value = toml::from_str(
            r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_tools = { node = "20.11.0", python = "3.11.7" }
"#,
        )
        .unwrap();

        let tools = read_runtime_tools(&manifest);
        assert_eq!(tools.get("node"), Some(&"20.11.0".to_string()));
        assert_eq!(tools.get("python"), Some(&"3.11.7".to_string()));
    }

    #[test]
    fn orchestration_service_targets_are_collected() {
        let manifest: toml::Value = toml::from_str(
            r#"
default_target = "dashboard"

[targets.dashboard]
runtime = "web"
driver = "node"

[targets.control_plane]
runtime = "source"
driver = "python"

[services.main]
target = "dashboard"
depends_on = ["control_plane"]

[services.control_plane]
target = "control_plane"
"#,
        )
        .unwrap();

        let mut labels = orchestration_service_target_labels(&manifest);
        labels.sort();
        assert_eq!(
            labels,
            vec!["control_plane".to_string(), "dashboard".to_string()]
        );
    }

    #[test]
    fn required_runtime_version_for_web_deno_target() {
        let manifest: toml::Value = toml::from_str(
            r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
"#,
        )
        .unwrap();

        let version = required_runtime_version(&manifest).unwrap();
        assert_eq!(version.as_deref(), Some("1.46.3"));
    }

    #[test]
    fn web_targets_include_all_supported_runtime_platforms_in_lockfile() {
        let manifest: toml::Value = toml::from_str(
            r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
"#,
        )
        .unwrap();

        let platforms = lockfile_runtime_platforms(&manifest).unwrap();
        assert_eq!(platforms.len(), SUPPORTED_RUNTIME_PLATFORMS.len());
        for expected in SUPPORTED_RUNTIME_PLATFORMS {
            assert!(platforms.contains(expected));
        }
    }

    #[test]
    fn source_managed_runtime_targets_include_all_supported_runtime_platforms_in_lockfile() {
        let manifest: toml::Value = toml::from_str(
            r#"
default_target = "default"
[targets.default]
runtime = "source"
driver = "deno"
runtime_version = "1.46.3"
"#,
        )
        .unwrap();

        let platforms = lockfile_runtime_platforms(&manifest).unwrap();
        assert_eq!(platforms.len(), SUPPORTED_RUNTIME_PLATFORMS.len());
        for expected in SUPPORTED_RUNTIME_PLATFORMS {
            assert!(platforms.contains(expected));
        }
    }

    #[test]
    fn source_targets_with_runtime_tools_include_all_supported_runtime_platforms_in_lockfile() {
        let manifest: toml::Value = toml::from_str(
            r#"
default_target = "default"
[targets.default]
runtime = "source"
driver = "node"
runtime_version = "20.11.0"
runtime_tools = { python = "3.11.7" }
"#,
        )
        .unwrap();

        let platforms = lockfile_runtime_platforms(&manifest).unwrap();
        assert_eq!(platforms.len(), SUPPORTED_RUNTIME_PLATFORMS.len());
        for expected in SUPPORTED_RUNTIME_PLATFORMS {
            assert!(platforms.contains(expected));
        }
    }

    #[test]
    fn stale_universal_lockfile_is_detected_when_runtime_targets_are_host_only() {
        let manifest: toml::Value = toml::from_str(
            r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
runtime_tools = { node = "20.11.0", python = "3.11.10" }
"#,
        )
        .unwrap();

        let host_only_targets = HashMap::from([(
            "aarch64-apple-darwin".to_string(),
            RuntimeArtifact {
                url: "https://example.com/runtime.tar.gz".to_string(),
                sha256: "deadbeef".to_string(),
            },
        )]);
        let host_only_tool_targets = HashMap::from([(
            "aarch64-apple-darwin".to_string(),
            ToolArtifact {
                url: "https://example.com/uv.tar.gz".to_string(),
                sha256: Some("deadbeef".to_string()),
                version: Some("0.4.19".to_string()),
            },
        )]);
        let lockfile = CapsuleLock {
            version: "1".to_string(),
            meta: LockMeta {
                created_at: "2026-03-08T00:00:00Z".to_string(),
                manifest_hash: "blake3:deadbeef".to_string(),
            },
            allowlist: None,
            tools: Some(ToolSection {
                uv: Some(ToolTargets {
                    targets: host_only_tool_targets,
                }),
                pnpm: None,
            }),
            runtimes: Some(RuntimeSection {
                python: Some(RuntimeEntry {
                    provider: "python-build-standalone".to_string(),
                    version: "3.11.10".to_string(),
                    targets: host_only_targets.clone(),
                }),
                deno: Some(RuntimeEntry {
                    provider: "official".to_string(),
                    version: "1.46.3".to_string(),
                    targets: host_only_targets.clone(),
                }),
                node: Some(RuntimeEntry {
                    provider: "official".to_string(),
                    version: "20.11.0".to_string(),
                    targets: host_only_targets,
                }),
                java: None,
                dotnet: None,
            }),
            targets: HashMap::new(),
        };

        assert!(!lockfile_has_required_platform_coverage(&lockfile, &manifest).unwrap());
    }

    #[test]
    fn universal_lockfile_passes_when_all_runtime_targets_are_present() {
        let manifest: toml::Value = toml::from_str(
            r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
runtime_tools = { node = "20.11.0", python = "3.11.10" }
"#,
        )
        .unwrap();

        let runtime_targets: HashMap<String, RuntimeArtifact> = SUPPORTED_RUNTIME_PLATFORMS
            .iter()
            .map(|platform| {
                (
                    platform.target_triple.to_string(),
                    RuntimeArtifact {
                        url: format!(
                            "https://example.com/{}/runtime.tar.gz",
                            platform.target_triple
                        ),
                        sha256: "deadbeef".to_string(),
                    },
                )
            })
            .collect();
        let tool_targets: HashMap<String, ToolArtifact> = SUPPORTED_RUNTIME_PLATFORMS
            .iter()
            .map(|platform| {
                (
                    platform.target_triple.to_string(),
                    ToolArtifact {
                        url: format!("https://example.com/{}/uv.tar.gz", platform.target_triple),
                        sha256: Some("deadbeef".to_string()),
                        version: Some("0.4.19".to_string()),
                    },
                )
            })
            .collect();
        let lockfile = CapsuleLock {
            version: "1".to_string(),
            meta: LockMeta {
                created_at: "2026-03-08T00:00:00Z".to_string(),
                manifest_hash: "blake3:deadbeef".to_string(),
            },
            allowlist: None,
            tools: Some(ToolSection {
                uv: Some(ToolTargets {
                    targets: tool_targets,
                }),
                pnpm: None,
            }),
            runtimes: Some(RuntimeSection {
                python: Some(RuntimeEntry {
                    provider: "python-build-standalone".to_string(),
                    version: "3.11.10".to_string(),
                    targets: runtime_targets.clone(),
                }),
                deno: Some(RuntimeEntry {
                    provider: "official".to_string(),
                    version: "1.46.3".to_string(),
                    targets: runtime_targets.clone(),
                }),
                node: Some(RuntimeEntry {
                    provider: "official".to_string(),
                    version: "20.11.0".to_string(),
                    targets: runtime_targets,
                }),
                java: None,
                dotnet: None,
            }),
            targets: HashMap::new(),
        };

        assert!(lockfile_has_required_platform_coverage(&lockfile, &manifest).unwrap());
    }

    #[test]
    fn universal_lockfile_allows_deno_without_windows_arm64_target() {
        let manifest: toml::Value = toml::from_str(
            r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
runtime_tools = { node = "20.11.0", python = "3.11.10" }
"#,
        )
        .unwrap();

        let common_runtime_targets: HashMap<String, RuntimeArtifact> = SUPPORTED_RUNTIME_PLATFORMS
            .iter()
            .map(|platform| {
                (
                    platform.target_triple.to_string(),
                    RuntimeArtifact {
                        url: format!(
                            "https://example.com/{}/runtime.tar.gz",
                            platform.target_triple
                        ),
                        sha256: "deadbeef".to_string(),
                    },
                )
            })
            .collect();
        let deno_runtime_targets: HashMap<String, RuntimeArtifact> = SUPPORTED_RUNTIME_PLATFORMS
            .iter()
            .filter(|platform| deno_artifact_filename(platform.os, platform.arch).is_ok())
            .map(|platform| {
                (
                    platform.target_triple.to_string(),
                    RuntimeArtifact {
                        url: format!("https://example.com/{}/deno.zip", platform.target_triple),
                        sha256: "deadbeef".to_string(),
                    },
                )
            })
            .collect();
        let tool_targets: HashMap<String, ToolArtifact> = SUPPORTED_RUNTIME_PLATFORMS
            .iter()
            .map(|platform| {
                (
                    platform.target_triple.to_string(),
                    ToolArtifact {
                        url: format!("https://example.com/{}/uv.tar.gz", platform.target_triple),
                        sha256: Some("deadbeef".to_string()),
                        version: Some("0.4.19".to_string()),
                    },
                )
            })
            .collect();
        let lockfile = CapsuleLock {
            version: "1".to_string(),
            meta: LockMeta {
                created_at: "2026-03-08T00:00:00Z".to_string(),
                manifest_hash: "blake3:deadbeef".to_string(),
            },
            allowlist: None,
            tools: Some(ToolSection {
                uv: Some(ToolTargets {
                    targets: tool_targets,
                }),
                pnpm: None,
            }),
            runtimes: Some(RuntimeSection {
                python: Some(RuntimeEntry {
                    provider: "python-build-standalone".to_string(),
                    version: "3.11.10".to_string(),
                    targets: common_runtime_targets.clone(),
                }),
                deno: Some(RuntimeEntry {
                    provider: "official".to_string(),
                    version: "1.46.3".to_string(),
                    targets: deno_runtime_targets,
                }),
                node: Some(RuntimeEntry {
                    provider: "official".to_string(),
                    version: "20.11.0".to_string(),
                    targets: common_runtime_targets,
                }),
                java: None,
                dotnet: None,
            }),
            targets: HashMap::new(),
        };

        assert!(lockfile_has_required_platform_coverage(&lockfile, &manifest).unwrap());
    }

    #[test]
    fn universal_lockfile_allows_python_without_windows_arm64_target() {
        let manifest: toml::Value = toml::from_str(
            r#"
default_target = "default"
[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
runtime_tools = { node = "20.11.0", python = "3.11.10" }
"#,
        )
        .unwrap();

        let python_targets: HashMap<String, RuntimeArtifact> = SUPPORTED_RUNTIME_PLATFORMS
            .iter()
            .filter(|platform| {
                RuntimeFetcher::get_python_download_url("3.11.10", platform.os, platform.arch)
                    .is_ok()
            })
            .map(|platform| {
                (
                    platform.target_triple.to_string(),
                    RuntimeArtifact {
                        url: format!(
                            "https://example.com/{}/python.tar.gz",
                            platform.target_triple
                        ),
                        sha256: "deadbeef".to_string(),
                    },
                )
            })
            .collect();
        let common_runtime_targets: HashMap<String, RuntimeArtifact> = SUPPORTED_RUNTIME_PLATFORMS
            .iter()
            .map(|platform| {
                (
                    platform.target_triple.to_string(),
                    RuntimeArtifact {
                        url: format!(
                            "https://example.com/{}/runtime.tar.gz",
                            platform.target_triple
                        ),
                        sha256: "deadbeef".to_string(),
                    },
                )
            })
            .collect();
        let uv_targets: HashMap<String, ToolArtifact> = SUPPORTED_RUNTIME_PLATFORMS
            .iter()
            .filter(|platform| uv_artifact_url(platform.target_triple).is_some())
            .map(|platform| {
                (
                    platform.target_triple.to_string(),
                    ToolArtifact {
                        url: uv_artifact_url(platform.target_triple).unwrap(),
                        sha256: Some("deadbeef".to_string()),
                        version: Some(UV_VERSION.to_string()),
                    },
                )
            })
            .collect();
        let lockfile = CapsuleLock {
            version: "1".to_string(),
            meta: LockMeta {
                created_at: "2026-03-08T00:00:00Z".to_string(),
                manifest_hash: "blake3:deadbeef".to_string(),
            },
            allowlist: None,
            tools: Some(ToolSection {
                uv: Some(ToolTargets {
                    targets: uv_targets,
                }),
                pnpm: None,
            }),
            runtimes: Some(RuntimeSection {
                python: Some(RuntimeEntry {
                    provider: "python-build-standalone".to_string(),
                    version: "3.11.10".to_string(),
                    targets: python_targets,
                }),
                deno: Some(RuntimeEntry {
                    provider: "official".to_string(),
                    version: "1.46.3".to_string(),
                    targets: common_runtime_targets.clone(),
                }),
                node: Some(RuntimeEntry {
                    provider: "official".to_string(),
                    version: "20.11.0".to_string(),
                    targets: common_runtime_targets,
                }),
                java: None,
                dotnet: None,
            }),
            targets: HashMap::new(),
        };

        assert!(lockfile_has_required_platform_coverage(&lockfile, &manifest).unwrap());
    }

    #[test]
    fn uv_artifact_url_uses_zip_for_windows_x64_and_skips_windows_arm64() {
        assert_eq!(
            uv_artifact_url("x86_64-pc-windows-msvc").as_deref(),
            Some("https://github.com/astral-sh/uv/releases/download/0.4.19/uv-x86_64-pc-windows-msvc.zip")
        );
        assert!(uv_artifact_url("aarch64-pc-windows-msvc").is_none());
        assert_eq!(
            uv_artifact_url("x86_64-unknown-linux-gnu").as_deref(),
            Some("https://github.com/astral-sh/uv/releases/download/0.4.19/uv-x86_64-unknown-linux-gnu.tar.gz")
        );
    }

    #[test]
    fn required_env_keys_from_manifest_collects_modern_and_legacy() {
        let manifest: toml::Value = toml::from_str(
            r#"
[targets.default]
runtime = "web"
driver = "deno"
required_env = ["API_TOKEN", " ACCOUNT_ID ", ""]
env = { ATO_ORCH_REQUIRED_ENVS = "LEGACY_ONE, LEGACY_TWO,API_TOKEN" }
"#,
        )
        .unwrap();

        let keys = required_env_keys_from_manifest(&manifest);
        assert_eq!(
            keys,
            vec![
                "ACCOUNT_ID".to_string(),
                "API_TOKEN".to_string(),
                "LEGACY_ONE".to_string(),
                "LEGACY_TWO".to_string(),
            ]
        );
    }

    #[test]
    fn atomic_write_replaces_file_without_temp_leaks() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("capsule.lock");

        write_atomic_bytes_with_os_lock(&target, b"first", "test lockfile", capsule_error_pack)
            .unwrap();
        write_atomic_bytes_with_os_lock(&target, b"second", "test lockfile", capsule_error_pack)
            .unwrap();

        let written = fs::read_to_string(&target).unwrap();
        assert_eq!(written, "second");

        let leftovers = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(".capsule.lock.tmp-"))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "temp files leaked: {:?}", leftovers);
    }

    #[test]
    fn atomic_temp_file_is_created_in_target_directory() {
        let temp = TempDir::new().unwrap();
        let tmp_path = create_atomic_temp_file(
            temp.path(),
            "capsule.lock",
            "test temp file",
            &capsule_error_pack,
        )
        .unwrap();

        assert_eq!(tmp_path.parent(), Some(temp.path()));
        assert!(tmp_path.exists());
        let _ = fs::remove_file(tmp_path);
    }

    #[test]
    fn ensure_lockfile_reuses_when_inputs_unchanged() {
        let temp = TempDir::new().unwrap();
        let manifest_path = temp.path().join("capsule.toml");
        let manifest_text = r#"
schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "default"
[targets.default]
runtime = "source"
driver = "native"
entrypoint = "source/main.sh"
"#;
        fs::write(&manifest_path, manifest_text).unwrap();
        fs::create_dir_all(temp.path().join("source")).unwrap();
        fs::write(temp.path().join("source/main.sh"), "echo demo").unwrap();

        let manifest_raw: toml::Value = toml::from_str(manifest_text).unwrap();
        let reporter: Arc<dyn CapsuleReporter + 'static> = Arc::new(crate::reporter::NoOpReporter);
        let rt = tokio::runtime::Runtime::new().unwrap();

        let first = rt
            .block_on(ensure_lockfile(
                &manifest_path,
                &manifest_raw,
                manifest_text,
                reporter.clone(),
                false,
            ))
            .unwrap();
        let first_lock = read_lockfile(&first).unwrap();

        std::thread::sleep(Duration::from_millis(20));

        let second = rt
            .block_on(ensure_lockfile(
                &manifest_path,
                &manifest_raw,
                manifest_text,
                reporter,
                false,
            ))
            .unwrap();
        let second_lock = read_lockfile(&second).unwrap();

        assert_eq!(first_lock.meta.created_at, second_lock.meta.created_at);
        assert!(temp.path().join(LOCKFILE_INPUT_SNAPSHOT_NAME).exists());
    }

    #[test]
    fn generate_lockfile_does_not_include_ambient_tools_for_native_target() {
        let temp = TempDir::new().unwrap();
        let manifest_path = temp.path().join("capsule.toml");
        let manifest_text = r#"
schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "default"
[targets.default]
runtime = "source"
driver = "native"
entrypoint = "main.sh"
"#;
        fs::write(&manifest_path, manifest_text).unwrap();
        fs::write(temp.path().join("main.sh"), "echo demo").unwrap();

        let manifest_raw: toml::Value = toml::from_str(manifest_text).unwrap();
        let reporter: Arc<dyn CapsuleReporter + 'static> = Arc::new(crate::reporter::NoOpReporter);
        let rt = tokio::runtime::Runtime::new().unwrap();

        let lockfile = rt
            .block_on(generate_lockfile(
                &manifest_raw,
                manifest_text,
                temp.path(),
                reporter,
                false,
            ))
            .unwrap();

        assert!(lockfile.tools.is_none());
    }

    #[tokio::test]
    async fn run_command_inner_rejects_relative_program() {
        let cmd = std::process::Command::new("echo");
        let err = run_command_inner(cmd).await.expect_err("must fail closed");
        assert!(err
            .to_string()
            .contains("Refusing to execute non-absolute command path"));
    }
}

use std::collections::{BTreeMap, HashMap};
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
use tracing::debug;
use url::form_urlencoded::byte_serialize;

use crate::common::paths::{nacelle_home_dir, toolchain_cache_dir};
use crate::error::{CapsuleError, Result};
use crate::packers::payload;
use crate::packers::runtime_fetcher::RuntimeFetcher;
use crate::reporter::CapsuleReporter;
use crate::types::{CapsuleManifest, ExternalCapsuleDependency};

#[path = "lockfile_runtime.rs"]
mod lockfile_runtime;
#[path = "lockfile_support.rs"]
mod lockfile_support;

use lockfile_runtime::*;
use lockfile_support::*;

const UV_VERSION: &str = "0.4.19";
const PNPM_VERSION: &str = "9.9.0";
const LOCKFILE_INPUT_SNAPSHOT_VERSION: u32 = 1;
const LOCKFILE_INPUT_SNAPSHOT_NAME: &str = ".capsule.lock.inputs.json";
const METADATA_CACHE_DIR_NAME: &str = "metadata-cache";
const DEFAULT_STORE_API_URL: &str = "https://api.ato.run";
const ENV_STORE_API_URL: &str = "ATO_STORE_API_URL";

pub const CAPSULE_LOCK_FILE_NAME: &str = "capsule.lock.json";
pub const LEGACY_CAPSULE_LOCK_FILE_NAME: &str = "capsule.lock";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapsuleLock {
    pub version: String,
    pub meta: LockMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowlist: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capsule_dependencies: Vec<LockedCapsuleDependency>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub injected_data: HashMap<String, LockedInjectedData>,
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
pub struct LockedCapsuleDependency {
    pub name: String,
    pub source: String,
    pub source_type: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub injection_bindings: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedInjectedData {
    pub source: String,
    pub digest: String,
    pub bytes: u64,
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
    let output_path = manifest_dir.join(CAPSULE_LOCK_FILE_NAME);
    let content = serde_jcs::to_vec(&lockfile).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to serialize {}: {}",
            CAPSULE_LOCK_FILE_NAME, e
        ))
    })?;
    write_atomic_bytes_with_os_lock(
        &output_path,
        &content,
        CAPSULE_LOCK_FILE_NAME,
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
    let lock_path = manifest_dir.join(CAPSULE_LOCK_FILE_NAME);
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
    lockfile.meta.created_at = reproducible_packaged_lock_created_at();
    lockfile.meta.manifest_hash = semantic_manifest_hash(manifest)?;
    serde_jcs::to_vec(&lockfile).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to serialize {}: {}",
            CAPSULE_LOCK_FILE_NAME, e
        ))
    })
}

fn reproducible_packaged_lock_created_at() -> String {
    let epoch = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    chrono::DateTime::<Utc>::from_timestamp(epoch, 0)
        .unwrap_or_else(|| chrono::DateTime::<Utc>::from_timestamp(0, 0).expect("unix epoch"))
        .to_rfc3339()
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
            "{} manifest hash mismatch (expected {}, got {})",
            lockfile_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(CAPSULE_LOCK_FILE_NAME),
            expected_hash,
            lockfile.meta.manifest_hash
        )));
    }
    Ok(())
}

fn read_lockfile(path: &Path) -> Result<CapsuleLock> {
    let raw = fs::read_to_string(path)
        .map_err(|e| CapsuleError::Config(format!("Failed to read {}: {}", path.display(), e)))?;
    parse_lockfile_text(&raw, path)
}

pub fn lockfile_output_path(manifest_dir: &Path) -> PathBuf {
    manifest_dir.join(CAPSULE_LOCK_FILE_NAME)
}

pub fn resolve_existing_lockfile_path(manifest_dir: &Path) -> Option<PathBuf> {
    let primary = lockfile_output_path(manifest_dir);
    if primary.exists() {
        return Some(primary);
    }

    let legacy = manifest_dir.join(LEGACY_CAPSULE_LOCK_FILE_NAME);
    legacy.exists().then_some(legacy)
}

pub fn parse_lockfile_text(raw: &str, path: &Path) -> Result<CapsuleLock> {
    serde_json::from_str(raw)
        .or_else(|_| toml::from_str(raw))
        .map_err(|e| CapsuleError::Config(format!("Failed to parse {}: {}", path.display(), e)))
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
        "deno.lock",
        "deno.json",
        "deno.jsonc",
        "uv.lock",
    ] {
        paths.push(manifest_dir.join(name));
    }

    paths.push(manifest_dir.join("source").join("deno.lock"));

    for language in ["python", "node"] {
        if let Some(path) = read_dependencies_path(manifest_raw, language, manifest_dir) {
            paths.push(path);
        }
    }

    paths
}

pub fn manifest_external_capsule_dependencies(
    manifest_raw: &toml::Value,
) -> Result<Vec<ExternalCapsuleDependency>> {
    let Some(targets) = manifest_raw.get("targets").and_then(toml::Value::as_table) else {
        return Ok(Vec::new());
    };

    let mut collected = Vec::new();
    let mut seen = HashMap::<String, String>::new();
    for (target_label, raw_target) in targets {
        let Some(external_dependencies) = raw_target
            .get("external_dependencies")
            .and_then(toml::Value::as_array)
        else {
            continue;
        };

        for raw_dependency in external_dependencies {
            let dependency: ExternalCapsuleDependency =
                raw_dependency.clone().try_into().map_err(|err| {
                    CapsuleError::Pack(format!(
                        "Failed to parse targets.{}.external_dependencies entry: {}",
                        target_label, err
                    ))
                })?;
            if let Some(existing_source) = seen.get(&dependency.alias) {
                if existing_source != &dependency.source {
                    return Err(CapsuleError::Pack(format!(
                        "External capsule dependency alias '{}' maps to multiple sources ('{}' and '{}')",
                        dependency.alias, existing_source, dependency.source
                    )));
                }
                continue;
            }
            seen.insert(dependency.alias.clone(), dependency.source.clone());
            collected.push(dependency);
        }
    }

    collected.sort_by(|a, b| a.alias.cmp(&b.alias));
    Ok(collected)
}

#[derive(Debug, Clone)]
struct StoreCapsuleSource {
    scoped_id: String,
    publisher: String,
    slug: String,
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LockfileStoreDistributionResponse {
    version: String,
    #[serde(default)]
    artifact_url: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    blake3: Option<String>,
}

fn parse_store_capsule_source(source: &str) -> Result<StoreCapsuleSource> {
    let raw = source.trim();
    let raw = raw.strip_prefix("capsule://store/").ok_or_else(|| {
        CapsuleError::Pack(format!("Unsupported capsule dependency source: {}", source))
    })?;
    let raw = raw.split_once('?').map(|(path, _)| path).unwrap_or(raw);
    let (path_part, version) = raw
        .rsplit_once('@')
        .map(|(path, version)| (path, Some(version.trim().to_string())))
        .unwrap_or((raw, None));
    let mut segments = path_part
        .split('/')
        .filter(|segment| !segment.trim().is_empty());
    let publisher = segments.next().ok_or_else(|| {
        CapsuleError::Pack(format!("Invalid capsule dependency source: {}", source))
    })?;
    let slug = segments.next().ok_or_else(|| {
        CapsuleError::Pack(format!("Invalid capsule dependency source: {}", source))
    })?;
    if segments.next().is_some() {
        return Err(CapsuleError::Pack(format!(
            "Invalid capsule dependency source: {}",
            source
        )));
    }

    Ok(StoreCapsuleSource {
        scoped_id: format!("{}/{}", publisher, slug),
        publisher: publisher.to_string(),
        slug: slug.to_string(),
        version,
    })
}

fn resolve_store_api_base_url() -> String {
    std::env::var(ENV_STORE_API_URL)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_STORE_API_URL.to_string())
}

async fn resolve_external_capsule_dependencies(
    manifest_raw: &toml::Value,
) -> Result<Vec<LockedCapsuleDependency>> {
    let dependencies = manifest_external_capsule_dependencies(manifest_raw)?;
    if dependencies.is_empty() {
        return Ok(Vec::new());
    }

    let client = reqwest::Client::new();
    let base_url = resolve_store_api_base_url();
    let mut locked = Vec::new();
    for dependency in dependencies {
        if dependency.source_type != "store" {
            return Err(CapsuleError::Pack(format!(
                "capsule dependency '{}' uses source_type '{}' but only store dependencies are supported in lockfile generation",
                dependency.alias, dependency.source_type
            )));
        }
        let parsed = parse_store_capsule_source(&dependency.source)?;
        let encoded_publisher: String = byte_serialize(parsed.publisher.as_bytes()).collect();
        let encoded_slug: String = byte_serialize(parsed.slug.as_bytes()).collect();
        let mut endpoint = format!(
            "{}/v1/capsules/by/{}/{}/distributions",
            base_url, encoded_publisher, encoded_slug
        );
        if let Some(version) = parsed.version.as_deref() {
            endpoint.push_str("?version=");
            let encoded_version: String = byte_serialize(version.as_bytes()).collect();
            endpoint.push_str(&encoded_version);
        }

        let response = client.get(&endpoint).send().await.map_err(|err| {
            CapsuleError::Pack(format!(
                "Failed to resolve capsule dependency '{}' from store: {}",
                dependency.alias, err
            ))
        })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(CapsuleError::Pack(format!(
                "Failed to resolve capsule dependency '{}' ({}) status={}: {}",
                dependency.alias, parsed.scoped_id, status, body
            )));
        }
        let resolved = response
            .json::<LockfileStoreDistributionResponse>()
            .await
            .map_err(|err| {
                CapsuleError::Pack(format!(
                    "Failed to parse lockfile dependency resolution for '{}': {}",
                    dependency.alias, err
                ))
            })?;
        let digest = resolved.blake3.clone().or_else(|| resolved.sha256.clone());
        locked.push(LockedCapsuleDependency {
            name: dependency.alias,
            source: dependency.source,
            source_type: dependency.source_type,
            injection_bindings: dependency.injection_bindings,
            resolved_version: Some(resolved.version),
            digest,
            sha256: resolved.sha256,
            artifact_url: resolved.artifact_url,
        });
    }

    locked.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(locked)
}

pub fn verify_lockfile_external_dependencies(
    manifest_raw: &toml::Value,
    lockfile: &CapsuleLock,
) -> Result<()> {
    let expected = manifest_external_capsule_dependencies(manifest_raw)?;
    if expected.is_empty() {
        return Ok(());
    }

    for dependency in expected {
        let Some(locked) = lockfile
            .capsule_dependencies
            .iter()
            .find(|item| item.name == dependency.alias)
        else {
            return Err(CapsuleError::Config(format!(
                "{} is missing capsule dependency '{}'",
                CAPSULE_LOCK_FILE_NAME, dependency.alias
            )));
        };
        if locked.source != dependency.source
            || locked.source_type != dependency.source_type
            || locked.injection_bindings != dependency.injection_bindings
        {
            return Err(CapsuleError::Config(format!(
                "{} capsule dependency '{}' does not match manifest source '{}'",
                CAPSULE_LOCK_FILE_NAME, dependency.alias, dependency.source
            )));
        }
    }

    Ok(())
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
    let capsule_dependencies = resolve_external_capsule_dependencies(manifest_raw).await?;

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
            configure_python_lockfile(
                manifest_raw,
                manifest_dir,
                &reporter,
                timings,
                &target_key,
                &runtime_platforms,
                required_runtime_version.as_deref(),
                &mut runtimes,
                &mut tools,
                &mut targets,
            )
            .await?;
        } else if lang == "node" {
            configure_node_lockfile(
                manifest_raw,
                manifest_dir,
                &reporter,
                timings,
                &target_key,
                &runtime_platforms,
                required_runtime_version.as_deref(),
                &mut runtimes,
                &mut tools,
                &mut targets,
            )
            .await?;
        } else if lang == "deno" {
            configure_deno_lockfile(
                manifest_raw,
                manifest_dir,
                &reporter,
                timings,
                &runtime_platforms,
                required_runtime_version.as_deref(),
                &runtime_tools,
                &mut runtimes,
                &mut tools,
            )
            .await?;
        }
    }

    ensure_orchestration_target_runtimes(
        manifest_raw,
        &reporter,
        &runtime_platforms,
        &mut runtimes,
        &mut tools,
    )
    .await?;

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
        capsule_dependencies,
        injected_data: HashMap::new(),
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

#[allow(clippy::too_many_arguments)]
async fn configure_python_lockfile(
    manifest_raw: &toml::Value,
    manifest_dir: &Path,
    reporter: &Arc<dyn CapsuleReporter + 'static>,
    timings: bool,
    target_key: &str,
    runtime_platforms: &[RuntimePlatform],
    required_runtime_version: Option<&str>,
    runtimes: &mut RuntimeSection,
    tools: &mut ToolSection,
    targets: &mut HashMap<String, TargetEntry>,
) -> Result<()> {
    let version = required_runtime_version
        .map(str::to_string)
        .or_else(|| read_runtime_version(manifest_raw))
        .unwrap_or_else(|| read_language_version(manifest_raw, "python", "3.11"));
    let step_started = Instant::now();
    let python_lockfile = generate_uv_lock(manifest_dir, manifest_raw, reporter.clone()).await?;
    maybe_report_timing(
        reporter,
        timings,
        "lockfile.generate_uv_lock",
        step_started.elapsed(),
    )
    .await?;
    let step_started = Instant::now();
    let runtime = resolve_python_runtime(&version, runtime_platforms, reporter.clone()).await?;
    maybe_report_timing(
        reporter,
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
            target_key,
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
        let target_entry = targets.entry(target_key.to_string()).or_default();
        target_entry.python_lockfile = Some("uv.lock".to_string());
        if let Some(artifacts) = python_artifacts {
            target_entry.artifacts.extend(artifacts);
        }
        let step_started = Instant::now();
        tools.uv = Some(resolve_uv_tool_targets(runtime_platforms, reporter.clone()).await?);
        maybe_report_timing(
            reporter,
            timings,
            "lockfile.resolve_uv_tool_targets",
            step_started.elapsed(),
        )
        .await?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn configure_node_lockfile(
    manifest_raw: &toml::Value,
    manifest_dir: &Path,
    reporter: &Arc<dyn CapsuleReporter + 'static>,
    timings: bool,
    target_key: &str,
    runtime_platforms: &[RuntimePlatform],
    required_runtime_version: Option<&str>,
    runtimes: &mut RuntimeSection,
    tools: &mut ToolSection,
    targets: &mut HashMap<String, TargetEntry>,
) -> Result<()> {
    let version = required_runtime_version
        .map(str::to_string)
        .or_else(|| read_runtime_version(manifest_raw))
        .unwrap_or_else(|| read_language_version(manifest_raw, "node", "20"));
    let step_started = Instant::now();
    let node_lockfile =
        generate_pnpm_lock(manifest_dir, manifest_raw, &version, reporter.clone()).await?;
    maybe_report_timing(
        reporter,
        timings,
        "lockfile.generate_pnpm_lock",
        step_started.elapsed(),
    )
    .await?;
    let step_started = Instant::now();
    let runtime = resolve_node_runtime(&version, runtime_platforms, reporter.clone()).await?;
    maybe_report_timing(
        reporter,
        timings,
        "lockfile.resolve_node_runtime",
        step_started.elapsed(),
    )
    .await?;
    runtimes.node = Some(runtime);

    if runtimes.deno.is_none() {
        let deno_version = read_language_version(manifest_raw, "deno", "2.6.8");
        let step_started = Instant::now();
        let deno_runtime =
            resolve_deno_runtime(&deno_version, runtime_platforms, reporter.clone()).await?;
        maybe_report_timing(
            reporter,
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
            target_key,
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
        let target_entry = targets.entry(target_key.to_string()).or_default();
        target_entry.node_lockfile = Some(format!("locks/{}/pnpm-lock.yaml", target_key));
        if let Some(artifacts) = node_artifacts {
            target_entry.artifacts.extend(artifacts);
        }
        tools.pnpm = Some(resolve_pnpm_tool_targets(runtime_platforms));
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn configure_deno_lockfile(
    manifest_raw: &toml::Value,
    manifest_dir: &Path,
    reporter: &Arc<dyn CapsuleReporter + 'static>,
    timings: bool,
    runtime_platforms: &[RuntimePlatform],
    required_runtime_version: Option<&str>,
    runtime_tools: &HashMap<String, String>,
    runtimes: &mut RuntimeSection,
    tools: &mut ToolSection,
) -> Result<()> {
    let version = required_runtime_version
        .map(str::to_string)
        .or_else(|| read_runtime_version(manifest_raw))
        .unwrap_or_else(|| read_language_version(manifest_raw, "deno", "2.6.8"));
    let step_started = Instant::now();
    let runtime = resolve_deno_runtime(&version, runtime_platforms, reporter.clone()).await?;
    maybe_report_timing(
        reporter,
        timings,
        "lockfile.resolve_deno_runtime",
        step_started.elapsed(),
    )
    .await?;
    runtimes.deno = Some(runtime);

    if let Some(node_version) = runtime_tools.get("node") {
        let step_started = Instant::now();
        let runtime =
            resolve_node_runtime(node_version, runtime_platforms, reporter.clone()).await?;
        maybe_report_timing(
            reporter,
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
            resolve_python_runtime(python_version, runtime_platforms, reporter.clone()).await?;
        maybe_report_timing(
            reporter,
            timings,
            "lockfile.resolve_python_runtime",
            step_started.elapsed(),
        )
        .await?;
        runtimes.python = Some(runtime);

        let step_started = Instant::now();
        tools.uv = Some(resolve_uv_tool_targets(runtime_platforms, reporter.clone()).await?);
        maybe_report_timing(
            reporter,
            timings,
            "lockfile.resolve_uv_tool_targets",
            step_started.elapsed(),
        )
        .await?;
    }

    let is_web_static = selected_target_runtime(manifest_raw).as_deref() == Some("web")
        && selected_target_driver(manifest_raw).as_deref() == Some("static");
    let skip_deno_lock_generation = selected_target_cmd_contains(manifest_raw, "--no-lock");
    if !is_web_static && !skip_deno_lock_generation {
        let step_started = Instant::now();
        let _ = generate_deno_lock(manifest_dir, manifest_raw, &version, reporter.clone()).await?;
        maybe_report_timing(
            reporter,
            timings,
            "lockfile.generate_deno_lock",
            step_started.elapsed(),
        )
        .await?;
    }

    Ok(())
}

async fn ensure_orchestration_target_runtimes(
    manifest_raw: &toml::Value,
    reporter: &Arc<dyn CapsuleReporter + 'static>,
    runtime_platforms: &[RuntimePlatform],
    runtimes: &mut RuntimeSection,
    tools: &mut ToolSection,
) -> Result<()> {
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
            ensure_node_runtime_if_missing(
                runtimes,
                runtime_version.as_deref().unwrap_or("20"),
                runtime_platforms,
                reporter,
            )
            .await?;
        }

        if matches!(driver.as_deref(), Some("python")) && runtimes.python.is_none() {
            ensure_python_runtime_if_missing(
                runtimes,
                runtime_version.as_deref().unwrap_or("3.11"),
                runtime_platforms,
                reporter,
            )
            .await?;
            ensure_uv_tool_if_missing(tools, runtime_platforms, reporter).await?;
        }

        if matches!(driver.as_deref(), Some("deno")) && runtimes.deno.is_none() {
            ensure_deno_runtime_if_missing(
                runtimes,
                runtime_version.as_deref().unwrap_or("2.6.8"),
                runtime_platforms,
                reporter,
            )
            .await?;
        }

        if runtime.as_deref() == Some("web")
            && matches!(driver.as_deref(), Some("static"))
            && runtimes.deno.is_none()
        {
            ensure_deno_runtime_if_missing(
                runtimes,
                runtime_version.as_deref().unwrap_or("2.6.8"),
                runtime_platforms,
                reporter,
            )
            .await?;
        }

        if let Some(node_version) = runtime_tools.get("node") {
            ensure_node_runtime_if_missing(runtimes, node_version, runtime_platforms, reporter)
                .await?;
        }
        if let Some(python_version) = runtime_tools.get("python") {
            ensure_python_runtime_if_missing(runtimes, python_version, runtime_platforms, reporter)
                .await?;
            ensure_uv_tool_if_missing(tools, runtime_platforms, reporter).await?;
        }
    }

    Ok(())
}

async fn ensure_node_runtime_if_missing(
    runtimes: &mut RuntimeSection,
    version: &str,
    runtime_platforms: &[RuntimePlatform],
    reporter: &Arc<dyn CapsuleReporter + 'static>,
) -> Result<()> {
    if runtimes.node.is_none() {
        runtimes.node =
            Some(resolve_node_runtime(version, runtime_platforms, reporter.clone()).await?);
    }
    Ok(())
}

async fn ensure_python_runtime_if_missing(
    runtimes: &mut RuntimeSection,
    version: &str,
    runtime_platforms: &[RuntimePlatform],
    reporter: &Arc<dyn CapsuleReporter + 'static>,
) -> Result<()> {
    if runtimes.python.is_none() {
        runtimes.python =
            Some(resolve_python_runtime(version, runtime_platforms, reporter.clone()).await?);
    }
    Ok(())
}

async fn ensure_deno_runtime_if_missing(
    runtimes: &mut RuntimeSection,
    version: &str,
    runtime_platforms: &[RuntimePlatform],
    reporter: &Arc<dyn CapsuleReporter + 'static>,
) -> Result<()> {
    if runtimes.deno.is_none() {
        runtimes.deno =
            Some(resolve_deno_runtime(version, runtime_platforms, reporter.clone()).await?);
    }
    Ok(())
}

async fn ensure_uv_tool_if_missing(
    tools: &mut ToolSection,
    runtime_platforms: &[RuntimePlatform],
    reporter: &Arc<dyn CapsuleReporter + 'static>,
) -> Result<()> {
    if tools.uv.is_none() {
        tools.uv = Some(resolve_uv_tool_targets(runtime_platforms, reporter.clone()).await?);
    }
    Ok(())
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
    if let Some(driver) = selected_target_driver(manifest) {
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
        .or_else(|| selected_target_cmd_driver(manifest))
}

fn selected_target_cmd_contains(manifest: &toml::Value, flag: &str) -> bool {
    selected_target_table(manifest)
        .and_then(|t| t.get("cmd"))
        .and_then(|v| v.as_array())
        .map(|cmd| cmd.iter().any(|entry| entry.as_str() == Some(flag)))
        .unwrap_or(false)
}

fn selected_target_cmd_driver(manifest: &toml::Value) -> Option<String> {
    let program = selected_target_table(manifest)
        .and_then(|t| t.get("cmd"))
        .and_then(|v| v.as_array())
        .and_then(|cmd| cmd.first())
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())?;

    match program.as_str() {
        "deno" => Some("deno".to_string()),
        "node" | "nodejs" => Some("node".to_string()),
        "python" | "python3" | "py" => Some("python".to_string()),
        _ => None,
    }
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
#[path = "lockfile_tests.rs"]
mod tests;

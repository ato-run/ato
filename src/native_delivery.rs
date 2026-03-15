use anyhow::{bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use goblin::Object;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use walkdir::WalkDir;

use crate::install;
use crate::registry::RegistryResolver;

#[cfg(windows)]
use mslnk::ShellLink;
#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::symlink_dir;

const DEFAULT_FETCHES_DIR: &str = ".ato/fetches";
const FETCH_ARTIFACT_DIR: &str = "artifact";
const FETCH_METADATA_FILE: &str = "fetch.json";
const FETCH_SOURCE_ARTIFACT_FILE: &str = "artifact.capsule";
const DELIVERY_CONFIG_FILE: &str = "ato.delivery.toml";
const PROVENANCE_FILE: &str = "local-derivation.json";
const DELIVERY_SCHEMA_VERSION_STABLE: &str = "0.1";
const DELIVERY_SCHEMA_VERSION_LEGACY: &str = "exp-0.1";
const DELIVERY_SCHEMA_VERSION: &str = DELIVERY_SCHEMA_VERSION_STABLE;
const DEFAULT_DELIVERY_FRAMEWORK: &str = "tauri";
const DELIVERY_STAGE: &str = "unsigned";
const DEFAULT_DELIVERY_TARGET: &str = "darwin/arm64";
const DEFAULT_FINALIZE_TOOL: &str = "codesign";
const DEFAULT_MACOS_LAUNCHER_DIR: &str = "Applications";
const DEFAULT_LINUX_DESKTOP_ENTRY_DIR: &str = ".local/share/applications";
const DEFAULT_LINUX_BIN_DIR: &str = ".local/bin";
const PROJECTIONS_DIR: &str = ".ato/native-delivery/projections";
const PROJECTION_KIND_SYMLINK: &str = "symlink";
const PROJECTION_KIND_LINUX_DESKTOP_ENTRY: &str = "linux-desktop-entry";
const DEFAULT_DERIVED_APPS_DIR: &str = ".ato/apps";
const LINUX_PROJECTION_EXEC_SEARCH_MAX_DEPTH: usize = 3;

#[derive(Debug, Serialize)]
pub struct FetchResult {
    pub schema_version: String,
    pub scoped_id: String,
    pub version: String,
    pub cache_dir: PathBuf,
    pub artifact_dir: PathBuf,
    pub parent_digest: String,
    pub registry: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeBuildCommand {
    pub program: String,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeBuildPlan {
    pub manifest_path: PathBuf,
    pub manifest_dir: PathBuf,
    pub delivery_config_path: Option<PathBuf>,
    pub staged_delivery_config_toml: String,
    pub source_app_path: PathBuf,
    pub input_relative: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<NativeBuildCommand>,
    pub framework: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeBuildResult {
    pub artifact_path: PathBuf,
    pub build_strategy: String,
    pub target: String,
    pub derived_from: PathBuf,
    pub schema_version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeArtifactSpec {
    pub framework: String,
    pub target: String,
    pub input: String,
    pub finalize_tool: String,
}

#[derive(Debug, Serialize)]
pub struct FinalizeResult {
    pub fetched_dir: PathBuf,
    pub output_dir: PathBuf,
    pub derived_app_path: PathBuf,
    pub provenance_path: PathBuf,
    pub parent_digest: String,
    pub derived_digest: String,
    pub schema_version: String,
}

#[derive(Debug, Serialize)]
pub struct ProjectResult {
    pub projection_id: String,
    pub metadata_path: PathBuf,
    pub launcher_dir: PathBuf,
    pub projected_path: PathBuf,
    pub derived_app_path: PathBuf,
    pub parent_digest: String,
    pub derived_digest: String,
    pub state: String,
    pub problems: Vec<String>,
    pub created: bool,
    pub schema_version: String,
}

#[derive(Debug, Serialize)]
pub struct UnprojectResult {
    pub projection_id: String,
    pub metadata_path: PathBuf,
    pub projected_path: PathBuf,
    pub removed_projected_path: bool,
    pub removed_metadata: bool,
    pub state_before: String,
    pub problems_before: Vec<String>,
    pub schema_version: String,
}

#[derive(Debug, Serialize)]
pub struct ProjectionListResult {
    pub projections: Vec<ProjectionStatus>,
    pub total: usize,
    pub broken: usize,
}

#[derive(Debug, Serialize, Clone)]
pub struct ProjectionStatus {
    pub projection_id: String,
    pub metadata_path: PathBuf,
    pub launcher_dir: PathBuf,
    pub projected_path: PathBuf,
    pub derived_app_path: PathBuf,
    pub parent_digest: String,
    pub derived_digest: String,
    pub state: String,
    pub problems: Vec<String>,
    pub projected_at: String,
    pub projection_kind: String,
    pub schema_version: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct FetchMetadata {
    schema_version: String,
    scoped_id: String,
    version: String,
    registry: String,
    fetched_at: String,
    parent_digest: String,
    artifact_blake3: String,
}

#[derive(Debug, Deserialize)]
struct CapsuleDetail {
    #[serde(rename = "latestVersion", alias = "latest_version", default)]
    latest_version: Option<String>,
    releases: Vec<ReleaseInfo>,
}

#[derive(Debug, Deserialize)]
struct ReleaseInfo {
    version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeliveryConfig {
    schema_version: String,
    artifact: DeliveryArtifact,
    finalize: DeliveryFinalize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeliveryArtifact {
    framework: String,
    stage: String,
    target: String,
    input: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeliveryFinalize {
    tool: String,
    args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeArtifactKind {
    MacOsAppBundle,
    Directory,
    File,
}

impl NativeArtifactKind {
    fn from_path(path: &Path) -> Self {
        if path_has_extension(path, "app") {
            Self::MacOsAppBundle
        } else if native_file_candidate_extension(path).is_some() || path.is_file() {
            Self::File
        } else {
            Self::Directory
        }
    }
}

impl std::fmt::Display for NativeArtifactKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MacOsAppBundle => write!(f, "macOS app bundle"),
            Self::Directory => write!(f, "directory"),
            Self::File => write!(f, "single-file artifact"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalizeRunnerKind {
    Codesign,
    ExternalCommand,
}

#[derive(Debug, Clone)]
struct FinalizeRunner {
    tool: String,
    kind: FinalizeRunnerKind,
}

impl FinalizeRunner {
    fn for_tool(tool: &str) -> Self {
        let trimmed = tool.trim();
        let kind = if trimmed.eq_ignore_ascii_case("codesign") {
            FinalizeRunnerKind::Codesign
        } else {
            FinalizeRunnerKind::ExternalCommand
        };
        Self {
            tool: trimmed.to_string(),
            kind,
        }
    }

    fn strip_existing_signature(&self, artifact_path: &Path) -> Result<()> {
        match self.kind {
            FinalizeRunnerKind::Codesign => strip_codesign_signature(&self.tool, artifact_path),
            FinalizeRunnerKind::ExternalCommand => Ok(()),
        }
    }

    fn run(&self, derived_dir: &Path, config: &DeliveryConfig) -> Result<()> {
        match self.kind {
            FinalizeRunnerKind::Codesign | FinalizeRunnerKind::ExternalCommand => {
                run_finalize_command(derived_dir, config)
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct LocalDerivationProvenance {
    #[serde(default = "default_delivery_schema_version")]
    schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scoped_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    registry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artifact_blake3: Option<String>,
    parent_digest: String,
    derived_digest: String,
    framework: String,
    target: String,
    finalized_locally: bool,
    finalize_tool: String,
    finalized_at: String,
}

#[derive(Debug, PartialEq, Eq)]
struct ResolvedFetchRequest {
    capsule_ref: String,
    registry_url: Option<String>,
    version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ProjectionMetadata {
    schema_version: String,
    projection_id: String,
    projection_kind: String,
    projected_at: String,
    launcher_dir: PathBuf,
    projected_path: PathBuf,
    derived_app_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    projected_command_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    projected_command_target: Option<PathBuf>,
    provenance_path: PathBuf,
    parent_digest: String,
    derived_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scoped_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    registry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artifact_blake3: Option<String>,
    framework: String,
    target: String,
    finalized_at: String,
}

#[derive(Debug, Clone)]
struct ProjectionSource {
    derived_app_path: PathBuf,
    provenance_path: PathBuf,
    projection_kind: ProjectionKind,
    projected_command_target: Option<PathBuf>,
    parent_digest: String,
    derived_digest: String,
    scoped_id: Option<String>,
    version: Option<String>,
    registry: Option<String>,
    artifact_blake3: Option<String>,
    framework: String,
    target: String,
    finalized_at: String,
}

#[derive(Debug)]
struct StoredProjection {
    metadata_path: PathBuf,
    metadata: ProjectionMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectionKind {
    Symlink,
    LinuxDesktopEntry,
}

impl ProjectionKind {
    fn for_target(target: &str) -> Option<Self> {
        match delivery_target_os_family(target) {
            Some("darwin") => Some(Self::Symlink),
            Some("windows") => Some(Self::Symlink),
            Some("linux") => Some(Self::LinuxDesktopEntry),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Symlink => PROJECTION_KIND_SYMLINK,
            Self::LinuxDesktopEntry => PROJECTION_KIND_LINUX_DESKTOP_ENTRY,
        }
    }
}

pub(crate) fn detect_build_strategy(manifest_dir: &Path) -> Result<Option<NativeBuildPlan>> {
    let manifest_path = manifest_dir.join("capsule.toml");
    let delivery_config_path = manifest_dir.join(DELIVERY_CONFIG_FILE);
    if !manifest_path.exists() {
        return Ok(None);
    }

    let manifest_raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest = capsule_core::types::CapsuleManifest::from_toml(&manifest_raw)
        .map_err(|err| anyhow::anyhow!("Failed to parse {}: {}", manifest_path.display(), err))?;
    let Ok(target) = manifest.resolve_default_target() else {
        return Ok(None);
    };

    let canonical_config = detect_native_manifest_contract(target)?;
    let inline_config = load_inline_delivery_config(&manifest_raw, &manifest_path)?;
    let explicit_config = match (delivery_config_path.exists(), inline_config) {
        (true, Some(inline)) => {
            let existing = load_delivery_config(&delivery_config_path)?;
            ensure_delivery_config_compatible(&existing, &inline, &delivery_config_path)?;
            existing
        }
        (true, None) => load_delivery_config(&delivery_config_path)?,
        (false, Some(inline)) => inline,
        (false, None) => match canonical_config.clone() {
            Some(config) => config,
            None => return Ok(None),
        },
    };
    if let Some(canonical) = &canonical_config {
        ensure_delivery_config_matches_context(&explicit_config, canonical, &manifest_path)?;
    }
    let config_path = if delivery_config_path.exists() {
        Some(delivery_config_path)
    } else {
        None
    };
    let config = explicit_config;

    let input_relative = PathBuf::from(config.artifact.input.trim());
    validate_relative_input_path(&input_relative)?;
    let source_app_path = manifest_dir.join(&input_relative);
    let build_command = detect_native_build_command(
        target,
        manifest_dir,
        config_path.is_some() || canonical_config.is_none(),
    )?;
    if build_command.is_none() {
        validate_native_bundle_directory(&source_app_path)?;
    }

    Ok(Some(NativeBuildPlan {
        manifest_path,
        manifest_dir: manifest_dir.to_path_buf(),
        delivery_config_path: config_path,
        staged_delivery_config_toml: serialize_delivery_config(&config)?,
        source_app_path,
        input_relative,
        build_command,
        framework: config.artifact.framework,
        target: config.artifact.target,
    }))
}

pub(crate) fn build_native_artifact(
    plan: &NativeBuildPlan,
    output_path: Option<&Path>,
) -> Result<NativeBuildResult> {
    if !host_supports_finalize() {
        bail!("native delivery build currently supports macOS and Windows hosts only");
    }

    let config = staged_delivery_config(plan)?;
    let runner = FinalizeRunner::for_tool(&config.finalize.tool);
    build_native_artifact_with_strip(plan, output_path, |artifact_path| {
        runner.strip_existing_signature(artifact_path)
    })
}

fn build_native_artifact_with_strip<F>(
    plan: &NativeBuildPlan,
    output_path: Option<&Path>,
    strip_signature: F,
) -> Result<NativeBuildResult>
where
    F: Fn(&Path) -> Result<()>,
{
    let _config = staged_delivery_config(plan)?;
    if let Some(build_command) = &plan.build_command {
        run_native_build_command(build_command)?;
    }

    validate_native_bundle_directory(&plan.source_app_path)?;
    ensure_native_artifact_kind_supported(&plan.source_app_path, "build")?;
    let manifest_raw = fs::read_to_string(&plan.manifest_path).with_context(|| {
        format!(
            "Failed to read capsule manifest for native build: {}",
            plan.manifest_path.display()
        )
    })?;
    let manifest =
        capsule_core::types::CapsuleManifest::from_toml(&manifest_raw).map_err(|err| {
            anyhow::anyhow!("Failed to parse {}: {}", plan.manifest_path.display(), err)
        })?;

    let artifact_path = output_path.map(Path::to_path_buf).unwrap_or_else(|| {
        default_native_artifact_path(&plan.manifest_dir, &manifest.name, &manifest.version)
    });
    if let Some(parent) = artifact_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    validate_minimal_native_artifact_permissions(&plan.source_app_path)?;

    let tmp_root = plan.manifest_dir.join(".tmp");
    fs::create_dir_all(&tmp_root)
        .with_context(|| format!("Failed to create {}", tmp_root.display()))?;
    let staging_root = create_temp_subdir(&tmp_root, "native-build")?;
    let payload_root = staging_root.join("payload");
    let staged_app_path = payload_root.join(&plan.input_relative);

    let result = (|| -> Result<NativeBuildResult> {
        if let Some(parent) = staged_app_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        copy_recursively(&plan.source_app_path, &staged_app_path)?;
        strip_signature(&staged_app_path)?;
        validate_minimal_native_artifact_permissions(&staged_app_path)?;

        fs::write(
            payload_root.join(DELIVERY_CONFIG_FILE),
            &plan.staged_delivery_config_toml,
        )
        .context("Failed to stage native delivery compatibility metadata")?;

        let payload_tar = create_payload_tar_from_directory(&payload_root)?;
        let payload_tar_zst = zstd::stream::encode_all(Cursor::new(&payload_tar), 3)
            .context("Failed to encode native payload.tar.zst")?;
        let capsule_bytes = build_capsule_archive(&manifest, &payload_tar_zst, &payload_tar)?;
        fs::write(&artifact_path, &capsule_bytes)
            .with_context(|| format!("Failed to write {}", artifact_path.display()))?;

        Ok(NativeBuildResult {
            artifact_path: artifact_path.clone(),
            build_strategy: "native-delivery".to_string(),
            target: plan.target.clone(),
            derived_from: plan.source_app_path.clone(),
            schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
        })
    })();

    let _ = fs::remove_dir_all(&staging_root);
    result
}

pub async fn execute_fetch(
    capsule_ref: &str,
    registry_url: Option<&str>,
    version: Option<&str>,
) -> Result<FetchResult> {
    let resolved = resolve_fetch_request(capsule_ref, registry_url, version)?;
    let request = install::parse_capsule_request(&resolved.capsule_ref)?;
    let scoped_ref = request.scoped_ref;
    let requested_version =
        install::merge_requested_version(request.version.as_deref(), resolved.version.as_deref())?;
    let registry = resolve_registry_url(resolved.registry_url.as_deref()).await?;
    let client = reqwest::Client::new();

    let detail_url = format!(
        "{}/v1/capsules/by/{}/{}",
        registry.trim_end_matches('/'),
        urlencoding::encode(&scoped_ref.publisher),
        urlencoding::encode(&scoped_ref.slug)
    );
    let detail: CapsuleDetail = with_ato_token(client.get(&detail_url))
        .send()
        .await
        .with_context(|| format!("Failed to connect to registry: {}", registry))?
        .error_for_status()
        .with_context(|| format!("Capsule not found: {}", scoped_ref.scoped_id))?
        .json()
        .await
        .with_context(|| {
            format!(
                "Invalid capsule detail payload for {}",
                scoped_ref.scoped_id
            )
        })?;

    let target_version = match requested_version.as_deref() {
        Some(explicit) => explicit.to_string(),
        None => detail
            .latest_version
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!(
                "No fetchable version available for '{}'. This capsule has no published release version.",
                scoped_ref.scoped_id
            ))?,
    };
    detail
        .releases
        .iter()
        .find(|release| release.version == target_version)
        .with_context(|| format!("Version {} not found", target_version))?;

    let download_url = format!(
        "{}/v1/capsules/by/{}/{}/download?version={}",
        registry.trim_end_matches('/'),
        urlencoding::encode(&scoped_ref.publisher),
        urlencoding::encode(&scoped_ref.slug),
        urlencoding::encode(&target_version)
    );
    let artifact_bytes = with_ato_token(client.get(&download_url))
        .send()
        .await
        .with_context(|| {
            format!(
                "Failed to download artifact for {}@{}",
                scoped_ref.scoped_id, target_version
            )
        })?
        .error_for_status()
        .with_context(|| {
            format!(
                "Artifact download failed for {}@{}",
                scoped_ref.scoped_id, target_version
            )
        })?
        .bytes()
        .await
        .with_context(|| {
            format!(
                "Failed to read artifact body for {}@{}",
                scoped_ref.scoped_id, target_version
            )
        })?
        .to_vec();

    materialize_fetch_cache(
        &scoped_ref.scoped_id,
        &target_version,
        &registry,
        &artifact_bytes,
    )
}

pub(crate) fn detect_install_requires_local_derivation(
    artifact_bytes: &[u8],
) -> Result<Option<NativeArtifactSpec>> {
    let payload_tar = extract_payload_tar_from_capsule(artifact_bytes)?;
    extract_native_artifact_spec_from_payload_tar(&payload_tar)
}

fn resolve_fetch_request(
    input: &str,
    registry_override: Option<&str>,
    version_override: Option<&str>,
) -> Result<ResolvedFetchRequest> {
    if let Some((inline_registry, inline_capsule_ref, inline_version)) =
        parse_inline_fetch_ref(input)?
    {
        let version =
            install::merge_requested_version(inline_version.as_deref(), version_override)?;
        let registry_url = merge_registry_override(registry_override, Some(&inline_registry))?;
        return Ok(ResolvedFetchRequest {
            capsule_ref: inline_capsule_ref,
            registry_url,
            version,
        });
    }

    Ok(ResolvedFetchRequest {
        capsule_ref: input.trim().to_string(),
        registry_url: registry_override.map(|value| value.trim().to_string()),
        version: version_override.map(|value| value.trim().to_string()),
    })
}

fn parse_inline_fetch_ref(input: &str) -> Result<Option<(String, String, Option<String>)>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("scoped_id_required: use publisher/slug (for example: koh0920/sample-capsule)");
    }

    let (registry_part, path_part) = if let Some(rest) = trimmed.strip_prefix("http://") {
        let Some((host_and_port, path)) = rest.split_once('/') else {
            return Ok(None);
        };
        (format!("http://{}", host_and_port), path)
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        let Some((host_and_port, path)) = rest.split_once('/') else {
            return Ok(None);
        };
        (format!("https://{}", host_and_port), path)
    } else {
        let Some((host_and_port, path)) = trimmed.split_once('/') else {
            return Ok(None);
        };
        if !(host_and_port.eq_ignore_ascii_case("localhost")
            || host_and_port.contains(':')
            || host_and_port.contains('.'))
        {
            return Ok(None);
        }
        (format!("http://{}", host_and_port), path)
    };

    let path = path_part.trim().trim_matches('/');
    if path.is_empty() {
        bail!("invalid_fetch_ref: missing capsule path after registry host");
    }

    let mut segments = path.split('/').collect::<Vec<_>>();
    if segments.len() > 2 {
        bail!(
            "invalid_fetch_ref: use <registry>/<slug>:<version> or <registry>/<publisher>/<slug>:<version>"
        );
    }
    let last = segments
        .pop()
        .ok_or_else(|| anyhow::anyhow!("invalid_fetch_ref: missing capsule slug"))?;
    let (slug, version) = split_inline_fetch_slug(last)?;

    let capsule_ref = match segments.as_slice() {
        [] => format!("local/{}", slug),
        [publisher] => format!("{}/{}", publisher.trim().to_ascii_lowercase(), slug),
        _ => unreachable!(),
    };

    Ok(Some((registry_part, capsule_ref, version)))
}

fn split_inline_fetch_slug(input: &str) -> Result<(String, Option<String>)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("invalid_fetch_ref: missing capsule slug");
    }
    if let Some((slug, version)) = trimmed.rsplit_once(':') {
        let slug = slug.trim();
        let version = version.trim();
        if slug.is_empty() {
            bail!("invalid_fetch_ref: missing capsule slug before version");
        }
        if version.is_empty() {
            bail!("version_required: use <registry>/<slug>:<version>");
        }
        return Ok((slug.to_ascii_lowercase(), Some(version.to_string())));
    }
    if let Some((slug, version)) = trimmed.rsplit_once('@') {
        let slug = slug.trim();
        let version = version.trim();
        if slug.is_empty() {
            bail!("invalid_fetch_ref: missing capsule slug before version");
        }
        if version.is_empty() {
            bail!("version_required: use <registry>/<slug>@<version>");
        }
        return Ok((slug.to_ascii_lowercase(), Some(version.to_string())));
    }
    Ok((trimmed.to_ascii_lowercase(), None))
}

fn merge_registry_override(
    registry_override: Option<&str>,
    inline_registry: Option<&str>,
) -> Result<Option<String>> {
    let explicit = registry_override
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let inline = inline_registry
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (explicit, inline) {
        (Some(left), Some(right))
            if normalize_registry_url(left) != normalize_registry_url(right) =>
        {
            bail!(
                "conflicting_registry_request: ref specifies registry '{}' but --registry requested '{}'",
                right,
                left
            );
        }
        (Some(left), _) => Ok(Some(left.to_string())),
        (None, Some(right)) => Ok(Some(right.to_string())),
        (None, None) => Ok(None),
    }
}

fn normalize_registry_url(input: &str) -> String {
    input.trim().trim_end_matches('/').to_ascii_lowercase()
}

fn default_delivery_schema_version() -> String {
    DELIVERY_SCHEMA_VERSION_STABLE.to_string()
}

pub(crate) fn delivery_schema_version() -> &'static str {
    DELIVERY_SCHEMA_VERSION_STABLE
}

fn default_delivery_framework() -> &'static str {
    DEFAULT_DELIVERY_FRAMEWORK
}

fn normalize_delivery_os(os: &str) -> &str {
    match os {
        "macos" => "darwin",
        other => other,
    }
}

fn normalize_delivery_arch(arch: &str) -> &str {
    match arch {
        "aarch64" => "arm64",
        other => other,
    }
}

fn default_delivery_target() -> String {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => DEFAULT_DELIVERY_TARGET.to_string(),
        ("macos", "x86_64") => "darwin/x86_64".to_string(),
        ("windows", "x86_64") => "windows/x86_64".to_string(),
        ("linux", "x86_64") => "linux/x86_64".to_string(),
        (os, arch) => {
            let os = normalize_delivery_os(os);
            let arch = normalize_delivery_arch(arch);
            format!("{os}/{arch}")
        }
    }
}

fn default_delivery_target_for_input(input: &str) -> String {
    let input_path = Path::new(input);
    if path_has_extension(input_path, "app") {
        if cfg!(target_os = "macos") && std::env::consts::ARCH == "x86_64" {
            return "darwin/x86_64".to_string();
        }
        return DEFAULT_DELIVERY_TARGET.to_string();
    }
    if path_has_extension(input_path, "exe") {
        return format!(
            "windows/{}",
            normalize_delivery_arch(std::env::consts::ARCH)
        );
    }
    default_delivery_target()
}

fn default_finalize_tool() -> &'static str {
    DEFAULT_FINALIZE_TOOL
}

fn default_finalize_tool_for_input(input: &str) -> &'static str {
    if path_has_extension(Path::new(input), "exe") {
        return "signtool";
    }
    default_finalize_tool()
}

fn default_finalize_args_for_input(input: &str) -> Vec<String> {
    if path_has_extension(Path::new(input), "exe") {
        return vec![
            "sign".to_string(),
            "/fd".to_string(),
            "SHA256".to_string(),
            input.to_string(),
        ];
    }
    vec![
        "--deep".to_string(),
        "--force".to_string(),
        "--sign".to_string(),
        "-".to_string(),
        input.to_string(),
    ]
}

fn delivery_config_from_input(input: &str) -> DeliveryConfig {
    DeliveryConfig {
        schema_version: DELIVERY_SCHEMA_VERSION_STABLE.to_string(),
        artifact: DeliveryArtifact {
            framework: default_delivery_framework().to_string(),
            stage: DELIVERY_STAGE.to_string(),
            target: default_delivery_target_for_input(input),
            input: input.to_string(),
        },
        finalize: DeliveryFinalize {
            tool: default_finalize_tool_for_input(input).to_string(),
            args: default_finalize_args_for_input(input),
        },
    }
}

fn detect_native_manifest_contract(
    target: &capsule_core::types::NamedTarget,
) -> Result<Option<DeliveryConfig>> {
    if target.driver.as_deref() != Some("native") {
        return Ok(None);
    }

    let input = target.entrypoint.trim();
    if input.is_empty() {
        return Ok(None);
    }

    let input_path = PathBuf::from(input);
    validate_relative_input_path(&input_path)?;
    if !matches!(
        NativeArtifactKind::from_path(&input_path),
        NativeArtifactKind::MacOsAppBundle | NativeArtifactKind::File
    ) {
        return Ok(None);
    }

    Ok(Some(delivery_config_from_input(input)))
}

fn detect_native_build_command(
    target: &capsule_core::types::NamedTarget,
    manifest_dir: &Path,
    has_explicit_delivery_config: bool,
) -> Result<Option<NativeBuildCommand>> {
    if target.driver.as_deref() != Some("native") || !has_explicit_delivery_config {
        return Ok(None);
    }

    let program = target.entrypoint.trim();
    if program.is_empty() || target.cmd.is_empty() {
        return Ok(None);
    }

    let program_path = Path::new(program);
    if program_path.extension().and_then(|ext| ext.to_str()) == Some("app") {
        return Ok(None);
    }

    let working_dir =
        resolve_native_build_working_dir(manifest_dir, target.working_dir.as_deref())?;

    Ok(Some(NativeBuildCommand {
        program: program.to_string(),
        args: target.cmd.clone(),
        working_dir,
    }))
}

fn ensure_delivery_config_compatible(
    actual: &DeliveryConfig,
    expected: &DeliveryConfig,
    path: &Path,
) -> Result<()> {
    if actual.artifact.framework != expected.artifact.framework
        || actual.artifact.stage != expected.artifact.stage
        || actual.artifact.target != expected.artifact.target
        || actual.artifact.input != expected.artifact.input
        || actual.finalize.tool != expected.finalize.tool
        || actual.finalize.args != expected.finalize.args
    {
        bail!(
            "{} conflicts with capsule.toml native target contract. Update capsule.toml or remove the compatibility sidecar.",
            path.display()
        );
    }
    Ok(())
}

fn ensure_delivery_config_matches_context(
    actual: &DeliveryConfig,
    expected: &DeliveryConfig,
    manifest_path: &Path,
) -> Result<()> {
    if actual.artifact.framework != expected.artifact.framework
        || actual.artifact.stage != expected.artifact.stage
        || actual.artifact.target != expected.artifact.target
        || actual.artifact.input != expected.artifact.input
        || actual.finalize.tool != expected.finalize.tool
        || actual.finalize.args != expected.finalize.args
    {
        bail!(
            "{} native delivery config conflicts with the default target contract",
            manifest_path.display()
        );
    }
    Ok(())
}

fn serialize_delivery_config(config: &DeliveryConfig) -> Result<String> {
    toml::to_string_pretty(config)
        .context("Failed to serialize native delivery compatibility metadata")
}

fn is_supported_delivery_schema(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed == DELIVERY_SCHEMA_VERSION_STABLE || trimmed == DELIVERY_SCHEMA_VERSION_LEGACY
}

fn validate_delivery_schema(value: &str, context: &str) -> Result<()> {
    if is_supported_delivery_schema(value) {
        return Ok(());
    }
    bail!(
        "Unsupported {} schema_version '{}'; expected '{}' (stable) or '{}' (legacy)",
        context,
        value,
        DELIVERY_SCHEMA_VERSION_STABLE,
        DELIVERY_SCHEMA_VERSION_LEGACY
    );
}

pub fn execute_finalize(
    fetched_dir: &Path,
    output_dir: &Path,
    allow_external_finalize: bool,
) -> Result<FinalizeResult> {
    if !allow_external_finalize {
        bail!("finalize requires --allow-external-finalize for any external signing step");
    }

    if !host_supports_finalize() {
        bail!("ato finalize currently supports macOS and Windows hosts only");
    }

    finalize_with_dispatch(fetched_dir, output_dir)
}

pub(crate) fn finalize_fetched_artifact(fetched_dir: &Path) -> Result<FinalizeResult> {
    let metadata = load_fetch_metadata(fetched_dir)?;
    let output_root = derived_apps_root(&metadata.scoped_id, &metadata.parent_digest)?;
    fs::create_dir_all(&output_root)
        .with_context(|| format!("Failed to create {}", output_root.display()))?;
    finalize_with_dispatch(fetched_dir, &output_root)
}

pub fn execute_project(
    derived_app_path: &Path,
    launcher_dir: Option<&Path>,
) -> Result<ProjectResult> {
    if !host_supports_projection() {
        bail!("ato project currently supports macOS, Linux, and Windows hosts only");
    }

    let launcher_dir = resolve_launcher_dir(launcher_dir)?;
    let metadata_root = projections_root()?;
    project_with_roots(derived_app_path, &launcher_dir, &metadata_root)
}

pub fn execute_project_ls() -> Result<ProjectionListResult> {
    if !host_supports_projection() {
        bail!("ato project ls currently supports macOS, Linux, and Windows hosts only");
    }

    list_projections(&projections_root()?)
}

pub fn execute_unproject(reference: &str) -> Result<UnprojectResult> {
    if !host_supports_projection() {
        bail!("ato unproject currently supports macOS, Linux, and Windows hosts only");
    }

    unproject_with_metadata_root(reference, &projections_root()?)
}

fn finalize_with_dispatch(fetched_dir: &Path, output_dir: &Path) -> Result<FinalizeResult> {
    finalize_with_runner(fetched_dir, output_dir, |derived_dir, config| {
        FinalizeRunner::for_tool(&config.finalize.tool).run(derived_dir, config)
    })
}

fn finalize_with_runner<F>(
    fetched_dir: &Path,
    output_dir: &Path,
    runner: F,
) -> Result<FinalizeResult>
where
    F: Fn(&Path, &DeliveryConfig) -> Result<()>,
{
    let metadata = load_fetch_metadata(fetched_dir)?;
    let artifact_root = fetched_dir.join(FETCH_ARTIFACT_DIR);
    if !artifact_root.is_dir() {
        bail!(
            "Fetched artifact directory is missing: {}",
            artifact_root.display()
        );
    }

    let config_path = artifact_root.join(DELIVERY_CONFIG_FILE);
    let config = load_delivery_config(&config_path)?;
    let parent_digest = compute_tree_digest(&artifact_root)?;
    if metadata.parent_digest != parent_digest {
        bail!(
            "Fetched artifact integrity mismatch: expected {}, got {}",
            metadata.parent_digest,
            parent_digest
        );
    }

    let input_relative = PathBuf::from(config.artifact.input.trim());
    validate_relative_input_path(&input_relative)?;
    let input_app_path = artifact_root.join(&input_relative);
    validate_native_bundle_directory(&input_app_path)?;
    ensure_native_artifact_kind_supported(&input_app_path, "finalize")?;

    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "Failed to create output directory: {}",
            output_dir.display()
        )
    })?;
    let derived_dir = create_unique_output_dir(output_dir)?;
    let input_name = input_app_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Finalize input path has no terminal name"))?;
    let derived_app_path = derived_dir.join(input_name);

    let result = (|| -> Result<FinalizeResult> {
        validate_minimal_native_artifact_permissions(&input_app_path)?;
        copy_recursively(&input_app_path, &derived_app_path)?;
        ensure_tree_writable(&derived_app_path)?;
        validate_minimal_native_artifact_permissions(&derived_app_path)?;
        let derived_config = rebase_delivery_config_for_finalize(&config, &derived_app_path)?;
        runner(&derived_dir, &derived_config)?;
        validate_minimal_native_artifact_permissions(&derived_app_path)?;
        let derived_digest = compute_tree_digest(&derived_app_path)?;
        let provenance = LocalDerivationProvenance {
            schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
            scoped_id: Some(metadata.scoped_id.clone()),
            version: Some(metadata.version.clone()),
            registry: Some(metadata.registry.clone()),
            artifact_blake3: Some(metadata.artifact_blake3.clone()),
            parent_digest: parent_digest.clone(),
            derived_digest: derived_digest.clone(),
            framework: config.artifact.framework.clone(),
            target: config.artifact.target.clone(),
            finalized_locally: true,
            finalize_tool: config.finalize.tool.clone(),
            finalized_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        };
        let provenance_path = derived_dir.join(PROVENANCE_FILE);
        write_json_pretty(&provenance_path, &provenance)?;
        Ok(FinalizeResult {
            fetched_dir: fetched_dir.to_path_buf(),
            output_dir: derived_dir.clone(),
            derived_app_path,
            provenance_path,
            parent_digest,
            derived_digest,
            schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
        })
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&derived_dir);
    }
    result
}

fn project_with_roots(
    derived_app_path: &Path,
    launcher_dir: &Path,
    metadata_root: &Path,
) -> Result<ProjectResult> {
    let projected_command_dir = resolve_projected_command_dir_for_host(launcher_dir)?;
    project_with_roots_and_command_dir(
        derived_app_path,
        launcher_dir,
        metadata_root,
        &projected_command_dir,
    )
}

fn project_with_roots_and_command_dir(
    derived_app_path: &Path,
    launcher_dir: &Path,
    metadata_root: &Path,
    projected_command_dir: &Path,
) -> Result<ProjectResult> {
    let source = load_projection_source(derived_app_path)?;
    fs::create_dir_all(launcher_dir).with_context(|| {
        format!(
            "Failed to create launcher directory: {}",
            launcher_dir.display()
        )
    })?;
    fs::create_dir_all(metadata_root).with_context(|| {
        format!(
            "Failed to create projection metadata directory: {}",
            metadata_root.display()
        )
    })?;

    let launcher_dir = absolute_path(launcher_dir)?;
    let projected_command_dir = absolute_path(projected_command_dir)?;
    let display_name =
        projection_display_name(&source.derived_app_path, source.scoped_id.as_deref())?;
    let command_name =
        projection_command_name(&source.derived_app_path, source.scoped_id.as_deref())?;
    let projected_path = projection_output_path(
        source.projection_kind,
        &launcher_dir,
        &source.derived_app_path,
        &command_name,
    )?;
    let projected_candidates = projection_candidate_paths(&projected_path);
    let projected_command_path = source
        .projected_command_target
        .as_ref()
        .map(|_| projected_command_dir.join(&command_name));

    let existing = load_projection_records(metadata_root)?;
    for record in &existing {
        if paths_match(&record.metadata.derived_app_path, &source.derived_app_path)? {
            let status = inspect_projection(&record.metadata, &record.metadata_path)?;
            if status.state == "ok" {
                return Ok(ProjectResult {
                    projection_id: record.metadata.projection_id.clone(),
                    metadata_path: record.metadata_path.clone(),
                    launcher_dir: record.metadata.launcher_dir.clone(),
                    projected_path: record.metadata.projected_path.clone(),
                    derived_app_path: source.derived_app_path.clone(),
                    parent_digest: source.parent_digest.clone(),
                    derived_digest: source.derived_digest.clone(),
                    state: status.state,
                    problems: status.problems,
                    created: false,
                    schema_version: record.metadata.schema_version.clone(),
                });
            }
            bail!(
                "Derived app is already projected via '{}' (id {}). Use 'ato unproject' first.",
                record.metadata.projected_path.display(),
                record.metadata.projection_id
            );
        }
        let mut candidate_conflict = false;
        for candidate in &projected_candidates {
            if paths_match(&record.metadata.projected_path, candidate)? {
                candidate_conflict = true;
                break;
            }
        }
        if candidate_conflict {
            bail!(
                "Projection name conflict: '{}' is already managed by projection {}",
                record.metadata.projected_path.display(),
                record.metadata.projection_id
            );
        }
        if let (Some(existing_command_path), Some(projected_command_path)) = (
            record.metadata.projected_command_path.as_ref(),
            projected_command_path.as_ref(),
        ) {
            if paths_match(existing_command_path, projected_command_path)? {
                bail!(
                    "Projection command conflict: '{}' is already managed by projection {}",
                    projected_command_path.display(),
                    record.metadata.projection_id
                );
            }
        }
    }

    if source.projection_kind == ProjectionKind::Symlink {
        if let Some(existing_path) =
            find_existing_projection_path(&projected_path, &source.derived_app_path)?
        {
            let projection_id = build_projection_id(
                &source.derived_app_path,
                &existing_path,
                &source.derived_digest,
            );
            let metadata_path = metadata_root.join(format!("{}.json", projection_id));
            return Ok(ProjectResult {
                projection_id,
                metadata_path,
                launcher_dir,
                projected_path: existing_path,
                derived_app_path: source.derived_app_path.clone(),
                parent_digest: source.parent_digest.clone(),
                derived_digest: source.derived_digest.clone(),
                state: "ok".to_string(),
                problems: Vec::new(),
                created: false,
                schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
            });
        }
    }

    if let Some(conflict_path) = first_existing_projection_candidate(&projected_path)? {
        bail!(
            "Projection name conflict: launcher path already exists: {}",
            conflict_path.display()
        );
    }
    if let (Some(projected_command_path), Some(projected_command_target)) = (
        projected_command_path.as_ref(),
        source.projected_command_target.as_ref(),
    ) {
        fs::create_dir_all(&projected_command_dir).with_context(|| {
            format!(
                "Failed to create projection command directory: {}",
                projected_command_dir.display()
            )
        })?;
        if (projected_command_path.exists() || fs::symlink_metadata(projected_command_path).is_ok())
            && !is_managed_projection_to(projected_command_path, projected_command_target)?
        {
            bail!(
                "Projection command conflict: command path already exists: {}",
                projected_command_path.display()
            );
        }
    }

    let source_projection_kind = source.projection_kind;
    let mut created_projected_path: Option<PathBuf> = None;
    let mut created_command_path = false;
    let mut written_metadata_path: Option<PathBuf> = None;
    let result = (|| -> Result<ProjectResult> {
        let projected_path = match source.projection_kind {
            ProjectionKind::Symlink => {
                let created = create_projection_symlink(&source.derived_app_path, &projected_path)
                    .with_context(|| {
                        format!(
                            "Failed to create projection {} -> {}",
                            projected_path.display(),
                            source.derived_app_path.display()
                        )
                    })?;
                created_projected_path = Some(created.clone());
                created
            }
            ProjectionKind::LinuxDesktopEntry => {
                let projected_command_path = projected_command_path
                    .as_ref()
                    .context("linux command path missing")?;
                let projected_command_target = source
                    .projected_command_target
                    .as_ref()
                    .context("linux command target missing")?;
                if !is_managed_projection_to(projected_command_path, projected_command_target)? {
                    create_projection_symlink(projected_command_target, projected_command_path)
                        .with_context(|| {
                            format!(
                                "Failed to create command symlink {} -> {}",
                                projected_command_path.display(),
                                projected_command_target.display()
                            )
                        })?;
                    created_command_path = true;
                }
                fs::write(
                    &projected_path,
                    render_linux_desktop_entry(
                        &display_name,
                        projected_command_path,
                        &source.derived_app_path,
                    ),
                )
                .with_context(|| {
                    format!("Failed to write desktop entry {}", projected_path.display())
                })?;
                created_projected_path = Some(projected_path.clone());
                projected_path.clone()
            }
        };
        let projection_id = build_projection_id(
            &source.derived_app_path,
            &projected_path,
            &source.derived_digest,
        );
        let metadata_path = metadata_root.join(format!("{}.json", projection_id));
        written_metadata_path = Some(metadata_path.clone());
        let metadata = ProjectionMetadata {
            schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
            projection_id: projection_id.clone(),
            projection_kind: source.projection_kind.as_str().to_string(),
            projected_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            launcher_dir: launcher_dir.clone(),
            projected_path: projected_path.clone(),
            derived_app_path: source.derived_app_path.clone(),
            projected_command_path: projected_command_path.clone(),
            projected_command_target: source.projected_command_target.clone(),
            provenance_path: source.provenance_path.clone(),
            parent_digest: source.parent_digest.clone(),
            derived_digest: source.derived_digest.clone(),
            scoped_id: source.scoped_id.clone(),
            version: source.version.clone(),
            registry: source.registry.clone(),
            artifact_blake3: source.artifact_blake3.clone(),
            framework: source.framework.clone(),
            target: source.target.clone(),
            finalized_at: source.finalized_at.clone(),
        };
        write_json_pretty(&metadata_path, &metadata)?;
        let status = inspect_projection(&metadata, &metadata_path)?;
        Ok(ProjectResult {
            projection_id,
            metadata_path: metadata_path.clone(),
            launcher_dir,
            projected_path: projected_path.clone(),
            derived_app_path: source.derived_app_path,
            parent_digest: source.parent_digest,
            derived_digest: source.derived_digest,
            state: status.state,
            problems: status.problems,
            created: true,
            schema_version: metadata.schema_version.clone(),
        })
    })();

    if result.is_err() {
        if let Some(path) = created_projected_path.as_ref() {
            let _ = remove_projected_path(path, source_projection_kind.as_str());
        }
        if created_command_path {
            if let Some(projected_command_path) = projected_command_path.as_ref() {
                let _ = remove_projection_path(projected_command_path);
            }
        }
        if let Some(metadata_path) = written_metadata_path.as_ref() {
            let _ = fs::remove_file(metadata_path);
        }
    }
    result
}

fn list_projections(metadata_root: &Path) -> Result<ProjectionListResult> {
    let projections = load_projection_records(metadata_root)?
        .into_iter()
        .map(|record| inspect_projection(&record.metadata, &record.metadata_path))
        .collect::<Result<Vec<_>>>()?;
    let broken = projections
        .iter()
        .filter(|item| item.state == "broken")
        .count();
    Ok(ProjectionListResult {
        total: projections.len(),
        broken,
        projections,
    })
}

fn unproject_with_metadata_root(reference: &str, metadata_root: &Path) -> Result<UnprojectResult> {
    let record = find_projection_record(reference, metadata_root)?;
    let status = inspect_projection(&record.metadata, &record.metadata_path)?;
    let schema_version = record.metadata.schema_version.clone();

    let removed_projected_path = remove_projected_path(
        &record.metadata.projected_path,
        &record.metadata.projection_kind,
    )
    .with_context(|| {
        format!(
            "Failed to remove projected path: {}",
            record.metadata.projected_path.display()
        )
    })?;

    if let Some(projected_command_path) = record.metadata.projected_command_path.as_ref() {
        remove_projected_path(projected_command_path, PROJECTION_KIND_SYMLINK).with_context(
            || {
                format!(
                    "Failed to remove projection command path: {}",
                    projected_command_path.display()
                )
            },
        )?;
    }

    fs::remove_file(&record.metadata_path).with_context(|| {
        format!(
            "Failed to remove projection metadata: {}",
            record.metadata_path.display()
        )
    })?;

    Ok(UnprojectResult {
        projection_id: record.metadata.projection_id,
        metadata_path: record.metadata_path,
        projected_path: record.metadata.projected_path,
        removed_projected_path,
        removed_metadata: true,
        state_before: status.state,
        problems_before: status.problems,
        schema_version,
    })
}

fn materialize_fetch_cache(
    scoped_id: &str,
    version: &str,
    registry: &str,
    artifact_bytes: &[u8],
) -> Result<FetchResult> {
    let fetches_root = fetches_root()?;
    fs::create_dir_all(&fetches_root).with_context(|| {
        format!(
            "Failed to create fetch cache root: {}",
            fetches_root.display()
        )
    })?;

    let temp_dir = create_temp_subdir(&fetches_root, ".tmp-fetch")?;
    let artifact_root = temp_dir.join(FETCH_ARTIFACT_DIR);
    fs::create_dir_all(&artifact_root).with_context(|| {
        format!(
            "Failed to create fetch artifact dir: {}",
            artifact_root.display()
        )
    })?;

    let result = (|| -> Result<FetchResult> {
        let payload_tar = extract_payload_tar_from_capsule(artifact_bytes)?;
        unpack_payload_tar(&payload_tar, &artifact_root)?;
        let parent_digest = compute_tree_digest(&artifact_root)?;
        let digest_dir_name = digest_dir_name(&parent_digest)?;
        let final_dir = fetches_root.join(digest_dir_name);
        let final_artifact_dir = final_dir.join(FETCH_ARTIFACT_DIR);

        if final_dir.exists() {
            let existing = load_fetch_metadata(&final_dir).ok();
            let existing_version = existing
                .as_ref()
                .map(|value| value.version.clone())
                .unwrap_or_else(|| version.to_string());
            let existing_schema = existing
                .as_ref()
                .map(|value| value.schema_version.clone())
                .unwrap_or_else(|| DELIVERY_SCHEMA_VERSION.to_string());
            return Ok(FetchResult {
                schema_version: existing_schema,
                scoped_id: scoped_id.to_string(),
                version: existing_version,
                cache_dir: final_dir,
                artifact_dir: final_artifact_dir,
                parent_digest,
                registry: registry.to_string(),
            });
        }

        fs::write(temp_dir.join(FETCH_SOURCE_ARTIFACT_FILE), artifact_bytes).with_context(
            || {
                format!(
                    "Failed to write fetched artifact: {}",
                    temp_dir.join(FETCH_SOURCE_ARTIFACT_FILE).display()
                )
            },
        )?;
        let metadata = FetchMetadata {
            schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
            scoped_id: scoped_id.to_string(),
            version: version.to_string(),
            registry: registry.to_string(),
            fetched_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            parent_digest: parent_digest.clone(),
            artifact_blake3: compute_blake3(artifact_bytes),
        };
        write_json_pretty(&temp_dir.join(FETCH_METADATA_FILE), &metadata)?;

        match fs::rename(&temp_dir, &final_dir) {
            Ok(()) => {}
            Err(_err) if final_dir.exists() => {
                let _ = fs::remove_dir_all(&temp_dir);
                return Ok(FetchResult {
                    schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
                    scoped_id: scoped_id.to_string(),
                    version: version.to_string(),
                    cache_dir: final_dir,
                    artifact_dir: final_artifact_dir,
                    parent_digest,
                    registry: registry.to_string(),
                });
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "Failed to finalize fetch cache {} -> {}",
                        temp_dir.display(),
                        final_dir.display()
                    )
                })
            }
        }

        Ok(FetchResult {
            schema_version: metadata.schema_version.clone(),
            scoped_id: scoped_id.to_string(),
            version: version.to_string(),
            cache_dir: final_dir,
            artifact_dir: final_artifact_dir,
            parent_digest,
            registry: registry.to_string(),
        })
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&temp_dir);
    }
    result
}

pub(crate) fn materialize_fetch_cache_from_artifact(
    scoped_id: &str,
    version: &str,
    registry: &str,
    artifact_bytes: &[u8],
) -> Result<FetchResult> {
    materialize_fetch_cache(scoped_id, version, registry, artifact_bytes)
}

fn load_fetch_metadata(fetched_dir: &Path) -> Result<FetchMetadata> {
    let metadata_path = fetched_dir.join(FETCH_METADATA_FILE);
    let raw = fs::read_to_string(&metadata_path)
        .with_context(|| format!("Failed to read fetch metadata: {}", metadata_path.display()))?;
    let metadata: FetchMetadata = serde_json::from_str(&raw).with_context(|| {
        format!(
            "Failed to parse fetch metadata: {}",
            metadata_path.display()
        )
    })?;
    validate_delivery_schema(&metadata.schema_version, "fetch.json")?;
    Ok(metadata)
}

fn load_projection_source(derived_app_path: &Path) -> Result<ProjectionSource> {
    let absolute_path = absolute_path(derived_app_path)?;
    let derived_dir = absolute_path.parent().ok_or_else(|| {
        anyhow::anyhow!("Projection input must be an ato finalize output with a parent directory")
    })?;
    let provenance_path = derived_dir.join(PROVENANCE_FILE);
    let raw = fs::read_to_string(&provenance_path).with_context(|| {
        format!(
            "ato project requires an ato finalize output containing {} next to the derived app: {}",
            PROVENANCE_FILE,
            provenance_path.display()
        )
    })?;
    let provenance: LocalDerivationProvenance = serde_json::from_str(&raw).with_context(|| {
        format!(
            "Failed to parse finalize provenance: {}",
            provenance_path.display()
        )
    })?;
    validate_delivery_schema(&provenance.schema_version, "local-derivation.json")?;
    if !provenance.finalized_locally {
        bail!("Projection input must be finalized locally via `ato finalize`");
    }
    if !supports_projection_target(&provenance.target) {
        bail!(
            "Projection input target '{}' is unsupported; expected a darwin/<arch>, linux/<arch>, or windows/<arch> target",
            provenance.target
        );
    }
    if !host_supports_projection_target(&provenance.target) {
        let expected_target = host_projection_os_family()
            .map(|family| format!("{family}/<arch>"))
            .unwrap_or_else(|| "darwin/<arch>, linux/<arch>, or windows/<arch>".to_string());
        bail!(
            "Projection input target '{}' is unsupported on this host; expected a {} target",
            provenance.target,
            expected_target
        );
    }
    let projection_kind = ProjectionKind::for_target(&provenance.target).ok_or_else(|| {
        anyhow::anyhow!(
            "Projection input target '{}' is unsupported",
            provenance.target
        )
    })?;
    validate_projection_input_shape(&absolute_path, &provenance.target, projection_kind)?;
    let derived_app_path = fs::canonicalize(&absolute_path).with_context(|| {
        format!(
            "Failed to canonicalize finalized app path: {}",
            absolute_path.display()
        )
    })?;
    let projected_command_target = match projection_kind {
        ProjectionKind::Symlink => None,
        ProjectionKind::LinuxDesktopEntry => {
            Some(resolve_linux_projection_command_target(&derived_app_path)?)
        }
    };

    let actual_digest = compute_tree_digest(&derived_app_path)?;
    if actual_digest != provenance.derived_digest {
        bail!(
            "Derived artifact digest mismatch: expected {}, got {}",
            provenance.derived_digest,
            actual_digest
        );
    }

    Ok(ProjectionSource {
        derived_app_path,
        provenance_path,
        projection_kind,
        projected_command_target,
        parent_digest: provenance.parent_digest,
        derived_digest: provenance.derived_digest,
        scoped_id: provenance.scoped_id,
        version: provenance.version,
        registry: provenance.registry,
        artifact_blake3: provenance.artifact_blake3,
        framework: provenance.framework,
        target: provenance.target,
        finalized_at: provenance.finalized_at,
    })
}

fn validate_projection_input_shape(
    path: &Path,
    target: &str,
    projection_kind: ProjectionKind,
) -> Result<()> {
    if !path.is_dir() {
        bail!(
            "Projection input must be a finalized directory artifact: {}",
            path.display()
        );
    }
    if projection_kind == ProjectionKind::Symlink
        && delivery_target_os_family(target) == Some("darwin")
        && path.extension().and_then(|ext| ext.to_str()) != Some("app")
    {
        bail!("Projection input must be a .app bundle: {}", path.display());
    }
    Ok(())
}

fn resolve_linux_projection_command_target(derived_app_path: &Path) -> Result<PathBuf> {
    let preferred = derived_app_path.join(
        derived_app_path
            .file_stem()
            .or_else(|| derived_app_path.file_name())
            .ok_or_else(|| anyhow::anyhow!("Derived app path has no terminal name"))?,
    );
    if preferred.is_file() && is_executable_file(&preferred)? {
        return Ok(preferred);
    }

    let mut candidates = Vec::new();
    for entry in WalkDir::new(derived_app_path)
        .min_depth(1)
        .max_depth(LINUX_PROJECTION_EXEC_SEARCH_MAX_DEPTH)
    {
        let entry = entry.with_context(|| {
            format!(
                "Failed to inspect projection command candidates in {}",
                derived_app_path.display()
            )
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        if is_executable_file(&path)? {
            candidates.push(path);
        }
    }
    candidates.sort();
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => bail!(
            "Projection input is missing an executable command within {} levels of {}",
            LINUX_PROJECTION_EXEC_SEARCH_MAX_DEPTH,
            derived_app_path.display()
        ),
        _ => {
            let joined = candidates
                .iter()
                .map(|path| {
                    path.strip_prefix(derived_app_path)
                        .unwrap_or(path)
                        .display()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Projection input contains multiple executable command candidates within {} levels of {}: {}",
                LINUX_PROJECTION_EXEC_SEARCH_MAX_DEPTH,
                derived_app_path.display(),
                joined
            )
        }
    }
}

fn is_executable_file(path: &Path) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("Failed to stat executable candidate {}", path.display()))?;
    if !metadata.is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        Ok(metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        Ok(true)
    }
}

fn load_projection_records(metadata_root: &Path) -> Result<Vec<StoredProjection>> {
    if !metadata_root.exists() {
        return Ok(Vec::new());
    }

    let mut entries = fs::read_dir(metadata_root)
        .with_context(|| format!("Failed to read {}", metadata_root.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("Failed to enumerate {}", metadata_root.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut out = Vec::new();
    for entry in entries {
        let metadata_path = entry.path();
        if metadata_path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&metadata_path)
            .with_context(|| format!("Failed to read {}", metadata_path.display()))?;
        let metadata: ProjectionMetadata = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse {}", metadata_path.display()))?;
        out.push(StoredProjection {
            metadata_path,
            metadata,
        });
    }
    Ok(out)
}

fn find_projection_record(reference: &str, metadata_root: &Path) -> Result<StoredProjection> {
    let records = load_projection_records(metadata_root)?;
    if records.is_empty() {
        bail!(
            "No projection metadata found in {}",
            metadata_root.display()
        );
    }

    let mut matches = Vec::new();
    let reference_path = PathBuf::from(reference);
    let reference_abs = absolute_path(&reference_path).ok();
    for record in records {
        if record.metadata.projection_id == reference {
            matches.push(record);
            continue;
        }
        if let Some(reference_abs) = reference_abs.as_ref() {
            if paths_match(reference_abs, &record.metadata.projected_path)?
                || paths_match(reference_abs, &record.metadata.derived_app_path)?
                || paths_match(reference_abs, &record.metadata_path)?
            {
                matches.push(record);
            }
        }
    }

    match matches.len() {
        0 => bail!("Projection not found: {}", reference),
        1 => Ok(matches.remove(0)),
        _ => bail!("Projection reference is ambiguous: {}", reference),
    }
}

fn inspect_projection(
    metadata: &ProjectionMetadata,
    metadata_path: &Path,
) -> Result<ProjectionStatus> {
    let mut problems = Vec::new();
    if !is_supported_delivery_schema(&metadata.schema_version) {
        problems.push(format!(
            "unsupported_schema_version:{}",
            metadata.schema_version
        ));
    }
    if !matches!(
        metadata.projection_kind.as_str(),
        PROJECTION_KIND_SYMLINK | PROJECTION_KIND_LINUX_DESKTOP_ENTRY
    ) {
        problems.push(format!(
            "unsupported_projection_kind:{}",
            metadata.projection_kind
        ));
    }
    if !supports_projection_target(&metadata.target) {
        problems.push(format!("unsupported_target:{}", metadata.target));
    }

    inspect_projected_path(metadata, &mut problems)?;

    if let Some(projected_command_path) = metadata.projected_command_path.as_ref() {
        let Some(projected_command_target) = metadata.projected_command_target.as_ref() else {
            problems.push("projected_command_target_missing".to_string());
            return finalize_projection_status(metadata, metadata_path, problems);
        };
        match inspect_projection_path(projected_command_path, projected_command_target)? {
            ProjectionPathStatus::MatchesTarget => {}
            ProjectionPathStatus::TargetMismatch => {
                problems.push("projected_command_target_mismatch".to_string())
            }
            ProjectionPathStatus::Replaced => {
                problems.push("projected_command_replaced".to_string())
            }
            ProjectionPathStatus::Missing => problems.push("projected_command_missing".to_string()),
        }
    } else if metadata.projection_kind == PROJECTION_KIND_LINUX_DESKTOP_ENTRY {
        problems.push("projected_command_missing".to_string());
    }

    if !metadata.derived_app_path.exists() {
        problems.push("derived_app_missing".to_string());
    } else if !metadata.derived_app_path.is_dir() {
        problems.push("derived_app_replaced".to_string());
    } else {
        let digest = compute_tree_digest(&metadata.derived_app_path)?;
        if digest != metadata.derived_digest {
            problems.push("derived_digest_mismatch".to_string());
        }
    }

    finalize_projection_status(metadata, metadata_path, problems)
}

fn inspect_projected_path(metadata: &ProjectionMetadata, problems: &mut Vec<String>) -> Result<()> {
    match metadata.projection_kind.as_str() {
        PROJECTION_KIND_SYMLINK => {
            match inspect_projection_path(&metadata.projected_path, &metadata.derived_app_path)? {
                ProjectionPathStatus::MatchesTarget => {}
                ProjectionPathStatus::TargetMismatch => {
                    problems.push("projected_symlink_target_mismatch".to_string())
                }
                ProjectionPathStatus::Replaced => {
                    problems.push("projected_path_replaced".to_string())
                }
                ProjectionPathStatus::Missing => {
                    problems.push("projected_path_missing".to_string())
                }
            }
            Ok(())
        }
        PROJECTION_KIND_LINUX_DESKTOP_ENTRY => inspect_linux_desktop_entry(metadata, problems),
        _ => Ok(()),
    }
}

fn inspect_linux_desktop_entry(
    metadata: &ProjectionMetadata,
    problems: &mut Vec<String>,
) -> Result<()> {
    match fs::symlink_metadata(&metadata.projected_path) {
        Ok(projected_meta) if projected_meta.is_file() => {
            let Some(projected_command_path) = metadata.projected_command_path.as_ref() else {
                problems.push("projected_command_missing".to_string());
                return Ok(());
            };
            let expected = render_linux_desktop_entry(
                &projection_display_name(
                    &metadata.derived_app_path,
                    metadata.scoped_id.as_deref(),
                )?,
                projected_command_path,
                &metadata.derived_app_path,
            );
            let actual = fs::read_to_string(&metadata.projected_path).with_context(|| {
                format!(
                    "Failed to read desktop entry: {}",
                    metadata.projected_path.display()
                )
            })?;
            if actual != expected {
                problems.push("projected_desktop_entry_mismatch".to_string());
            }
        }
        Ok(_) => problems.push("projected_path_replaced".to_string()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            problems.push("projected_path_missing".to_string())
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "Failed to inspect projected path: {}",
                    metadata.projected_path.display()
                )
            })
        }
    }
    Ok(())
}

fn finalize_projection_status(
    metadata: &ProjectionMetadata,
    metadata_path: &Path,
    problems: Vec<String>,
) -> Result<ProjectionStatus> {
    Ok(ProjectionStatus {
        projection_id: metadata.projection_id.clone(),
        metadata_path: metadata_path.to_path_buf(),
        launcher_dir: metadata.launcher_dir.clone(),
        projected_path: metadata.projected_path.clone(),
        derived_app_path: metadata.derived_app_path.clone(),
        parent_digest: metadata.parent_digest.clone(),
        derived_digest: metadata.derived_digest.clone(),
        state: if problems.is_empty() {
            "ok".to_string()
        } else {
            "broken".to_string()
        },
        problems,
        projected_at: metadata.projected_at.clone(),
        projection_kind: metadata.projection_kind.clone(),
        schema_version: metadata.schema_version.clone(),
    })
}

fn load_delivery_config(path: &Path) -> Result<DeliveryConfig> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let config: DeliveryConfig =
        toml::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))?;
    validate_delivery_config(&config)?;
    Ok(config)
}

fn load_inline_delivery_config(
    manifest_raw: &str,
    manifest_path: &Path,
) -> Result<Option<DeliveryConfig>> {
    let parsed: toml::Value = toml::from_str(manifest_raw)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
    let artifact = parsed.get("artifact").cloned();
    let finalize = parsed.get("finalize").cloned();

    match (artifact, finalize) {
        (None, None) => Ok(None),
        (Some(_), None) => bail!(
            "{} defines [artifact] without [finalize] for native delivery",
            manifest_path.display()
        ),
        (None, Some(_)) => bail!(
            "{} defines [finalize] without [artifact] for native delivery",
            manifest_path.display()
        ),
        (Some(artifact), Some(finalize)) => {
            let config = DeliveryConfig {
                schema_version: DELIVERY_SCHEMA_VERSION_STABLE.to_string(),
                artifact: artifact.try_into().with_context(|| {
                    format!(
                        "Failed to parse [artifact] from {}",
                        manifest_path.display()
                    )
                })?,
                finalize: finalize.try_into().with_context(|| {
                    format!(
                        "Failed to parse [finalize] from {}",
                        manifest_path.display()
                    )
                })?,
            };
            validate_delivery_config(&config)?;
            Ok(Some(config))
        }
    }
}

fn extract_native_artifact_spec_from_payload_tar(
    payload_tar: &[u8],
) -> Result<Option<NativeArtifactSpec>> {
    let mut archive = tar::Archive::new(Cursor::new(payload_tar));
    let entries = archive
        .entries()
        .context("Failed to read payload.tar entries for native delivery detection")?;
    for entry in entries {
        let mut entry = entry.context("Invalid payload.tar entry")?;
        let path = entry.path().context("Failed to read payload entry path")?;
        if path != Path::new(DELIVERY_CONFIG_FILE) {
            continue;
        }
        let mut raw = String::new();
        entry
            .read_to_string(&mut raw)
            .context("Failed to read ato.delivery.toml from payload")?;
        let config: DeliveryConfig =
            toml::from_str(&raw).context("Failed to parse ato.delivery.toml from payload")?;
        validate_delivery_config(&config)?;
        return Ok(Some(NativeArtifactSpec {
            framework: config.artifact.framework,
            target: config.artifact.target,
            input: config.artifact.input,
            finalize_tool: config.finalize.tool,
        }));
    }
    Ok(None)
}

fn validate_delivery_config(config: &DeliveryConfig) -> Result<()> {
    validate_delivery_schema(&config.schema_version, "ato.delivery.toml")?;
    if config.artifact.framework.trim().is_empty() {
        bail!("artifact.framework must not be empty");
    }
    if config.artifact.stage != DELIVERY_STAGE {
        bail!(
            "Unsupported artifact.stage '{}'; expected '{}'",
            config.artifact.stage,
            DELIVERY_STAGE
        );
    }
    validate_delivery_target(config.artifact.target.trim())?;
    let input = config.artifact.input.trim();
    if input.is_empty() {
        bail!("artifact.input must not be empty");
    }
    let tool = config.finalize.tool.trim();
    if tool.is_empty() {
        bail!("finalize.tool must not be empty");
    }
    validate_finalize_tool(tool)?;
    validate_finalize_args(tool, &config.finalize.args, input)?;
    Ok(())
}

fn validate_delivery_target(target: &str) -> Result<()> {
    let mut segments = target.split('/');
    let os = segments.next().unwrap_or_default().trim();
    let arch = segments.next().unwrap_or_default().trim();
    if os.is_empty() || arch.is_empty() || segments.next().is_some() {
        bail!("artifact.target must use the '<os>/<arch>' format");
    }
    let normalized_os = normalize_delivery_os(os);
    let normalized_arch = normalize_delivery_arch(arch);
    if !matches!(normalized_os, "darwin" | "linux" | "windows")
        || !matches!(normalized_arch, "arm64" | "x86_64")
    {
        bail!(
            "Unsupported artifact.target '{}'; expected darwin|linux|windows with arm64|x86_64",
            target
        );
    }
    Ok(())
}

fn validate_finalize_tool(tool: &str) -> Result<()> {
    if tool.is_empty() {
        bail!("finalize.tool must not be empty");
    }
    if tool.chars().any(char::is_control) {
        bail!(
            "finalize.tool '{}' must not contain control characters",
            tool
        );
    }
    Ok(())
}

fn validate_finalize_args(tool: &str, args: &[String], input: &str) -> Result<()> {
    if args.iter().any(|argument| argument.trim().is_empty()) {
        bail!("finalize.args must not contain empty arguments");
    }
    if tool.eq_ignore_ascii_case("codesign") {
        return validate_codesign_finalize_args(args, input);
    }
    if tool.eq_ignore_ascii_case("signtool") {
        return validate_signtool_finalize_args(args, input);
    }
    Ok(())
}

fn validate_codesign_finalize_args(args: &[String], input: &str) -> Result<()> {
    let mut expects_value_for: Option<&str> = None;
    let mut saw_input = false;

    for argument in args {
        let trimmed = argument.trim();
        if let Some(option) = expects_value_for.take() {
            if option == "--sign" || option == "--timestamp" || option == "--options" {
                continue;
            }
            if trimmed == input {
                saw_input = true;
            }
            continue;
        }

        match trimmed {
            "--deep" | "--force" | "--strict" | "--verbose" => {}
            "--sign" | "--options" | "--entitlements" | "--requirements" | "--timestamp"
            | "--prefix" | "--identifier" => {
                expects_value_for = Some(trimmed);
            }
            value if value.starts_with("--timestamp=") => {}
            value if value == input => saw_input = true,
            _ => {
                bail!(
                    "Unsupported finalize.args entry '{}' for finalize.tool '{}'",
                    trimmed,
                    "codesign"
                );
            }
        }
    }

    if let Some(option) = expects_value_for {
        bail!("finalize.args is missing a value for '{}'", option);
    }
    if !saw_input {
        bail!(
            "finalize.args must include artifact.input '{}' for finalize.tool '{}'",
            input,
            "codesign"
        );
    }
    Ok(())
}

fn validate_signtool_finalize_args(args: &[String], input: &str) -> Result<()> {
    // Common `signtool sign` switches that do not take a following value.
    const SIGNTOOL_BOOLEAN_SWITCHES: &[&str] =
        &["a", "as", "debug", "nph", "ph", "q", "sm", "uw", "v"];
    // Common `signtool sign` switches that require one following value.
    const SIGNTOOL_VALUE_SWITCHES: &[&str] = &[
        "ac", "c", "csp", "d", "dg", "di", "ds", "du", "f", "fd", "i", "kc", "n", "p", "p7ce",
        "p7co", "pg", "r", "s", "sha1", "t", "td", "tr", "u",
    ];

    let mut arguments = args.iter();
    let Some(command) = arguments.next() else {
        bail!("finalize.args must not be empty");
    };
    if !command.trim().eq_ignore_ascii_case("sign") {
        bail!(
            "Unsupported finalize.args subcommand '{}' for finalize.tool '{}'",
            command.trim(),
            "signtool"
        );
    }

    let mut expects_value_for: Option<String> = None;
    let mut saw_input = false;
    for argument in arguments {
        let trimmed = argument.trim();
        if expects_value_for.take().is_some() {
            if trimmed == input {
                saw_input = true;
            }
            continue;
        }

        if trimmed == input {
            saw_input = true;
            continue;
        }

        let Some(option) = trimmed
            .strip_prefix('/')
            .or_else(|| trimmed.strip_prefix('-'))
        else {
            bail!(
                "Unsupported finalize.args entry '{}' for finalize.tool '{}'",
                trimmed,
                "signtool"
            );
        };
        let normalized = option.to_ascii_lowercase();
        if SIGNTOOL_BOOLEAN_SWITCHES.contains(&normalized.as_str()) {
            continue;
        }
        if SIGNTOOL_VALUE_SWITCHES.contains(&normalized.as_str()) {
            expects_value_for = Some(trimmed.to_string());
            continue;
        }
        bail!(
            "Unsupported finalize.args entry '{}' for finalize.tool '{}'",
            trimmed,
            "signtool"
        );
    }

    if let Some(option) = expects_value_for {
        bail!("finalize.args is missing a value for '{}'", option);
    }
    if !saw_input {
        bail!(
            "finalize.args must include artifact.input '{}' for finalize.tool '{}'",
            input,
            "signtool"
        );
    }
    Ok(())
}

fn validate_relative_input_path(path: &Path) -> Result<()> {
    if path.is_absolute() {
        bail!("artifact.input must be a relative path inside fetched artifact");
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("artifact.input must not escape fetched artifact root");
    }
    Ok(())
}

fn validate_relative_project_path(path: &Path, field_name: &str) -> Result<()> {
    if path.is_absolute() {
        bail!("{field_name} must be a relative path inside the project root");
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("{field_name} must not escape the project root");
    }
    Ok(())
}

fn rebase_delivery_config_for_finalize(
    config: &DeliveryConfig,
    derived_app_path: &Path,
) -> Result<DeliveryConfig> {
    let input_name = derived_app_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("Derived app path has no terminal app name"))?;
    let rebased_input = input_name.to_string();
    let original_input = config.artifact.input.clone();
    let mut derived_config = config.clone();
    derived_config.artifact.input = rebased_input.clone();
    for argument in &mut derived_config.finalize.args {
        if *argument == original_input {
            *argument = rebased_input.clone();
        }
    }
    Ok(derived_config)
}

fn ensure_native_artifact_kind_supported(path: &Path, action: &str) -> Result<NativeArtifactKind> {
    let kind = NativeArtifactKind::from_path(path);
    if kind == NativeArtifactKind::File && !path_has_extension(path, "exe") {
        bail!(
            "Native delivery {} does not support single-file artifacts yet: {}",
            action,
            path.display()
        );
    }
    Ok(kind)
}

fn delivery_target_os_family(target: &str) -> Option<&str> {
    target
        .split('/')
        .next()
        .filter(|value| !value.trim().is_empty())
}

fn supports_projection_target(target: &str) -> bool {
    matches!(
        delivery_target_os_family(target),
        Some("darwin" | "linux" | "windows")
    )
}

fn host_projection_os_family() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("darwin")
    } else if cfg!(target_os = "linux") {
        Some("linux")
    } else if cfg!(windows) {
        Some("windows")
    } else {
        None
    }
}

fn host_supports_projection_target(target: &str) -> bool {
    delivery_target_os_family(target) == host_projection_os_family()
}

fn resolve_native_build_working_dir(
    manifest_dir: &Path,
    working_dir: Option<&str>,
) -> Result<PathBuf> {
    let relative = working_dir
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(".");
    let relative_path = PathBuf::from(relative);
    validate_relative_project_path(&relative_path, "targets.<default>.working_dir")?;
    let resolved = manifest_dir.join(relative_path);
    if !resolved.is_dir() {
        bail!(
            "targets.<default>.working_dir is not a directory: {}",
            resolved.display()
        );
    }
    Ok(resolved)
}

fn validate_native_bundle_directory(source_app_path: &Path) -> Result<()> {
    match NativeArtifactKind::from_path(source_app_path) {
        NativeArtifactKind::MacOsAppBundle => {
            if !source_app_path.is_dir() {
                let candidates = discover_nearby_native_artifacts(source_app_path, 6);
                bail!(
                    "Native delivery build input is not a .app directory: {}{}",
                    source_app_path.display(),
                    format_nearby_native_artifact_candidates(source_app_path, &candidates)
                );
            }
        }
        NativeArtifactKind::Directory => {
            if !source_app_path.is_dir() {
                bail!(
                    "Native delivery build input must be a directory: {}",
                    source_app_path.display()
                );
            }
        }
        NativeArtifactKind::File => {
            if !source_app_path.is_file() {
                let candidates = discover_nearby_native_artifacts(source_app_path, 6);
                bail!(
                    "Native delivery build input is not a file: {}{}",
                    source_app_path.display(),
                    format_nearby_native_artifact_candidates(source_app_path, &candidates)
                );
            }
        }
    }
    validate_minimal_native_artifact_permissions(source_app_path)?;
    Ok(())
}

fn discover_nearby_native_artifacts(expected_path: &Path, max_depth: usize) -> Vec<PathBuf> {
    let Some(search_root) = nearest_existing_directory(expected_path) else {
        return Vec::new();
    };

    let kind = NativeArtifactKind::from_path(expected_path);
    let expected_file_extension = native_file_candidate_extension(expected_path);
    let mut bundles = WalkDir::new(&search_root)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.into_path())
        .filter(|path| match kind {
            NativeArtifactKind::MacOsAppBundle => path.is_dir() && path_has_extension(path, "app"),
            NativeArtifactKind::File => {
                path.is_file()
                    && expected_file_extension
                        .map(|extension| path_has_extension(path, extension))
                        .unwrap_or(true)
            }
            NativeArtifactKind::Directory => path.is_dir(),
        })
        .collect::<Vec<_>>();
    bundles.sort();
    bundles.dedup();
    bundles.truncate(5);
    bundles
}

fn nearest_existing_directory(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.is_dir() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

fn format_nearby_native_artifact_candidates(
    expected_path: &Path,
    candidates: &[PathBuf],
) -> String {
    let kind = NativeArtifactKind::from_path(expected_path);
    let label = match kind {
        NativeArtifactKind::MacOsAppBundle => ".app bundle",
        NativeArtifactKind::File => native_file_candidate_label(expected_path).unwrap_or("file"),
        NativeArtifactKind::Directory => "directory",
    };
    if candidates.is_empty() {
        return format!(
            "\nHint: confirm that [artifact].input matches the actual {} output path.",
            label
        );
    }

    let formatted = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "\nFound nearby {} candidates: {}\nHint: update [artifact].input to the correct path.",
        label, formatted
    )
}

fn format_native_build_command(command: &NativeBuildCommand) -> String {
    std::iter::once(command.program.as_str())
        .chain(command.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

fn run_native_build_command(command: &NativeBuildCommand) -> Result<()> {
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        .current_dir(&command.working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = process.output().with_context(|| {
        format!(
            "Failed to execute native delivery build command '{}' in {}",
            format_native_build_command(command),
            command.working_dir.display()
        )
    })?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        stdout.trim().to_string()
    };
    bail!(
        "Native delivery build command failed with status {}: {}{}",
        output
            .status
            .code()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "signal".to_string()),
        format_native_build_command(command),
        if details.is_empty() {
            String::new()
        } else {
            format!("\n{}", details)
        }
    );
}

fn staged_delivery_config(plan: &NativeBuildPlan) -> Result<DeliveryConfig> {
    let config: DeliveryConfig = toml::from_str(&plan.staged_delivery_config_toml)
        .context("Failed to parse staged native delivery metadata")?;
    validate_delivery_config(&config)?;
    Ok(config)
}

fn run_finalize_command(derived_dir: &Path, config: &DeliveryConfig) -> Result<()> {
    let tool = config.finalize.tool.trim();
    let mut command = Command::new(tool);
    command
        .args(&config.finalize.args)
        .current_dir(derived_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command
        .output()
        .with_context(|| format!("Failed to execute {} in {}", tool, derived_dir.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        stdout.trim().to_string()
    };
    bail!(
        "{} failed with status {}{}",
        tool,
        output
            .status
            .code()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "signal".to_string()),
        if details.is_empty() {
            String::new()
        } else {
            format!(": {}", details)
        },
    )
}

fn strip_codesign_signature(tool: &str, app_path: &Path) -> Result<()> {
    let output = Command::new(tool)
        .arg("--remove-signature")
        .arg(app_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to execute {} for {}", tool, app_path.display()))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        stdout.trim().to_string()
    };
    if details.contains("not signed at all") || details.contains("code object is not signed") {
        return Ok(());
    }

    bail!(
        "{} --remove-signature failed for {}{}",
        tool,
        app_path.display(),
        if details.is_empty() {
            String::new()
        } else {
            format!(": {}", details)
        }
    )
}

async fn resolve_registry_url(registry_url: Option<&str>) -> Result<String> {
    if let Some(url) = registry_url {
        return Ok(url.to_string());
    }
    let resolver = RegistryResolver::default();
    let info = resolver.resolve("localhost").await?;
    Ok(info.url)
}

fn with_ato_token(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    if let Some(token) = current_ato_token() {
        request.header("authorization", format!("Bearer {}", token))
    } else {
        request
    }
}

fn current_ato_token() -> Option<String> {
    std::env::var("ATO_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn extract_payload_tar_from_capsule(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;
    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let path = entry.path().context("Failed to read .capsule entry path")?;
        if path.to_string_lossy() != "payload.tar.zst" {
            continue;
        }
        let mut payload_zst = Vec::new();
        entry
            .read_to_end(&mut payload_zst)
            .context("Failed to read payload.tar.zst from artifact")?;
        let mut decoder = zstd::stream::Decoder::new(Cursor::new(payload_zst))
            .context("Failed to decode payload.tar.zst")?;
        let mut payload_tar = Vec::new();
        decoder
            .read_to_end(&mut payload_tar)
            .context("Failed to read payload.tar bytes")?;
        return Ok(payload_tar);
    }
    bail!("Invalid artifact: payload.tar.zst not found in .capsule archive")
}

fn unpack_payload_tar(payload_tar: &[u8], destination: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(Cursor::new(payload_tar));
    let entries = archive
        .entries()
        .context("Failed to read payload.tar entries")?;
    for entry in entries {
        let mut entry = entry.context("Invalid payload.tar entry")?;
        let path = entry.path().context("Failed to read payload entry path")?;
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            bail!("Refusing to unpack unsafe payload path: {}", path.display());
        }
        entry.unpack_in(destination).with_context(|| {
            format!(
                "Failed to unpack payload entry into {}",
                destination.display()
            )
        })?;
    }
    Ok(())
}

fn compute_tree_digest(root: &Path) -> Result<String> {
    if !root.exists() {
        bail!("Digest root does not exist: {}", root.display());
    }
    let mut hasher = blake3::Hasher::new();
    hash_tree_node(root, Path::new(""), &mut hasher)?;
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn hash_tree_node(path: &Path, relative: &Path, hasher: &mut blake3::Hasher) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    let file_type = metadata.file_type();

    if file_type.is_dir() {
        if !relative.as_os_str().is_empty() {
            update_tree_header(hasher, b"dir", relative, mode_bits(&metadata));
        }
        let mut entries = fs::read_dir(path)
            .with_context(|| format!("Failed to read directory {}", path.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("Failed to enumerate directory {}", path.display()))?;
        entries.sort_by_key(|left| left.file_name());
        for entry in entries {
            let child_path = entry.path();
            let child_relative = if relative.as_os_str().is_empty() {
                PathBuf::from(entry.file_name())
            } else {
                relative.join(entry.file_name())
            };
            hash_tree_node(&child_path, &child_relative, hasher)?;
        }
        return Ok(());
    }

    if file_type.is_symlink() {
        update_tree_header(hasher, b"symlink", relative, 0);
        let target = fs::read_link(path)
            .with_context(|| format!("Failed to read symlink {}", path.display()))?;
        hasher.update(target.as_os_str().to_string_lossy().as_bytes());
        hasher.update(b"\0");
        return Ok(());
    }

    if file_type.is_file() {
        update_tree_header(hasher, b"file", relative, mode_bits(&metadata));
        hasher.update(format!("{}\0", metadata.len()).as_bytes());
        let mut file =
            fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = file
                .read(&mut buf)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        hasher.update(b"\0");
        return Ok(());
    }

    bail!(
        "Unsupported filesystem entry in digest walk: {}",
        path.display()
    )
}

fn update_tree_header(hasher: &mut blake3::Hasher, kind: &[u8], relative: &Path, mode: u32) {
    hasher.update(kind);
    hasher.update(b"\0");
    hasher.update(relative.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{:o}", mode).as_bytes());
    hasher.update(b"\0");
}

fn copy_recursively(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("Failed to stat {}", source.display()))?;
    let file_type = metadata.file_type();

    if file_type.is_dir() {
        fs::create_dir_all(destination)
            .with_context(|| format!("Failed to create directory {}", destination.display()))?;
        fs::set_permissions(destination, metadata.permissions())
            .with_context(|| format!("Failed to set permissions on {}", destination.display()))?;
        let mut entries = fs::read_dir(source)
            .with_context(|| format!("Failed to read directory {}", source.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("Failed to enumerate directory {}", source.display()))?;
        entries.sort_by_key(|left| left.file_name());
        for entry in entries {
            copy_recursively(&entry.path(), &destination.join(entry.file_name()))?;
        }
        return Ok(());
    }

    if file_type.is_symlink() {
        #[cfg(unix)]
        {
            let target = fs::read_link(source)
                .with_context(|| format!("Failed to read symlink {}", source.display()))?;
            symlink(&target, destination).with_context(|| {
                format!(
                    "Failed to recreate symlink {} -> {}",
                    destination.display(),
                    target.display()
                )
            })?;
            return Ok(());
        }
        #[cfg(not(unix))]
        {
            let _ = destination;
            bail!("Symlink copy is not supported on this platform")
        }
    }

    if file_type.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create parent directory {}", parent.display())
            })?;
        }
        fs::copy(source, destination).with_context(|| {
            format!(
                "Failed to copy file {} -> {}",
                source.display(),
                destination.display()
            )
        })?;
        fs::set_permissions(destination, metadata.permissions())
            .with_context(|| format!("Failed to set permissions on {}", destination.display()))?;
        return Ok(());
    }

    bail!(
        "Unsupported filesystem entry for copy: {}",
        source.display()
    )
}

fn ensure_tree_writable(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    let file_type = metadata.file_type();

    if !file_type.is_symlink() {
        #[cfg(unix)]
        {
            let mode = metadata.permissions().mode();
            if mode & 0o200 == 0 {
                let mut permissions = metadata.permissions();
                permissions.set_mode(mode | 0o200);
                fs::set_permissions(path, permissions)
                    .with_context(|| format!("Failed to set permissions on {}", path.display()))?;
            }
        }

        #[cfg(windows)]
        {
            let mut permissions = metadata.permissions();
            if permissions.readonly() {
                permissions.set_readonly(false);
                fs::set_permissions(path, permissions)
                    .with_context(|| format!("Failed to set permissions on {}", path.display()))?;
            }
        }
    }

    if file_type.is_dir() {
        let mut entries = fs::read_dir(path)
            .with_context(|| format!("Failed to read directory {}", path.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("Failed to enumerate directory {}", path.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            ensure_tree_writable(&entry.path())?;
        }
    }

    Ok(())
}

fn validate_minimal_native_artifact_permissions(path: &Path) -> Result<()> {
    match NativeArtifactKind::from_path(path) {
        NativeArtifactKind::MacOsAppBundle => validate_minimal_macos_app_permissions(path),
        NativeArtifactKind::File if path_has_extension(path, "exe") => {
            validate_minimal_windows_executable(path)
        }
        NativeArtifactKind::File if path_has_extension(path, "deb") => Ok(()),
        NativeArtifactKind::File => validate_minimal_linux_elf_file(path),
        NativeArtifactKind::Directory => validate_minimal_linux_elf_directory(path),
    }
}

fn validate_minimal_macos_app_permissions(app_dir: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }

    let macos_dir = app_dir.join("Contents").join("MacOS");
    if !macos_dir.is_dir() {
        return Ok(());
    }

    let mut found_regular_file = false;
    for entry in fs::read_dir(&macos_dir)
        .with_context(|| format!("Failed to read directory {}", macos_dir.display()))?
    {
        let entry = entry
            .with_context(|| format!("Failed to enumerate directory {}", macos_dir.display()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("Failed to stat {}", path.display()))?;
        if !metadata.is_file() {
            continue;
        }
        found_regular_file = true;
        validate_unix_executable_permissions(&path, &metadata)?;
    }

    if !found_regular_file {
        bail!(
            "Finalize input is missing a regular executable in {}",
            macos_dir.display()
        );
    }

    Ok(())
}

fn validate_minimal_linux_elf_directory(root: &Path) -> Result<()> {
    let mut found_regular_elf = false;
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.into_path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("Failed to stat {}", path.display()))?;
        #[cfg(unix)]
        if metadata.permissions().mode() & 0o111 == 0 {
            continue;
        }

        if !path_has_elf_magic(&path)? {
            continue;
        }

        validate_unix_executable_permissions(&path, &metadata)?;
        let bytes = fs::read(&path)
            .with_context(|| format!("Failed to read Linux executable {}", path.display()))?;
        validate_linux_elf_bytes(&path, &bytes)?;
        found_regular_elf = true;
    }

    if !found_regular_elf {
        bail!(
            "Native delivery input is missing a regular ELF executable in {}",
            root.display()
        );
    }

    Ok(())
}

fn validate_minimal_linux_elf_file(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    validate_unix_executable_permissions(path, &metadata)?;
    let bytes = fs::read(path)
        .with_context(|| format!("Failed to read Linux executable {}", path.display()))?;
    validate_linux_elf_bytes(path, &bytes)
}

fn validate_linux_elf_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let object = Object::parse(bytes).with_context(|| {
        format!(
            "Linux executable failed minimum ELF validation: {}",
            path.display()
        )
    })?;
    let Object::Elf(_) = object else {
        bail!(
            "Linux executable failed minimum ELF validation: {} is not an ELF image",
            path.display()
        );
    };
    Ok(())
}

fn path_has_elf_magic(path: &Path) -> Result<bool> {
    let mut file =
        fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut magic = [0u8; 4];
    let bytes_read = file
        .read(&mut magic)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    Ok(bytes_read == magic.len() && magic == *b"\x7FELF")
}

fn native_file_candidate_extension(path: &Path) -> Option<&'static str> {
    if path_has_extension(path, "exe") {
        Some("exe")
    } else if path_has_extension(path, "AppImage") {
        Some("AppImage")
    } else if path_has_extension(path, "deb") {
        Some("deb")
    } else {
        None
    }
}

fn native_file_candidate_label(path: &Path) -> Option<&'static str> {
    match native_file_candidate_extension(path) {
        Some("exe") => Some(".exe"),
        Some("AppImage") => Some(".AppImage"),
        Some("deb") => Some(".deb"),
        Some(_) | None => None,
    }
}

#[cfg(unix)]
fn validate_unix_executable_permissions(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 {
        bail!(
            "Executable bit is missing for {} (mode {:o})",
            path.display(),
            mode & 0o777
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_unix_executable_permissions(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

fn validate_minimal_windows_executable(path: &Path) -> Result<()> {
    if !path_has_extension(path, "exe") {
        return Ok(());
    }

    let bytes = fs::read(path)
        .with_context(|| format!("Failed to read Windows executable {}", path.display()))?;
    let object = Object::parse(&bytes).with_context(|| {
        format!(
            "Windows executable failed minimum PE validation: {}",
            path.display()
        )
    })?;
    let Object::PE(pe) = object else {
        bail!(
            "Windows executable failed minimum PE validation: {} is not a PE image",
            path.display()
        );
    };
    if pe.is_lib {
        bail!(
            "Windows executable failed minimum PE validation: {} is a DLL, not an .exe",
            path.display()
        );
    }

    Ok(())
}

fn path_has_extension(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).context("Failed to serialize JSON")?;
    let mut file =
        fs::File::create(path).with_context(|| format!("Failed to create {}", path.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("Failed to finalize {}", path.display()))?;
    Ok(())
}

fn append_tar_entry(
    builder: &mut tar::Builder<&mut Vec<u8>>,
    path: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder.append_data(&mut header, path, Cursor::new(bytes))?;
    Ok(())
}

fn build_capsule_archive(
    manifest: &capsule_core::types::CapsuleManifest,
    payload_tar_zst: &[u8],
    payload_tar: &[u8],
) -> Result<Vec<u8>> {
    let (_distribution_manifest, manifest_toml_bytes) =
        capsule_core::packers::payload::build_distribution_manifest(manifest, payload_tar)
            .map_err(anyhow::Error::from)
            .context("Failed to build distribution metadata for native capsule")?;
    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        append_tar_entry(&mut builder, "capsule.toml", &manifest_toml_bytes, 0o644)?;
        append_tar_entry(&mut builder, "payload.tar.zst", payload_tar_zst, 0o644)?;
        builder.finish()?;
    }
    Ok(out)
}

fn create_payload_tar_from_directory(root: &Path) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        append_tree_to_tar(root, root, &mut builder)?;
        builder.finish()?;
    }
    Ok(out)
}

fn append_tree_to_tar(
    root: &Path,
    path: &Path,
    builder: &mut tar::Builder<&mut Vec<u8>>,
) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let file_type = metadata.file_type();

    if file_type.is_dir() {
        if !relative.is_empty() {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(mode_bits(&metadata));
            header.set_size(0);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_cksum();
            builder.append_data(&mut header, format!("{relative}/"), std::io::empty())?;
        }
        let mut entries = fs::read_dir(path)
            .with_context(|| format!("Failed to read directory {}", path.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("Failed to enumerate directory {}", path.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            append_tree_to_tar(root, &entry.path(), builder)?;
        }
        return Ok(());
    }

    if file_type.is_symlink() {
        let target = fs::read_link(path)
            .with_context(|| format!("Failed to read symlink {}", path.display()))?;
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_size(0);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_link_name(&target)?;
        header.set_cksum();
        builder.append_data(&mut header, &relative, std::io::empty())?;
        return Ok(());
    }

    if file_type.is_file() {
        let mut header = tar::Header::new_gnu();
        header.set_mode(mode_bits(&metadata));
        header.set_size(metadata.len());
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        let mut file =
            fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
        builder.append_data(&mut header, &relative, &mut file)?;
        return Ok(());
    }

    bail!(
        "Unsupported filesystem entry for tar payload: {}",
        path.display()
    )
}

fn create_unique_output_dir(output_root: &Path) -> Result<PathBuf> {
    for _ in 0..32 {
        let candidate = output_root.join(format!(
            "derived-{}-{}",
            Utc::now().format("%Y%m%dT%H%M%SZ"),
            random_hex(4)
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("Failed to create {}", candidate.display()))
            }
        }
    }
    bail!("Failed to allocate unique finalize output directory")
}

fn create_temp_subdir(root: &Path, prefix: &str) -> Result<PathBuf> {
    for _ in 0..32 {
        let candidate = root.join(format!("{}-{}", prefix, random_hex(8)));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("Failed to create {}", candidate.display()))
            }
        }
    }
    bail!(
        "Failed to allocate temporary directory in {}",
        root.display()
    )
}

fn digest_dir_name(digest: &str) -> Result<String> {
    let normalized = digest
        .trim()
        .trim_start_matches("blake3:")
        .trim_start_matches("sha256:")
        .to_ascii_lowercase();
    if normalized.is_empty() {
        bail!("Digest label is empty");
    }
    Ok(normalized)
}

fn compute_blake3(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn fetches_root() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_FETCHES_DIR))
}

fn derived_apps_root(scoped_id: &str, parent_digest: &str) -> Result<PathBuf> {
    let mut root = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_DERIVED_APPS_DIR);
    for segment in scoped_id.split('/') {
        root.push(segment.trim());
    }
    root.push(digest_dir_name(parent_digest)?);
    Ok(root)
}

fn projections_root() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(PROJECTIONS_DIR))
}

fn resolve_launcher_dir(launcher_dir: Option<&Path>) -> Result<PathBuf> {
    match launcher_dir {
        Some(path) => absolute_path(path),
        None => Ok(dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(default_launcher_dir_for_host())),
    }
}

fn default_launcher_dir_for_host() -> &'static str {
    match host_projection_os_family() {
        Some("linux") => DEFAULT_LINUX_DESKTOP_ENTRY_DIR,
        _ => DEFAULT_MACOS_LAUNCHER_DIR,
    }
}

fn resolve_projected_command_dir_for_host(launcher_dir: &Path) -> Result<PathBuf> {
    match host_projection_os_family() {
        Some("linux") => Ok(dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(DEFAULT_LINUX_BIN_DIR)),
        _ => absolute_path(launcher_dir),
    }
}

fn default_native_artifact_path(manifest_dir: &Path, name: &str, version: &str) -> PathBuf {
    manifest_dir
        .join("dist")
        .join(format!("{}-{}.capsule", name, version))
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("Failed to read current working directory")?
            .join(path))
    }
}

fn build_projection_id(
    derived_app_path: &Path,
    projected_path: &Path,
    derived_digest: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(derived_app_path.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    hasher.update(projected_path.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    hasher.update(derived_digest.as_bytes());
    hex::encode(&hasher.finalize().as_bytes()[..8])
}

fn paths_match(left: &Path, right: &Path) -> Result<bool> {
    if left == right {
        return Ok(true);
    }
    let left_canon = fs::canonicalize(left).ok();
    let right_canon = fs::canonicalize(right).ok();
    if let (Some(left_canon), Some(right_canon)) = (left_canon, right_canon) {
        return Ok(left_canon == right_canon);
    }
    Ok(absolute_path(left)? == absolute_path(right)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectionPathStatus {
    MatchesTarget,
    TargetMismatch,
    Replaced,
    Missing,
}

fn projection_candidate_paths(path: &Path) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        vec![path.to_path_buf(), projection_shortcut_path(path)]
    }
    #[cfg(not(windows))]
    {
        vec![path.to_path_buf()]
    }
}

fn first_existing_projection_candidate(path: &Path) -> Result<Option<PathBuf>> {
    for candidate in projection_candidate_paths(path) {
        match fs::symlink_metadata(&candidate) {
            Ok(_) => return Ok(Some(candidate)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("Failed to stat {}", candidate.display()))
            }
        }
    }
    Ok(None)
}

fn find_existing_projection_path(path: &Path, target: &Path) -> Result<Option<PathBuf>> {
    for candidate in projection_candidate_paths(path) {
        if is_managed_projection_to(&candidate, target)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn is_managed_projection_to(path: &Path, target: &Path) -> Result<bool> {
    Ok(matches!(
        inspect_projection_path(path, target)?,
        ProjectionPathStatus::MatchesTarget
    ))
}

fn inspect_projection_path(path: &Path, target: &Path) -> Result<ProjectionPathStatus> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(ProjectionPathStatus::Missing)
        }
        Err(err) => return Err(err).with_context(|| format!("Failed to stat {}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        let link_target = fs::read_link(path)
            .with_context(|| format!("Failed to read symlink {}", path.display()))?;
        let resolved_target = if link_target.is_absolute() {
            link_target
        } else {
            path.parent()
                .unwrap_or_else(|| Path::new("."))
                .join(link_target)
        };
        return Ok(if paths_match(&resolved_target, target)? {
            ProjectionPathStatus::MatchesTarget
        } else {
            ProjectionPathStatus::TargetMismatch
        });
    }

    #[cfg(windows)]
    {
        if junction::exists(path)
            .with_context(|| format!("Failed to inspect junction {}", path.display()))?
        {
            let junction_target = junction::get_target(path)
                .with_context(|| format!("Failed to read junction {}", path.display()))?;
            return Ok(if paths_match(&junction_target, target)? {
                ProjectionPathStatus::MatchesTarget
            } else {
                ProjectionPathStatus::TargetMismatch
            });
        }
        if is_projection_shortcut(path, &metadata) {
            let shortcut_target = resolve_projection_shortcut_target(path).with_context(|| {
                format!(
                    "Failed to validate projection shortcut target for {}",
                    path.display()
                )
            })?;
            return Ok(if paths_match(&shortcut_target, target)? {
                ProjectionPathStatus::MatchesTarget
            } else {
                ProjectionPathStatus::TargetMismatch
            });
        }
    }

    Ok(ProjectionPathStatus::Replaced)
}

fn remove_projection_path(path: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Failed to inspect projected path: {}", path.display()))
        }
    };

    if metadata.file_type().is_symlink() {
        remove_projection_symlink(path)
            .with_context(|| format!("Failed to remove projection symlink: {}", path.display()))?;
        return Ok(true);
    }

    #[cfg(windows)]
    {
        if junction::exists(path)
            .with_context(|| format!("Failed to inspect junction {}", path.display()))?
        {
            junction::delete(path).with_context(|| {
                format!("Failed to remove projection junction: {}", path.display())
            })?;
            return Ok(true);
        }
        if is_projection_shortcut(path, &metadata) {
            fs::remove_file(path).with_context(|| {
                format!("Failed to remove projection shortcut: {}", path.display())
            })?;
            return Ok(true);
        }
    }

    bail!(
        "Refusing to remove unmanaged projected path: {}",
        path.display()
    )
}

#[cfg(unix)]
fn create_projection_symlink(target: &Path, destination: &Path) -> std::io::Result<PathBuf> {
    symlink(target, destination)?;
    Ok(destination.to_path_buf())
}

#[cfg(windows)]
fn create_projection_symlink(target: &Path, destination: &Path) -> std::io::Result<PathBuf> {
    match symlink_dir(target, destination) {
        Ok(()) => Ok(destination.to_path_buf()),
        Err(symlink_err) => match junction::create(target, destination) {
            Ok(()) => Ok(destination.to_path_buf()),
            Err(junction_err) => {
                let shortcut_path = projection_shortcut_path(destination);
                match create_projection_shortcut(target, &shortcut_path) {
                    Ok(()) => Ok(shortcut_path),
                    Err(shortcut_err) => Err(io::Error::new(
                        shortcut_err.kind(),
                        format!(
                            "Failed to create projection after attempting symlink, junction, and shortcut fallbacks: symlink failed: {}; junction failed: {}; shortcut failed: {}",
                            symlink_err, junction_err, shortcut_err
                        ),
                    )),
                }
            }
        },
    }
}

#[cfg(unix)]
fn remove_projection_symlink(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}

#[cfg(windows)]
fn remove_projection_symlink(path: &Path) -> io::Result<()> {
    fs::remove_dir(path).or_else(|_| fs::remove_file(path))
}

#[cfg(windows)]
fn projection_shortcut_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "projection".to_string());
    path.with_file_name(format!("{file_name}.lnk"))
}

#[cfg(windows)]
fn create_projection_shortcut(target: &Path, destination: &Path) -> io::Result<()> {
    let shortcut = ShellLink::new(target).map_err(|err| {
        io::Error::other(format!(
            "Failed to prepare shortcut target {}: {}",
            target.display(),
            err
        ))
    })?;
    shortcut.create_lnk(destination).map_err(|err| {
        io::Error::other(format!(
            "Failed to write shortcut {}: {}",
            destination.display(),
            err
        ))
    })
}

#[cfg(windows)]
fn resolve_projection_shortcut_target(path: &Path) -> Result<PathBuf> {
    if !path.is_file() {
        bail!(
            "Projection shortcut does not exist as a file: {}",
            path.display()
        );
    }
    let output = powershell_command()
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ws = New-Object -ComObject WScript.Shell; $shortcut = $ws.CreateShortcut($args[0]); if (-not $shortcut.TargetPath) { exit 1 }; [Console]::Out.Write($shortcut.TargetPath)",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to resolve projection shortcut {}", path.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Failed to resolve projection shortcut {}: {}",
            path.display(),
            stderr.trim()
        );
    }
    let target = String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(&['\r', '\n'][..])
        .to_string();
    if target.is_empty() {
        bail!("Projection shortcut target is empty: {}", path.display());
    }
    Ok(PathBuf::from(target))
}

#[cfg(windows)]
fn powershell_command() -> Command {
    if let Ok(system_root) = std::env::var("SYSTEMROOT") {
        let candidate = PathBuf::from(system_root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        if candidate.is_file() {
            return Command::new(candidate);
        }
    }
    Command::new("powershell")
}

#[cfg(windows)]
fn is_projection_shortcut(path: &Path, metadata: &fs::Metadata) -> bool {
    metadata.is_file()
        && path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.eq_ignore_ascii_case("lnk"))
            .unwrap_or(false)
}

fn host_supports_projection() -> bool {
    host_projection_os_family().is_some()
}

pub(crate) fn host_supports_finalize() -> bool {
    cfg!(target_os = "macos") || cfg!(windows)
}

fn projection_output_path(
    projection_kind: ProjectionKind,
    launcher_dir: &Path,
    derived_app_path: &Path,
    command_name: &str,
) -> Result<PathBuf> {
    Ok(match projection_kind {
        ProjectionKind::Symlink => launcher_dir.join(
            derived_app_path
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Derived app path has no terminal name"))?,
        ),
        ProjectionKind::LinuxDesktopEntry => launcher_dir.join(format!("{command_name}.desktop")),
    })
}

fn projection_display_name(derived_app_path: &Path, scoped_id: Option<&str>) -> Result<String> {
    let raw = derived_app_path
        .file_stem()
        .or_else(|| derived_app_path.file_name())
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            scoped_id
                .and_then(|value| value.rsplit('/').next())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| anyhow::anyhow!("Derived app path has no usable launcher name"))?;
    Ok(raw)
}

fn projection_command_name(derived_app_path: &Path, scoped_id: Option<&str>) -> Result<String> {
    let seed = scoped_id
        .and_then(|value| value.rsplit('/').next())
        .or_else(|| {
            derived_app_path
                .file_stem()
                .or_else(|| derived_app_path.file_name())
                .and_then(|value| value.to_str())
        })
        .ok_or_else(|| anyhow::anyhow!("Derived app path has no usable command name"))?;
    Ok(sanitize_projection_segment(seed))
}

fn sanitize_projection_segment(value: &str) -> String {
    let mut out = String::new();
    let mut previous_dash = false;
    for ch in value.chars() {
        let normalized = if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if normalized == '-' {
            if !previous_dash {
                out.push('-');
            }
            previous_dash = true;
        } else {
            out.push(normalized);
            previous_dash = false;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "ato-app".to_string()
    } else {
        trimmed.to_string()
    }
}

fn render_linux_desktop_entry(
    display_name: &str,
    projected_command_path: &Path,
    derived_app_path: &Path,
) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName={}\nExec={}\nPath={}\nTerminal=false\n",
        escape_desktop_entry_string_value(display_name),
        escape_desktop_entry_exec_value(projected_command_path),
        escape_desktop_entry_string_value(&derived_app_path.to_string_lossy()),
    )
}

fn escape_desktop_entry_string_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn escape_desktop_entry_exec_value(path: &Path) -> String {
    escape_desktop_entry_string_value(&path.to_string_lossy())
        .replace(' ', "\\ ")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

fn remove_projected_path(path: &Path, projection_kind: &str) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(path)?;
            Ok(true)
        }
        Ok(metadata)
            if projection_kind == PROJECTION_KIND_LINUX_DESKTOP_ENTRY && metadata.is_file() =>
        {
            fs::remove_file(path)?;
            Ok(true)
        }
        Ok(_) => bail!(
            "Refusing to remove unexpected projected path: {}",
            path.display()
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

fn random_hex(len_bytes: usize) -> String {
    let mut bytes = vec![0u8; len_bytes];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    hex::encode(bytes)
}

#[cfg(unix)]
fn mode_bits(metadata: &fs::Metadata) -> u32 {
    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn mode_bits(_metadata: &fs::Metadata) -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use tempfile::tempdir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn assert_json_object_has_keys(value: &serde_json::Value, keys: &[&str]) {
        let object = value.as_object().expect("expected JSON object");
        for key in keys {
            assert!(
                object.contains_key(*key),
                "expected key '{}' in JSON object: {object:?}",
                key
            );
        }
    }

    fn sample_delivery_toml() -> &'static str {
        r#"schema_version = "0.1"
[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "MyApp.app"
[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "MyApp.app"]
"#
    }

    fn sample_fetch_dir(root: &Path) -> Result<PathBuf> {
        sample_fetch_dir_with_mode(root, 0o755)
    }

    fn sample_nested_delivery_toml() -> &'static str {
        r#"schema_version = "0.1"
[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "src-tauri/target/release/bundle/macos/My App.app"
[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "src-tauri/target/release/bundle/macos/My App.app"]
"#
    }

    fn sample_file_delivery_toml() -> String {
        format!(
            "schema_version = \"0.1\"\n[artifact]\nframework = \"tauri\"\nstage = \"unsigned\"\ntarget = \"{}\"\ninput = \"dist/MyApp.exe\"\n[finalize]\ntool = \"signtool\"\nargs = [\"sign\", \"/fd\", \"SHA256\", \"dist/MyApp.exe\"]\n",
            default_delivery_target_for_input("dist/MyApp.exe")
        )
    }

    fn sample_windows_pe_bytes(is_dll: bool) -> Vec<u8> {
        const SAMPLE_PE_SIZE: usize = 0x400;
        const PE_OFFSET: usize = 0x80;
        const PE32_PLUS_OPTIONAL_HEADER_SIZE: usize = 0xF0;
        const SECTION_ALIGNMENT: u32 = 0x1000;
        const FILE_ALIGNMENT: u32 = 0x200;
        const IMAGE_BASE: u64 = 0x1_4000_0000;
        // IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_LARGE_ADDRESS_AWARE
        const EXECUTABLE_CHARACTERISTICS: u16 = 0x0022;
        // EXECUTABLE_CHARACTERISTICS | IMAGE_FILE_DLL
        const DLL_CHARACTERISTICS: u16 = 0x2022;

        let mut bytes = vec![0u8; SAMPLE_PE_SIZE];
        bytes[0..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&(PE_OFFSET as u32).to_le_bytes());

        bytes[PE_OFFSET..PE_OFFSET + 4].copy_from_slice(b"PE\0\0");

        let coff_offset = PE_OFFSET + 4;
        bytes[coff_offset..coff_offset + 2].copy_from_slice(&(0x8664u16).to_le_bytes());
        bytes[coff_offset + 2..coff_offset + 4].copy_from_slice(&(1u16).to_le_bytes());
        bytes[coff_offset + 16..coff_offset + 18]
            .copy_from_slice(&(PE32_PLUS_OPTIONAL_HEADER_SIZE as u16).to_le_bytes());
        bytes[coff_offset + 18..coff_offset + 20].copy_from_slice(
            &(if is_dll {
                DLL_CHARACTERISTICS
            } else {
                EXECUTABLE_CHARACTERISTICS
            })
            .to_le_bytes(),
        );

        let optional_offset = coff_offset + 20;
        bytes[optional_offset..optional_offset + 2].copy_from_slice(&(0x20bu16).to_le_bytes());
        bytes[optional_offset + 4..optional_offset + 8]
            .copy_from_slice(&FILE_ALIGNMENT.to_le_bytes());
        bytes[optional_offset + 16..optional_offset + 20]
            .copy_from_slice(&SECTION_ALIGNMENT.to_le_bytes());
        bytes[optional_offset + 20..optional_offset + 24]
            .copy_from_slice(&SECTION_ALIGNMENT.to_le_bytes());
        bytes[optional_offset + 24..optional_offset + 32]
            .copy_from_slice(&IMAGE_BASE.to_le_bytes());
        bytes[optional_offset + 32..optional_offset + 36]
            .copy_from_slice(&SECTION_ALIGNMENT.to_le_bytes());
        bytes[optional_offset + 36..optional_offset + 40]
            .copy_from_slice(&FILE_ALIGNMENT.to_le_bytes());
        bytes[optional_offset + 40..optional_offset + 42].copy_from_slice(&(6u16).to_le_bytes());
        bytes[optional_offset + 48..optional_offset + 50].copy_from_slice(&(6u16).to_le_bytes());
        bytes[optional_offset + 56..optional_offset + 60]
            .copy_from_slice(&(SECTION_ALIGNMENT * 2).to_le_bytes());
        bytes[optional_offset + 60..optional_offset + 64]
            .copy_from_slice(&FILE_ALIGNMENT.to_le_bytes());
        bytes[optional_offset + 68..optional_offset + 70].copy_from_slice(&(3u16).to_le_bytes());
        bytes[optional_offset + 72..optional_offset + 80]
            .copy_from_slice(&(0x10_0000u64).to_le_bytes());
        bytes[optional_offset + 80..optional_offset + 88]
            .copy_from_slice(&(0x1000u64).to_le_bytes());
        bytes[optional_offset + 88..optional_offset + 96]
            .copy_from_slice(&(0x10_0000u64).to_le_bytes());
        bytes[optional_offset + 96..optional_offset + 104]
            .copy_from_slice(&(0x1000u64).to_le_bytes());
        bytes[optional_offset + 108..optional_offset + 112].copy_from_slice(&(16u32).to_le_bytes());

        let section_offset = optional_offset + PE32_PLUS_OPTIONAL_HEADER_SIZE;
        bytes[section_offset..section_offset + 8].copy_from_slice(b".text\0\0\0");
        bytes[section_offset + 8..section_offset + 12].copy_from_slice(&(1u32).to_le_bytes());
        bytes[section_offset + 12..section_offset + 16]
            .copy_from_slice(&SECTION_ALIGNMENT.to_le_bytes());
        bytes[section_offset + 16..section_offset + 20]
            .copy_from_slice(&FILE_ALIGNMENT.to_le_bytes());
        bytes[section_offset + 20..section_offset + 24]
            .copy_from_slice(&FILE_ALIGNMENT.to_le_bytes());
        bytes[section_offset + 36..section_offset + 40]
            .copy_from_slice(&(0x6000_0020u32).to_le_bytes());

        bytes[FILE_ALIGNMENT as usize] = 0xC3;
        bytes
    }

    fn sample_windows_executable_bytes() -> Vec<u8> {
        sample_windows_pe_bytes(false)
    }

    fn sample_windows_dll_bytes() -> Vec<u8> {
        sample_windows_pe_bytes(true)
    }

    fn sample_linux_elf_bytes() -> Vec<u8> {
        let mut bytes = vec![0u8; 64];
        bytes[0..4].copy_from_slice(b"\x7FELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[6] = 1;
        bytes[16..18].copy_from_slice(&(2u16).to_le_bytes());
        bytes[18..20].copy_from_slice(&(62u16).to_le_bytes());
        bytes[20..24].copy_from_slice(&(1u32).to_le_bytes());
        bytes[24..32].copy_from_slice(&(0x400000u64).to_le_bytes());
        bytes[52..54].copy_from_slice(&(64u16).to_le_bytes());
        bytes
    }

    #[cfg(unix)]
    fn write_linux_elf(path: &Path, mode: u32) -> Result<()> {
        fs::write(path, sample_linux_elf_bytes())?;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions)?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn write_linux_elf(path: &Path, _mode: u32) -> Result<()> {
        fs::write(path, sample_linux_elf_bytes())?;
        Ok(())
    }

    fn sample_native_build_plan(root: &Path, mode: u32) -> Result<NativeBuildPlan> {
        let manifest_dir = root.join("native-build-project");
        let source_app_path = manifest_dir.join("MyApp.app");
        let binary_path = source_app_path.join("Contents/MacOS/MyApp");
        fs::create_dir_all(binary_path.parent().context("binary parent missing")?)?;
        fs::write(
            manifest_dir.join("capsule.toml"),
            r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "MyApp.app"
"#,
        )?;
        fs::write(
            manifest_dir.join(DELIVERY_CONFIG_FILE),
            sample_delivery_toml(),
        )?;
        fs::write(&binary_path, b"unsigned-app")?;
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&binary_path)?.permissions();
            permissions.set_mode(mode);
            fs::set_permissions(&binary_path, permissions)?;
        }

        detect_build_strategy(&manifest_dir)?.context("expected native delivery build plan")
    }

    fn sample_file_native_build_plan(root: &Path) -> Result<NativeBuildPlan> {
        let manifest_dir = root.join("native-file-build-project");
        let source_file_path = manifest_dir.join("dist/MyApp.exe");
        fs::create_dir_all(
            source_file_path
                .parent()
                .context("source file parent missing")?,
        )?;
        fs::write(
            manifest_dir.join("capsule.toml"),
            r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "dist/MyApp.exe"
"#,
        )?;
        fs::write(
            manifest_dir.join(DELIVERY_CONFIG_FILE),
            sample_file_delivery_toml(),
        )?;
        fs::write(&source_file_path, sample_windows_executable_bytes())?;

        detect_build_strategy(&manifest_dir)?.context("expected native delivery build plan")
    }

    #[test]
    fn detect_build_strategy_accepts_command_mode_with_explicit_delivery_sidecar() -> Result<()> {
        let tmp = tempdir()?;
        let manifest_dir = tmp.path().join("command-build-project");
        fs::create_dir_all(&manifest_dir)?;
        fs::write(
            manifest_dir.join("capsule.toml"),
            r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "sh"
cmd = ["build-app.sh"]
working_dir = "."
"#,
        )?;
        fs::write(
            manifest_dir.join(DELIVERY_CONFIG_FILE),
            sample_delivery_toml(),
        )?;

        let plan =
            detect_build_strategy(&manifest_dir)?.context("expected native delivery build plan")?;
        let build_command = plan.build_command.context("expected build command")?;
        assert_eq!(build_command.program, "sh");
        assert_eq!(build_command.args, vec!["build-app.sh".to_string()]);
        assert_eq!(build_command.working_dir, manifest_dir);
        assert_eq!(plan.source_app_path, plan.manifest_dir.join("MyApp.app"));
        Ok(())
    }

    #[test]
    fn detect_build_strategy_accepts_windows_exe_manifest_contract() -> Result<()> {
        let tmp = tempdir()?;
        let manifest_dir = tmp.path().join("windows-build-project");
        let source_file_path = manifest_dir.join("dist/MyApp.exe");
        fs::create_dir_all(
            source_file_path
                .parent()
                .context("source file parent missing")?,
        )?;
        fs::write(
            manifest_dir.join("capsule.toml"),
            r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "dist/MyApp.exe"
"#,
        )?;
        fs::write(&source_file_path, sample_windows_executable_bytes())?;

        let plan =
            detect_build_strategy(&manifest_dir)?.context("expected native delivery build plan")?;
        let config = staged_delivery_config(&plan)?;
        assert_eq!(plan.source_app_path, source_file_path);
        assert_eq!(config.artifact.input, "dist/MyApp.exe");
        assert_eq!(
            config.artifact.target,
            format!(
                "windows/{}",
                normalize_delivery_arch(std::env::consts::ARCH)
            )
        );
        Ok(())
    }

    #[test]
    fn detect_build_strategy_ignores_command_mode_without_delivery_sidecar() -> Result<()> {
        let tmp = tempdir()?;
        let manifest_dir = tmp.path().join("command-build-project");
        fs::create_dir_all(&manifest_dir)?;
        fs::write(
            manifest_dir.join("capsule.toml"),
            r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "sh"
cmd = ["build-app.sh"]
working_dir = "."
"#,
        )?;

        assert!(detect_build_strategy(&manifest_dir)?.is_none());
        Ok(())
    }

    #[test]
    fn detect_build_strategy_accepts_inline_delivery_config() -> Result<()> {
        let tmp = tempdir()?;
        let manifest_dir = tmp.path().join("inline-command-build-project");
        fs::create_dir_all(&manifest_dir)?;
        fs::write(
            manifest_dir.join("capsule.toml"),
            r#"schema_version = "0.2"
name = "time-management-desktop"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "sh"
cmd = ["build-app.sh"]
working_dir = "."

[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "dist/time-management-desktop.app"

[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "dist/time-management-desktop.app"]
"#,
        )?;

        let plan =
            detect_build_strategy(&manifest_dir)?.context("expected native delivery build plan")?;
        let build_command = plan.build_command.context("expected build command")?;
        assert_eq!(build_command.program, "sh");
        assert_eq!(build_command.args, vec!["build-app.sh".to_string()]);
        assert_eq!(
            plan.source_app_path,
            manifest_dir.join("dist/time-management-desktop.app")
        );
        Ok(())
    }

    #[test]
    fn detect_build_strategy_generates_canonical_delivery_config_from_capsule_manifest(
    ) -> Result<()> {
        let tmp = tempdir()?;
        let manifest_dir = tmp.path().join("native-build-project");
        let source_app_path = manifest_dir.join("MyApp.app");
        let binary_path = source_app_path.join("Contents/MacOS/MyApp");
        fs::create_dir_all(binary_path.parent().context("binary parent missing")?)?;
        fs::write(
            manifest_dir.join("capsule.toml"),
            r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "MyApp.app"
"#,
        )?;
        fs::write(&binary_path, b"unsigned-app")?;
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&binary_path)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&binary_path, permissions)?;
        }

        let plan =
            detect_build_strategy(&manifest_dir)?.context("expected native delivery build plan")?;
        let staged: DeliveryConfig = toml::from_str(&plan.staged_delivery_config_toml)?;

        assert!(plan.delivery_config_path.is_none());
        assert_eq!(plan.source_app_path, manifest_dir.join("MyApp.app"));
        assert_eq!(staged.schema_version, DELIVERY_SCHEMA_VERSION_STABLE);
        assert_eq!(staged.artifact.input, "MyApp.app");
        assert_eq!(staged.artifact.framework, DEFAULT_DELIVERY_FRAMEWORK);
        assert_eq!(staged.artifact.target, DEFAULT_DELIVERY_TARGET);
        assert_eq!(staged.finalize.tool, DEFAULT_FINALIZE_TOOL);
        assert_eq!(
            staged.finalize.args,
            vec![
                "--deep".to_string(),
                "--force".to_string(),
                "--sign".to_string(),
                "-".to_string(),
                "MyApp.app".to_string(),
            ]
        );
        Ok(())
    }

    #[test]
    fn detect_build_strategy_rejects_sidecar_that_conflicts_with_canonical_capsule_manifest() {
        let tmp = tempdir().expect("tmp dir");
        let manifest_dir = tmp.path().join("native-build-project");
        let source_app_path = manifest_dir.join("MyApp.app");
        let binary_path = source_app_path.join("Contents/MacOS/MyApp");
        fs::create_dir_all(binary_path.parent().expect("binary parent")).expect("create app");
        fs::write(
            manifest_dir.join("capsule.toml"),
            r#"schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
entrypoint = "MyApp.app"
"#,
        )
        .expect("write manifest");
        fs::write(
            manifest_dir.join(DELIVERY_CONFIG_FILE),
            r#"schema_version = "0.1"
[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "Other.app"
[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "Other.app"]
"#,
        )
        .expect("write sidecar");
        fs::write(&binary_path, b"unsigned-app").expect("write binary");
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&binary_path)
                .expect("binary metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&binary_path, permissions).expect("set permissions");
        }

        let err = detect_build_strategy(&manifest_dir)
            .expect_err("conflicting compatibility sidecar must be rejected");
        assert!(err
            .to_string()
            .contains("native delivery config conflicts with the default target contract"));
    }

    #[test]
    fn detect_build_strategy_rejects_partial_inline_delivery_config() {
        let tmp = tempdir().expect("tmp dir");
        let manifest_dir = tmp.path().join("inline-command-build-project");
        fs::create_dir_all(&manifest_dir).expect("create manifest dir");
        fs::write(
            manifest_dir.join("capsule.toml"),
            r#"schema_version = "0.2"
name = "time-management-desktop"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "sh"
cmd = ["build-app.sh"]

[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "dist/time-management-desktop.app"
"#,
        )
        .expect("write manifest");

        let err =
            detect_build_strategy(&manifest_dir).expect_err("should reject partial inline config");
        assert!(err
            .to_string()
            .contains("defines [artifact] without [finalize]"));
    }

    #[test]
    fn validate_native_bundle_directory_reports_nearby_candidates() -> Result<()> {
        let tmp = tempdir()?;
        let macos_dir = tmp.path().join("src-tauri/target/release/bundle/macos");
        let candidate = macos_dir.join("Time Management Desktop.app");
        fs::create_dir_all(&candidate)?;

        let err = validate_native_bundle_directory(&macos_dir.join("time-management-desktop.app"))
            .expect_err("missing exact app path should fail");
        let message = err.to_string();
        assert!(message.contains("Found nearby .app bundle candidates"));
        assert!(message.contains("Time Management Desktop.app"));
        Ok(())
    }

    #[test]
    fn validate_native_bundle_directory_reports_nearby_exe_candidates() -> Result<()> {
        let tmp = tempdir()?;
        let windows_dir = tmp.path().join("src-tauri/target/release/bundle/windows");
        let candidate = windows_dir.join("Time Management Desktop.exe");
        fs::create_dir_all(&windows_dir)?;
        fs::write(&candidate, sample_windows_executable_bytes())?;

        let err =
            validate_native_bundle_directory(&windows_dir.join("time-management-desktop.exe"))
                .expect_err("missing exact exe path should fail");
        let message = err.to_string();
        assert!(message.contains("Found nearby .exe candidates"));
        assert!(message.contains("Time Management Desktop.exe"));
        Ok(())
    }

    #[test]
    fn validate_native_bundle_directory_reports_nearby_appimage_candidates() -> Result<()> {
        let tmp = tempdir()?;
        let linux_dir = tmp.path().join("src-tauri/target/release/bundle/appimage");
        let candidate = linux_dir.join("Time Management Desktop.AppImage");
        fs::create_dir_all(&linux_dir)?;
        write_linux_elf(&candidate, 0o755)?;

        let err =
            validate_native_bundle_directory(&linux_dir.join("time-management-desktop.AppImage"))
                .expect_err("missing exact AppImage path should fail");
        let message = err.to_string();
        assert!(message.contains("Found nearby .AppImage candidates"));
        assert!(message.contains("Time Management Desktop.AppImage"));
        Ok(())
    }

    #[test]
    fn validate_native_bundle_directory_accepts_linux_directory_and_files() -> Result<()> {
        let tmp = tempdir()?;
        let linux_dir = tmp.path().join("dist/linux");
        let linux_binary = linux_dir.join("MyApp");
        let linux_deb = tmp.path().join("dist/MyApp.deb");
        let windows_exe = tmp.path().join("dist/MyApp.exe");
        fs::create_dir_all(&linux_dir)?;
        fs::create_dir_all(windows_exe.parent().context("missing exe parent")?)?;
        write_linux_elf(&linux_binary, 0o755)?;
        fs::write(&linux_deb, b"!<arch>\n")?;
        fs::write(&windows_exe, sample_windows_executable_bytes())?;

        validate_native_bundle_directory(&linux_dir)?;
        validate_native_bundle_directory(&linux_deb)?;
        validate_native_bundle_directory(&windows_exe)?;
        Ok(())
    }

    #[test]
    fn validate_native_bundle_directory_rejects_invalid_linux_elf() -> Result<()> {
        let tmp = tempdir()?;
        let linux_file = tmp.path().join("dist/MyApp.AppImage");
        fs::create_dir_all(linux_file.parent().context("missing linux parent")?)?;
        fs::write(&linux_file, b"\x7FELFnot-a-valid-elf")?;
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&linux_file)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&linux_file, permissions)?;
        }

        let err = validate_native_bundle_directory(&linux_file)
            .expect_err("invalid AppImage should fail ELF validation");
        assert!(err
            .to_string()
            .contains("Linux executable failed minimum ELF validation"));
        Ok(())
    }

    #[test]
    fn validate_native_bundle_directory_rejects_linux_directory_without_elf_executable(
    ) -> Result<()> {
        let tmp = tempdir()?;
        let linux_dir = tmp.path().join("dist/linux");
        let launcher = linux_dir.join("AppRun");
        fs::create_dir_all(&linux_dir)?;
        fs::write(&launcher, b"#!/bin/sh\nexit 0\n")?;
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&launcher)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&launcher, permissions)?;
        }

        let err = validate_native_bundle_directory(&linux_dir)
            .expect_err("directory without an ELF executable should fail closed");
        assert!(err.to_string().contains("missing a regular ELF executable"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn validate_native_bundle_directory_rejects_linux_elf_without_executable_bit() -> Result<()> {
        let tmp = tempdir()?;
        let linux_file = tmp.path().join("dist/MyApp");
        fs::create_dir_all(linux_file.parent().context("missing linux parent")?)?;
        write_linux_elf(&linux_file, 0o644)?;

        let err = validate_native_bundle_directory(&linux_file)
            .expect_err("missing executable bit should fail closed");
        assert!(err.to_string().contains("Executable bit is missing"));
        Ok(())
    }

    #[test]
    fn validate_native_bundle_directory_rejects_invalid_windows_executable() -> Result<()> {
        let tmp = tempdir()?;
        let windows_exe = tmp.path().join("dist/MyApp.exe");
        fs::create_dir_all(windows_exe.parent().context("missing exe parent")?)?;
        fs::write(&windows_exe, b"not-a-pe-file")?;

        let err = validate_native_bundle_directory(&windows_exe)
            .expect_err("invalid exe should fail PE validation");
        assert!(err
            .to_string()
            .contains("Windows executable failed minimum PE validation"));
        Ok(())
    }

    #[test]
    fn validate_native_bundle_directory_rejects_windows_dll_renamed_to_exe() -> Result<()> {
        let tmp = tempdir()?;
        let windows_exe = tmp.path().join("dist/MyApp.exe");
        fs::create_dir_all(windows_exe.parent().context("missing exe parent")?)?;
        fs::write(&windows_exe, sample_windows_dll_bytes())?;

        let err = validate_native_bundle_directory(&windows_exe)
            .expect_err("dll-shaped PE should fail executable validation");
        assert!(err.to_string().contains("is a DLL, not an .exe"));
        Ok(())
    }

    #[test]
    fn build_accepts_windows_single_file_native_artifacts() -> Result<()> {
        let tmp = tempdir()?;
        let plan = sample_file_native_build_plan(tmp.path())?;
        let artifact_path = tmp.path().join("out/my-app-0.1.0.capsule");

        let result = build_native_artifact_with_strip(&plan, Some(&artifact_path), |_path| Ok(()))?;

        assert_eq!(result.artifact_path, artifact_path);
        assert_eq!(result.derived_from, plan.source_app_path);
        let entry_modes = read_payload_entry_modes(&result.artifact_path)?;
        assert!(entry_modes.contains_key("dist/MyApp.exe"));
        Ok(())
    }

    fn read_payload_entry_modes(artifact_path: &Path) -> Result<BTreeMap<String, u32>> {
        let capsule_bytes = fs::read(artifact_path)?;
        let mut capsule = tar::Archive::new(Cursor::new(capsule_bytes));
        let mut payload_tar_zst = None;
        for entry in capsule.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.to_path_buf();
            if path == Path::new("payload.tar.zst") {
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes)?;
                payload_tar_zst = Some(bytes);
                break;
            }
        }

        let payload_tar_zst = payload_tar_zst.context("payload.tar.zst missing from capsule")?;
        let payload_tar = zstd::stream::decode_all(Cursor::new(payload_tar_zst))?;
        let mut payload = tar::Archive::new(Cursor::new(payload_tar));
        let mut entry_modes = BTreeMap::new();
        for entry in payload.entries()? {
            let entry = entry?;
            let path = entry.path()?.display().to_string();
            entry_modes.insert(path, entry.header().mode()?);
        }
        Ok(entry_modes)
    }

    fn read_capsule_manifest_value(artifact_path: &Path) -> Result<toml::Value> {
        let capsule_bytes = fs::read(artifact_path)?;
        let mut capsule = tar::Archive::new(Cursor::new(capsule_bytes));
        for entry in capsule.entries()? {
            let mut entry = entry?;
            if entry.path()?.as_ref() == Path::new("capsule.toml") {
                let mut raw = String::new();
                entry.read_to_string(&mut raw)?;
                return toml::from_str(&raw).map_err(anyhow::Error::from);
            }
        }
        bail!("capsule.toml missing from capsule")
    }

    fn sample_fetch_dir_with_mode(root: &Path, mode: u32) -> Result<PathBuf> {
        let fetched_dir = root.join("fetched");
        let artifact_dir = fetched_dir.join(FETCH_ARTIFACT_DIR);
        fs::create_dir_all(artifact_dir.join("MyApp.app/Contents/MacOS"))?;
        fs::write(
            artifact_dir.join(DELIVERY_CONFIG_FILE),
            sample_delivery_toml(),
        )?;
        fs::write(
            artifact_dir.join("MyApp.app/Contents/MacOS/MyApp"),
            b"unsigned-app",
        )?;
        #[cfg(unix)]
        {
            let binary = artifact_dir.join("MyApp.app/Contents/MacOS/MyApp");
            let mut permissions = fs::metadata(&binary)?.permissions();
            permissions.set_mode(mode);
            fs::set_permissions(&binary, permissions)?;
        }
        let metadata = FetchMetadata {
            schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
            scoped_id: "local/my-app".to_string(),
            version: "0.1.0".to_string(),
            registry: "http://127.0.0.1:8787".to_string(),
            fetched_at: "2026-03-09T00:00:00Z".to_string(),
            parent_digest: compute_tree_digest(&artifact_dir)?,
            artifact_blake3: compute_blake3(b"artifact"),
        };
        fs::create_dir_all(&fetched_dir)?;
        write_json_pretty(&fetched_dir.join(FETCH_METADATA_FILE), &metadata)?;
        Ok(fetched_dir)
    }

    fn sample_file_fetch_dir(root: &Path) -> Result<PathBuf> {
        let fetched_dir = root.join("fetched-file");
        let artifact_dir = fetched_dir.join(FETCH_ARTIFACT_DIR);
        fs::create_dir_all(artifact_dir.join("dist"))?;
        fs::write(
            artifact_dir.join(DELIVERY_CONFIG_FILE),
            sample_file_delivery_toml(),
        )?;
        fs::write(
            artifact_dir.join("dist/MyApp.exe"),
            sample_windows_executable_bytes(),
        )?;
        let metadata = FetchMetadata {
            schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
            scoped_id: "local/my-app".to_string(),
            version: "0.1.0".to_string(),
            registry: "http://127.0.0.1:8787".to_string(),
            fetched_at: "2026-03-09T00:00:00Z".to_string(),
            parent_digest: compute_tree_digest(&artifact_dir)?,
            artifact_blake3: compute_blake3(b"artifact"),
        };
        fs::create_dir_all(&fetched_dir)?;
        write_json_pretty(&fetched_dir.join(FETCH_METADATA_FILE), &metadata)?;
        Ok(fetched_dir)
    }

    fn sample_nested_fetch_dir(root: &Path) -> Result<PathBuf> {
        let fetched_dir = root.join("fetched-nested");
        let artifact_dir = fetched_dir.join(FETCH_ARTIFACT_DIR);
        let app_dir = artifact_dir.join("src-tauri/target/release/bundle/macos/My App.app");
        fs::create_dir_all(app_dir.join("Contents/MacOS"))?;
        fs::write(
            artifact_dir.join(DELIVERY_CONFIG_FILE),
            sample_nested_delivery_toml(),
        )?;
        fs::write(app_dir.join("Contents/MacOS/My App"), b"unsigned-app")?;
        #[cfg(unix)]
        {
            let binary = app_dir.join("Contents/MacOS/My App");
            let mut permissions = fs::metadata(&binary)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&binary, permissions)?;
        }
        let metadata = FetchMetadata {
            schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
            scoped_id: "local/my-app".to_string(),
            version: "0.1.0".to_string(),
            registry: "http://127.0.0.1:8787".to_string(),
            fetched_at: "2026-03-09T00:00:00Z".to_string(),
            parent_digest: compute_tree_digest(&artifact_dir)?,
            artifact_blake3: compute_blake3(b"artifact"),
        };
        fs::create_dir_all(&fetched_dir)?;
        write_json_pretty(&fetched_dir.join(FETCH_METADATA_FILE), &metadata)?;
        Ok(fetched_dir)
    }

    fn sample_finalized_app(root: &Path) -> Result<(PathBuf, PathBuf)> {
        sample_finalized_app_with_target(root, sample_supported_projection_target())
    }

    fn sample_finalized_app_with_target(root: &Path, target: &str) -> Result<(PathBuf, PathBuf)> {
        let derived_dir = root.join("derived-output");
        let derived_app = if delivery_target_os_family(target) == Some("linux") {
            let derived_app = derived_dir.join("my-app");
            let binary = derived_app.join("my-app");
            fs::create_dir_all(&derived_app)?;
            fs::write(&binary, b"#!/bin/sh\necho signed-app\n")?;
            #[cfg(unix)]
            {
                let mut permissions = fs::metadata(&binary)?.permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&binary, permissions)?;
            }
            derived_app
        } else if delivery_target_os_family(target) == Some("windows") {
            let derived_app = derived_dir.join("MyApp");
            let binary = derived_app.join("MyApp.exe");
            fs::create_dir_all(&derived_app)?;
            fs::write(&binary, b"signed-app")?;
            #[cfg(unix)]
            {
                let mut permissions = fs::metadata(&binary)?.permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&binary, permissions)?;
            }
            derived_app
        } else {
            let derived_app = derived_dir.join("MyApp.app");
            fs::create_dir_all(derived_app.join("Contents/MacOS"))?;
            fs::write(derived_app.join("Contents/MacOS/MyApp"), b"signed-app")?;
            #[cfg(unix)]
            {
                let binary = derived_app.join("Contents/MacOS/MyApp");
                let mut permissions = fs::metadata(&binary)?.permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&binary, permissions)?;
            }
            derived_app
        };
        let provenance = LocalDerivationProvenance {
            schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
            scoped_id: None,
            version: None,
            registry: None,
            artifact_blake3: None,
            parent_digest: "blake3:parent-digest".to_string(),
            derived_digest: compute_tree_digest(&derived_app)?,
            framework: DEFAULT_DELIVERY_FRAMEWORK.to_string(),
            target: target.to_string(),
            finalized_locally: true,
            finalize_tool: DEFAULT_FINALIZE_TOOL.to_string(),
            finalized_at: "2026-03-09T00:00:00Z".to_string(),
        };
        write_json_pretty(&derived_dir.join(PROVENANCE_FILE), &provenance)?;
        Ok((derived_dir, derived_app))
    }

    fn sample_supported_projection_target() -> &'static str {
        match host_projection_os_family() {
            Some("linux") => "linux/x86_64",
            Some("windows") => "windows/x86_64",
            _ => "darwin/arm64",
        }
    }

    fn sample_projection_launcher_dir(root: &Path) -> PathBuf {
        root.join("launcher")
    }

    fn sample_projection_command_dir(root: &Path) -> PathBuf {
        root.join("bin")
    }

    fn sample_projection_binary_path(derived_app: &Path) -> PathBuf {
        if path_has_extension(derived_app, "app") {
            derived_app.join("Contents/MacOS/MyApp")
        } else {
            let windows_binary = derived_app.join("MyApp.exe");
            if windows_binary.exists() {
                windows_binary
            } else {
                derived_app.join("my-app")
            }
        }
    }

    #[test]
    fn sanitize_projection_segment_normalizes_special_characters() {
        assert_eq!(sanitize_projection_segment("My App"), "my-app");
        assert_eq!(sanitize_projection_segment("---"), "ato-app");
        assert_eq!(sanitize_projection_segment("my___app"), "my___app");
        assert_eq!(sanitize_projection_segment("My.App"), "my-app");
        assert_eq!(sanitize_projection_segment("My...App"), "my-app");
    }

    #[test]
    fn projection_name_helpers_prefer_scoped_slug_when_available() -> Result<()> {
        let derived_app_path = Path::new("Time Management Desktop.app");
        assert_eq!(
            projection_display_name(derived_app_path, Some("koh0920/time-management-desktop"))?,
            "Time Management Desktop"
        );
        assert_eq!(
            projection_command_name(derived_app_path, Some("koh0920/time-management-desktop"))?,
            "time-management-desktop"
        );
        Ok(())
    }

    #[test]
    fn render_linux_desktop_entry_escapes_special_characters() {
        let rendered = render_linux_desktop_entry(
            "My App\nTabbed\tName",
            Path::new("My App/bin/my\"app"),
            Path::new("My App/root"),
        );
        assert!(rendered.contains("Name=My App\\nTabbed\\tName"));
        assert!(rendered.contains("Exec=My\\ App/bin/my\\\"app"));
        assert!(rendered.contains("Path=My App/root"));
    }

    #[test]
    fn resolve_linux_projection_command_target_prefers_named_binary() -> Result<()> {
        let tmp = tempdir()?;
        let app_dir = tmp.path().join("my-app");
        let preferred = app_dir.join("my-app");
        let other = app_dir.join("bin/helper");
        fs::create_dir_all(other.parent().context("helper parent missing")?)?;
        fs::write(&preferred, b"#!/bin/sh\n")?;
        fs::write(&other, b"#!/bin/sh\n")?;
        #[cfg(unix)]
        {
            for path in [&preferred, &other] {
                let mut permissions = fs::metadata(path)?.permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(path, permissions)?;
            }
        }

        assert_eq!(
            resolve_linux_projection_command_target(&app_dir)?,
            preferred
        );
        Ok(())
    }

    #[test]
    fn resolve_linux_projection_command_target_rejects_multiple_candidates() -> Result<()> {
        let tmp = tempdir()?;
        let app_dir = tmp.path().join("my-app");
        let first = app_dir.join("bin/alpha");
        let second = app_dir.join("bin/beta");
        fs::create_dir_all(first.parent().context("bin parent missing")?)?;
        fs::write(&first, b"#!/bin/sh\n")?;
        fs::write(&second, b"#!/bin/sh\n")?;
        #[cfg(unix)]
        {
            for path in [&first, &second] {
                let mut permissions = fs::metadata(path)?.permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(path, permissions)?;
            }
        }

        let err = resolve_linux_projection_command_target(&app_dir)
            .expect_err("multiple executable candidates should fail");
        assert!(err
            .to_string()
            .contains("multiple executable command candidates"));
        Ok(())
    }

    #[test]
    fn delivery_config_accepts_non_codesign_tool_and_non_default_target() {
        let config: DeliveryConfig = toml::from_str(
            r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "windows/x86_64"
    input = "MyApp.app"
[finalize]
    tool = "signtool"
    args = ["sign", "/fd", "SHA256", "MyApp.app"]
"#,
        )
        .expect("config parse");
        validate_delivery_config(&config).expect("config should be accepted");
    }

    #[test]
    fn delivery_config_accepts_signtool_with_timestamp_args() {
        let config: DeliveryConfig = toml::from_str(
            r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "windows/x86_64"
    input = "dist/MyApp.exe"
[finalize]
    tool = "signtool"
    args = ["sign", "/fd", "SHA256", "/tr", "http://tsa.test", "/td", "SHA256", "dist/MyApp.exe"]
"#,
        )
        .expect("config parse");
        validate_delivery_config(&config).expect("config should be accepted");
    }

    #[test]
    fn delivery_config_rejects_unknown_signtool_switch() {
        let config: DeliveryConfig = toml::from_str(
            r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "windows/x86_64"
    input = "dist/MyApp.exe"
[finalize]
    tool = "signtool"
    args = ["sign", "/bogus", "dist/MyApp.exe"]
"#,
        )
        .expect("config parse");
        let err = validate_delivery_config(&config).expect_err("config should be rejected");
        assert!(err
            .to_string()
            .contains("Unsupported finalize.args entry '/bogus'"));
    }

    #[test]
    fn delivery_config_rejects_unsupported_target() {
        let config: DeliveryConfig = toml::from_str(
            r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "solaris/x86_64"
    input = "MyApp.app"
[finalize]
    tool = "codesign"
    args = ["--deep", "--force", "--sign", "-", "MyApp.app"]
"#,
        )
        .expect("config parse");
        let err = validate_delivery_config(&config).expect_err("config should be rejected");
        assert!(err
            .to_string()
            .contains("Unsupported artifact.target 'solaris/x86_64'"));
    }

    #[test]
    fn delivery_config_accepts_linux_target_with_noop_finalize_tool() {
        let config: DeliveryConfig = toml::from_str(
            r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "linux/x86_64"
    input = "dist/my-app"
[finalize]
    tool = "none"
    args = []
"#,
        )
        .expect("config parse");
        validate_delivery_config(&config).expect("config should be accepted");
    }

    #[test]
    fn delivery_config_accepts_linux_aarch64_with_chmod_finalize_tool() {
        let config: DeliveryConfig = toml::from_str(
            r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "linux/aarch64"
    input = "dist/my-app"
[finalize]
    tool = "chmod"
    args = ["0755", "dist/my-app"]
"#,
        )
        .expect("config parse");
        validate_delivery_config(&config).expect("config should be accepted");
    }

    #[test]
    fn resolve_fetch_request_accepts_issue_style_inline_registry_ref() -> Result<()> {
        let resolved =
            resolve_fetch_request("localhost:8080/my-tauri-app:unsigned-0.1.0", None, None)?;
        assert_eq!(
            resolved,
            ResolvedFetchRequest {
                capsule_ref: "local/my-tauri-app".to_string(),
                registry_url: Some("http://localhost:8080".to_string()),
                version: Some("unsigned-0.1.0".to_string()),
            }
        );
        Ok(())
    }

    #[test]
    fn resolve_fetch_request_accepts_inline_registry_with_explicit_scope() -> Result<()> {
        let resolved = resolve_fetch_request(
            "https://127.0.0.1:8787/koh0920/sample-native-capsule:0.1.0",
            None,
            None,
        )?;
        assert_eq!(
            resolved,
            ResolvedFetchRequest {
                capsule_ref: "koh0920/sample-native-capsule".to_string(),
                registry_url: Some("https://127.0.0.1:8787".to_string()),
                version: Some("0.1.0".to_string()),
            }
        );
        Ok(())
    }

    #[test]
    fn resolve_fetch_request_rejects_conflicting_registry_override() {
        let err = resolve_fetch_request(
            "localhost:8080/my-tauri-app:unsigned-0.1.0",
            Some("http://127.0.0.1:8787"),
            None,
        )
        .expect_err("registry conflict must fail");
        assert!(err.to_string().contains("conflicting_registry_request"));
    }

    #[test]
    fn tree_digest_is_stable_for_identical_trees() -> Result<()> {
        let tmp = tempdir()?;
        let left = tmp.path().join("left");
        let right = tmp.path().join("right");
        fs::create_dir_all(left.join("a/b"))?;
        fs::create_dir_all(right.join("a/b"))?;
        fs::write(left.join("a/b/file.txt"), b"hello")?;
        fs::write(right.join("a/b/file.txt"), b"hello")?;
        assert_eq!(compute_tree_digest(&left)?, compute_tree_digest(&right)?);
        Ok(())
    }

    #[test]
    fn native_delivery_documented_json_contract_fields_are_present() -> Result<()> {
        let tmp = tempdir()?;

        let fetched_dir = sample_fetch_dir(tmp.path())?;
        let fetch_metadata = load_fetch_metadata(&fetched_dir)?;
        let fetch_json = serde_json::to_value(&fetch_metadata)?;
        assert_json_object_has_keys(
            &fetch_json,
            &[
                "schema_version",
                "scoped_id",
                "version",
                "registry",
                "parent_digest",
            ],
        );

        let build_json = serde_json::to_value(NativeBuildResult {
            artifact_path: tmp.path().join("out/my-app-0.1.0.capsule"),
            build_strategy: "native-delivery".to_string(),
            target: DEFAULT_DELIVERY_TARGET.to_string(),
            derived_from: tmp.path().join("MyApp.app"),
            schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
        })?;
        assert_json_object_has_keys(
            &build_json,
            &["build_strategy", "schema_version", "target", "derived_from"],
        );

        let (derived_dir, derived_app) = sample_finalized_app(tmp.path())?;
        let provenance_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(derived_dir.join(PROVENANCE_FILE))?)?;
        assert_json_object_has_keys(
            &provenance_json,
            &[
                "schema_version",
                "parent_digest",
                "derived_digest",
                "framework",
                "target",
                "finalize_tool",
                "finalized_at",
            ],
        );

        let finalize_json = serde_json::to_value(FinalizeResult {
            fetched_dir: fetched_dir.clone(),
            output_dir: derived_dir.clone(),
            derived_app_path: derived_app.clone(),
            provenance_path: derived_dir.join(PROVENANCE_FILE),
            parent_digest: "blake3:parent-digest".to_string(),
            derived_digest: "blake3:derived-digest".to_string(),
            schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
        })?;
        assert_json_object_has_keys(
            &finalize_json,
            &[
                "schema_version",
                "derived_app_path",
                "provenance_path",
                "parent_digest",
                "derived_digest",
            ],
        );

        let launcher_dir = tmp.path().join("Applications");
        let metadata_root = tmp.path().join("projection-metadata");
        let project_result = project_with_roots(&derived_app, &launcher_dir, &metadata_root)?;
        let project_json = serde_json::to_value(&project_result)?;
        assert_json_object_has_keys(
            &project_json,
            &[
                "schema_version",
                "projection_id",
                "metadata_path",
                "projected_path",
                "derived_app_path",
                "parent_digest",
                "derived_digest",
                "state",
            ],
        );

        let unproject_result =
            unproject_with_metadata_root(&project_result.projection_id, &metadata_root)?;
        let unproject_json = serde_json::to_value(&unproject_result)?;
        assert_json_object_has_keys(
            &unproject_json,
            &[
                "schema_version",
                "projection_id",
                "metadata_path",
                "projected_path",
                "removed_projected_path",
                "removed_metadata",
                "state_before",
            ],
        );

        Ok(())
    }

    #[test]
    fn build_native_artifact_preserves_source_and_payload_executable_mode() -> Result<()> {
        let tmp = tempdir()?;
        let plan = sample_native_build_plan(tmp.path(), 0o755)?;
        let source_digest_before = compute_tree_digest(&plan.source_app_path)?;
        let artifact_path = tmp.path().join("out/my-app-0.1.0.capsule");

        let result = build_native_artifact_with_strip(&plan, Some(&artifact_path), |_app| Ok(()))?;

        assert_eq!(result.build_strategy, "native-delivery");
        assert_eq!(
            result.target,
            default_delivery_target_for_input("MyApp.app")
        );
        assert_eq!(result.derived_from, plan.source_app_path);
        assert_eq!(
            compute_tree_digest(&plan.source_app_path)?,
            source_digest_before
        );

        let entry_modes = read_payload_entry_modes(&artifact_path)?;
        assert!(entry_modes.contains_key(DELIVERY_CONFIG_FILE));
        #[cfg(unix)]
        assert_eq!(
            entry_modes
                .get("MyApp.app/Contents/MacOS/MyApp")
                .copied()
                .unwrap_or_default()
                & 0o111,
            0o111
        );
        let manifest_value = read_capsule_manifest_value(&artifact_path)?;
        assert!(manifest_value
            .get("distribution")
            .and_then(|value| value.as_table())
            .is_some());
        Ok(())
    }

    #[test]
    fn test_build_rejects_non_executable_without_mutation() -> Result<()> {
        let tmp = tempdir()?;
        let plan = sample_native_build_plan(tmp.path(), 0o755)?;
        #[cfg(unix)]
        {
            let binary_path = plan.source_app_path.join("Contents/MacOS/MyApp");
            let mut permissions = fs::metadata(&binary_path)?.permissions();
            permissions.set_mode(0o644);
            fs::set_permissions(&binary_path, permissions)?;
        }
        let source_digest_before = compute_tree_digest(&plan.source_app_path)?;
        let artifact_path = tmp.path().join("out/my-app-0.1.0.capsule");

        let result = build_native_artifact_with_strip(&plan, Some(&artifact_path), |_app| Ok(()));

        if cfg!(unix) {
            let err = result.expect_err("build must fail closed when executable bit is missing");
            assert!(err.to_string().contains("Executable bit is missing"));
            assert!(!artifact_path.exists());
        } else {
            let built = result.expect("non-macOS hosts currently skip app permission enforcement");
            assert_eq!(built.artifact_path, artifact_path);
        }
        assert_eq!(
            compute_tree_digest(&plan.source_app_path)?,
            source_digest_before
        );
        Ok(())
    }

    #[test]
    fn finalize_creates_derived_copy_without_mutating_parent() -> Result<()> {
        let tmp = tempdir()?;
        let fetched_dir = sample_fetch_dir(tmp.path())?;
        let artifact_dir = fetched_dir.join(FETCH_ARTIFACT_DIR);
        let parent_before = compute_tree_digest(&artifact_dir)?;
        let output_root = tmp.path().join("dist");

        let result = finalize_with_runner(&fetched_dir, &output_root, |derived_dir, _config| {
            let app_binary = derived_dir.join("MyApp.app/Contents/MacOS/MyApp");
            fs::write(&app_binary, b"signed-app")?;
            Ok(())
        })?;

        assert_eq!(parent_before, result.parent_digest);
        assert_eq!(compute_tree_digest(&artifact_dir)?, parent_before);
        assert!(result.derived_app_path.exists());
        assert!(result.provenance_path.exists());
        assert_ne!(result.parent_digest, result.derived_digest);
        #[cfg(unix)]
        {
            let derived_binary = result.derived_app_path.join("Contents/MacOS/MyApp");
            assert_ne!(
                fs::metadata(&derived_binary)?.permissions().mode() & 0o111,
                0
            );
        }
        let sidecar: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&result.provenance_path)?)?;
        assert_eq!(sidecar["parent_digest"], result.parent_digest);
        assert_eq!(sidecar["derived_digest"], result.derived_digest);
        assert_eq!(sidecar["finalize_tool"], DEFAULT_FINALIZE_TOOL);
        Ok(())
    }

    #[test]
    fn finalize_rejects_missing_executable_bit() -> Result<()> {
        let tmp = tempdir()?;
        let fetched_dir = sample_fetch_dir_with_mode(tmp.path(), 0o644)?;
        let output_root = tmp.path().join("dist");

        let result =
            finalize_with_runner(&fetched_dir, &output_root, |_derived_dir, _config| Ok(()));
        if cfg!(unix) {
            let err = result.expect_err("finalize must fail closed when executable bit is missing");
            assert!(err.to_string().contains("Executable bit is missing"));
        } else {
            result.expect("non-macOS hosts currently skip app permission enforcement");
        }
        Ok(())
    }

    #[test]
    fn finalize_accepts_windows_single_file_native_artifacts() -> Result<()> {
        let tmp = tempdir()?;
        let fetched_dir = sample_file_fetch_dir(tmp.path())?;
        let output_root = tmp.path().join("dist");

        let result =
            finalize_with_runner(&fetched_dir, &output_root, |_derived_dir, _config| Ok(()))?;

        assert_eq!(
            result
                .derived_app_path
                .file_name()
                .and_then(|value| value.to_str()),
            Some("MyApp.exe")
        );
        assert!(result.derived_app_path.is_file());
        Ok(())
    }

    #[test]
    fn finalize_rebases_nested_input_to_local_app_name() -> Result<()> {
        let tmp = tempdir()?;
        let fetched_dir = sample_nested_fetch_dir(tmp.path())?;
        let output_root = tmp.path().join("dist");

        let result = finalize_with_runner(&fetched_dir, &output_root, |derived_dir, config| {
            assert_eq!(config.artifact.input, "My App.app");
            assert_eq!(config.finalize.args[4], "My App.app");
            let app_binary = derived_dir.join("My App.app/Contents/MacOS/My App");
            fs::write(&app_binary, b"signed-app")?;
            Ok(())
        })?;

        assert_eq!(
            result
                .derived_app_path
                .file_name()
                .and_then(|value| value.to_str()),
            Some("My App.app")
        );
        Ok(())
    }

    #[test]
    fn rebase_delivery_config_updates_matching_finalize_args() -> Result<()> {
        let tmp = tempdir()?;
        let config: DeliveryConfig = toml::from_str(
            r#"schema_version = "0.1"
[artifact]
    framework = "tauri"
    stage = "unsigned"
    target = "windows/x86_64"
    input = "dist/MyApp.exe"
[finalize]
    tool = "signtool"
    args = ["sign", "/fd", "SHA256", "dist/MyApp.exe", "/tr", "http://tsa.test/dist/MyApp.exe"]
"#,
        )?;
        let rebased = rebase_delivery_config_for_finalize(&config, &tmp.path().join("MyApp.exe"))?;
        assert_eq!(rebased.artifact.input, "MyApp.exe");
        assert_eq!(rebased.finalize.args[3], "MyApp.exe");
        assert_eq!(rebased.finalize.args[5], "http://tsa.test/dist/MyApp.exe");
        Ok(())
    }

    #[test]
    fn delivery_target_os_family_parses_expected_values() {
        assert_eq!(delivery_target_os_family("darwin/arm64"), Some("darwin"));
        assert_eq!(delivery_target_os_family("windows/x86_64"), Some("windows"));
        assert_eq!(delivery_target_os_family(""), None);
        assert_eq!(delivery_target_os_family("/arm64"), None);
    }

    #[test]
    fn supports_projection_target_accepts_supported_platforms() {
        assert!(supports_projection_target("darwin/arm64"));
        assert!(supports_projection_target("darwin/x86_64"));
        assert!(supports_projection_target("linux/x86_64"));
        assert!(supports_projection_target("windows/x86_64"));
        assert!(!supports_projection_target(""));
    }

    #[test]
    fn first_existing_projection_candidate_returns_none_for_missing_paths() -> Result<()> {
        let tmp = tempdir()?;
        let missing = tmp.path().join("Applications").join("MissingApp");
        assert_eq!(first_existing_projection_candidate(&missing)?, None);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn windows_shortcut_roundtrip_resolves_expected_target() -> Result<()> {
        let tmp = tempdir()?;
        let target = tmp.path().join("MyApp");
        fs::create_dir_all(&target)?;
        let shortcut = projection_shortcut_path(&tmp.path().join("Launcher").join("MyApp"));
        let shortcut_parent = shortcut
            .parent()
            .ok_or_else(|| anyhow::anyhow!("shortcut path missing parent"))?;
        fs::create_dir_all(shortcut_parent)?;

        create_projection_shortcut(&target, &shortcut)?;

        assert!(shortcut.is_file());
        assert!(is_projection_shortcut(&shortcut, &fs::metadata(&shortcut)?));
        assert!(paths_match(
            &resolve_projection_shortcut_target(&shortcut)?,
            &target
        )?);
        assert_eq!(
            first_existing_projection_candidate(&tmp.path().join("Launcher").join("MyApp"))?,
            Some(shortcut)
        );
        Ok(())
    }

    #[test]
    fn copy_recursively_preserves_executable_mode() -> Result<()> {
        let tmp = tempdir()?;
        let source = tmp.path().join("source.bin");
        let destination = tmp.path().join("nested/destination.bin");
        fs::write(&source, b"hello")?;
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&source)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&source, permissions)?;
        }

        copy_recursively(&source, &destination)?;

        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&destination)?.permissions().mode() & 0o777,
                0o755
            );
        }
        Ok(())
    }

    #[test]
    fn ensure_tree_writable_clears_readonly_on_files() -> Result<()> {
        let tmp = tempdir()?;
        let app_dir = tmp.path().join("MyApp.app");
        let binary = app_dir.join("Contents/MacOS/MyApp");
        fs::create_dir_all(binary.parent().expect("binary parent"))?;
        fs::write(&binary, b"unsigned-app")?;

        let mut permissions = fs::metadata(&binary)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&binary, permissions)?;

        ensure_tree_writable(&app_dir)?;

        assert!(!fs::metadata(&binary)?.permissions().readonly());
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn materialize_fetch_cache_extracts_payload_tree() -> Result<()> {
        let tmp_home = tempdir()?;
        std::env::set_var("HOME", tmp_home.path());

        let payload_tar = {
            let mut out = Vec::new();
            {
                let mut builder = tar::Builder::new(&mut out);
                append_tar_entry(
                    &mut builder,
                    DELIVERY_CONFIG_FILE,
                    sample_delivery_toml().as_bytes(),
                    0o644,
                )?;
                append_tar_entry(
                    &mut builder,
                    "MyApp.app/Contents/MacOS/MyApp",
                    b"unsigned-app",
                    0o644,
                )?;
                builder.finish()?;
            }
            out
        };
        let artifact = build_capsule_bytes(&payload_tar)?;
        let result =
            materialize_fetch_cache("local/my-app", "0.1.0", "http://127.0.0.1:8787", &artifact)?;

        assert!(result.cache_dir.exists());
        assert!(result.artifact_dir.join(DELIVERY_CONFIG_FILE).exists());
        assert!(result
            .artifact_dir
            .join("MyApp.app/Contents/MacOS/MyApp")
            .exists());
        let metadata = load_fetch_metadata(&result.cache_dir)?;
        assert_eq!(metadata.parent_digest, result.parent_digest);
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn materialize_fetch_cache_preserves_executable_mode_from_payload() -> Result<()> {
        let tmp_home = tempdir()?;
        std::env::set_var("HOME", tmp_home.path());

        let payload_tar = {
            let mut out = Vec::new();
            {
                let mut builder = tar::Builder::new(&mut out);
                append_tar_entry(
                    &mut builder,
                    DELIVERY_CONFIG_FILE,
                    sample_delivery_toml().as_bytes(),
                    0o644,
                )?;
                append_tar_entry(
                    &mut builder,
                    "MyApp.app/Contents/MacOS/MyApp",
                    b"unsigned-app",
                    0o755,
                )?;
                builder.finish()?;
            }
            out
        };
        let artifact = build_capsule_bytes(&payload_tar)?;
        let result =
            materialize_fetch_cache("local/my-app", "0.1.0", "http://127.0.0.1:8787", &artifact)?;

        #[cfg(unix)]
        {
            let binary = result.artifact_dir.join("MyApp.app/Contents/MacOS/MyApp");
            assert_ne!(fs::metadata(binary)?.permissions().mode() & 0o111, 0);
        }
        Ok(())
    }

    #[test]
    fn project_creates_projection_metadata_without_mutating_derived_artifact() -> Result<()> {
        let tmp = tempdir()?;
        let metadata_root = tmp.path().join("projection-metadata");
        let launcher_dir = sample_projection_launcher_dir(tmp.path());
        let command_dir = sample_projection_command_dir(tmp.path());
        let (_derived_dir, derived_app) = sample_finalized_app(tmp.path())?;
        let digest_before = compute_tree_digest(&derived_app)?;

        let result = project_with_roots_and_command_dir(
            &derived_app,
            &launcher_dir,
            &metadata_root,
            &command_dir,
        )?;

        assert!(result.created);
        assert_eq!(result.state, "ok");
        assert_eq!(compute_tree_digest(&derived_app)?, digest_before);
        assert!(result.projected_path.exists());
        if cfg!(target_os = "linux") {
            let projected_meta = fs::symlink_metadata(&result.projected_path)?;
            assert!(projected_meta.is_file());
            let desktop = fs::read_to_string(&result.projected_path)?;
            assert!(desktop.contains("[Desktop Entry]"));
            assert!(desktop.contains("Exec="));
            let command_path = command_dir.join("my-app");
            assert!(fs::symlink_metadata(&command_path)?
                .file_type()
                .is_symlink());
            assert_eq!(
                fs::read_link(&command_path)?,
                sample_projection_binary_path(&derived_app)
            );
        } else if !cfg!(windows) {
            let symlink_meta = fs::symlink_metadata(&result.projected_path)?;
            assert!(symlink_meta.file_type().is_symlink());
        }
        assert!(result.metadata_path.exists());
        Ok(())
    }

    #[test]
    fn project_rejects_name_conflict() -> Result<()> {
        if !host_supports_projection() {
            return Ok(());
        }
        let tmp = tempdir()?;
        let metadata_root = tmp.path().join("projection-metadata");
        let launcher_dir = sample_projection_launcher_dir(tmp.path());
        let command_dir = sample_projection_command_dir(tmp.path());
        let (_derived_dir, derived_app) = sample_finalized_app(tmp.path())?;
        fs::create_dir_all(&launcher_dir)?;
        let conflict_path = if cfg!(target_os = "linux") {
            launcher_dir.join("my-app.desktop")
        } else {
            launcher_dir.join("MyApp.app")
        };
        fs::write(conflict_path, b"occupied")?;

        let err = project_with_roots_and_command_dir(
            &derived_app,
            &launcher_dir,
            &metadata_root,
            &command_dir,
        )
        .expect_err("projection must reject name conflicts");
        assert!(err.to_string().contains("Projection name conflict"));
        Ok(())
    }

    #[test]
    fn project_list_reports_broken_projection_when_target_missing() -> Result<()> {
        if !host_supports_projection() {
            return Ok(());
        }
        let tmp = tempdir()?;
        let metadata_root = tmp.path().join("projection-metadata");
        let launcher_dir = sample_projection_launcher_dir(tmp.path());
        let command_dir = sample_projection_command_dir(tmp.path());
        let (_derived_dir, derived_app) = sample_finalized_app(tmp.path())?;
        let result = project_with_roots_and_command_dir(
            &derived_app,
            &launcher_dir,
            &metadata_root,
            &command_dir,
        )?;
        let orphaned_app = tmp.path().join(if cfg!(target_os = "linux") {
            "my-app-orphaned"
        } else {
            "MyApp-orphaned.app"
        });
        fs::rename(&derived_app, orphaned_app)?;

        let listing = list_projections(&metadata_root)?;
        assert_eq!(listing.total, 1);
        assert_eq!(listing.broken, 1);
        assert_eq!(listing.projections[0].projection_id, result.projection_id);
        assert!(listing.projections[0]
            .problems
            .iter()
            .any(|problem| problem == "derived_app_missing"));
        Ok(())
    }

    #[test]
    fn linux_project_list_reports_missing_command_symlink() -> Result<()> {
        if !cfg!(target_os = "linux") {
            return Ok(());
        }
        let tmp = tempdir()?;
        let metadata_root = tmp.path().join("projection-metadata");
        let launcher_dir = sample_projection_launcher_dir(tmp.path());
        let command_dir = sample_projection_command_dir(tmp.path());
        let (_derived_dir, derived_app) =
            sample_finalized_app_with_target(tmp.path(), "linux/x86_64")?;
        let result = project_with_roots_and_command_dir(
            &derived_app,
            &launcher_dir,
            &metadata_root,
            &command_dir,
        )?;
        fs::remove_file(command_dir.join("my-app"))?;

        let listing = list_projections(&metadata_root)?;
        assert_eq!(listing.total, 1);
        assert_eq!(listing.broken, 1);
        assert_eq!(listing.projections[0].projection_id, result.projection_id);
        assert!(listing.projections[0]
            .problems
            .iter()
            .any(|problem| problem == "projected_command_missing"));
        Ok(())
    }

    #[test]
    fn unproject_removes_symlink_and_metadata_even_when_target_missing() -> Result<()> {
        if !host_supports_projection() {
            return Ok(());
        }
        let tmp = tempdir()?;
        let metadata_root = tmp.path().join("projection-metadata");
        let launcher_dir = sample_projection_launcher_dir(tmp.path());
        let command_dir = sample_projection_command_dir(tmp.path());
        let (_derived_dir, derived_app) = sample_finalized_app(tmp.path())?;
        let result = project_with_roots_and_command_dir(
            &derived_app,
            &launcher_dir,
            &metadata_root,
            &command_dir,
        )?;
        let orphaned_app = tmp.path().join(if cfg!(target_os = "linux") {
            "my-app-orphaned"
        } else {
            "MyApp-orphaned.app"
        });
        fs::rename(&derived_app, orphaned_app)?;

        let unprojected = unproject_with_metadata_root(&result.projection_id, &metadata_root)?;
        assert!(unprojected.removed_projected_path);
        assert!(unprojected.removed_metadata);
        assert!(!result.projected_path.exists());
        if cfg!(target_os = "linux") {
            assert!(!command_dir.join("my-app").exists());
        }
        assert!(!result.metadata_path.exists());
        Ok(())
    }

    #[test]
    fn project_rejects_digest_mismatch() -> Result<()> {
        if !host_supports_projection() {
            return Ok(());
        }
        let tmp = tempdir()?;
        let metadata_root = tmp.path().join("projection-metadata");
        let launcher_dir = sample_projection_launcher_dir(tmp.path());
        let command_dir = sample_projection_command_dir(tmp.path());
        let (derived_dir, derived_app) = sample_finalized_app(tmp.path())?;
        fs::write(sample_projection_binary_path(&derived_app), b"tampered-app")?;

        let err = project_with_roots_and_command_dir(
            &derived_app,
            &launcher_dir,
            &metadata_root,
            &command_dir,
        )
        .expect_err("projection must reject digest mismatches");
        assert!(err.to_string().contains("Derived artifact digest mismatch"));
        assert!(derived_dir.join(PROVENANCE_FILE).exists());
        Ok(())
    }

    #[test]
    fn project_rejects_mismatched_host_targets_even_with_valid_shape() -> Result<()> {
        let tmp = tempdir()?;
        let metadata_root = tmp.path().join("projection-metadata");
        let launcher_dir = sample_projection_launcher_dir(tmp.path());
        let command_dir = sample_projection_command_dir(tmp.path());
        let unsupported_target = match host_projection_os_family() {
            Some("linux") => "windows/x86_64",
            Some("windows") => "linux/x86_64",
            _ => "linux/x86_64",
        };
        let (_derived_dir, derived_app) =
            sample_finalized_app_with_target(tmp.path(), unsupported_target)?;

        let err = project_with_roots_and_command_dir(
            &derived_app,
            &launcher_dir,
            &metadata_root,
            &command_dir,
        )
        .expect_err("projection must fail closed for unsupported targets");

        assert!(err.to_string().contains("unsupported on this host"));
        Ok(())
    }

    fn append_tar_entry(
        builder: &mut tar::Builder<&mut Vec<u8>>,
        path: &str,
        bytes: &[u8],
        mode: u32,
    ) -> Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        builder.append_data(&mut header, path, Cursor::new(bytes))?;
        Ok(())
    }

    fn build_capsule_bytes(payload_tar: &[u8]) -> Result<Vec<u8>> {
        let payload_tar_zst = zstd::stream::encode_all(Cursor::new(payload_tar), 3)?;
        let mut out = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut out);
            append_tar_entry(&mut builder, "capsule.toml", b"schema_version = \"0.2\"\nname = \"demo\"\nversion = \"0.1.0\"\ntype = \"app\"\ndefault_target = \"cli\"\n[targets.cli]\nruntime = \"static\"\npath = \"MyApp.app\"\n", 0o644)?;
            append_tar_entry(&mut builder, "payload.tar.zst", &payload_tar_zst, 0o644)?;
            builder.finish()?;
        }
        Ok(out)
    }
}

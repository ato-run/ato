use anyhow::{bail, Context, Result};
use capsule_core::bootstrap::BootstrapBoundary;
use capsule_core::router::{CompatManifestBridge, ExecutionDescriptor};
use chrono::{SecondsFormat, Utc};
use goblin::Object;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use walkdir::WalkDir;

use crate::artifact_hash::compute_blake3_label as compute_blake3;
use crate::capsule_archive::extract_payload_tar_from_capsule;
use crate::install;
use crate::registry::http;

mod filesystem;

#[cfg(windows)]
use mslnk::ShellLink;
#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::symlink_dir;

pub(crate) use filesystem::*;

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
    pub workspace_root: PathBuf,
    #[serde(skip_serializing)]
    pub legacy_manifest_bridge: Option<CompatManifestBridge>,
    pub package_name: String,
    pub package_version: String,
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

#[derive(Debug, Clone, Serialize)]
pub struct FinalizeResult {
    pub fetched_dir: PathBuf,
    pub output_dir: PathBuf,
    pub derived_app_path: PathBuf,
    pub provenance_path: PathBuf,
    pub parent_digest: String,
    pub derived_digest: String,
    pub schema_version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FinalizedPublishArtifactResult {
    pub artifact_path: PathBuf,
    pub identity_class: &'static str,
    pub finalize: FinalizeResult,
}

#[derive(Debug, Clone)]
struct DerivedFinalizePlan {
    fetched_dir: PathBuf,
    output_dir: PathBuf,
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

#[derive(Debug, Clone)]
struct DerivedProjectionPlan {
    launcher_dir: PathBuf,
    metadata_root: PathBuf,
    projected_command_dir: PathBuf,
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

    if delivery_config_path.exists() {
        bail!(
            "{} is no longer accepted in source projects. Move native delivery metadata into capsule.toml [artifact] and [finalize].",
            delivery_config_path.display()
        );
    }

    let canonical_config = detect_native_manifest_contract(target)?;
    let source_label = manifest_path.display().to_string();
    let inline_config = load_inline_delivery_config(&manifest_raw, &source_label)?;
    let has_explicit_delivery_config = inline_config.is_some();
    let config = match inline_config {
        Some(inline) => inline,
        None => match canonical_config.clone() {
            Some(config) => config,
            None => return Ok(None),
        },
    };
    if let Some(canonical) = &canonical_config {
        ensure_delivery_config_matches_context(&config, canonical, &source_label)?;
    }

    let input_relative = PathBuf::from(config.artifact.input.trim());
    validate_relative_input_path(&input_relative)?;
    let source_app_path = manifest_dir.join(&input_relative);
    let build_command = detect_native_build_command(
        target,
        manifest_dir,
        has_explicit_delivery_config || canonical_config.is_none(),
    )?;
    if build_command.is_none() {
        validate_native_bundle_directory(&source_app_path)?;
    }

    let compat_manifest = CompatManifestBridge::from_manifest_value(
        &toml::from_str(&manifest_raw)
            .with_context(|| format!("Failed to parse {} as TOML", manifest_path.display()))?,
    )?;

    Ok(Some(NativeBuildPlan {
        workspace_root: manifest_dir.to_path_buf(),
        legacy_manifest_bridge: Some(compat_manifest),
        package_name: manifest.name.clone(),
        package_version: manifest.version.clone(),
        delivery_config_path: None,
        staged_delivery_config_toml: serialize_delivery_config(&config)?,
        source_app_path,
        input_relative,
        build_command,
        framework: config.artifact.framework,
        target: config.artifact.target,
    }))
}

pub(crate) fn detect_build_strategy_from_descriptor(
    descriptor: &ExecutionDescriptor,
) -> Result<Option<NativeBuildPlan>> {
    if let Some(plan) = detect_build_strategy_from_lock_delivery(descriptor)? {
        return Ok(Some(plan));
    }

    let delivery_config_path = descriptor.workspace_root.join(DELIVERY_CONFIG_FILE);
    let Some(bridge) = descriptor.compat_manifest.as_ref() else {
        return Ok(None);
    };
    let Ok(target) = bridge.manifest_model().resolve_default_target() else {
        return Ok(None);
    };

    if delivery_config_path.exists() {
        bail!(
            "{} is no longer accepted in source projects. Move native delivery metadata into capsule.toml [artifact] and [finalize].",
            delivery_config_path.display()
        );
    }

    let canonical_config = detect_native_manifest_contract(target)?;
    let source_label = format!(
        "compatibility manifest bridge for {}",
        descriptor.workspace_root.display()
    );
    let inline_config = load_inline_delivery_config(bridge.manifest_text(), &source_label)?;
    let has_explicit_delivery_config = inline_config.is_some();
    let config = match inline_config {
        Some(inline) => inline,
        None => match canonical_config.clone() {
            Some(config) => config,
            None => return Ok(None),
        },
    };
    if let Some(canonical) = &canonical_config {
        ensure_delivery_config_matches_context(&config, canonical, &source_label)?;
    }

    let input_relative = PathBuf::from(config.artifact.input.trim());
    validate_relative_input_path(&input_relative)?;
    let source_app_path = descriptor.workspace_root.join(&input_relative);
    let build_command = detect_native_build_command(
        target,
        &descriptor.workspace_root,
        has_explicit_delivery_config || canonical_config.is_none(),
    )?;
    if build_command.is_none() {
        validate_native_bundle_directory(&source_app_path)?;
    }

    Ok(Some(NativeBuildPlan {
        workspace_root: descriptor.workspace_root.clone(),
        legacy_manifest_bridge: Some(bridge.clone()),
        package_name: descriptor
            .runtime_model
            .metadata
            .name
            .clone()
            .filter(|value| !value.trim().is_empty())
            .context("authoritative lock metadata is missing package name")?,
        package_version: descriptor
            .runtime_model
            .metadata
            .version
            .clone()
            .unwrap_or_default(),
        delivery_config_path: None,
        staged_delivery_config_toml: serialize_delivery_config(&config)?,
        source_app_path,
        input_relative,
        build_command,
        framework: config.artifact.framework,
        target: config.artifact.target,
    }))
}

fn detect_build_strategy_from_lock_delivery(
    descriptor: &ExecutionDescriptor,
) -> Result<Option<NativeBuildPlan>> {
    let Some(delivery) = descriptor
        .lock
        .contract
        .entries
        .get("delivery")
        .and_then(Value::as_object)
    else {
        return Ok(None);
    };

    let mode = delivery
        .get("mode")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if !matches!(mode, "source-draft" | "source-derivation") {
        return Ok(None);
    }

    let Some(artifact) = delivery.get("artifact").and_then(Value::as_object) else {
        return Ok(None);
    };
    if artifact.get("kind").and_then(Value::as_str) != Some("desktop-native") {
        return Ok(None);
    }

    let framework = artifact
        .get("framework")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("desktop-native delivery artifact.framework is missing")?
        .to_string();
    let target = artifact
        .get("target")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("desktop-native delivery artifact.target is missing")?
        .to_string();
    let input = artifact
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("desktop-native delivery artifact.path is missing")?
        .to_string();

    let finalize = delivery
        .get("finalize")
        .and_then(Value::as_object)
        .context("desktop-native delivery.finalize is missing")?;
    let finalize_tool = finalize
        .get("tool")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("desktop-native delivery.finalize.tool is missing")?
        .to_string();
    let finalize_args = finalize
        .get("args")
        .and_then(Value::as_array)
        .context("desktop-native delivery.finalize.args is missing")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .context("desktop-native delivery.finalize.args must contain only strings")
        })
        .collect::<Result<Vec<_>>>()?;

    let config = DeliveryConfig {
        schema_version: DELIVERY_SCHEMA_VERSION_STABLE.to_string(),
        artifact: DeliveryArtifact {
            framework: framework.clone(),
            stage: DELIVERY_STAGE.to_string(),
            target: target.clone(),
            input: input.clone(),
        },
        finalize: DeliveryFinalize {
            tool: finalize_tool,
            args: finalize_args,
        },
    };
    validate_delivery_config(&config)?;

    let input_relative = PathBuf::from(&input);
    validate_relative_input_path(&input_relative)?;
    let source_app_path = descriptor.workspace_root.join(&input_relative);
    let build_command = delivery
        .get("build")
        .and_then(Value::as_object)
        .and_then(|build| build.get("build_command"))
        .and_then(Value::as_object)
        .map(|command| parse_lock_native_build_command(command, &descriptor.workspace_root))
        .transpose()?;
    if build_command.is_none() {
        validate_native_bundle_directory(&source_app_path)?;
    }

    Ok(Some(NativeBuildPlan {
        workspace_root: descriptor.workspace_root.clone(),
        legacy_manifest_bridge: descriptor.compat_manifest.clone(),
        package_name: descriptor
            .runtime_model
            .metadata
            .name
            .clone()
            .filter(|value| !value.trim().is_empty())
            .context("authoritative lock metadata is missing package name")?,
        package_version: descriptor
            .runtime_model
            .metadata
            .version
            .clone()
            .unwrap_or_default(),
        delivery_config_path: None,
        staged_delivery_config_toml: serialize_delivery_config(&config)?,
        source_app_path,
        input_relative,
        build_command,
        framework,
        target,
    }))
}

fn parse_lock_native_build_command(
    command: &serde_json::Map<String, Value>,
    workspace_root: &Path,
) -> Result<NativeBuildCommand> {
    let program = command
        .get("program")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("desktop-native delivery.build.build_command.program is missing")?
        .to_string();
    let args = command
        .get("args")
        .and_then(Value::as_array)
        .context("desktop-native delivery.build.build_command.args is missing")?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).context(
                "desktop-native delivery.build.build_command.args must contain only strings",
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let working_dir_raw = command
        .get("working_dir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("desktop-native delivery.build.build_command.working_dir is missing")?;
    let working_dir_path = PathBuf::from(working_dir_raw);
    let working_dir = if working_dir_path.is_absolute() {
        working_dir_path
    } else {
        workspace_root.join(working_dir_path)
    };

    Ok(NativeBuildCommand {
        program,
        args,
        working_dir,
    })
}

pub(crate) fn detect_build_strategy_with_legacy_fallback(
    descriptor: &ExecutionDescriptor,
) -> Result<Option<NativeBuildPlan>> {
    if descriptor.compat_manifest.is_some() {
        return detect_build_strategy_from_descriptor(descriptor);
    }

    detect_build_strategy(&descriptor.workspace_root)
}

pub(crate) fn build_native_artifact(
    plan: &NativeBuildPlan,
    output_path: Option<&Path>,
) -> Result<NativeBuildResult> {
    build_native_artifact_with_distribution_lock(plan, output_path, None)
}

pub(crate) fn build_native_artifact_with_distribution_lock(
    plan: &NativeBuildPlan,
    output_path: Option<&Path>,
    capsule_lock_json: Option<&str>,
) -> Result<NativeBuildResult> {
    ensure_current_host_delivery_target(&plan.target, "native delivery build")?;

    let config = staged_delivery_config(plan)?;
    let runner = FinalizeRunner::for_tool(&config.finalize.tool);
    build_native_artifact_with_strip(
        plan,
        output_path,
        |artifact_path| {
            if host_supports_finalize() {
                runner.strip_existing_signature(artifact_path)
            } else {
                Ok(())
            }
        },
        capsule_lock_json,
    )
}

fn build_native_artifact_with_strip<F>(
    plan: &NativeBuildPlan,
    output_path: Option<&Path>,
    strip_signature: F,
    capsule_lock_json: Option<&str>,
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
    let manifest = plan
        .legacy_manifest_bridge
        .as_ref()
        .map(|bridge| bridge.manifest_model().clone())
        .context("native delivery build requires legacy manifest bridge")?;

    let artifact_path = output_path.map(Path::to_path_buf).unwrap_or_else(|| {
        default_native_artifact_path(
            &plan.workspace_root,
            &plan.package_name,
            &plan.package_version,
        )
    });
    if let Some(parent) = artifact_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    validate_minimal_native_artifact_permissions(&plan.source_app_path)?;

    let tmp_root = plan.workspace_root.join(".tmp");
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
        let capsule_bytes =
            build_capsule_archive(&manifest, &payload_tar_zst, &payload_tar, capsule_lock_json)?;
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

pub(crate) fn finalize_capsule_artifact_for_publish(
    unsigned_artifact_path: &Path,
    scoped_id: &str,
    version: &str,
    source_lock_json: Option<&str>,
    allow_external_finalize: bool,
) -> Result<FinalizedPublishArtifactResult> {
    let artifact_bytes = fs::read(unsigned_artifact_path).with_context(|| {
        format!(
            "Failed to read unsigned artifact for finalize: {}",
            unsigned_artifact_path.display()
        )
    })?;
    let fetch_result = materialize_fetch_cache_from_artifact(
        scoped_id,
        version,
        "local-source-publish",
        &artifact_bytes,
    )?;
    let output_root = unsigned_artifact_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let finalize = execute_finalize(
        &fetch_result.cache_dir,
        &output_root,
        allow_external_finalize,
    )?;

    let delivery_config_path = fetch_result.artifact_dir.join(DELIVERY_CONFIG_FILE);
    let delivery_config = load_delivery_config(&delivery_config_path)?;
    let rebased_delivery =
        rebase_delivery_config_for_finalize(&delivery_config, &finalize.derived_app_path)?;

    let staging_root = create_temp_subdir(&output_root, "native-publish-finalize")?;
    let payload_root = staging_root.join("payload");
    fs::create_dir_all(&payload_root)
        .with_context(|| format!("Failed to create {}", payload_root.display()))?;

    let artifact_name = finalize
        .derived_app_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("finalized app path has no terminal name"))?;
    let staged_artifact = payload_root.join(artifact_name);
    copy_recursively(&finalize.derived_app_path, &staged_artifact)?;
    fs::write(
        payload_root.join(DELIVERY_CONFIG_FILE),
        serialize_delivery_config(&rebased_delivery)?,
    )
    .context("Failed to stage finalized native delivery metadata")?;
    fs::copy(
        &finalize.provenance_path,
        payload_root.join(PROVENANCE_FILE),
    )
    .with_context(|| {
        format!(
            "Failed to stage finalized provenance from {}",
            finalize.provenance_path.display()
        )
    })?;

    let payload_tar = create_payload_tar_from_directory(&payload_root)?;
    let payload_tar_zst = zstd::stream::encode_all(Cursor::new(&payload_tar), 3)
        .context("Failed to encode finalized native payload.tar.zst")?;
    let manifest = extract_capsule_manifest_from_archive(&artifact_bytes)?;
    let finalized_name = unsigned_artifact_path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(|value| format!("{value}-signed.capsule"))
        .unwrap_or_else(|| format!("{}-{}-signed.capsule", manifest.name, manifest.version));
    let finalized_artifact_path = unsigned_artifact_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(finalized_name);
    let capsule_bytes =
        build_capsule_archive(&manifest, &payload_tar_zst, &payload_tar, source_lock_json)?;
    fs::write(&finalized_artifact_path, capsule_bytes).with_context(|| {
        format!(
            "Failed to write finalized publish artifact: {}",
            finalized_artifact_path.display()
        )
    })?;
    let _ = fs::remove_dir_all(&staging_root);

    Ok(FinalizedPublishArtifactResult {
        artifact_path: finalized_artifact_path,
        identity_class: "locally_finalized_signed_bundle",
        finalize,
    })
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
    let registry = crate::registry::url::resolve_normalized_registry_url(
        resolved.registry_url.as_deref(),
        "registry",
        "resolved registry",
    )
    .await?;
    let client = reqwest::Client::new();
    let detail = install::fetch_capsule_detail_record(&client, &registry, &scoped_ref).await?;
    let target_version = install::select_requested_or_latest_version(
        requested_version.as_deref(),
        detail.latest_version.as_deref(),
        &scoped_ref.scoped_id,
        "fetchable",
    )?;
    install::ensure_release_exists(&detail.releases, &target_version)?;
    let artifact_bytes =
        install::download_capsule_artifact_bytes(&client, &registry, &scoped_ref, &target_version)
            .await?;

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
            if normalized_registry_url_for_compare(left)
                != normalized_registry_url_for_compare(right) =>
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

fn normalized_registry_url_for_compare(input: &str) -> String {
    http::normalize_registry_url(input, "registry")
        .unwrap_or_else(|_| input.trim().trim_end_matches('/').to_ascii_lowercase())
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

pub(crate) fn native_delivery_build_environment_skeleton(plan: &NativeBuildPlan) -> Value {
    let package_managers = detect_build_environment_package_managers(&plan.workspace_root);
    let mut toolchains = Vec::new();
    let mut sdks = Vec::new();
    let mut helper_tools = Vec::new();

    match plan.framework.as_str() {
        "tauri" => {
            push_unique(&mut toolchains, "rust");
            push_unique(&mut toolchains, "cargo");
            push_unique(&mut helper_tools, "tauri-cli");
        }
        framework if !framework.trim().is_empty() => {
            push_unique(&mut helper_tools, framework);
        }
        _ => {}
    }

    if package_managers
        .iter()
        .any(|manager| matches!(manager.as_str(), "npm" | "pnpm" | "yarn" | "bun"))
    {
        push_unique(&mut toolchains, "node");
    }
    if package_managers.iter().any(|manager| manager == "deno") {
        push_unique(&mut toolchains, "deno");
    }
    if package_managers.iter().any(|manager| manager == "cargo") {
        push_unique(&mut toolchains, "rust");
        push_unique(&mut toolchains, "cargo");
    }
    if package_managers.iter().any(|manager| manager == "go") {
        push_unique(&mut toolchains, "go");
    }

    match delivery_target_os_family(&plan.target) {
        Some("darwin") => push_unique(&mut sdks, "apple-sdk"),
        Some("windows") => push_unique(&mut sdks, "windows-sdk"),
        Some("linux") => push_unique(&mut sdks, "linux-system-libs"),
        _ => {}
    }

    if let Ok(config) = staged_delivery_config(plan) {
        let boundary = finalize_helper_boundary(config.finalize.tool.trim());
        push_unique(&mut helper_tools, &boundary.subject_name);
    }

    json!({
        "toolchains": toolchains,
        "package_managers": package_managers,
        "sdks": sdks,
        "helper_tools": helper_tools,
    })
}

pub(crate) fn finalize_helper_boundary(tool: &str) -> BootstrapBoundary {
    BootstrapBoundary::finalize_helper(tool.trim())
}

pub(crate) fn native_delivery_contract_from_build_plan(
    plan: &NativeBuildPlan,
    mode: &str,
    closure_status: &str,
) -> Result<Value> {
    let config = staged_delivery_config(plan)?;
    Ok(json!({
        "mode": mode,
        "artifact": {
            "kind": "desktop-native",
            "framework": config.artifact.framework,
            "target": config.artifact.target,
            "path": config.artifact.input,
            "canonical_build_input": false,
            "provenance_limited": false,
            "reproducibility": match closure_status {
                "complete" => "closure-tracked-build",
                _ => "closure-incomplete-draft",
            },
        },
        "build": {
            "kind": "native-delivery",
            "requires_build_closure": true,
            "closure_status": closure_status,
            "build_command": plan.build_command.as_ref().map(|command| {
                json!({
                    "program": command.program,
                    "args": command.args,
                    "working_dir": command.working_dir,
                })
            }),
            "build_environment": native_delivery_build_environment_skeleton(plan),
        },
        "finalize": {
            "tool": config.finalize.tool,
            "args": config.finalize.args,
            "host_local": true,
        },
        "install": {
            "kind": "local-derivation",
            "host_local": true,
            "requires_local_derivation": true,
        },
        "projection": {
            "kind": "launcher-surface",
            "host_local": true,
        },
    }))
}

pub(crate) fn native_delivery_draft_contract_from_manifest(
    manifest_path: &Path,
) -> Result<Option<Value>> {
    let manifest_raw = fs::read_to_string(manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest = capsule_core::types::CapsuleManifest::from_toml(&manifest_raw)
        .map_err(|err| anyhow::anyhow!("Failed to parse {}: {}", manifest_path.display(), err))?;
    let target = match manifest.resolve_default_target() {
        Ok(target) if target.driver.as_deref() == Some("native") => target,
        _ => return Ok(None),
    };

    let parsed: toml::Value = toml::from_str(&manifest_raw)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
    let artifact = parsed.get("artifact");
    let finalize = parsed.get("finalize");
    let canonical = detect_native_manifest_contract(target)?;

    let mut artifact_value = serde_json::Map::new();
    artifact_value.insert(
        "kind".to_string(),
        Value::String("desktop-native".to_string()),
    );
    artifact_value.insert("canonical_build_input".to_string(), Value::Bool(false));
    artifact_value.insert("provenance_limited".to_string(), Value::Bool(false));
    artifact_value.insert(
        "reproducibility".to_string(),
        Value::String("closure-incomplete-draft".to_string()),
    );

    let framework = artifact
        .and_then(|table| table.get("framework"))
        .and_then(toml::Value::as_str)
        .or_else(|| {
            canonical
                .as_ref()
                .map(|config| config.artifact.framework.as_str())
        });
    if let Some(framework) = framework {
        artifact_value.insert(
            "framework".to_string(),
            Value::String(framework.to_string()),
        );
    }

    let delivery_target = artifact
        .and_then(|table| table.get("target"))
        .and_then(toml::Value::as_str)
        .or_else(|| {
            canonical
                .as_ref()
                .map(|config| config.artifact.target.as_str())
        });
    if let Some(delivery_target) = delivery_target {
        artifact_value.insert(
            "target".to_string(),
            Value::String(delivery_target.to_string()),
        );
    }

    let artifact_path = artifact
        .and_then(|table| table.get("input"))
        .and_then(toml::Value::as_str)
        .or_else(|| {
            canonical
                .as_ref()
                .map(|config| config.artifact.input.as_str())
        });
    if let Some(artifact_path) = artifact_path {
        artifact_value.insert("path".to_string(), Value::String(artifact_path.to_string()));
    }

    let mut build = serde_json::Map::new();
    build.insert(
        "kind".to_string(),
        Value::String("native-delivery".to_string()),
    );
    build.insert("requires_build_closure".to_string(), Value::Bool(true));
    build.insert(
        "closure_status".to_string(),
        Value::String("incomplete".to_string()),
    );
    build.insert(
        "program".to_string(),
        Value::String(target.entrypoint.trim().to_string()),
    );
    if !target.cmd.is_empty() {
        build.insert(
            "args".to_string(),
            Value::Array(target.cmd.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(working_dir) = target.working_dir.as_ref() {
        build.insert(
            "working_dir".to_string(),
            Value::String(working_dir.clone()),
        );
    }

    let finalize_value = finalize
        .map(toml_to_json)
        .or_else(|| {
            canonical.as_ref().map(|config| {
                json!({
                    "tool": config.finalize.tool,
                    "args": config.finalize.args,
                })
            })
        })
        .map(|mut value| {
            if let Some(object) = value.as_object_mut() {
                object.insert("host_local".to_string(), Value::Bool(true));
            }
            value
        })
        .unwrap_or_else(|| json!({ "host_local": true }));

    Ok(Some(json!({
        "mode": "source-draft",
        "artifact": Value::Object(artifact_value),
        "build": Value::Object(build),
        "finalize": finalize_value,
        "install": {
            "kind": "local-derivation",
            "host_local": true,
            "requires_local_derivation": true,
        },
        "projection": {
            "kind": "launcher-surface",
            "host_local": true,
        },
    })))
}

pub(crate) fn imported_native_artifact_delivery_contract(
    artifact_path: &Path,
    artifact_type: &str,
) -> Value {
    json!({
        "mode": "artifact-import",
        "artifact": {
            "kind": "desktop-native",
            "artifact_type": artifact_type,
            "path": artifact_path,
            "canonical_build_input": false,
            "provenance_limited": true,
            "reproducibility": "imported-artifact-no-reproducibility-claim",
        },
        "install": {
            "kind": "local-derivation",
            "host_local": true,
            "requires_local_derivation": true,
        },
        "projection": {
            "kind": "launcher-surface",
            "host_local": true,
        },
    })
}

pub(crate) fn imported_native_artifact_closure(
    artifact_path: &Path,
    artifact_type: &str,
) -> Result<Value> {
    Ok(json!({
        "kind": "imported_artifact_closure",
        "status": "complete",
        "artifact": {
            "artifact_type": artifact_type,
            "digest": compute_tree_digest(artifact_path)?,
            "provenance_limited": true,
        }
    }))
}

pub(crate) fn imported_native_artifact_type(artifact_path: &Path) -> Option<&'static str> {
    if artifact_path.is_dir() && path_has_extension(artifact_path, "app") {
        return Some("macos_app_bundle");
    }
    if artifact_path.is_file() && path_has_extension(artifact_path, "exe") {
        return Some("windows_executable");
    }
    if artifact_path.is_file() && path_has_extension(artifact_path, "AppImage") {
        return Some("appimage");
    }
    None
}

fn toml_to_json(value: &toml::Value) -> Value {
    serde_json::to_value(value).expect("toml value should serialize into json")
}

fn detect_build_environment_package_managers(manifest_dir: &Path) -> Vec<String> {
    let mut package_managers = Vec::new();
    let candidates = [
        ("Cargo.lock", "cargo"),
        ("go.sum", "go"),
        ("package-lock.json", "npm"),
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lockb", "bun"),
        ("bun.lock", "bun"),
        ("deno.lock", "deno"),
    ];

    for (file_name, label) in candidates {
        if manifest_dir.join(file_name).exists() {
            push_unique(&mut package_managers, label);
        }
    }

    package_managers
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() || values.iter().any(|existing| existing == trimmed) {
        return;
    }
    values.push(trimmed.to_string());
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

fn ensure_delivery_config_matches_context(
    actual: &DeliveryConfig,
    expected: &DeliveryConfig,
    source_label: &str,
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
            source_label
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

include!("projection.rs");

fn finalize_with_dispatch(fetched_dir: &Path, output_dir: &Path) -> Result<FinalizeResult> {
    let derived_plan = DerivedFinalizePlan {
        fetched_dir: fetched_dir.to_path_buf(),
        output_dir: output_dir.to_path_buf(),
    };
    finalize_with_runner_plan(&derived_plan, |derived_dir, config| {
        FinalizeRunner::for_tool(&config.finalize.tool).run(derived_dir, config)
    })
}

#[allow(dead_code)]
fn finalize_with_runner<F>(
    fetched_dir: &Path,
    output_dir: &Path,
    runner: F,
) -> Result<FinalizeResult>
where
    F: Fn(&Path, &DeliveryConfig) -> Result<()>,
{
    let derived_plan = DerivedFinalizePlan {
        fetched_dir: fetched_dir.to_path_buf(),
        output_dir: output_dir.to_path_buf(),
    };
    finalize_with_runner_plan(&derived_plan, runner)
}

fn finalize_with_runner_plan<F>(
    derived_plan: &DerivedFinalizePlan,
    runner: F,
) -> Result<FinalizeResult>
where
    F: Fn(&Path, &DeliveryConfig) -> Result<()>,
{
    let metadata = load_fetch_metadata(&derived_plan.fetched_dir)?;
    let artifact_root = derived_plan.fetched_dir.join(FETCH_ARTIFACT_DIR);
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

    fs::create_dir_all(&derived_plan.output_dir).with_context(|| {
        format!(
            "Failed to create output directory: {}",
            derived_plan.output_dir.display()
        )
    })?;
    let derived_dir = create_unique_output_dir(&derived_plan.output_dir)?;
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
            fetched_dir: derived_plan.fetched_dir.clone(),
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
    source_label: &str,
) -> Result<Option<DeliveryConfig>> {
    let parsed: toml::Value = toml::from_str(manifest_raw)
        .with_context(|| format!("Failed to parse {}", source_label))?;
    let artifact = parsed.get("artifact").cloned();
    let finalize = parsed.get("finalize").cloned();

    match (artifact, finalize) {
        (None, None) => Ok(None),
        (Some(_), None) => bail!(
            "{} defines [artifact] without [finalize] for native delivery",
            source_label
        ),
        (None, Some(_)) => bail!(
            "{} defines [finalize] without [artifact] for native delivery",
            source_label
        ),
        (Some(artifact), Some(finalize)) => {
            let config = DeliveryConfig {
                schema_version: DELIVERY_SCHEMA_VERSION_STABLE.to_string(),
                artifact: artifact
                    .try_into()
                    .with_context(|| format!("Failed to parse [artifact] from {}", source_label))?,
                finalize: finalize
                    .try_into()
                    .with_context(|| format!("Failed to parse [finalize] from {}", source_label))?,
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

pub(crate) fn ensure_current_host_delivery_target(target: &str, action: &str) -> Result<()> {
    let Some(target_os) = delivery_target_os_family(target) else {
        bail!(
            "{} requires a normalized delivery target (got '{}')",
            action,
            target
        );
    };
    let Some(host_os) = host_projection_os_family() else {
        bail!("{} is not supported on this host platform", action);
    };
    if target_os != host_os {
        bail!(
            "{} requires current-host target alignment (target='{}', host_os='{}')",
            action,
            target,
            host_os
        );
    }

    let target_arch = target
        .split('/')
        .nth(1)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    if !target_arch.is_empty() {
        let host_arch = normalized_host_delivery_arch();
        if !delivery_arch_matches_host(target_arch, host_arch) {
            bail!(
                "{} requires current-host arch alignment (target='{}', host_arch='{}')",
                action,
                target_arch,
                host_arch
            );
        }
    }

    Ok(())
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

fn normalized_host_delivery_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" | "i386" | "i586" | "i686" => "x86",
        other => other,
    }
}

fn delivery_arch_matches_host(target_arch: &str, host_arch: &str) -> bool {
    let normalized_target = match target_arch {
        "x86_64" | "amd64" | "x64" => "x64",
        "aarch64" | "arm64" => "arm64",
        "x86" | "ia32" | "i386" | "i686" => "x86",
        other => other,
    };
    normalized_target == host_arch
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

fn extract_capsule_manifest_from_archive(
    bytes: &[u8],
) -> Result<capsule_core::types::CapsuleManifest> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read finalized source artifact")?;
    for entry in entries {
        let mut entry = entry.context("Invalid finalized source artifact entry")?;
        if entry
            .path()
            .ok()
            .and_then(|path| path.to_str().map(|value| value.to_string()))
            .as_deref()
            != Some("capsule.toml")
        {
            continue;
        }
        let mut manifest = String::new();
        entry
            .read_to_string(&mut manifest)
            .context("Failed to read capsule.toml from source artifact")?;
        return capsule_core::types::CapsuleManifest::from_toml(&manifest).map_err(|err| {
            anyhow::anyhow!("Failed to parse capsule.toml from source artifact: {err}")
        });
    }
    bail!("source artifact is missing capsule.toml")
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
        .current_dir(&command.working_dir);
    let output = run_captured_command(&mut process, || {
        format!(
            "Failed to execute native delivery build command '{}' in {}",
            format_native_build_command(command),
            command.working_dir.display()
        )
    })?;
    if output.status.success() {
        return Ok(());
    }

    let details = command_output_details(&output);
    bail!(
        "Native delivery build command failed with status {}: {}{}",
        command_exit_status(&output),
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
    command.args(&config.finalize.args).current_dir(derived_dir);
    let output = run_captured_command(&mut command, || {
        format!("Failed to execute {} in {}", tool, derived_dir.display())
    })?;
    if output.status.success() {
        return Ok(());
    }
    let details = command_output_details(&output);
    bail!(
        "{} failed with status {}{}",
        tool,
        command_exit_status(&output),
        if details.is_empty() {
            String::new()
        } else {
            format!(": {}", details)
        },
    )
}

fn strip_codesign_signature(tool: &str, app_path: &Path) -> Result<()> {
    let mut command = Command::new(tool);
    command.arg("--remove-signature").arg(app_path);
    let output = run_captured_command(&mut command, || {
        format!("Failed to execute {} for {}", tool, app_path.display())
    })?;
    if output.status.success() {
        return Ok(());
    }

    let details = command_output_details(&output);
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

fn run_captured_command(
    command: &mut Command,
    context: impl FnOnce() -> String,
) -> Result<std::process::Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(context)
}

fn command_output_details(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        return stderr.trim().to_string();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.trim().to_string()
}

fn command_exit_status(output: &std::process::Output) -> String {
    output
        .status
        .code()
        .map(|value| value.to_string())
        .unwrap_or_else(|| "signal".to_string())
}

#[cfg(test)]
mod tests;

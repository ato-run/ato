use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::ato_lock::AtoLock;
use crate::lock_runtime::{self, LockContractMetadata, LockServiceUnit, ResolvedLockRuntimeModel};
use crate::manifest;
use crate::orchestration;
use crate::types::{
    CapsuleManifest, ExternalInjectionSpec, Mount, NamedTarget, OrchestrationPlan, ReadinessProbe,
    ResolvedService, ResolvedServiceNetwork, ResolvedServiceRuntime, ResolvedTargetRuntime,
    ServiceConnectionInfo, ServiceSpec, ValidationMode,
};

mod services;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    Oci,
    Wasm,
    Source,
    Web,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionProfile {
    Dev,
    Release,
}

#[derive(Debug, Clone)]
pub struct CompatManifestBridge {
    pub(crate) raw_toml: String,
    pub(crate) manifest: CapsuleManifest,
    pub(crate) sha256: String,
}

impl CompatManifestBridge {
    pub fn from_normalized_toml(raw_toml: String) -> Result<Self> {
        let parsed =
            CapsuleManifest::from_toml(&raw_toml).map_err(|err| anyhow!(err.to_string()))?;
        Ok(Self {
            raw_toml: raw_toml.clone(),
            manifest: parsed,
            sha256: sha256_hex(raw_toml.as_bytes()),
        })
    }

    pub fn toml_value(&self) -> Result<toml::Value> {
        toml::from_str(&self.raw_toml).map_err(|err| anyhow!("parse compat manifest bridge: {err}"))
    }

    /// Build a bridge where the manifest model and the compat raw TOML come from separate sources.
    /// Use this when the normalized compat TOML (with `[targets]`) cannot be re-parsed via the
    /// normal `CapsuleManifest::from_toml` path (e.g. it contains v0.2-style `entrypoint` fields
    /// that the v0.3 validator would reject).
    pub fn from_compat_normalized(manifest: CapsuleManifest, compat_toml: String) -> Self {
        Self {
            sha256: sha256_hex(compat_toml.as_bytes()),
            raw_toml: compat_toml,
            manifest,
        }
    }

    pub fn from_manifest_value(manifest: &toml::Value) -> Result<Self> {
        // Detect flat v0.3 manifests that need normalization before direct serde
        // deserialization (e.g. `build = "cmd"` string, `runtime = "source/native"`
        // composite selectors, etc.). A flat v0.3 manifest has schema_version=0.3
        // and no [targets] table yet.
        let is_flat_v03 = crate::types::is_v03_like_schema(manifest)
            && manifest.get("targets").and_then(|v| v.as_table()).is_none();

        if is_flat_v03 {
            let raw_toml = toml::to_string(manifest)
                .map_err(|err| anyhow!("serialize manifest bridge: {err}"))?;
            let compat_toml = CapsuleManifest::normalize_to_compat_toml(&raw_toml)
                .map_err(|err| anyhow!(err.to_string()))?;
            let parsed = toml::from_str::<CapsuleManifest>(&compat_toml)
                .map_err(|err| anyhow!("parse compat manifest bridge: {err}"))?;
            return Ok(Self {
                raw_toml: compat_toml.clone(),
                manifest: parsed,
                sha256: sha256_hex(compat_toml.as_bytes()),
            });
        }

        let raw_toml =
            toml::to_string(manifest).map_err(|err| anyhow!("serialize manifest bridge: {err}"))?;
        // Use serde deserialization directly — the value is already a structured manifest
        // (possibly with v0.2-style `entrypoint` in targets). Calling CapsuleManifest::from_toml
        // would re-run normalization and reject those legacy fields.
        let parsed = toml::from_str::<CapsuleManifest>(&raw_toml)
            .map_err(|err| anyhow!("parse compat manifest bridge: {err}"))?;
        Ok(Self {
            raw_toml: raw_toml.clone(),
            manifest: parsed,
            sha256: sha256_hex(raw_toml.as_bytes()),
        })
    }

    pub fn from_lock(lock: &AtoLock, runtime_model: &ResolvedLockRuntimeModel) -> Result<Self> {
        Self::from_manifest_value(&synthesize_manifest_from_lock(lock, runtime_model))
    }

    pub fn manifest_text(&self) -> &str {
        self.raw_toml.as_str()
    }

    pub fn manifest_model(&self) -> &CapsuleManifest {
        &self.manifest
    }

    pub fn manifest_sha256(&self) -> &str {
        self.sha256.as_str()
    }

    pub fn raw_value(&self) -> Result<toml::Value> {
        self.toml_value()
    }

    pub fn package_name(&self) -> &str {
        self.manifest.name.as_str()
    }

    pub fn package_version(&self) -> &str {
        self.manifest.version.as_str()
    }

    pub fn repository(&self) -> Option<String> {
        self.toml_value().ok().and_then(|parsed| {
            parsed
                .get("metadata")
                .and_then(|value| value.get("repository"))
                .and_then(|value| value.as_str())
                .or_else(|| parsed.get("repository").and_then(|value| value.as_str()))
                .map(str::to_string)
        })
    }

    pub fn store_playground_enabled(&self) -> bool {
        self.toml_value()
            .ok()
            .and_then(|parsed| {
                parsed
                    .get("store")
                    .and_then(|value| value.get("playground"))
                    .and_then(|value| value.as_bool())
            })
            .unwrap_or(false)
    }

    pub fn is_schema_v03(&self) -> bool {
        self.manifest.schema_version.trim() == "0.3"
    }

    pub fn ipc_section(&self) -> Result<Option<toml::Value>> {
        Ok(self.toml_value()?.get("ipc").cloned())
    }

    pub fn publish_registry(&self) -> Option<String> {
        self.toml_value().ok().and_then(|parsed| {
            parsed
                .get("store")
                .and_then(|value| value.get("registry"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
    }
}

#[derive(Debug, Clone)]
pub struct CompatProjectInput {
    workspace_root: PathBuf,
    logical_source_label: String,
    manifest_value: toml::Value,
    bridge: CompatManifestBridge,
}

impl CompatProjectInput {
    pub fn from_bridge(workspace_root: PathBuf, bridge: CompatManifestBridge) -> Result<Self> {
        let logical_source_label = format!(
            "compatibility manifest bridge for {}",
            workspace_root.display()
        );
        Self::from_bridge_with_label(workspace_root, logical_source_label, bridge)
    }

    pub fn from_bridge_with_label(
        workspace_root: PathBuf,
        logical_source_label: impl Into<String>,
        bridge: CompatManifestBridge,
    ) -> Result<Self> {
        let manifest_value = bridge.toml_value()?;
        Ok(Self {
            workspace_root,
            logical_source_label: logical_source_label.into(),
            manifest_value,
            bridge,
        })
    }

    pub fn workspace_root(&self) -> &Path {
        self.workspace_root.as_path()
    }

    pub fn logical_source_label(&self) -> &str {
        self.logical_source_label.as_str()
    }

    pub fn manifest_value(&self) -> &toml::Value {
        &self.manifest_value
    }

    pub fn manifest_text(&self) -> &str {
        self.bridge.manifest_text()
    }

    pub fn manifest(&self) -> &CapsuleManifest {
        self.bridge.manifest_model()
    }

    pub fn package_name(&self) -> &str {
        self.bridge.package_name()
    }

    pub fn package_version(&self) -> &str {
        self.bridge.package_version()
    }

    pub fn sha256(&self) -> &str {
        self.bridge.manifest_sha256()
    }

    pub fn ipc_section(&self) -> Result<Option<toml::Value>> {
        self.bridge.ipc_section()
    }

    pub fn publish_registry(&self) -> Option<String> {
        self.bridge.publish_registry()
    }

    pub fn source_digest(&self) -> Option<&str> {
        self.bridge
            .manifest_model()
            .targets
            .as_ref()
            .and_then(|targets| targets.source_digest.as_deref())
    }

    pub fn engine_nacelle_path(&self) -> Option<PathBuf> {
        self.manifest_value
            .get("engine")
            .and_then(|table| table.get("nacelle_path"))
            .and_then(|value| value.as_str())
            .map(PathBuf::from)
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    self.workspace_root.join(path)
                }
            })
    }

    pub fn engine_source_alias(&self) -> Option<&str> {
        self.manifest_value
            .get("engine")
            .and_then(|table| table.get("source"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionDescriptor {
    // Transitional compatibility surface retained for run/install-oriented APIs.
    pub manifest: toml::Value,
    pub compat_manifest: Option<CompatManifestBridge>,
    // Transitional source-location metadata; build/publish semantic authority must not depend on these.
    pub manifest_path: PathBuf,
    pub manifest_dir: PathBuf,
    pub lock: AtoLock,
    pub lock_path: PathBuf,
    pub workspace_root: PathBuf,
    pub profile: ExecutionProfile,
    pub selected_target: String,
    pub runtime_model: ResolvedLockRuntimeModel,
    pub state_source_overrides: HashMap<String, String>,
}

pub type ManifestData = ExecutionDescriptor;

#[derive(Debug, Clone)]
pub struct RuntimeDecision {
    pub kind: RuntimeKind,
    pub reason: String,
    pub plan: ExecutionDescriptor,
}

pub fn route_manifest(
    manifest_path: &Path,
    profile: ExecutionProfile,
    target_label: Option<&str>,
) -> Result<RuntimeDecision> {
    route_manifest_with_validation_mode(
        manifest_path,
        profile,
        target_label,
        ValidationMode::Strict,
    )
}

pub fn route_manifest_with_state_overrides(
    manifest_path: &Path,
    profile: ExecutionProfile,
    target_label: Option<&str>,
    state_source_overrides: HashMap<String, String>,
) -> Result<RuntimeDecision> {
    route_manifest_with_state_overrides_and_validation_mode(
        manifest_path,
        profile,
        target_label,
        state_source_overrides,
        ValidationMode::Strict,
    )
}

pub fn route_manifest_with_validation_mode(
    manifest_path: &Path,
    profile: ExecutionProfile,
    target_label: Option<&str>,
    validation_mode: ValidationMode,
) -> Result<RuntimeDecision> {
    route_manifest_with_state_overrides_and_validation_mode(
        manifest_path,
        profile,
        target_label,
        HashMap::new(),
        validation_mode,
    )
}

pub fn route_manifest_with_state_overrides_and_validation_mode(
    manifest_path: &Path,
    profile: ExecutionProfile,
    target_label: Option<&str>,
    state_source_overrides: HashMap<String, String>,
    validation_mode: ValidationMode,
) -> Result<RuntimeDecision> {
    let loaded = manifest::load_manifest_with_validation_mode(manifest_path, validation_mode)?;
    let plan = execution_descriptor_from_manifest_parts(
        loaded.raw,
        loaded.path,
        loaded.dir,
        profile,
        target_label,
        state_source_overrides,
    )?;

    let runtime = plan.execution_runtime().ok_or_else(|| {
        anyhow!(
            "Target '{}' is missing required field: runtime",
            plan.selected_target
        )
    })?;

    let chosen = parse_runtime_kind(&runtime).ok_or_else(|| {
        anyhow!(
            "Unsupported runtime '{}' for target '{}'",
            runtime,
            plan.selected_target
        )
    })?;

    let reason = format!(
        "targets.{}.runtime={}",
        plan.selected_target,
        runtime.to_ascii_lowercase()
    );

    debug!(
        "RuntimeRouter: chosen={:?}, reason={}, target={}",
        chosen, reason, plan.selected_target
    );

    Ok(RuntimeDecision {
        kind: chosen,
        reason,
        plan,
    })
}

pub fn execution_descriptor_from_manifest_parts(
    manifest: toml::Value,
    manifest_path: PathBuf,
    workspace_root: PathBuf,
    profile: ExecutionProfile,
    target_label: Option<&str>,
    state_source_overrides: HashMap<String, String>,
) -> Result<ExecutionDescriptor> {
    // Detect v0.3-native manifests: schema_version == "0.3", or flat v0.3
    // fields present without a [targets] table (e.g. previewToml from the
    // ato.run API).
    let has_targets = manifest.get("targets").and_then(|v| v.as_table()).is_some();
    let is_v03_native = !has_targets
        && (manifest
            .get("schema_version")
            .and_then(|v| v.as_str())
            .map(|v| v.trim() == "0.3")
            .unwrap_or(false)
            || manifest.get("run").is_some()
            || manifest.get("packages").is_some());

    let (selected_target, runtime_model) = if is_v03_native {
        let target = resolve_v03_target(&manifest, target_label)?;
        let model = synthesize_runtime_model_from_v03(&manifest, &target)?;
        (target, model)
    } else {
        let target = resolve_target_label(&manifest, target_label)?;
        let model = synthesize_runtime_model_from_manifest(&manifest, &target)?;
        (target, model)
    };

    Ok(ExecutionDescriptor {
        manifest: manifest.clone(),
        compat_manifest: Some(CompatManifestBridge::from_manifest_value(&manifest)?),
        manifest_path,
        manifest_dir: workspace_root.clone(),
        lock: AtoLock::default(),
        lock_path: PathBuf::new(),
        workspace_root,
        profile,
        selected_target,
        runtime_model,
        state_source_overrides,
    })
}

pub fn route_lock(
    lock_path: &Path,
    lock: &AtoLock,
    workspace_root: &Path,
    profile: ExecutionProfile,
    target_label: Option<&str>,
) -> Result<RuntimeDecision> {
    route_lock_with_state_overrides(
        lock_path,
        lock,
        workspace_root,
        profile,
        target_label,
        HashMap::new(),
    )
}

pub fn route_lock_with_state_overrides(
    lock_path: &Path,
    lock: &AtoLock,
    workspace_root: &Path,
    profile: ExecutionProfile,
    target_label: Option<&str>,
    state_source_overrides: HashMap<String, String>,
) -> Result<RuntimeDecision> {
    let runtime_model = lock_runtime::resolve_lock_runtime_model(lock, target_label)
        .map_err(|err| anyhow!(err.to_string()))?;
    let runtime = runtime_model.selected.runtime.runtime.clone();
    let chosen = parse_runtime_kind(&runtime).ok_or_else(|| {
        anyhow!(
            "Unsupported runtime '{}' for target '{}'",
            runtime,
            runtime_model.selected.target_label
        )
    })?;
    let compat_manifest = CompatManifestBridge::from_lock(lock, &runtime_model)?;
    let manifest = compat_manifest.raw_value()?;
    let plan = ExecutionDescriptor {
        manifest,
        compat_manifest: Some(compat_manifest),
        manifest_path: lock_path.to_path_buf(),
        manifest_dir: workspace_root.to_path_buf(),
        lock: lock.clone(),
        lock_path: lock_path.to_path_buf(),
        workspace_root: workspace_root.to_path_buf(),
        profile,
        selected_target: runtime_model.selected.target_label.clone(),
        runtime_model,
        state_source_overrides,
    };
    Ok(RuntimeDecision {
        kind: chosen,
        reason: format!("lock target {}", plan.selected_target),
        plan,
    })
}

fn synthesize_manifest_from_lock(
    lock: &AtoLock,
    runtime_model: &ResolvedLockRuntimeModel,
) -> toml::Value {
    let mut manifest = toml::map::Map::new();
    if let Some(name) = runtime_model.metadata.name.as_ref() {
        manifest.insert("name".to_string(), toml::Value::String(name.clone()));
    }
    if let Some(version) = runtime_model.metadata.version.as_ref() {
        manifest.insert("version".to_string(), toml::Value::String(version.clone()));
    }
    manifest.insert(
        "schema_version".to_string(),
        toml::Value::String("0.3".to_string()),
    );
    manifest.insert(
        "type".to_string(),
        toml::Value::String(
            runtime_model
                .metadata
                .capsule_type
                .clone()
                .unwrap_or_else(|| "app".to_string()),
        ),
    );
    manifest.insert(
        "default_target".to_string(),
        toml::Value::String(runtime_model.selected.target_label.clone()),
    );

    if let Some(network) = runtime_model.network.as_ref() {
        if let Ok(value) = toml::Value::try_from(network.clone()) {
            manifest.insert("network".to_string(), value);
        }
    }

    let mut targets = toml::map::Map::new();
    for service in &runtime_model.services {
        let mut target = toml::map::Map::new();
        let runtime = &service.runtime;
        target.insert(
            "runtime".to_string(),
            toml::Value::String(runtime.runtime.clone()),
        );
        if let Some(driver) = runtime.driver.as_ref() {
            target.insert("driver".to_string(), toml::Value::String(driver.clone()));
        }
        if let Some(image) = runtime.image.as_ref() {
            target.insert("image".to_string(), toml::Value::String(image.clone()));
        }
        // schema_version "0.3" rejects legacy `entrypoint`/`cmd` fields.
        // Execution entrypoint is read from the lock runtime model directly, not
        // from this synthesized manifest, so these fields can be omitted here.
        if let Some(run_command) = runtime.run_command.as_ref() {
            if !run_command.trim().is_empty() {
                target.insert(
                    "run_command".to_string(),
                    toml::Value::String(run_command.clone()),
                );
            }
        }
        if !runtime.env.is_empty() {
            let env = runtime
                .env
                .iter()
                .map(|(key, value)| (key.clone(), toml::Value::String(value.clone())))
                .collect();
            target.insert("env".to_string(), toml::Value::Table(env));
        }
        if let Some(working_dir) = runtime.working_dir.as_ref() {
            target.insert(
                "working_dir".to_string(),
                toml::Value::String(working_dir.clone()),
            );
        }
        if let Some(source_layout) = runtime.source_layout.as_ref() {
            target.insert(
                "source_layout".to_string(),
                toml::Value::String(source_layout.clone()),
            );
        }
        if let Some(port) = runtime.port {
            target.insert("port".to_string(), toml::Value::Integer(i64::from(port)));
        }
        if !runtime.required_env.is_empty() {
            target.insert(
                "required_env".to_string(),
                toml::Value::Array(
                    runtime
                        .required_env
                        .iter()
                        .cloned()
                        .map(toml::Value::String)
                        .collect(),
                ),
            );
        }
        if let Some(runtime_version) =
            resolved_target_string_from_lock(lock, &service.target_label, "runtime_version")
        {
            target.insert(
                "runtime_version".to_string(),
                toml::Value::String(runtime_version),
            );
        }
        if let Some(runtime_tools) =
            resolved_target_table_from_lock(lock, &service.target_label, &["runtime_tools"])
        {
            target.insert(
                "runtime_tools".to_string(),
                toml::Value::Table(runtime_tools),
            );
        }
        if let Some(readiness_probe) = service.readiness_probe.as_ref() {
            if let Ok(value) = toml::Value::try_from(readiness_probe.clone()) {
                target.insert("readiness_probe".to_string(), value);
            }
        }
        targets.insert(service.target_label.clone(), toml::Value::Table(target));
    }
    manifest.insert("targets".to_string(), toml::Value::Table(targets));

    if runtime_model.services.len() > 1 {
        let mut services = toml::map::Map::new();
        for service in &runtime_model.services {
            let mut service_table = toml::map::Map::new();
            service_table.insert(
                "target".to_string(),
                toml::Value::String(service.target_label.clone()),
            );
            if !service.depends_on.is_empty() {
                service_table.insert(
                    "depends_on".to_string(),
                    toml::Value::Array(
                        service
                            .depends_on
                            .iter()
                            .cloned()
                            .map(toml::Value::String)
                            .collect(),
                    ),
                );
            }
            if let Some(readiness_probe) = service.readiness_probe.as_ref() {
                if let Ok(value) = toml::Value::try_from(readiness_probe.clone()) {
                    service_table.insert("readiness_probe".to_string(), value);
                }
            }
            services.insert(service.name.clone(), toml::Value::Table(service_table));
        }
        manifest.insert("services".to_string(), toml::Value::Table(services));
    }

    toml::Value::Table(manifest)
}

fn resolved_target_string_from_lock(
    lock: &AtoLock,
    target_label: &str,
    key: &str,
) -> Option<String> {
    let target_value = lock
        .resolution
        .entries
        .get("resolved_targets")
        .and_then(|value| value.as_array())
        .and_then(|targets| {
            targets.iter().find(|target| {
                target
                    .get("label")
                    .and_then(|value| value.as_str())
                    .map(|label| label == target_label)
                    .unwrap_or(false)
            })
        })
        .and_then(|target| target.get(key))
        .and_then(|value| value.as_str())
        .map(str::to_string);

    if target_value.is_some() || key != "runtime_version" {
        return target_value;
    }

    lock.resolution
        .entries
        .get("runtime")
        .and_then(|runtime| runtime.get("version"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn resolved_target_table_from_lock(
    lock: &AtoLock,
    target_label: &str,
    keys: &[&str],
) -> Option<toml::value::Table> {
    let mut current = lock
        .resolution
        .entries
        .get("resolved_targets")
        .and_then(|value| value.as_array())
        .and_then(|targets| {
            targets.iter().find(|target| {
                target
                    .get("label")
                    .and_then(|value| value.as_str())
                    .map(|label| label == target_label)
                    .unwrap_or(false)
            })
        })?;
    for key in keys {
        current = current.get(*key)?;
    }
    let object = current.as_object()?;
    Some(
        object
            .iter()
            .filter_map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), toml::Value::String(value.to_string())))
            })
            .collect(),
    )
}

impl ExecutionDescriptor {
    pub fn with_selected_target(&self, selected_target: impl Into<String>) -> Self {
        let mut cloned = self.clone();
        cloned.selected_target = selected_target.into();
        if let Ok(runtime_model) =
            lock_runtime::resolve_lock_runtime_model(&cloned.lock, Some(&cloned.selected_target))
        {
            cloned.runtime_model = runtime_model;
        }
        cloned
    }

    pub fn compat_project_input(&self) -> Result<Option<CompatProjectInput>> {
        self.compat_manifest
            .clone()
            .map(|bridge| CompatProjectInput::from_bridge(self.workspace_root.clone(), bridge))
            .transpose()
    }

    pub fn execution_entrypoint(&self) -> Option<String> {
        self.runtime_for_target(&self.selected_target)
            .map(|runtime| runtime.entrypoint.clone())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn execution_runtime(&self) -> Option<String> {
        self.runtime_for_target(&self.selected_target)
            .map(|runtime| runtime.runtime.clone())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn execution_driver(&self) -> Option<String> {
        self.runtime_for_target(&self.selected_target)
            .and_then(|runtime| runtime.driver.clone())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn execution_run_command(&self) -> Option<String> {
        self.runtime_for_target(&self.selected_target)
            .and_then(|runtime| runtime.run_command.clone())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn execution_package_type(&self) -> Option<String> {
        self.compat_str(&["targets", &self.selected_target, "package_type"])
    }

    pub fn execution_runtime_version(&self) -> Option<String> {
        self.resolved_target_string(&self.selected_target, "runtime_version")
            .or_else(|| self.compat_str(&["targets", &self.selected_target, "runtime_version"]))
    }

    pub fn execution_runtime_tool_version(&self, tool: &str) -> Option<String> {
        self.resolved_target_nested_string(&self.selected_target, &["runtime_tools", tool])
            .or_else(|| self.compat_str(&["targets", &self.selected_target, "runtime_tools", tool]))
    }

    pub fn execution_language(&self) -> Option<String> {
        self.compat_str(&["targets", &self.selected_target, "language"])
    }

    pub fn execution_image(&self) -> Option<String> {
        self.compat_str(&["targets", &self.selected_target, "image"])
    }

    pub fn execution_env(&self) -> HashMap<String, String> {
        self.runtime_for_target(&self.selected_target)
            .map(|runtime| runtime.env.clone())
            .filter(|env| !env.is_empty())
            .unwrap_or_else(|| {
                self.compat_table(&["targets", &self.selected_target, "env"])
                    .map(|table| table_to_map(&table))
                    .unwrap_or_default()
            })
    }

    pub fn execution_required_envs(&self) -> Vec<String> {
        let mut ordered = Vec::new();
        let mut seen = HashSet::new();

        if let Some(required) =
            self.compat_array(&["targets", &self.selected_target, "required_env"])
        {
            for value in &required {
                if let Some(name) = value.as_str() {
                    let trimmed = name.trim();
                    if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
                        ordered.push(trimmed.to_string());
                    }
                }
            }
        }

        if let Some(csv) = self
            .execution_env()
            .get("ATO_ORCH_REQUIRED_ENVS")
            .map(|v| v.to_string())
        {
            for name in csv.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if seen.insert(name.to_string()) {
                    ordered.push(name.to_string());
                }
            }
        }

        ordered
    }

    pub fn execution_working_directory(&self) -> PathBuf {
        self.execution_working_dir()
            .map(|value| self.workspace_root.join(value))
            .unwrap_or_else(|| self.workspace_root.clone())
    }

    pub fn target_package_dependencies(&self, target_label: &str) -> Vec<String> {
        self.compat_array(&["targets", target_label, "package_dependencies"])
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn selected_target_package_order(&self) -> Result<Vec<String>> {
        let targets = self
            .compat_table(&["targets"])
            .ok_or_else(|| anyhow!("Missing required [targets] table"))?;

        let mut closure = HashSet::new();
        let mut stack = vec![self.selected_target.clone()];
        while let Some(target) = stack.pop() {
            if !closure.insert(target.clone()) {
                continue;
            }
            for dependency in self.target_package_dependencies(&target) {
                if !targets.contains_key(&dependency) {
                    return Err(anyhow!(
                        "Target '{}' depends on unknown workspace package '{}'",
                        target,
                        dependency
                    ));
                }
                stack.push(dependency);
            }
        }

        let dependencies = closure
            .iter()
            .map(|target| (target.clone(), self.target_package_dependencies(target)))
            .collect::<HashMap<_, _>>();
        orchestration::startup_order_from_dependencies(&dependencies)
    }

    pub fn selected_target_external_injection(&self) -> HashMap<String, ExternalInjectionSpec> {
        self.compat_manifest
            .as_ref()
            .and_then(|bridge| bridge.raw_value().ok())
            .and_then(|raw| {
                raw.get("targets")
                    .and_then(|targets| targets.get(&self.selected_target))
                    .cloned()
            })
            .and_then(|value| value.try_into::<NamedTarget>().ok())
            .map(|target| target.external_injection)
            .unwrap_or_default()
    }

    pub fn selected_target_readiness_probe(&self) -> Option<ReadinessProbe> {
        self.runtime_model
            .services
            .iter()
            .find(|service| service.target_label == self.selected_target)
            .and_then(|service| service.readiness_probe.clone())
    }

    pub fn services(&self) -> HashMap<String, ServiceSpec> {
        self.compat_table(&["services"])
            .map(|services| {
                services
                    .iter()
                    .filter_map(|(name, raw)| {
                        let spec: ServiceSpec = raw.clone().try_into().ok()?;
                        Some((name.to_string(), spec))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn is_orchestration_mode(&self) -> bool {
        if self.runtime_model.services.len() > 1 {
            return true;
        }
        self.services().values().any(|service| {
            service
                .target
                .as_ref()
                .map(|target| !target.trim().is_empty())
                .unwrap_or(false)
        })
    }

    pub fn is_web_services_mode(&self) -> bool {
        self.execution_runtime()
            .map(|runtime| runtime.eq_ignore_ascii_case("web"))
            .unwrap_or(false)
            && self
                .execution_driver()
                .map(|driver| driver.eq_ignore_ascii_case("deno"))
                .unwrap_or(false)
            && !self.services().is_empty()
    }

    pub fn manifest_name(&self) -> Option<String> {
        self.runtime_model.metadata.name.clone()
    }

    pub fn typed_manifest(&self) -> Result<CapsuleManifest> {
        self.compat_manifest
            .as_ref()
            .map(|bridge| bridge.manifest_model().clone())
            .ok_or_else(|| anyhow!("compat manifest bridge is unavailable"))
    }

    pub fn manifest_version(&self) -> Option<String> {
        self.runtime_model.metadata.version.clone()
    }

    pub fn execution_port(&self) -> Option<u16> {
        if self.is_job_capsule() {
            return None;
        }

        self.target_port(&self.selected_target)
    }

    pub fn execution_working_dir(&self) -> Option<String> {
        self.target_working_dir(&self.selected_target)
    }

    pub fn execution_source_layout(&self) -> Option<String> {
        self.target_source_layout(&self.selected_target)
    }

    pub fn build_lifecycle_build(&self) -> Option<String> {
        self.compat_str(&["targets", &self.selected_target, "build_command"])
            .or_else(|| self.compat_str(&["build", "lifecycle", "build"]))
    }

    pub fn build_cache_outputs(&self) -> Vec<String> {
        self.compat_array(&["targets", &self.selected_target, "outputs"])
            .map(|values| array_to_vec(&values))
            .unwrap_or_default()
    }

    pub fn build_cache_env(&self) -> Vec<String> {
        self.compat_array(&["targets", &self.selected_target, "build_env"])
            .map(|values| array_to_vec(&values))
            .unwrap_or_default()
    }

    pub fn execution_preference(&self) -> Option<Vec<RuntimeKind>> {
        let pref = self.compat_array(&["targets", "preference"])?;

        let mut out = Vec::new();
        for value in pref {
            if let Some(name) = value.as_str() {
                if let Some(kind) = parse_runtime_kind(name) {
                    out.push(kind);
                }
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    pub fn targets_oci_image(&self) -> Option<String> {
        let runtime = self.execution_runtime()?;
        if !runtime.eq_ignore_ascii_case("oci") {
            return None;
        }
        self.target_image(&self.selected_target)
    }

    pub fn targets_oci_cmd(&self) -> Vec<String> {
        let cmd = self.target_cmd(&self.selected_target);
        if !cmd.is_empty() {
            return cmd;
        }

        self.execution_run_command()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(|value| vec!["sh".to_string(), "-lc".to_string(), value])
            .unwrap_or_default()
    }

    pub fn targets_oci_env(&self) -> HashMap<String, String> {
        self.target_env(&self.selected_target)
    }

    pub fn targets_oci_working_dir(&self) -> Option<String> {
        self.target_working_dir(&self.selected_target)
    }

    pub fn targets_wasm_component(&self) -> Option<String> {
        let runtime = self.execution_runtime()?;
        if !runtime.eq_ignore_ascii_case("wasm") {
            return None;
        }
        self.compat_str(&["targets", &self.selected_target, "component"])
            .or_else(|| self.compat_str(&["targets", &self.selected_target, "path"]))
            .or_else(|| self.execution_entrypoint())
    }

    pub fn targets_wasm_args(&self) -> Vec<String> {
        self.compat_array(&["targets", &self.selected_target, "args"])
            .or_else(|| self.compat_array(&["targets", &self.selected_target, "cmd"]))
            .map(|a| array_to_vec(&a))
            .unwrap_or_default()
    }

    pub fn targets_web_public(&self) -> Vec<String> {
        self.compat_array(&["targets", &self.selected_target, "public"])
            .map(|a| array_to_vec(&a))
            .unwrap_or_default()
    }

    pub fn selected_target_label(&self) -> &str {
        &self.selected_target
    }

    pub fn default_target_label(&self) -> Result<String> {
        self.runtime_model
            .metadata
            .default_target
            .clone()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("Missing required field: default_target"))
    }

    pub fn build_gpu(&self) -> bool {
        self.compat_value(&["build", "gpu"])
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn build_context(&self) -> Option<String> {
        self.compat_str(&["build", "context"])
    }

    pub fn build_dockerfile(&self) -> Option<String> {
        self.compat_str(&["build", "dockerfile"])
    }

    pub fn build_image(&self) -> Option<String> {
        self.compat_str(&["build", "image"])
    }

    pub fn build_tag(&self) -> Option<String> {
        self.compat_str(&["build", "tag"])
    }

    pub fn build_target(&self) -> Option<String> {
        self.compat_str(&["build", "target"])
    }

    #[allow(dead_code)]
    pub fn requirements_vram_min(&self) -> Option<String> {
        self.compat_str(&["requirements", "vram_min"])
    }

    pub fn resolve_path(&self, raw: &str) -> PathBuf {
        let p = PathBuf::from(raw);
        if p.is_absolute() {
            p
        } else {
            self.workspace_root.join(p)
        }
    }

    pub fn target_runtime(&self, target_label: &str) -> Option<String> {
        self.runtime_for_target(target_label)
            .map(|runtime| runtime.runtime.clone())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn target_driver(&self, target_label: &str) -> Option<String> {
        self.runtime_for_target(target_label)
            .and_then(|runtime| runtime.driver.clone())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn target_entrypoint(&self, target_label: &str) -> Option<String> {
        self.runtime_for_target(target_label)
            .map(|runtime| runtime.entrypoint.clone())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn target_run_command(&self, target_label: &str) -> Option<String> {
        self.runtime_for_target(target_label)
            .and_then(|runtime| runtime.run_command.clone())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn target_image(&self, target_label: &str) -> Option<String> {
        self.compat_str(&["targets", target_label, "image"])
            .or_else(|| self.target_entrypoint(target_label))
    }

    pub fn target_cmd(&self, target_label: &str) -> Vec<String> {
        if let Some(runtime) = self.runtime_for_target(target_label) {
            if !runtime.cmd.is_empty() {
                return runtime.cmd.clone();
            }
        }
        if let Some(values) = self.compat_array(&["targets", target_label, "cmd"]) {
            return array_to_vec(&values);
        }

        let is_oci = self
            .target_runtime(target_label)
            .map(|runtime| runtime.eq_ignore_ascii_case("oci"))
            .unwrap_or(false);
        if is_oci {
            if let Some(run_command) = self
                .target_run_command(target_label)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
            {
                return vec!["sh".to_string(), "-lc".to_string(), run_command];
            }
        }

        Vec::new()
    }

    pub fn target_env(&self, target_label: &str) -> HashMap<String, String> {
        self.runtime_for_target(target_label)
            .map(|runtime| runtime.env.clone())
            .filter(|env| !env.is_empty())
            .unwrap_or_else(|| {
                self.compat_table(&["targets", target_label, "env"])
                    .map(|table| table_to_map(&table))
                    .unwrap_or_default()
            })
    }

    pub fn target_required_envs(&self, target_label: &str) -> Vec<String> {
        if let Some(runtime) = self.runtime_for_target(target_label) {
            if !runtime.required_env.is_empty() {
                return runtime.required_env.clone();
            }
        }
        let mut ordered = Vec::new();
        let mut seen = HashSet::new();

        if let Some(required) = self.compat_array(&["targets", target_label, "required_env"]) {
            for value in required {
                if let Some(name) = value.as_str() {
                    let trimmed = name.trim();
                    if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
                        ordered.push(trimmed.to_string());
                    }
                }
            }
        }

        ordered
    }

    pub fn target_port(&self, target_label: &str) -> Option<u16> {
        if self.is_job_capsule() {
            return None;
        }

        self.runtime_for_target(target_label)
            .and_then(|runtime| runtime.port)
            .or_else(|| {
                self.compat_value(&["targets", target_label, "port"])
                    .or_else(|| self.compat_value(&["port"]))
                    .and_then(|v| v.as_integer())
                    .and_then(|v| u16::try_from(v).ok())
            })
    }

    pub fn target_working_dir(&self, target_label: &str) -> Option<String> {
        self.runtime_for_target(target_label)
            .and_then(|runtime| runtime.working_dir.clone())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn target_source_layout(&self, target_label: &str) -> Option<String> {
        self.runtime_for_target(target_label)
            .and_then(|runtime| runtime.source_layout.clone())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn compat_manifest(&self) -> Option<&CompatManifestBridge> {
        self.compat_manifest.as_ref()
    }

    pub fn is_schema_v03(&self) -> bool {
        self.compat_manifest
            .as_ref()
            .map(CompatManifestBridge::is_schema_v03)
            .unwrap_or(false)
    }

    pub fn compat_manifest_dir(&self) -> &Path {
        self.manifest_dir.as_path()
    }

    pub fn compat_manifest_path(&self) -> Option<&Path> {
        Some(self.manifest_path.as_path())
    }

    fn is_job_capsule(&self) -> bool {
        self.runtime_model
            .metadata
            .capsule_type
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("job"))
    }

    pub fn compat_target_working_dir(&self, target_label: &str) -> Option<String> {
        self.compat_str(&["targets", target_label, "working_dir"])
    }

    fn runtime_for_target(&self, target_label: &str) -> Option<&ResolvedTargetRuntime> {
        self.runtime_model
            .services
            .iter()
            .find(|service| service.target_label == target_label)
            .map(|service| &service.runtime)
            .or_else(|| {
                (self.runtime_model.selected.target_label == target_label)
                    .then_some(&self.runtime_model.selected.runtime)
            })
    }

    fn resolved_target_value<'a>(
        &'a self,
        target_label: &str,
        key: &str,
    ) -> Option<&'a serde_json::Value> {
        self.lock
            .resolution
            .entries
            .get("resolved_targets")
            .and_then(|value| value.as_array())
            .and_then(|targets| {
                targets.iter().find(|target| {
                    target
                        .get("label")
                        .and_then(|value| value.as_str())
                        .map(|label| label == target_label)
                        .unwrap_or(false)
                })
            })
            .and_then(|target| target.get(key))
    }

    fn resolved_target_string(&self, target_label: &str, key: &str) -> Option<String> {
        self.resolved_target_value(target_label, key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
    }

    fn resolved_target_nested_string(&self, target_label: &str, keys: &[&str]) -> Option<String> {
        let mut current = self
            .lock
            .resolution
            .entries
            .get("resolved_targets")
            .and_then(|value| value.as_array())
            .and_then(|targets| {
                targets.iter().find(|target| {
                    target
                        .get("label")
                        .and_then(|value| value.as_str())
                        .map(|label| label == target_label)
                        .unwrap_or(false)
                })
            })?;
        for key in keys {
            current = current.get(*key)?;
        }
        current.as_str().map(str::to_string)
    }

    fn target_named(&self, service_name: &str, target_label: &str) -> Result<NamedTarget> {
        let value = self
            .compat_value(&["targets", target_label])
            .ok_or_else(|| {
                anyhow!(
                    "services.{}.target '{}' does not exist",
                    service_name,
                    target_label
                )
            })?;
        value
            .clone()
            .try_into()
            .map_err(|_| anyhow!("targets.{} is not a valid target table", target_label))
    }

    fn compat_value(&self, path: &[&str]) -> Option<toml::Value> {
        let mut current = self.compat_manifest.as_ref()?.raw_value().ok()?;
        for key in path {
            let table = current.as_table()?;
            current = table.get(*key)?.clone();
        }
        Some(current)
    }

    fn compat_table(&self, path: &[&str]) -> Option<toml::value::Table> {
        self.compat_value(path)?.as_table().cloned()
    }

    fn compat_array(&self, path: &[&str]) -> Option<Vec<toml::Value>> {
        self.compat_value(path)?.as_array().cloned()
    }

    fn compat_str(&self, path: &[&str]) -> Option<String> {
        self.compat_value(path)
            .and_then(|v| v.as_str().map(str::to_string))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn synthesize_runtime_model_from_manifest(
    manifest: &toml::Value,
    selected_target: &str,
) -> Result<ResolvedLockRuntimeModel> {
    let metadata = LockContractMetadata {
        name: manifest
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        version: manifest
            .get("version")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        capsule_type: manifest
            .get("type")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        default_target: manifest
            .get("default_target")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    };
    let target = manifest
        .get("targets")
        .and_then(|targets| targets.get(selected_target))
        .cloned()
        .ok_or_else(|| anyhow!("Missing required [targets.{}] table", selected_target))?;
    let named_target: NamedTarget = target
        .try_into()
        .map_err(|_| anyhow!("targets.{} is not a valid target table", selected_target))?;
    let runtime = ResolvedTargetRuntime {
        target: selected_target.to_string(),
        runtime: named_target.runtime,
        driver: named_target.driver,
        runtime_version: named_target.runtime_version,
        image: named_target.image,
        entrypoint: named_target.entrypoint,
        run_command: named_target.run_command,
        cmd: named_target.cmd,
        env: named_target.env,
        working_dir: named_target.working_dir,
        source_layout: named_target.source_layout,
        port: named_target.port,
        required_env: named_target.required_env,
        mounts: Vec::<Mount>::new(),
    };
    let selected = LockServiceUnit {
        name: "main".to_string(),
        target_label: selected_target.to_string(),
        runtime: runtime.clone(),
        depends_on: Vec::new(),
        readiness_probe: named_target.readiness_probe,
    };
    Ok(ResolvedLockRuntimeModel {
        metadata,
        network: None,
        selected: selected.clone(),
        services: vec![selected],
    })
}

fn parse_runtime_kind(value: &str) -> Option<RuntimeKind> {
    match value.to_ascii_lowercase().as_str() {
        "oci" | "docker" | "youki" | "runc" => Some(RuntimeKind::Oci),
        "wasm" => Some(RuntimeKind::Wasm),
        "source" | "native" => Some(RuntimeKind::Source),
        "web" => Some(RuntimeKind::Web),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// v0.3-native runtime extraction — reads flat fields directly without [targets]
// ---------------------------------------------------------------------------

/// Determine the effective target label for a v0.3 manifest.
///
/// * Single-app manifests use `default_target` if present, otherwise `"app"`.
/// * Workspace manifests (`[packages]`) use `default_target` or the first
///   runnable package name.
fn resolve_v03_target(manifest: &toml::Value, explicit: Option<&str>) -> Result<String> {
    if let Some(label) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(label.to_string());
    }

    // Honour explicit default_target if present.
    if let Some(dt) = manifest
        .get("default_target")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(dt.to_string());
    }

    // Workspace: pick the first package that declares a runtime.
    if let Some(packages) = manifest.get("packages").and_then(|v| v.as_table()) {
        for (name, pkg) in packages {
            if pkg.get("runtime").and_then(|v| v.as_str()).is_some() {
                return Ok(name.clone());
            }
        }
        if let Some((name, _)) = packages.iter().next() {
            return Ok(name.clone());
        }
    }

    // Single-app fallback.
    Ok("app".to_string())
}

/// Split a v0.3 `runtime` selector (e.g. `"web/static"`, `"source/node"`)
/// into `(runtime, driver)` pair using the same logic as the v0.3 normalizer.
fn split_v03_runtime(value: &str) -> (String, Option<String>) {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "web/static" => ("web".to_string(), Some("static".to_string())),
        "web/node" | "source/node" => ("source".to_string(), Some("node".to_string())),
        "web/deno" | "source/deno" => ("source".to_string(), Some("deno".to_string())),
        "web/python" | "source/python" => ("source".to_string(), Some("python".to_string())),
        "source/native" | "source/go" => ("source".to_string(), Some("native".to_string())),
        "source" | "web" | "oci" | "wasm" => (normalized, None),
        other => {
            if let Some((runtime, driver)) = other.split_once('/') {
                let runtime = runtime.trim();
                let driver = driver.trim();
                let runtime = if runtime == "web" && driver != "static" {
                    "source"
                } else {
                    runtime
                };
                (
                    runtime.to_string(),
                    (!driver.is_empty()).then(|| driver.to_string()),
                )
            } else {
                (other.to_string(), None)
            }
        }
    }
}

/// Infer a `language` hint from a v0.3 driver token.
fn infer_language_from_driver(driver: Option<&str>) -> Option<String> {
    match driver.map(|v| v.trim().to_ascii_lowercase()) {
        Some(d) if matches!(d.as_str(), "node" | "python" | "deno" | "bun") => Some(d),
        _ => None,
    }
}

/// Build a `ResolvedLockRuntimeModel` directly from a v0.3 `toml::Value`.
///
/// For single-app manifests the runtime fields are read from the top level.
/// For workspace manifests they are read from `packages.<selected>`.
fn synthesize_runtime_model_from_v03(
    manifest: &toml::Value,
    selected_target: &str,
) -> Result<ResolvedLockRuntimeModel> {
    let metadata = LockContractMetadata {
        name: manifest
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        version: manifest
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        capsule_type: manifest
            .get("type")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        default_target: Some(selected_target.to_string()),
    };

    // Determine which table carries the runtime fields.
    // Workspace: packages.<selected>, Single-app: top-level.
    let source = manifest
        .get("packages")
        .and_then(|pkgs| pkgs.get(selected_target))
        .unwrap_or(manifest);

    let runtime_selector = source
        .get("runtime")
        .and_then(|v| v.as_str())
        .unwrap_or("source");

    let (runtime, selector_driver) = split_v03_runtime(runtime_selector);
    // If the runtime selector didn't contain a driver (e.g. `runtime = "source"`),
    // fall back to an explicit top-level `driver` field (e.g. `driver = "tauri"`).
    let driver = selector_driver.or_else(|| {
        source
            .get("driver")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });
    let language = infer_language_from_driver(driver.as_deref());

    let run_command = source
        .get("run")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let image = source
        .get("image")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let port = source
        .get("port")
        .and_then(|v| v.as_integer())
        .map(|v| v as u16);

    // For web/static, the `run` field is an entrypoint (directory), not a command.
    let is_web_static = runtime == "web" && driver.as_deref() == Some("static");
    let entrypoint = if is_web_static {
        run_command.clone().unwrap_or_else(|| ".".to_string())
    } else {
        String::new()
    };
    let effective_run_command = if is_web_static { None } else { run_command };

    let mut env = HashMap::new();
    if let Some(env_table) = source.get("env").and_then(|v| v.as_table()) {
        for (k, v) in env_table {
            if let Some(s) = v.as_str() {
                env.insert(k.clone(), s.to_string());
            }
        }
    }

    let required_env: Vec<String> = source
        .get("required_env")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let readiness_probe = source.get("readiness_probe").and_then(|v| {
        v.as_str()
            .map(|s| ReadinessProbe {
                http_get: Some(s.to_string()),
                tcp_connect: None,
                port: "PORT".to_string(),
            })
            .or_else(|| {
                v.as_table().map(|table| ReadinessProbe {
                    http_get: table
                        .get("http_get")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    tcp_connect: table
                        .get("tcp_connect")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    port: table
                        .get("port")
                        .and_then(|v| v.as_str())
                        .unwrap_or("PORT")
                        .to_string(),
                })
            })
    });

    let resolved_runtime = ResolvedTargetRuntime {
        target: selected_target.to_string(),
        runtime: runtime.clone(),
        driver: driver.or(language),
        runtime_version: source
            .get("runtime_version")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        image,
        entrypoint,
        run_command: effective_run_command,
        cmd: Vec::new(),
        env,
        working_dir: None,
        source_layout: None,
        port,
        required_env,
        mounts: Vec::new(),
    };

    let selected = LockServiceUnit {
        name: "main".to_string(),
        target_label: selected_target.to_string(),
        runtime: resolved_runtime,
        depends_on: Vec::new(),
        readiness_probe,
    };

    Ok(ResolvedLockRuntimeModel {
        metadata,
        network: None,
        selected: selected.clone(),
        services: vec![selected],
    })
}

fn resolve_target_label(manifest: &toml::Value, target_label: Option<&str>) -> Result<String> {
    let targets = manifest
        .get("targets")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow!("Missing required [targets] table"))?;

    let default_target = manifest
        .get("default_target")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Missing required field: default_target"))?;

    let selected = target_label
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_target);

    if !targets.contains_key(selected) {
        return Err(anyhow!("Target '{}' not found under [targets]", selected));
    }

    Ok(selected.to_string())
}

fn table_to_map(table: &toml::value::Table) -> HashMap<String, String> {
    table
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}

fn array_to_vec(values: &[toml::Value]) -> Vec<String> {
    values
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{route_manifest, route_manifest_with_state_overrides, ExecutionProfile};
    use crate::types::Mount;
    use std::fs;

    fn write_manifest(contents: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("capsule.toml"), contents).expect("write manifest");
        dir
    }

    #[test]
    fn orchestration_mode_detects_target_services() {
        let dir = write_manifest(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

default_target = "web"

[targets.web]
runtime = "web/node"
run = "server.js"
port = 3000
required_env = ["API_KEY"]

[targets.db]
runtime = "oci"
image = "mysql:8"
port = 3306
[services.main]
target = "web"
depends_on = ["db"]

[services.db]
target = "db"
"#,
        );

        let decision = route_manifest(
            &dir.path().join("capsule.toml"),
            ExecutionProfile::Dev,
            None,
        )
        .expect("route manifest");

        assert!(decision.plan.is_orchestration_mode());
        assert_eq!(
            decision
                .plan
                .target_for_service("main")
                .expect("main target"),
            Some("web".to_string())
        );
    }

    #[test]
    fn v03_selected_target_package_order_respects_workspace_dependencies() {
        let dir = write_manifest(
            r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.web]
type = "app"
runtime = "source/node"
run = "pnpm --filter web start"

    [packages.web.dependencies]
    api = "workspace:api"
    ui = "workspace:ui"

[packages.api]
type = "app"
runtime = "source/node"
run = "pnpm --filter api start"

    [packages.api.dependencies]
    ui = "workspace:ui"

[packages.ui]
type = "library"
runtime = "source/node"
build = "pnpm --filter ui build"
"#,
        );

        let decision = route_manifest(
            &dir.path().join("capsule.toml"),
            ExecutionProfile::Dev,
            Some("web"),
        )
        .expect("route manifest");

        assert_eq!(
            decision
                .plan
                .selected_target_package_order()
                .expect("package order"),
            vec!["ui".to_string(), "api".to_string(), "web".to_string()]
        );
    }

    #[test]
    fn v03_oci_target_uses_shell_wrapped_run_command() {
        let dir = write_manifest(
            r#"
schema_version = "0.3"
name = "oci-demo"
version = "0.1.0"
type = "app"
runtime = "oci"
image = "ghcr.io/example/app:latest"
run = "echo 'Hello World' && /app/server --port $PORT"
"#,
        );

        let decision = route_manifest(
            &dir.path().join("capsule.toml"),
            ExecutionProfile::Dev,
            None,
        )
        .expect("route manifest");

        assert_eq!(
            decision.plan.targets_oci_cmd(),
            vec![
                "sh".to_string(),
                "-lc".to_string(),
                "echo 'Hello World' && /app/server --port $PORT".to_string()
            ]
        );
    }

    #[test]
    fn orchestration_mode_defaults_main_target() {
        let dir = write_manifest(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

default_target = "web"

[targets.web]
runtime = "web/node"
run = "server.js"
port = 3000

[targets.db]
runtime = "oci"
image = "mysql:8"
port = 3306
[services.main]
depends_on = ["db"]

[services.db]
target = "db"
"#,
        );

        let decision = route_manifest(
            &dir.path().join("capsule.toml"),
            ExecutionProfile::Dev,
            None,
        )
        .expect("route manifest");

        assert_eq!(
            decision
                .plan
                .target_for_service("main")
                .expect("main target"),
            Some("web".to_string())
        );
    }

    #[test]
    fn resolve_services_builds_connections() {
        let dir = write_manifest(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

default_target = "web"

[targets.web]
runtime = "web/node"
run = "server.js"
port = 3000
required_env = ["API_KEY"]

[targets.db]
runtime = "oci"
image = "mysql:8"
port = 3306
[services.main]
target = "web"
depends_on = ["db"]

[services.db]
target = "db"
network = { aliases = ["mysql"] }
"#,
        );

        let decision = route_manifest(
            &dir.path().join("capsule.toml"),
            ExecutionProfile::Dev,
            None,
        )
        .expect("route manifest");
        let plan = decision.plan.resolve_services().expect("resolve services");

        assert_eq!(
            plan.startup_order,
            vec!["db".to_string(), "main".to_string()]
        );
        let main = plan.service("main").expect("main service");
        assert_eq!(main.runtime.runtime().target, "web");
        assert_eq!(
            main.runtime.runtime().required_env,
            vec!["API_KEY".to_string()]
        );
        assert_eq!(main.connections.len(), 1);
        assert_eq!(main.connections[0].dependency, "db");
        assert_eq!(main.connections[0].default_host, "mysql");
        assert_eq!(main.connections[0].host_env, "ATO_SERVICE_DB_HOST");
        assert_eq!(main.connections[0].port_env, "ATO_SERVICE_DB_PORT");
    }

    #[test]
    fn resolve_services_includes_ephemeral_state_mounts_for_oci_targets() {
        let dir = write_manifest(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

runtime = "oci"
image = "ghcr.io/example/app:latest"
[state.data]
kind = "filesystem"
durability = "ephemeral"
purpose = "primary-data"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#,
        );

        let decision = route_manifest(
            &dir.path().join("capsule.toml"),
            ExecutionProfile::Dev,
            None,
        )
        .expect("route manifest");
        let plan = decision.plan.resolve_services().expect("resolve services");
        let main = plan.service("main").expect("main service");

        assert_eq!(
            main.runtime.runtime().mounts,
            vec![Mount {
                source: "/var/lib/ato/state/demo-app/data".to_string(),
                target: "/var/lib/app".to_string(),
                readonly: false,
            }]
        );
    }

    #[test]
    fn resolve_services_requires_explicit_bind_for_persistent_state() {
        let dir = write_manifest(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

runtime = "oci"
image = "ghcr.io/example/app:latest"
[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#,
        );

        let decision = route_manifest(
            &dir.path().join("capsule.toml"),
            ExecutionProfile::Dev,
            None,
        )
        .expect("route manifest");
        let err = decision
            .plan
            .resolve_services()
            .expect_err("missing bind must fail");
        assert!(err
            .to_string()
            .contains("requires an explicit persistent binding"));
    }

    #[test]
    fn resolve_services_uses_explicit_bind_for_persistent_state() {
        let dir = write_manifest(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

runtime = "oci"
image = "ghcr.io/example/app:latest"
[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#,
        );

        let decision = route_manifest_with_state_overrides(
            &dir.path().join("capsule.toml"),
            ExecutionProfile::Dev,
            None,
            [(
                "data".to_string(),
                "/var/lib/ato/persistent/demo-app/data".to_string(),
            )]
            .into_iter()
            .collect(),
        )
        .expect("route manifest");
        let plan = decision.plan.resolve_services().expect("resolve services");
        let main = plan.service("main").expect("main service");

        assert_eq!(
            main.runtime.runtime().mounts,
            vec![Mount {
                source: "/var/lib/ato/persistent/demo-app/data".to_string(),
                target: "/var/lib/app".to_string(),
                readonly: false,
            }]
        );
    }

    // ── v0.3-native extractor tests ──────────────────────────────────────

    #[test]
    fn v03_resolve_target_defaults_to_app() {
        let manifest: toml::Value = toml::from_str(
            r#"
schema_version = "0.3"
name = "hello"
runtime = "web/static"
run = "index.html"
"#,
        )
        .unwrap();
        assert_eq!(super::resolve_v03_target(&manifest, None).unwrap(), "app");
    }

    #[test]
    fn v03_resolve_target_uses_explicit_label() {
        let manifest: toml::Value = toml::from_str(
            r#"
schema_version = "0.3"
name = "hello"
runtime = "web/static"
"#,
        )
        .unwrap();
        assert_eq!(
            super::resolve_v03_target(&manifest, Some("custom")).unwrap(),
            "custom"
        );
    }

    #[test]
    fn v03_resolve_target_workspace_picks_first_runnable() {
        let manifest: toml::Value = toml::from_str(
            r#"
schema_version = "0.3"
name = "ws"

[packages.web]
type = "app"
runtime = "source/node"
run = "pnpm start"

[packages.lib]
type = "library"
"#,
        )
        .unwrap();
        let target = super::resolve_v03_target(&manifest, None).unwrap();
        // lib has no runtime, so web is picked
        assert_eq!(target, "web");
    }

    #[test]
    fn v03_synthesize_runtime_web_static() {
        let manifest: toml::Value = toml::from_str(
            r#"
schema_version = "0.3"
name = "hello"
version = "1.0.0"
type = "app"
runtime = "web/static"
run = "dist"
port = 4173
"#,
        )
        .unwrap();

        let model = super::synthesize_runtime_model_from_v03(&manifest, "app").unwrap();

        assert_eq!(model.selected.runtime.runtime, "web");
        assert_eq!(model.selected.runtime.driver.as_deref(), Some("static"));
        assert_eq!(model.selected.runtime.entrypoint, "dist");
        assert!(model.selected.runtime.run_command.is_none());
        assert_eq!(model.selected.runtime.port, Some(4173));
        assert_eq!(model.metadata.name.as_deref(), Some("hello"));
    }

    #[test]
    fn v03_synthesize_runtime_source_node() {
        let manifest: toml::Value = toml::from_str(
            r#"
schema_version = "0.3"
name = "app"
type = "app"
runtime = "source/node"
run = "npm start"
port = 3000
"#,
        )
        .unwrap();

        let model = super::synthesize_runtime_model_from_v03(&manifest, "app").unwrap();

        assert_eq!(model.selected.runtime.runtime, "source");
        assert_eq!(model.selected.runtime.driver.as_deref(), Some("node"));
        assert_eq!(
            model.selected.runtime.run_command.as_deref(),
            Some("npm start")
        );
        assert_eq!(model.selected.runtime.port, Some(3000));
    }

    #[test]
    fn v03_synthesize_runtime_workspace_package() {
        let manifest: toml::Value = toml::from_str(
            r#"
schema_version = "0.3"
name = "ws"

[packages.api]
type = "app"
runtime = "source/python"
run = "python -m uvicorn main:app"
port = 8000
"#,
        )
        .unwrap();

        let model = super::synthesize_runtime_model_from_v03(&manifest, "api").unwrap();

        assert_eq!(model.selected.runtime.runtime, "source");
        assert_eq!(model.selected.runtime.driver.as_deref(), Some("python"));
        assert_eq!(
            model.selected.runtime.run_command.as_deref(),
            Some("python -m uvicorn main:app")
        );
        assert_eq!(model.selected.runtime.port, Some(8000));
        assert_eq!(model.selected.target_label, "api");
    }

    #[test]
    fn v03_split_runtime_selector() {
        assert_eq!(
            super::split_v03_runtime("web/static"),
            ("web".to_string(), Some("static".to_string()))
        );
        assert_eq!(
            super::split_v03_runtime("source/node"),
            ("source".to_string(), Some("node".to_string()))
        );
        assert_eq!(super::split_v03_runtime("oci"), ("oci".to_string(), None));
        assert_eq!(
            super::split_v03_runtime("source/go"),
            ("source".to_string(), Some("native".to_string()))
        );
    }
}

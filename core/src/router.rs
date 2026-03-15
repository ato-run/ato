use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::manifest;
use crate::orchestration;
use crate::types::{
    CapsuleManifest, ExternalInjectionSpec, Mount, NamedTarget, OrchestrationPlan, ReadinessProbe,
    ResolvedService, ResolvedServiceNetwork, ResolvedServiceRuntime, ResolvedTargetRuntime,
    ServiceConnectionInfo, ServiceSpec,
};

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
pub struct ManifestData {
    pub manifest: toml::Value,
    pub manifest_path: PathBuf,
    pub manifest_dir: PathBuf,
    pub profile: ExecutionProfile,
    pub selected_target: String,
    pub state_source_overrides: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeDecision {
    pub kind: RuntimeKind,
    pub reason: String,
    pub plan: ManifestData,
}

pub fn route_manifest(
    manifest_path: &Path,
    profile: ExecutionProfile,
    target_label: Option<&str>,
) -> Result<RuntimeDecision> {
    route_manifest_with_state_overrides(manifest_path, profile, target_label, HashMap::new())
}

pub fn route_manifest_with_state_overrides(
    manifest_path: &Path,
    profile: ExecutionProfile,
    target_label: Option<&str>,
    state_source_overrides: HashMap<String, String>,
) -> Result<RuntimeDecision> {
    let loaded = manifest::load_manifest(manifest_path)?;
    let manifest = loaded.raw;
    let manifest_dir = loaded.dir.clone();
    let selected_target = resolve_target_label(&manifest, target_label)?;

    let plan = ManifestData {
        manifest,
        manifest_path: loaded.path,
        manifest_dir,
        profile,
        selected_target,
        state_source_overrides,
    };

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

impl ManifestData {
    pub fn with_selected_target(&self, selected_target: impl Into<String>) -> Self {
        let mut cloned = self.clone();
        cloned.selected_target = selected_target.into();
        cloned
    }

    pub fn execution_entrypoint(&self) -> Option<String> {
        self.get_str(&["targets", &self.selected_target, "entrypoint"])
    }

    pub fn execution_runtime(&self) -> Option<String> {
        self.get_str(&["targets", &self.selected_target, "runtime"])
    }

    pub fn execution_driver(&self) -> Option<String> {
        self.get_str(&["targets", &self.selected_target, "driver"])
    }

    pub fn execution_run_command(&self) -> Option<String> {
        self.get_str(&["targets", &self.selected_target, "run_command"])
    }

    pub fn execution_package_type(&self) -> Option<String> {
        self.get_str(&["targets", &self.selected_target, "package_type"])
    }

    pub fn execution_runtime_version(&self) -> Option<String> {
        self.get_str(&["targets", &self.selected_target, "runtime_version"])
    }

    pub fn execution_runtime_tool_version(&self, tool: &str) -> Option<String> {
        self.get_str(&["targets", &self.selected_target, "runtime_tools", tool])
    }

    pub fn execution_language(&self) -> Option<String> {
        self.get_str(&["targets", &self.selected_target, "language"])
    }

    pub fn execution_image(&self) -> Option<String> {
        self.get_str(&["targets", &self.selected_target, "image"])
    }

    pub fn execution_env(&self) -> HashMap<String, String> {
        self.get_table(&["targets", &self.selected_target, "env"])
            .map(table_to_map)
            .unwrap_or_default()
    }

    pub fn execution_required_envs(&self) -> Vec<String> {
        let mut ordered = Vec::new();
        let mut seen = HashSet::new();

        if let Some(required) = self.get_array(&["targets", &self.selected_target, "required_env"])
        {
            for value in required {
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
            .map(|value| self.manifest_dir.join(value))
            .unwrap_or_else(|| self.manifest_dir.clone())
    }

    pub fn target_package_dependencies(&self, target_label: &str) -> Vec<String> {
        self.get_array(&["targets", target_label, "package_dependencies"])
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
            .get_table(&["targets"])
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
        self.manifest
            .get("targets")
            .and_then(|targets| targets.get(&self.selected_target))
            .cloned()
            .and_then(|value| value.try_into::<NamedTarget>().ok())
            .map(|target| target.external_injection)
            .unwrap_or_default()
    }

    pub fn selected_target_readiness_probe(&self) -> Option<ReadinessProbe> {
        self.manifest
            .get("targets")
            .and_then(|targets| targets.get(&self.selected_target))
            .cloned()
            .and_then(|value| value.try_into::<NamedTarget>().ok())
            .and_then(|target| target.readiness_probe)
    }

    pub fn services(&self) -> HashMap<String, ServiceSpec> {
        self.get_table(&["services"])
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
        self.services().values().any(|service| {
            service
                .target
                .as_ref()
                .map(|target| !target.trim().is_empty())
                .unwrap_or(false)
        })
    }

    pub fn target_for_service(&self, service_name: &str) -> Result<Option<String>> {
        let services = self.services();
        let service = services
            .get(service_name)
            .ok_or_else(|| anyhow!("services.{} is missing", service_name))?;

        if let Some(target) = service
            .target
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            return Ok(Some(target.to_string()));
        }

        if self.is_orchestration_mode()
            && service_name == "main"
            && service.entrypoint.trim().is_empty()
        {
            return Ok(Some(self.default_target_label()?));
        }

        Ok(None)
    }

    pub fn resolve_services(&self) -> Result<OrchestrationPlan> {
        if !self.is_orchestration_mode() {
            anyhow::bail!("services target-based orchestration mode is not enabled");
        }

        let typed_manifest = self.typed_manifest()?;
        let services = self.services();
        if services.is_empty() {
            anyhow::bail!("top-level [services] must define at least one service");
        }

        let mut dependencies = HashMap::new();
        let mut resolved_services = Vec::new();
        let mut resolved_runtime_by_name = HashMap::new();

        let mut names: Vec<String> = services.keys().cloned().collect();
        names.sort();

        for name in &names {
            let service = services
                .get(name)
                .ok_or_else(|| anyhow!("services.{} is missing from parsed manifest", name))?;

            if !service.entrypoint.trim().is_empty() {
                anyhow::bail!(
                    "services.{}.entrypoint is only supported in legacy inline services mode",
                    name
                );
            }

            let target_label = self
                .target_for_service(name)?
                .ok_or_else(|| anyhow!("services.{}.target is required", name))?;
            let target = self.target_named(name, &target_label)?;
            let depends_on = service.depends_on.clone().unwrap_or_default();
            let runtime_kind = parse_runtime_kind(&target.runtime).ok_or_else(|| {
                anyhow!(
                    "services.{}.target '{}' has unsupported runtime '{}'",
                    name,
                    target_label,
                    target.runtime
                )
            })?;

            let target_runtime = ResolvedTargetRuntime {
                target: target_label.clone(),
                runtime: target.runtime.clone(),
                driver: target.driver.clone(),
                image: target.image.clone().or_else(|| {
                    (!target.entrypoint.trim().is_empty()).then(|| target.entrypoint.clone())
                }),
                entrypoint: target.entrypoint.clone(),
                cmd: target.cmd.clone(),
                env: {
                    let mut env = self.target_env(&target_label);
                    if let Some(extra_env) = service.env.as_ref() {
                        env.extend(extra_env.clone());
                    }
                    env
                },
                working_dir: target.working_dir.clone(),
                port: self.target_port(&target_label),
                required_env: self.target_required_envs(&target_label),
                mounts: state_mounts_for_service(
                    &typed_manifest,
                    name,
                    &self.state_source_overrides,
                )?,
            };

            let runtime = match runtime_kind {
                RuntimeKind::Oci => ResolvedServiceRuntime::Oci(target_runtime),
                RuntimeKind::Wasm => {
                    anyhow::bail!(
                        "services.{}.target '{}' cannot use runtime=wasm",
                        name,
                        target_label
                    )
                }
                RuntimeKind::Source | RuntimeKind::Web => {
                    ResolvedServiceRuntime::Managed(target_runtime)
                }
            };

            let mut aliases = vec![name.clone()];
            if let Some(network) = service.network.as_ref() {
                for alias in &network.aliases {
                    let trimmed = alias.trim();
                    if !trimmed.is_empty() && !aliases.iter().any(|value| value == trimmed) {
                        aliases.push(trimmed.to_string());
                    }
                }
            }

            let connections = depends_on
                .iter()
                .filter_map(|dependency| {
                    let dependency_service = services.get(dependency)?;
                    let dependency_target = self.target_for_service(dependency).ok().flatten()?;
                    let dependency_port = self.target_port(&dependency_target);
                    let dependency_network = dependency_service.network.as_ref();
                    let default_host = dependency_network
                        .and_then(|network| network.aliases.first())
                        .cloned()
                        .unwrap_or_else(|| dependency.clone());
                    Some(ServiceConnectionInfo {
                        dependency: dependency.clone(),
                        host_env: connection_env_key(dependency, "HOST"),
                        port_env: connection_env_key(dependency, "PORT"),
                        container_port: dependency_port,
                        default_host,
                    })
                })
                .collect();

            let mut network = ResolvedServiceNetwork {
                aliases,
                publish: service
                    .network
                    .as_ref()
                    .map(|network| network.publish)
                    .unwrap_or(false),
                allow_from: service
                    .network
                    .as_ref()
                    .map(|network| network.allow_from.clone())
                    .unwrap_or_default(),
            };
            if name == "main" && runtime.runtime().port.is_some() {
                network.publish = true;
            }

            dependencies.insert(name.clone(), depends_on.clone());
            resolved_runtime_by_name.insert(name.clone(), runtime_kind);
            resolved_services.push(ResolvedService {
                name: name.clone(),
                depends_on,
                connections,
                readiness_probe: service.readiness_probe.clone(),
                network,
                runtime,
            });
        }

        for service in &resolved_services {
            for dependency in &service.depends_on {
                let Some(dependency_service) = services.get(dependency) else {
                    anyhow::bail!(
                        "services.{}.depends_on references unknown service '{}'",
                        service.name,
                        dependency
                    );
                };
                if let Some(network) = dependency_service.network.as_ref() {
                    if !network.allow_from.is_empty()
                        && !network
                            .allow_from
                            .iter()
                            .any(|value| value == &service.name)
                    {
                        anyhow::bail!(
                            "service '{}' is not allowed to connect to '{}'",
                            service.name,
                            dependency
                        );
                    }
                }

                let dependency_runtime = resolved_runtime_by_name
                    .get(dependency)
                    .ok_or_else(|| anyhow!("service '{}' is unresolved", dependency))?;
                if service.runtime.is_oci() && *dependency_runtime != RuntimeKind::Oci {
                    anyhow::bail!(
                        "OCI service '{}' cannot depend on non-OCI service '{}'",
                        service.name,
                        dependency
                    );
                }
            }
        }

        let startup_order = orchestration::startup_order_from_dependencies(&dependencies)?;
        Ok(OrchestrationPlan {
            startup_order,
            services: resolved_services,
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
        self.get_str(&["name"])
    }

    pub fn typed_manifest(&self) -> Result<CapsuleManifest> {
        let manifest_toml =
            toml::to_string(&self.manifest).map_err(|err| anyhow!("serialize manifest: {err}"))?;
        CapsuleManifest::from_toml(&manifest_toml).map_err(|err| anyhow!(err.to_string()))
    }

    pub fn manifest_version(&self) -> Option<String> {
        self.get_str(&["version"])
    }

    pub fn execution_port(&self) -> Option<u16> {
        self.target_port(&self.selected_target)
    }

    pub fn execution_working_dir(&self) -> Option<String> {
        self.target_working_dir(&self.selected_target)
    }

    pub fn build_lifecycle_build(&self) -> Option<String> {
        self.get_str(&["targets", &self.selected_target, "build_command"])
            .or_else(|| self.get_str(&["build", "lifecycle", "build"]))
    }

    pub fn execution_preference(&self) -> Option<Vec<RuntimeKind>> {
        let pref = self.get_array(&["targets", "preference"])?;

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
        self.target_cmd(&self.selected_target)
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
        self.get_str(&["targets", &self.selected_target, "component"])
            .or_else(|| self.get_str(&["targets", &self.selected_target, "path"]))
            .or_else(|| self.execution_entrypoint())
    }

    pub fn targets_wasm_args(&self) -> Vec<String> {
        self.get_array(&["targets", &self.selected_target, "args"])
            .or_else(|| self.get_array(&["targets", &self.selected_target, "cmd"]))
            .map(|a| array_to_vec(a))
            .unwrap_or_default()
    }

    pub fn targets_web_public(&self) -> Vec<String> {
        self.get_array(&["targets", &self.selected_target, "public"])
            .map(|a| array_to_vec(a))
            .unwrap_or_default()
    }

    pub fn selected_target_label(&self) -> &str {
        &self.selected_target
    }

    pub fn default_target_label(&self) -> Result<String> {
        self.get_str(&["default_target"])
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("Missing required field: default_target"))
    }

    pub fn build_gpu(&self) -> bool {
        self.get_value(&["build", "gpu"])
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn build_context(&self) -> Option<String> {
        self.get_str(&["build", "context"])
    }

    pub fn build_dockerfile(&self) -> Option<String> {
        self.get_str(&["build", "dockerfile"])
    }

    pub fn build_image(&self) -> Option<String> {
        self.get_str(&["build", "image"])
    }

    pub fn build_tag(&self) -> Option<String> {
        self.get_str(&["build", "tag"])
    }

    pub fn build_target(&self) -> Option<String> {
        self.get_str(&["build", "target"])
    }

    #[allow(dead_code)]
    pub fn requirements_vram_min(&self) -> Option<String> {
        self.get_str(&["requirements", "vram_min"])
    }

    pub fn resolve_path(&self, raw: &str) -> PathBuf {
        let p = PathBuf::from(raw);
        if p.is_absolute() {
            p
        } else {
            self.manifest_dir.join(p)
        }
    }

    pub fn target_runtime(&self, target_label: &str) -> Option<String> {
        self.get_str(&["targets", target_label, "runtime"])
    }

    pub fn target_driver(&self, target_label: &str) -> Option<String> {
        self.get_str(&["targets", target_label, "driver"])
    }

    pub fn target_entrypoint(&self, target_label: &str) -> Option<String> {
        self.get_str(&["targets", target_label, "entrypoint"])
    }

    pub fn target_image(&self, target_label: &str) -> Option<String> {
        self.get_str(&["targets", target_label, "image"])
            .or_else(|| self.target_entrypoint(target_label))
    }

    pub fn target_cmd(&self, target_label: &str) -> Vec<String> {
        self.get_array(&["targets", target_label, "cmd"])
            .map(|values| array_to_vec(values))
            .unwrap_or_default()
    }

    pub fn target_env(&self, target_label: &str) -> HashMap<String, String> {
        self.get_table(&["targets", target_label, "env"])
            .map(table_to_map)
            .unwrap_or_default()
    }

    pub fn target_required_envs(&self, target_label: &str) -> Vec<String> {
        let mut ordered = Vec::new();
        let mut seen = HashSet::new();

        if let Some(required) = self.get_array(&["targets", target_label, "required_env"]) {
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
        self.get_value(&["targets", target_label, "port"])
            .or_else(|| self.get_value(&["port"]))
            .and_then(|v| v.as_integer())
            .and_then(|v| u16::try_from(v).ok())
    }

    pub fn target_working_dir(&self, target_label: &str) -> Option<String> {
        self.get_str(&["targets", target_label, "working_dir"])
    }

    fn target_named(&self, service_name: &str, target_label: &str) -> Result<NamedTarget> {
        let value = self.get_value(&["targets", target_label]).ok_or_else(|| {
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

    fn get_value<'a>(&'a self, path: &[&str]) -> Option<&'a toml::Value> {
        let mut current = &self.manifest;
        for key in path {
            let table = current.as_table()?;
            current = table.get(*key)?;
        }
        Some(current)
    }

    fn get_table<'a>(&'a self, path: &[&str]) -> Option<&'a toml::value::Table> {
        self.get_value(path)?.as_table()
    }

    fn get_array<'a>(&'a self, path: &[&str]) -> Option<&'a Vec<toml::Value>> {
        self.get_value(path)?.as_array()
    }

    fn get_str(&self, path: &[&str]) -> Option<String> {
        self.get_value(path)
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
    }
}

fn connection_env_key(service_name: &str, suffix: &str) -> String {
    let sanitized = service_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("ATO_SERVICE_{}_{}", sanitized, suffix)
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

fn state_mounts_for_service(
    manifest: &CapsuleManifest,
    service_name: &str,
    state_source_overrides: &HashMap<String, String>,
) -> Result<Vec<Mount>> {
    let Some(service) = manifest
        .services
        .as_ref()
        .and_then(|services| services.get(service_name))
    else {
        return Ok(Vec::new());
    };

    service
        .state_bindings
        .iter()
        .map(|binding| {
            let state_name = binding.state.trim();
            let requirement = manifest.state.get(state_name).ok_or_else(|| {
                anyhow!(
                    "services.{}.state_bindings references unknown state '{}'",
                    service_name,
                    state_name
                )
            })?;

            Ok(Mount {
                source: manifest.state_source_path(
                    state_name,
                    requirement,
                    Some(state_source_overrides),
                )?,
                target: binding.target.trim().to_string(),
                readonly: false,
            })
        })
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
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "node"
entrypoint = "server.js"
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
    fn orchestration_mode_defaults_main_target() {
        let dir = write_manifest(
            r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "node"
entrypoint = "server.js"
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
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "node"
entrypoint = "server.js"
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
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
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
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
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
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
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
}

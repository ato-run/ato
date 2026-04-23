use crate::error::{CapsuleError, Result};

use super::*;

impl ManifestData {
    pub fn target_for_service(&self, service_name: &str) -> Result<Option<String>> {
        let services = self.services();
        let service = services
            .get(service_name)
            .ok_or_else(|| CapsuleError::Config(format!("services.{} is missing", service_name)))?;

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
            return Err(CapsuleError::Config(
                "services target-based orchestration mode is not enabled".into(),
            ));
        }

        let typed_manifest = self.typed_manifest()?;
        let services = self.services();
        if services.is_empty() {
            return Err(CapsuleError::Config(
                "top-level [services] must define at least one service".into(),
            ));
        }

        let mut dependencies = HashMap::new();
        let mut resolved_services = Vec::new();
        let mut resolved_runtime_by_name = HashMap::new();

        let mut names: Vec<String> = services.keys().cloned().collect();
        names.sort();

        for name in &names {
            let service = services.get(name).ok_or_else(|| {
                CapsuleError::Config(format!(
                    "services.{} is missing from parsed manifest",
                    name
                ))
            })?;

            if !service.entrypoint.trim().is_empty() {
                return Err(CapsuleError::Config(format!(
                    "services.{}.entrypoint is only supported in legacy inline services mode",
                    name
                )));
            }

            let target_label = self
                .target_for_service(name)?
                .ok_or_else(|| {
                    CapsuleError::Config(format!("services.{}.target is required", name))
                })?;
            let target = self.target_named(name, &target_label)?;
            let depends_on = service.depends_on.clone().unwrap_or_default();
            let runtime_kind = parse_runtime_kind(&target.runtime).ok_or_else(|| {
                CapsuleError::Config(format!(
                    "services.{}.target '{}' has unsupported runtime '{}'",
                    name, target_label, target.runtime
                ))
            })?;

            let target_runtime = ResolvedTargetRuntime {
                target: target_label.clone(),
                runtime: target.runtime.clone(),
                driver: target.driver.clone(),
                runtime_version: target.runtime_version.clone(),
                image: target.image.clone().or_else(|| {
                    (!target.entrypoint.trim().is_empty()).then(|| target.entrypoint.clone())
                }),
                entrypoint: target.entrypoint.clone(),
                run_command: target.run_command.clone(),
                cmd: target.cmd.clone(),
                env: {
                    let mut env = self.target_env(&target_label);
                    if let Some(extra_env) = service.env.as_ref() {
                        env.extend(extra_env.clone());
                    }
                    env
                },
                working_dir: target.working_dir.clone(),
                source_layout: target.source_layout.clone(),
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
                    return Err(CapsuleError::Config(format!(
                        "services.{}.target '{}' cannot use runtime=wasm",
                        name, target_label
                    )));
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
                    return Err(CapsuleError::Config(format!(
                        "services.{}.depends_on references unknown service '{}'",
                        service.name, dependency
                    )));
                };
                if let Some(network) = dependency_service.network.as_ref() {
                    if !network.allow_from.is_empty()
                        && !network
                            .allow_from
                            .iter()
                            .any(|value| value == &service.name)
                    {
                        return Err(CapsuleError::Config(format!(
                            "service '{}' is not allowed to connect to '{}'",
                            service.name, dependency
                        )));
                    }
                }

                let dependency_runtime =
                    resolved_runtime_by_name.get(dependency).ok_or_else(|| {
                        CapsuleError::Config(format!(
                            "service '{}' is unresolved",
                            dependency
                        ))
                    })?;
                if service.runtime.is_oci() && *dependency_runtime != RuntimeKind::Oci {
                    return Err(CapsuleError::Config(format!(
                        "OCI service '{}' cannot depend on non-OCI service '{}'",
                        service.name, dependency
                    )));
                }
            }
        }

        let startup_order = orchestration::startup_order_from_dependencies(&dependencies)?;
        Ok(OrchestrationPlan {
            startup_order,
            services: resolved_services,
        })
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
                CapsuleError::Config(format!(
                    "services.{}.state_bindings references unknown state '{}'",
                    service_name, state_name
                ))
            })?;

            Ok(Mount {
                source: manifest
                    .state_source_path(state_name, requirement, Some(state_source_overrides))
                    .map_err(|e| CapsuleError::Runtime(e.to_string()))?,
                target: binding.target.trim().to_string(),
                readonly: false,
            })
        })
        .collect()
}

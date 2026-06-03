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
        let mut services = self.services();
        if services.is_empty() {
            return Err(CapsuleError::Config(
                "top-level [services] must define at least one service".into(),
            ));
        }

        // Auto-create implicit service entries for dependency targets referenced
        // via [targets.X] depends_on / needs that have no explicit [services.X].
        {
            // Collect all target-level needs across all targets.
            let all_targets = self.all_targets();
            let mut implicit: Vec<(String, ServiceSpec)> = Vec::new();
            for (service_name, service) in &services {
                let target_label = service.target.as_deref().unwrap_or(service_name.as_str());
                if let Some(target) = all_targets.get(target_label) {
                    for dep_label in &target.needs {
                        // Only auto-create if not already an explicit service.
                        if !services.contains_key(dep_label) {
                            implicit.push((
                                dep_label.clone(),
                                ServiceSpec {
                                    target: Some(dep_label.clone()),
                                    // Inherit readiness_probe from the dependency target.
                                    readiness_probe: all_targets
                                        .get(dep_label.as_str())
                                        .and_then(|t| t.readiness_probe.clone()),
                                    ..ServiceSpec::default()
                                },
                            ));
                        }
                    }
                }
            }
            for (name, spec) in implicit {
                services.entry(name).or_insert(spec);
            }
        }

        let mut dependencies = HashMap::new();
        let mut resolved_services = Vec::new();
        let mut resolved_runtime_by_name = HashMap::new();

        let mut names: Vec<String> = services.keys().cloned().collect();
        names.sort();

        for name in &names {
            let service = services.get(name).ok_or_else(|| {
                CapsuleError::Config(format!("services.{} is missing from parsed manifest", name))
            })?;

            if !service.entrypoint.trim().is_empty() {
                return Err(CapsuleError::Config(format!(
                    "services.{}.entrypoint is only supported in legacy inline services mode",
                    name
                )));
            }

            let target_label = service
                .target
                .as_ref()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .or_else(|| {
                    // Legacy: service named "main" without explicit target falls
                    // back to default_target, same as target_for_service().
                    if name == "main" && service.entrypoint.trim().is_empty() {
                        self.default_target_label().ok()
                    } else {
                        None
                    }
                })
                .ok_or_else(|| {
                    CapsuleError::Config(format!("services.{}.target is required", name))
                })?;
            let target = self.target_named(name, &target_label)?;
            // Merge service-level and target-level (needs/depends_on) dependencies.
            let mut depends_on = service.depends_on.clone().unwrap_or_default();
            for dep in &target.needs {
                if !depends_on.contains(dep) {
                    depends_on.push(dep.clone());
                }
            }
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
                user: target.user.clone(),
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
                    let dependency_target_label = services
                        .get(dependency)
                        .and_then(|s| s.target.clone())
                        .filter(|t| !t.trim().is_empty())?;
                    let dependency_target = self
                        .target_named(dependency, &dependency_target_label)
                        .ok()?;
                    if dependency_target.run_once {
                        return None;
                    }
                    let dependency_port = self.target_port(&dependency_target_label);
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
                egress_proxy: service
                    .network
                    .as_ref()
                    .map(|network| network.egress_proxy)
                    .unwrap_or(true),
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
                // Service-level probe takes priority; fall back to target-level probe.
                readiness_probe: service
                    .readiness_probe
                    .clone()
                    .or_else(|| target.readiness_probe.clone()),
                network,
                run_once: target.run_once,
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
                if let Some(network) = dependency_service.network.as_ref()
                    && !network.allow_from.is_empty()
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

                let dependency_runtime =
                    resolved_runtime_by_name.get(dependency).ok_or_else(|| {
                        CapsuleError::Config(format!("service '{}' is unresolved", dependency))
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
    let services = match manifest.services.as_ref() {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };

    // Collect bindings from ALL service entries where `service_target` matches
    // `service_name` (or is absent and the binding belongs to `service_name` itself).
    let mut mounts = Vec::new();
    for (svc_name, service) in services {
        for binding in &service.state_bindings {
            let effective_target = binding
                .service_target
                .as_deref()
                .unwrap_or(svc_name.as_str());
            if effective_target == service_name {
                let state_name = binding.state.trim();
                let requirement = manifest.state.get(state_name).ok_or_else(|| {
                    CapsuleError::Config(format!(
                        "services.{}.state_bindings references unknown state '{}'",
                        svc_name, state_name
                    ))
                })?;
                mounts.push(Mount {
                    source: manifest
                        .state_source_path(state_name, requirement, Some(state_source_overrides))
                        .map_err(|e| CapsuleError::Runtime(e.to_string()))?,
                    target: binding.target.trim().to_string(),
                    readonly: false,
                    ownership: mount_ownership_from_binding(svc_name, binding)?,
                });
            }
        }
    }
    Ok(mounts)
}

/// Resolve a state binding's optional `owner`/`mode` into a [`MountOwnership`].
///
/// Returns `None` when neither `owner` nor `mode` is declared (Ato leaves the
/// bound path untouched). Either may appear independently:
/// * `owner` → best-effort `chown` to that uid/gid.
/// * `mode` → `chmod` (octal string, e.g. `"0777"`). This is the load-bearing
///   op on Podman-machine/virtiofs; it is valid on its own (chmod-only).
fn mount_ownership_from_binding(
    svc_name: &str,
    binding: &crate::types::ServiceStateBinding,
) -> Result<Option<MountOwnership>> {
    let mode = match binding.mode.as_deref() {
        Some(raw) => Some(parse_octal_mode(svc_name, &binding.state, raw)?),
        None => None,
    };
    if binding.owner.is_none() && mode.is_none() {
        return Ok(None);
    }
    Ok(Some(MountOwnership {
        uid: binding.owner.as_ref().map(|o| o.uid),
        gid: binding.owner.as_ref().and_then(|o| o.gid),
        recursive: binding.owner.as_ref().is_some_and(|o| o.recursive),
        mode,
    }))
}

/// Parse an octal permission string like `"0700"`/`"755"` into mode bits.
fn parse_octal_mode(svc_name: &str, state: &str, raw: &str) -> Result<u32> {
    let trimmed = raw.trim().trim_start_matches("0o");
    u32::from_str_radix(trimmed, 8).map_err(|_| {
        CapsuleError::Config(format!(
            "services.{svc_name}.state_bindings for state '{state}' has invalid `mode` \"{raw}\"; \
             expected an octal string like \"0700\" or \"0755\""
        ))
    })
}

#[cfg(test)]
mod state_ownership_tests {
    use super::*;
    use crate::types::{ServiceStateBinding, StateOwner};

    fn binding(owner: Option<StateOwner>, mode: Option<&str>) -> ServiceStateBinding {
        ServiceStateBinding {
            state: "data".to_string(),
            target: "/opt/app/data".to_string(),
            service_target: None,
            owner,
            mode: mode.map(str::to_string),
        }
    }

    #[test]
    fn parse_octal_mode_accepts_common_forms() {
        assert_eq!(parse_octal_mode("s", "data", "0700").unwrap(), 0o700);
        assert_eq!(parse_octal_mode("s", "data", "755").unwrap(), 0o755);
        assert_eq!(parse_octal_mode("s", "data", "0o640").unwrap(), 0o640);
    }

    #[test]
    fn parse_octal_mode_rejects_non_octal() {
        assert!(parse_octal_mode("s", "data", "u+rwx").is_err());
        assert!(parse_octal_mode("s", "data", "999").is_err()); // 9 is not an octal digit
    }

    #[test]
    fn owner_with_mode_resolves_to_mount_ownership() {
        let b = binding(
            Some(StateOwner {
                uid: 1001,
                gid: Some(1001),
                recursive: true,
            }),
            Some("0777"),
        );
        let own = mount_ownership_from_binding("main", &b).unwrap().unwrap();
        assert_eq!(own.uid, Some(1001));
        assert_eq!(own.gid, Some(1001));
        assert!(own.recursive);
        assert_eq!(own.mode, Some(0o777));
    }

    #[test]
    fn owner_without_mode_is_ok_with_no_mode() {
        let b = binding(
            Some(StateOwner {
                uid: 1001,
                gid: None,
                recursive: false,
            }),
            None,
        );
        let own = mount_ownership_from_binding("main", &b).unwrap().unwrap();
        assert_eq!(own.uid, Some(1001));
        assert_eq!(own.gid, None);
        assert_eq!(own.mode, None);
    }

    #[test]
    fn mode_without_owner_is_allowed_chmod_only() {
        // mode-only is the common Podman-machine case: chmod is the load-bearing
        // op, no chown needed/possible for a non-root host user.
        let b = binding(None, Some("0777"));
        let own = mount_ownership_from_binding("main", &b).unwrap().unwrap();
        assert_eq!(own.uid, None, "no owner → no chown target");
        assert_eq!(own.gid, None);
        assert!(!own.recursive);
        assert_eq!(own.mode, Some(0o777));
    }

    #[test]
    fn no_owner_no_mode_yields_no_ownership() {
        let b = binding(None, None);
        assert!(mount_ownership_from_binding("main", &b).unwrap().is_none());
    }
}

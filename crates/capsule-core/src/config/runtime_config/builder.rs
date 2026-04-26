use jsonschema::{Draft, JSONSchema};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

use crate::error::{CapsuleError, Result};

use crate::common::hash::sha256_hex;
use crate::lock_runtime::{LockCompilerOverlay, LockServiceUnit};
use crate::policy::egress_resolver::{resolve_egress_policy, EgressRule};
use crate::python_runtime::extend_python_selector_env;
use crate::router::{self, CompatProjectInput, ExecutionProfile};
use crate::types::{ResolvedService, ResolvedServiceRuntime, ResolvedTargetRuntime};

use super::{
    ConfigJson, EgressConfig, EgressRuleEntry, FilesystemConfig, HealthCheck, MetadataConfig,
    NetworkConfig, SandboxConfig, ServiceSpec, SidecarConfig, SignalsConfig, TsnetSidecarConfig,
    CONFIG_VERSION,
};

type CommandResolution = (
    String,
    Vec<String>,
    Option<HashMap<String, String>>,
    Option<SignalsConfig>,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceLayoutHint {
    Standard,
    AnchoredEntrypoint,
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestService {
    #[serde(default)]
    target: Option<String>,

    #[serde(default)]
    entrypoint: String,
    #[serde(default)]
    depends_on: Option<Vec<String>>,
    #[serde(default)]
    env: Option<HashMap<String, String>>,
    #[serde(default)]
    readiness_probe: Option<ManifestReadinessProbe>,
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestReadinessProbe {
    #[serde(default)]
    http_get: Option<String>,
    #[serde(default)]
    tcp_connect: Option<String>,
    port: String,
}

pub(super) fn build_config_json(
    compat_input: &CompatProjectInput,
    enforcement_override: Option<String>,
    standalone: bool,
) -> Result<ConfigJson> {
    let manifest = compat_input.manifest_value();
    let mut services = HashMap::new();
    let manifest_services = manifest
        .get("services")
        .and_then(|s| s.as_table())
        .map(|tbl| {
            tbl.iter()
                .filter_map(|(k, v)| {
                    let svc: Option<ManifestService> = v.clone().try_into().ok();
                    svc.map(|s| (k.to_string(), s))
                })
                .collect::<HashMap<String, ManifestService>>()
        })
        .unwrap_or_default();

    if uses_target_based_services(&manifest_services) {
        services = build_target_based_services(compat_input, standalone)?;
    } else {
        let layout = read_source_layout(manifest);
        let (executable, args, env, signals) = if let Some(run_command) = read_run_command(manifest)
        {
            if run_command_should_be_entrypoint(&run_command, manifest) {
                resolve_command(&run_command, None, manifest, standalone, layout)
            } else {
                resolve_shell_command(&run_command, manifest)
            }
        } else {
            let entrypoint = read_entrypoint(manifest)?;
            let command = read_command(manifest);
            let cmd_overrides_entrypoint = target_cmd_overrides_entrypoint(manifest);
            if cmd_overrides_entrypoint {
                let command_entrypoint = command.as_deref().unwrap_or(&entrypoint);
                resolve_command(command_entrypoint, None, manifest, standalone, layout)
            } else {
                resolve_command(
                    &entrypoint,
                    command.as_deref(),
                    manifest,
                    standalone,
                    layout,
                )
            }
        };
        let main_spec = ServiceSpec {
            executable,
            args,
            cwd: Some(source_cwd(read_working_dir(manifest), layout)),
            env,
            user: None,
            signals,
            depends_on: None,
            health_check: read_health_check(manifest),
            ports: None,
        };
        services.insert("main".to_string(), main_spec);
    }

    if !manifest_services.is_empty() && !uses_target_based_services(&manifest_services) {
        for (name, svc) in &manifest_services {
            if name == "main" {
                return Err(CapsuleError::Config(
                    "services.main conflicts with execution entrypoint; remove services.main or execution".to_string()
                ));
            }

            let command = None;
            let layout = read_source_layout(manifest);
            let (executable, args, env, signals) =
                resolve_command(&svc.entrypoint, command, manifest, standalone, layout);
            let health_check = svc.readiness_probe.as_ref().map(|p| HealthCheck {
                http_get: p.http_get.clone(),
                tcp_connect: p.tcp_connect.clone(),
                port: p.port.clone(),
                interval_secs: None,
                timeout_secs: None,
            });

            let spec = ServiceSpec {
                executable,
                args,
                cwd: Some(source_cwd(read_working_dir(manifest), layout)),
                env: merge_envs(env, svc.env.clone()),
                user: None,
                signals,
                depends_on: svc.depends_on.clone(),
                health_check,
                ports: None,
            };
            services.insert(name.clone(), spec);
        }
    }

    validate_services_dag(&services)?;

    let (egress_rules, allow_domains) = build_egress(manifest)?;

    let sandbox = SandboxConfig {
        enabled: true,
        filesystem: read_filesystem(manifest),
        network: NetworkConfig {
            enabled: true,
            enforcement: enforcement_override.unwrap_or_else(|| "best_effort".to_string()),
            egress: egress_rules,
        },
        development_mode: None,
    };

    let metadata = MetadataConfig {
        name: manifest
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        version: manifest
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        generated_at: None,
        generated_by: Some(format!("ato-cli v{}", env!("CARGO_PKG_VERSION"))),
        source_manifest: Some(format!(
            "sha256:{}",
            sha256_hex(compat_input.manifest_text().as_bytes())
        )),
    };

    let sidecar = build_sidecar_config(manifest, &allow_domains)?;

    Ok(ConfigJson {
        version: CONFIG_VERSION.to_string(),
        services,
        sandbox,
        metadata: Some(metadata),
        annotations: None,
        sidecar,
    })
}

fn uses_target_based_services(services: &HashMap<String, ManifestService>) -> bool {
    services.values().any(|service| {
        service
            .target
            .as_deref()
            .map(|target| !target.trim().is_empty())
            .unwrap_or(false)
    })
}

fn build_target_based_services(
    compat_input: &CompatProjectInput,
    standalone: bool,
) -> Result<HashMap<String, ServiceSpec>> {
    let decision = router::execution_descriptor_from_manifest_parts(
        compat_input.manifest_value().clone(),
        compat_input.workspace_root().to_path_buf(),
        compat_input.workspace_root().to_path_buf(),
        ExecutionProfile::Release,
        None,
        HashMap::new(),
    )
    .map_err(|e| {
        CapsuleError::Config(format!(
            "Failed to resolve target-based services from {}: {}",
            compat_input.logical_source_label(),
            e
        ))
    })?;
    let orchestration = decision
        .resolve_services()
        .map_err(|e| CapsuleError::Config(e.to_string()))?;
    let mut services = HashMap::new();

    for service in orchestration.services {
        services.insert(
            service.name.clone(),
            build_target_service_spec(&service, standalone)?,
        );
    }

    Ok(services)
}

fn build_target_service_spec(service: &ResolvedService, standalone: bool) -> Result<ServiceSpec> {
    let runtime = match &service.runtime {
        ResolvedServiceRuntime::Managed(runtime) => runtime,
        ResolvedServiceRuntime::Oci(runtime) => {
            return Err(CapsuleError::Config(format!(
                "target-based config.json generation does not yet support runtime=oci services (service='{}', target='{}')",
                service.name,
                runtime.target
            )));
        }
    };

    let layout = source_layout_hint(runtime.source_layout.as_deref());
    let (executable, args, env) = resolve_target_command(runtime, standalone, layout);
    let mut merged_env = env.unwrap_or_default();
    for connection in &service.connections {
        merged_env.insert(connection.host_env.clone(), "127.0.0.1".to_string());
        if let Some(port) = connection.container_port {
            merged_env.insert(connection.port_env.clone(), port.to_string());
        }
    }

    Ok(ServiceSpec {
        executable,
        args,
        cwd: Some(source_cwd(runtime.working_dir.as_deref(), layout)),
        env: if merged_env.is_empty() {
            None
        } else {
            Some(merged_env)
        },
        user: None,
        signals: None,
        depends_on: if service.depends_on.is_empty() {
            None
        } else {
            Some(service.depends_on.clone())
        },
        health_check: service.readiness_probe.as_ref().map(|probe| HealthCheck {
            http_get: probe.http_get.clone(),
            tcp_connect: probe.tcp_connect.clone(),
            port: probe.port.clone(),
            interval_secs: None,
            timeout_secs: None,
        }),
        ports: None,
    })
}

pub(super) fn build_lock_service_spec(
    service: &LockServiceUnit,
    standalone: bool,
) -> Result<ServiceSpec> {
    let layout = source_layout_hint(service.runtime.source_layout.as_deref());
    let (executable, args, env) = resolve_target_command(&service.runtime, standalone, layout);
    Ok(ServiceSpec {
        executable,
        args,
        cwd: Some(source_cwd(service.runtime.working_dir.as_deref(), layout)),
        env,
        user: None,
        signals: None,
        depends_on: if service.depends_on.is_empty() {
            None
        } else {
            Some(service.depends_on.clone())
        },
        health_check: service.readiness_probe.as_ref().map(|probe| HealthCheck {
            http_get: probe.http_get.clone(),
            tcp_connect: probe.tcp_connect.clone(),
            port: probe.port.clone(),
            interval_secs: None,
            timeout_secs: None,
        }),
        ports: None,
    })
}

pub(super) fn build_lock_filesystem(overlay: &LockCompilerOverlay) -> Option<FilesystemConfig> {
    if overlay.filesystem_read_only.is_none() && overlay.filesystem_read_write.is_none() {
        return None;
    }

    Some(FilesystemConfig {
        read_only: overlay.filesystem_read_only.clone(),
        read_write: overlay.filesystem_read_write.clone(),
    })
}

pub(super) fn build_lock_egress(
    network: Option<&crate::types::NetworkConfig>,
    overlay_allow_hosts: Option<&Vec<String>>,
) -> Result<(Option<EgressConfig>, Vec<String>)> {
    let mut allow_domains = overlay_allow_hosts
        .cloned()
        .or_else(|| network.map(|value| value.egress_allow.clone()))
        .unwrap_or_default();
    allow_domains.sort();
    allow_domains.dedup();

    let resolved = if allow_domains.is_empty() {
        None
    } else {
        Some(resolve_egress_policy(&allow_domains)?)
    };

    let mut rules = Vec::new();
    let mut seen_ips: HashSet<String> = HashSet::new();
    let mut seen_cidrs: HashSet<String> = HashSet::new();

    if let Some(resolved) = resolved {
        for ip in resolved.resolved_ips {
            if seen_ips.insert(ip.clone()) {
                rules.push(EgressRuleEntry {
                    rule_type: "ip".to_string(),
                    value: ip,
                });
            }
        }
        for rule in resolved.rules {
            if let EgressRule::Cidr { value } = rule {
                if seen_cidrs.insert(value.clone()) {
                    rules.push(EgressRuleEntry {
                        rule_type: "cidr".to_string(),
                        value,
                    });
                }
            }
        }
    }

    if let Some(network) = network {
        for rule in &network.egress_id_allow {
            match rule.rule_type {
                crate::types::EgressIdType::Ip => {
                    if seen_ips.insert(rule.value.clone()) {
                        rules.push(EgressRuleEntry {
                            rule_type: "ip".to_string(),
                            value: rule.value.clone(),
                        });
                    }
                }
                crate::types::EgressIdType::Cidr => {
                    if seen_cidrs.insert(rule.value.clone()) {
                        rules.push(EgressRuleEntry {
                            rule_type: "cidr".to_string(),
                            value: rule.value.clone(),
                        });
                    }
                }
                crate::types::EgressIdType::Spiffe => {}
            }
        }
    }

    if rules.is_empty() && allow_domains.is_empty() {
        return Ok((None, allow_domains));
    }

    Ok((
        Some(EgressConfig {
            mode: "allowlist".to_string(),
            rules: if rules.is_empty() { None } else { Some(rules) },
        }),
        allow_domains,
    ))
}

pub(super) fn build_lock_sidecar_config(
    _network: Option<&crate::types::NetworkConfig>,
    _allow_domains: &[String],
) -> Option<SidecarConfig> {
    None
}

fn command_tokens(entrypoint: &str, command: Option<&str>) -> (String, Vec<String>) {
    let mut tokens =
        shell_words::split(entrypoint).unwrap_or_else(|_| vec![entrypoint.to_string()]);
    if let Some(command) = command {
        if !command.trim().is_empty() {
            let extra = shell_words::split(command).unwrap_or_else(|_| vec![command.to_string()]);
            tokens.extend(extra);
        }
    }
    let program = tokens
        .first()
        .cloned()
        .unwrap_or_else(|| entrypoint.to_string());
    (program, tokens)
}

struct LanguageCommandContext<'a> {
    standalone: bool,
    env: &'a mut HashMap<String, String>,
    language: Option<&'a str>,
    runtime_version: Option<&'a str>,
    synthesize_default_args: bool,
    layout: SourceLayoutHint,
}

fn resolve_language_command(
    program: &str,
    tokens: &[String],
    context: LanguageCommandContext<'_>,
) -> (String, Vec<String>) {
    let args = tokens.get(1..).unwrap_or(&[]).to_vec();
    let default_program_arg = normalized_source_entrypoint(program, context.layout);
    match context.language {
        Some("python") => {
            let args = if context.synthesize_default_args && args.is_empty() {
                vec![default_program_arg.clone()]
            } else {
                args
            };
            context
                .env
                .insert("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string());
            if context.standalone {
                context
                    .env
                    .insert("PYTHONHOME".to_string(), "runtime/python".to_string());
                context
                    .env
                    .insert("PYTHONPATH".to_string(), "source".to_string());
                ("runtime/python/bin/python3".to_string(), args)
            } else {
                insert_uv_python_runtime_env(context.env, context.runtime_version);
                let mut uv_args = vec!["run".to_string(), "--offline".to_string()];
                uv_args.push("python3".to_string());
                uv_args.extend(args);
                ("uv".to_string(), uv_args)
            }
        }
        Some("node") => {
            let args = if context.synthesize_default_args && args.is_empty() {
                vec![default_program_arg.clone()]
            } else {
                args
            };
            if context.standalone {
                ("runtime/node/bin/node".to_string(), args)
            } else {
                ("node".to_string(), args)
            }
        }
        Some("deno") => {
            let args = if context.synthesize_default_args && args.is_empty() {
                vec![
                    "run".to_string(),
                    "-A".to_string(),
                    default_program_arg.clone(),
                ]
            } else {
                args
            };
            if context.standalone {
                ("runtime/deno/bin/deno".to_string(), args)
            } else {
                ("deno".to_string(), args)
            }
        }
        Some("bun") => {
            let args = if context.synthesize_default_args && args.is_empty() {
                vec![default_program_arg]
            } else {
                args
            };
            if context.standalone {
                ("runtime/bun/bin/bun".to_string(), args)
            } else {
                ("bun".to_string(), args)
            }
        }
        _ => (
            normalize_program(program, context.standalone),
            tokens.get(1..).unwrap_or(&[]).to_vec(),
        ),
    }
}

fn merged_manifest_env(manifest: &toml::Value) -> HashMap<String, String> {
    let mut env = selected_target_table(manifest)
        .and_then(|t| t.get("env"))
        .or_else(|| manifest.get("env"))
        .and_then(|e| e.as_table())
        .map(|tbl| {
            tbl.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.to_string(), s.to_string())))
                .collect::<HashMap<String, String>>()
        })
        .unwrap_or_default();

    let execution_env = manifest
        .get("execution")
        .and_then(|e| e.get("env"))
        .and_then(|e| e.as_table())
        .map(|tbl| {
            tbl.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.to_string(), s.to_string())))
                .collect::<HashMap<String, String>>()
        })
        .unwrap_or_default();
    env.extend(execution_env);
    env
}

fn execution_signals(manifest: &toml::Value) -> Option<SignalsConfig> {
    manifest
        .get("execution")
        .and_then(|e| e.get("signals"))
        .and_then(|s| s.as_table())
        .map(|tbl| SignalsConfig {
            stop: tbl
                .get("stop")
                .and_then(|v| v.as_str())
                .unwrap_or("SIGTERM")
                .to_string(),
            kill: tbl
                .get("kill")
                .and_then(|v| v.as_str())
                .unwrap_or("SIGKILL")
                .to_string(),
        })
}

/// Same heuristic as `run_command_should_be_entrypoint` but for the
/// `ResolvedTargetRuntime` carried by target-based services. We trust the
/// resolved driver/runtime metadata rather than the un-normalized manifest
/// tree, but apply the same shell-metadata / interpreter-prefix rules.
fn target_run_command_should_be_entrypoint(
    command: &str,
    runtime: &ResolvedTargetRuntime,
) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains(['$', '|', '&', ';', '`', '(', ')', '<', '>']) {
        return false;
    }
    if let Some(first_token) = trimmed.split_whitespace().next() {
        if detect_language_from_program(first_token).is_some() {
            return true;
        }
    }
    if trimmed.contains(char::is_whitespace) {
        return false;
    }
    if let Some(driver) = runtime.driver.as_deref() {
        let driver = driver.trim().to_ascii_lowercase();
        if matches!(driver.as_str(), "python" | "node" | "deno" | "bun") {
            return true;
        }
    }
    if detect_language_from_entrypoint(trimmed).is_some() {
        return true;
    }
    trimmed.starts_with("./") || trimmed.starts_with('/') || trimmed.starts_with("runtime/")
}

fn resolve_target_command(
    runtime: &ResolvedTargetRuntime,
    standalone: bool,
    layout: SourceLayoutHint,
) -> (String, Vec<String>, Option<HashMap<String, String>>) {
    let run_command = runtime
        .run_command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(run_command) = run_command {
        if !target_run_command_should_be_entrypoint(run_command, runtime) {
            return (
                "sh".to_string(),
                vec!["-c".to_string(), run_command.to_string()],
                if runtime.env.is_empty() {
                    None
                } else {
                    Some(runtime.env.clone())
                },
            );
        }
    }

    // For target-based services we treat run_command as the synthetic
    // entrypoint when the driver/file extension implies a known interpreter.
    let entrypoint = run_command.unwrap_or_else(|| runtime.entrypoint.as_str());
    let (program, tokens) = command_tokens(entrypoint, None);

    let language = runtime
        .driver
        .as_deref()
        .map(normalize_language)
        .or_else(|| detect_language_from_program(&program))
        .or_else(|| detect_language_from_entrypoint(entrypoint));

    let mut env = runtime.env.clone();

    let (executable, args) = resolve_language_command(
        &program,
        &tokens,
        LanguageCommandContext {
            standalone,
            env: &mut env,
            language: language.as_deref(),
            runtime_version: runtime.runtime_version.as_deref(),
            synthesize_default_args: true,
            layout,
        },
    );

    let (executable, mut args) = if let Some((cmd_executable, cmd_args)) =
        resolve_explicit_cmd_override(
            &runtime.cmd,
            runtime.runtime_version.as_deref(),
            standalone,
            &mut env,
            layout,
        ) {
        (cmd_executable, cmd_args)
    } else {
        (executable, args)
    };

    if !args.is_empty() {
        args = args
            .into_iter()
            .map(|arg| normalize_arg(&arg, standalone))
            .collect();
    }

    (
        executable,
        args,
        if env.is_empty() { None } else { Some(env) },
    )
}

fn source_cwd(working_dir: Option<&str>, layout: SourceLayoutHint) -> String {
    match working_dir.map(str::trim).filter(|value| !value.is_empty()) {
        Some(dir) => {
            let normalized = dir.trim_start_matches("./").trim_matches('/');
            if normalized.is_empty() || normalized == "." {
                if matches!(layout, SourceLayoutHint::AnchoredEntrypoint) {
                    ".".to_string()
                } else {
                    "source".to_string()
                }
            } else if matches!(layout, SourceLayoutHint::AnchoredEntrypoint) {
                normalized.to_string()
            } else {
                format!("source/{normalized}")
            }
        }
        None => {
            if matches!(layout, SourceLayoutHint::AnchoredEntrypoint) {
                ".".to_string()
            } else {
                "source".to_string()
            }
        }
    }
}

fn source_layout_hint(value: Option<&str>) -> SourceLayoutHint {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some("anchored_entrypoint") => SourceLayoutHint::AnchoredEntrypoint,
        _ => SourceLayoutHint::Standard,
    }
}

fn normalized_source_entrypoint(program: &str, layout: SourceLayoutHint) -> String {
    if matches!(layout, SourceLayoutHint::AnchoredEntrypoint) {
        normalize_arg(program, true)
    } else {
        program.to_string()
    }
}

pub(super) fn validate_config_json(config: &ConfigJson) -> Result<()> {
    let schema_json: serde_json::Value =
        serde_json::from_str(include_str!("../../../schema/config-schema.json")).map_err(|e| {
            CapsuleError::Config(format!("Failed to parse embedded config schema: {}", e))
        })?;
    let schema_json = Box::leak(Box::new(schema_json));

    let compiled = JSONSchema::options()
        .with_draft(Draft::Draft7)
        .compile(schema_json)
        .map_err(|e| CapsuleError::Config(format!("Failed to compile config schema: {}", e)))?;

    let instance = serde_json::to_value(config)
        .map_err(|e| CapsuleError::Config(format!("Failed to convert config to JSON: {}", e)))?;
    if let Err(errors) = compiled.validate(&instance) {
        let details: Vec<String> = errors.map(|e| e.to_string()).collect();
        return Err(CapsuleError::Config(format!(
            "config.json schema validation failed: {}",
            details.join("; ")
        )));
    }

    Ok(())
}

fn read_command(manifest: &toml::Value) -> Option<String> {
    let target = selected_target_table(manifest)?;
    if let Some(cmd_str) = target.get("command").and_then(|c| c.as_str()) {
        let v = cmd_str.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    if let Some(cmd_arr) = target.get("cmd").and_then(|v| v.as_array()) {
        let parts: Vec<String> = cmd_arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect();
        if !parts.is_empty() {
            return Some(parts.join(" "));
        }
    }
    None
}

fn read_working_dir(manifest: &toml::Value) -> Option<&str> {
    selected_target_table(manifest)
        .and_then(|target| target.get("working_dir"))
        .and_then(toml::Value::as_str)
}

fn read_source_layout(manifest: &toml::Value) -> SourceLayoutHint {
    source_layout_hint(
        selected_target_table(manifest)
            .and_then(|target| target.get("source_layout"))
            .or_else(|| manifest.get("source_layout"))
            .and_then(toml::Value::as_str),
    )
}

fn read_run_command(manifest: &toml::Value) -> Option<String> {
    selected_target_table(manifest)
        .and_then(|target| target.get("run_command"))
        .and_then(|value| value.as_str())
        .or_else(|| manifest.get("run").and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn target_cmd_overrides_entrypoint(manifest: &toml::Value) -> bool {
    selected_target_table(manifest)
        .and_then(|target| target.get("cmd"))
        .and_then(|value| value.as_array())
        .map(|cmd| !cmd.is_empty())
        .unwrap_or(false)
}

fn read_entrypoint(manifest: &toml::Value) -> Result<String> {
    if let Some(entrypoint) = selected_target_table(manifest)
        .and_then(|t| t.get("entrypoint"))
        .and_then(|e| e.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return Ok(entrypoint.to_string());
    }

    // v0.3 manifests carry the execution string in `run_command` and may
    // omit `entrypoint` entirely. Treat run_command as the entrypoint when
    // it is a single bare token (the build_config_json caller routes
    // multi-token / shell-style values to resolve_shell_command first).
    if let Some(run_command) = read_run_command(manifest) {
        let trimmed = run_command.trim();
        if !trimmed.is_empty() && !trimmed.contains(char::is_whitespace) {
            return Ok(trimmed.to_string());
        }
    }

    Err(CapsuleError::Config(
        "No entrypoint defined in capsule.toml".to_string(),
    ))
}

/// Decide whether a v0.3 `run_command` value should be treated as an
/// interpreter entrypoint or local binary (routed through `resolve_command`)
/// instead of a shell-style command (routed through `sh -c`).
///
/// We treat it as an entrypoint when:
/// - it has no shell metacharacters AND one of:
///   - the manifest declares a known interpreter language;
///   - the (single) token's extension implies an interpreter;
///   - the (single) token is a local binary path (`./...`, `/abs/...`,
///     `runtime/...`);
/// - OR its first whitespace-separated token is a known interpreter binary
///   (python/python3/node/nodejs/deno/bun) — in which case the rest of the
///   command line is passed through verbatim.
fn run_command_should_be_entrypoint(command: &str, manifest: &toml::Value) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains(['$', '|', '&', ';', '`', '(', ')', '<', '>']) {
        return false;
    }
    if let Some(first_token) = trimmed.split_whitespace().next() {
        if detect_language_from_program(first_token).is_some() {
            return true;
        }
    }
    if trimmed.contains(char::is_whitespace) {
        return false;
    }
    if let Some(language) = read_language(manifest) {
        if matches!(language.as_str(), "python" | "node" | "deno" | "bun") {
            return true;
        }
    }
    if detect_language_from_entrypoint(trimmed).is_some() {
        return true;
    }
    trimmed.starts_with("./") || trimmed.starts_with('/') || trimmed.starts_with("runtime/")
}

fn resolve_shell_command(command: &str, manifest: &toml::Value) -> CommandResolution {
    let env = merged_manifest_env(manifest);
    let signals = execution_signals(manifest);

    // `npm:binary` is a package-binary shorthand (e.g. "npm:vite --host 0.0.0.0").
    // Pass it through as-is so spawn_main_service can resolve it via node_modules/.bin/.
    // Wrapping it in `sh -c` would cause "/bin/sh: npm:vite: command not found".
    if let Some(rest) = command.strip_prefix("npm:") {
        let mut parts = rest.splitn(2, ' ');
        let bin = parts.next().unwrap_or(rest);
        let args: Vec<String> = parts
            .next()
            .map(|tail| tail.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        return (
            format!("npm:{bin}"),
            args,
            if env.is_empty() { None } else { Some(env) },
            signals,
        );
    }

    (
        "sh".to_string(),
        vec!["-c".to_string(), command.to_string()],
        if env.is_empty() { None } else { Some(env) },
        signals,
    )
}

fn read_health_check(manifest: &toml::Value) -> Option<HealthCheck> {
    let target = selected_target_table(manifest)?;
    let http_get = target
        .get("health_check")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let port = target
        .get("port")
        .or_else(|| manifest.get("port"))
        .and_then(|v| v.as_integer())
        .map(|p| p.to_string());

    if http_get.is_none() || port.is_none() {
        return None;
    }

    Some(HealthCheck {
        http_get,
        tcp_connect: None,
        port: port?,
        interval_secs: None,
        timeout_secs: None,
    })
}

fn selected_target_table(manifest: &toml::Value) -> Option<&toml::value::Table> {
    let default_target = manifest
        .get("default_target")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())?;
    manifest
        .get("targets")
        .and_then(|v| v.as_table())
        .and_then(|targets| targets.get(default_target))
        .and_then(|v| v.as_table())
}

fn read_filesystem(manifest: &toml::Value) -> Option<FilesystemConfig> {
    let fs = manifest
        .get("sandbox")
        .and_then(|s| s.get("filesystem"))
        .or_else(|| {
            manifest
                .get("isolation")
                .and_then(|i| i.get("sandbox"))
                .and_then(|s| s.get("filesystem"))
        })?;

    let read_only = fs.get("read_only").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect::<Vec<String>>()
    });

    let read_write = fs.get("read_write").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect::<Vec<String>>()
    });

    if read_only.is_none() && read_write.is_none() {
        return None;
    }

    Some(FilesystemConfig {
        read_only,
        read_write,
    })
}

fn build_egress(manifest: &toml::Value) -> Result<(Option<EgressConfig>, Vec<String>)> {
    let mut allow_domains: Vec<String> = manifest
        .get("network")
        .and_then(|n| n.get("egress_allow"))
        .or_else(|| manifest.get("sandbox").and_then(|s| s.get("egress_allow")))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    allow_domains.sort();
    allow_domains.dedup();
    let has_allow_domains = !allow_domains.is_empty();
    let resolved = if allow_domains.is_empty() {
        None
    } else {
        Some(resolve_egress_policy(&allow_domains)?)
    };

    let mut rules: Vec<EgressRuleEntry> = Vec::new();
    let mut seen_ips: HashSet<String> = HashSet::new();
    let mut seen_cidrs: HashSet<String> = HashSet::new();

    if let Some(resolved) = resolved {
        for ip in resolved.resolved_ips {
            if seen_ips.insert(ip.clone()) {
                rules.push(EgressRuleEntry {
                    rule_type: "ip".to_string(),
                    value: ip,
                });
            }
        }

        for rule in resolved.rules {
            if let EgressRule::Cidr { value } = rule {
                if seen_cidrs.insert(value.clone()) {
                    rules.push(EgressRuleEntry {
                        rule_type: "cidr".to_string(),
                        value,
                    });
                }
            }
        }
    }

    let mut has_id_allow = false;
    if let Some(id_allow) = manifest
        .get("network")
        .and_then(|n| n.get("egress_id_allow"))
        .and_then(|v| v.as_array())
    {
        if !id_allow.is_empty() {
            has_id_allow = true;
        }
        for rule in id_allow {
            let rule_type = rule.get("type").and_then(|v| v.as_str());
            let value = rule.get("value").and_then(|v| v.as_str());
            match (rule_type, value) {
                (Some("ip"), Some(val)) if seen_ips.insert(val.to_string()) => {
                    rules.push(EgressRuleEntry {
                        rule_type: "ip".to_string(),
                        value: val.to_string(),
                    });
                }
                (Some("cidr"), Some(val)) if seen_cidrs.insert(val.to_string()) => {
                    rules.push(EgressRuleEntry {
                        rule_type: "cidr".to_string(),
                        value: val.to_string(),
                    });
                }
                _ => {}
            }
        }
    }

    if rules.is_empty() && !has_allow_domains && !has_id_allow {
        return Ok((None, allow_domains));
    }

    let egress = EgressConfig {
        mode: "allowlist".to_string(),
        rules: if rules.is_empty() { None } else { Some(rules) },
    };

    Ok((Some(egress), allow_domains))
}

fn build_sidecar_config(
    manifest: &toml::Value,
    allow_domains: &[String],
) -> Result<Option<SidecarConfig>> {
    let network = match manifest.get("network") {
        Some(n) => n.clone(),
        None => return Ok(None),
    };

    let tsnet_enabled = network
        .get("tsnet")
        .and_then(|t| t.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !tsnet_enabled {
        return Ok(None);
    }

    let control_url = network
        .get("tsnet")
        .and_then(|t| t.get("control_url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();

    let auth_key = network
        .get("tsnet")
        .and_then(|t| t.get("auth_key"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();

    let hostname = network
        .get("tsnet")
        .and_then(|t| t.get("hostname"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            manifest
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| format!("{}-tsnet", s))
                .unwrap_or_else(|| "capsule-tsnet".to_string())
        });

    let socks_port = network
        .get("tsnet")
        .and_then(|t| t.get("socks_port"))
        .and_then(|v| v.as_integer())
        .map(|p| p as u16)
        .unwrap_or(0);

    let mut allow_net: Vec<String> = allow_domains.to_vec();

    if let Some(id_allow) = manifest
        .get("network")
        .and_then(|n| n.get("egress_id_allow"))
        .and_then(|v| v.as_array())
    {
        for rule in id_allow {
            let rule_type = rule.get("type").and_then(|v| v.as_str());
            let value = rule.get("value").and_then(|v| v.as_str());
            match (rule_type, value) {
                (Some("cidr"), Some(val)) => {
                    allow_net.push(val.to_string());
                }
                (Some("ip"), Some(val)) => {
                    allow_net.push(val.to_string());
                }
                _ => {}
            }
        }
    }

    let tsnet_config = TsnetSidecarConfig {
        enabled: true,
        control_url,
        auth_key,
        hostname,
        socks_port,
        allow_net,
    };

    Ok(Some(SidecarConfig {
        tsnet: tsnet_config,
    }))
}

fn resolve_command(
    entrypoint: &str,
    command: Option<&str>,
    manifest: &toml::Value,
    standalone: bool,
    layout: SourceLayoutHint,
) -> CommandResolution {
    let (program, tokens) = command_tokens(entrypoint, command);

    let language = read_language(manifest)
        .or_else(|| detect_language_from_program(&program))
        .or_else(|| detect_language_from_entrypoint(entrypoint));

    let mut env = merged_manifest_env(manifest);
    let (executable, mut args) = resolve_language_command(
        &program,
        &tokens,
        LanguageCommandContext {
            standalone,
            env: &mut env,
            language: language.as_deref(),
            runtime_version: read_target_runtime_version(manifest).as_deref(),
            synthesize_default_args: true,
            layout,
        },
    );

    if !args.is_empty() {
        args = args
            .into_iter()
            .map(|a| normalize_arg(&a, standalone))
            .collect();
    }

    (
        executable,
        args,
        if env.is_empty() { None } else { Some(env) },
        execution_signals(manifest),
    )
}

fn read_language(manifest: &toml::Value) -> Option<String> {
    if let Some(language) = selected_target_table(manifest)
        .and_then(|t| t.get("language"))
        .and_then(|l| l.as_str())
        .map(normalize_language)
    {
        return Some(language);
    }
    // Flat v0.3 manifests carry the driver inside `runtime = "source/<driver>"`.
    // The bridge's manifest_value() exposes the un-normalized tree, so derive
    // the language from the runtime selector when no targets table exists yet.
    let driver = selected_target_table(manifest)
        .and_then(|t| t.get("driver"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            manifest
                .get("runtime")
                .and_then(|v| v.as_str())
                .and_then(|selector| selector.split_once('/').map(|(_, driver)| driver))
        })?;
    let driver = driver.trim().to_ascii_lowercase();
    if matches!(driver.as_str(), "python" | "node" | "deno" | "bun") {
        Some(driver)
    } else {
        None
    }
}

fn detect_language_from_program(program: &str) -> Option<String> {
    match program {
        "python" | "python3" => Some("python".to_string()),
        "node" | "nodejs" => Some("node".to_string()),
        "deno" => Some("deno".to_string()),
        "bun" => Some("bun".to_string()),
        _ => None,
    }
}

fn detect_language_from_entrypoint(entrypoint: &str) -> Option<String> {
    let lower = entrypoint.to_ascii_lowercase();
    if lower.ends_with(".py") {
        return Some("python".to_string());
    }
    if lower.ends_with(".js") || lower.ends_with(".mjs") || lower.ends_with(".cjs") {
        return Some("node".to_string());
    }
    if lower.ends_with(".ts") || lower.ends_with(".tsx") {
        return Some("bun".to_string());
    }
    None
}

fn resolve_explicit_cmd_override(
    runtime_cmd: &[String],
    runtime_version: Option<&str>,
    standalone: bool,
    env: &mut HashMap<String, String>,
    layout: SourceLayoutHint,
) -> Option<(String, Vec<String>)> {
    let program = runtime_cmd.first()?.trim().to_string();
    if program.is_empty() {
        return None;
    }

    Some(resolve_language_command(
        &program,
        runtime_cmd,
        LanguageCommandContext {
            standalone,
            env,
            language: detect_language_from_program(&program).as_deref(),
            runtime_version,
            synthesize_default_args: false,
            layout,
        },
    ))
}

fn insert_uv_python_runtime_env(env: &mut HashMap<String, String>, runtime_version: Option<&str>) {
    extend_python_selector_env(env, runtime_version);
}

fn read_target_runtime_version(manifest: &toml::Value) -> Option<String> {
    selected_target_table(manifest)
        .and_then(|target| target.get("runtime_version"))
        .or_else(|| manifest.get("runtime_version"))
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalize_language(lang: &str) -> String {
    match lang.trim().to_ascii_lowercase().as_str() {
        "python3" => "python".to_string(),
        "nodejs" => "node".to_string(),
        other => other.to_string(),
    }
}

fn normalize_program(program: &str, _standalone: bool) -> String {
    let p = program.trim();
    if p.is_empty() {
        return program.to_string();
    }

    if p.starts_with('/') || p.starts_with("runtime/") {
        return p.to_string();
    }

    if p.starts_with("./") {
        return p.trim_start_matches("./").to_string();
    }

    if p.starts_with("source/") {
        return p.to_string();
    }

    if p.contains('/') || p.contains('.') {
        return p.to_string();
    }

    p.to_string()
}

fn normalize_arg(arg: &str, standalone: bool) -> String {
    let a = arg.trim();
    if a.is_empty() || a.starts_with('-') {
        return arg.to_string();
    }

    if a.starts_with("source/") || a.starts_with("runtime/") || a.starts_with('/') {
        return a.to_string();
    }

    if a.starts_with("./") {
        let normalized = a.trim_start_matches("./");
        if standalone {
            return format!("source/{}", normalized);
        }
        return normalized.to_string();
    }

    if a.contains('/')
        || a.ends_with(".py")
        || a.ends_with(".js")
        || a.ends_with(".mjs")
        || a.ends_with(".cjs")
        || a.ends_with(".ts")
        || a.ends_with(".tsx")
        || a.ends_with(".wasm")
    {
        if standalone {
            return format!("source/{a}");
        }
        return a.to_string();
    }

    a.to_string()
}

fn merge_envs(
    base: Option<HashMap<String, String>>,
    extra: Option<HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    let mut out = base.unwrap_or_default();
    if let Some(extra) = extra {
        out.extend(extra);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

pub(super) fn validate_services_dag(services: &HashMap<String, ServiceSpec>) -> Result<()> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut visiting: HashSet<String> = HashSet::new();

    fn visit(
        name: &str,
        services: &HashMap<String, ServiceSpec>,
        visited: &mut HashSet<String>,
        visiting: &mut HashSet<String>,
        stack: &mut Vec<String>,
    ) -> Result<()> {
        if visited.contains(name) {
            return Ok(());
        }
        if visiting.contains(name) {
            stack.push(name.to_string());
            return Err(CapsuleError::Config(format!(
                "Circular dependency detected: {}",
                stack.join(" -> ")
            )));
        }

        let spec = services.get(name).ok_or_else(|| {
            CapsuleError::Config(format!("Unknown service '{}' (depends_on)", name))
        })?;

        visiting.insert(name.to_string());
        stack.push(name.to_string());

        if let Some(deps) = &spec.depends_on {
            for dep in deps {
                if !services.contains_key(dep) {
                    return Err(CapsuleError::Config(format!(
                        "Service '{}' depends on unknown service '{}'",
                        name, dep
                    )));
                }
                visit(dep, services, visited, visiting, stack)?;
            }
        }

        stack.pop();
        visiting.remove(name);
        visited.insert(name.to_string());
        Ok(())
    }

    let mut names: Vec<&String> = services.keys().collect();
    names.sort();
    for name in names {
        let mut stack = Vec::new();
        visit(name, services, &mut visited, &mut visiting, &mut stack)?;
    }

    Ok(())
}

use anyhow::{Context, Result};
use jsonschema::{Draft, JSONSchema};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ato_lock::AtoLock;
use crate::common::paths::workspace_derived_dir;
use crate::lock_runtime::{LockCompilerOverlay, LockServiceUnit, ResolvedLockRuntimeModel};
use crate::manifest;
use crate::policy::egress_resolver::{resolve_egress_policy, EgressRule};
use crate::router::{self, CompatProjectInput, ExecutionProfile};
use crate::types::{ResolvedService, ResolvedServiceRuntime, ResolvedTargetRuntime};

const CONFIG_VERSION: &str = "1.0.0";
const CONFIG_FILE_NAME: &str = "config.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigJson {
    pub version: String,
    pub services: HashMap<String, ServiceSpec>,
    pub sandbox: SandboxConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MetadataConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidecar: Option<SidecarConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceSpec {
    pub executable: String,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<UserConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signals: Option<SignalsConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ports: Option<HashMap<String, u16>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalsConfig {
    pub stop: String,
    pub kill: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_get: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_connect: Option<String>,
    pub port: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FilesystemConfig>,
    pub network: NetworkConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub development_mode: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_write: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub enabled: bool,
    pub enforcement: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress: Option<EgressConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressConfig {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<EgressRuleEntry>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressRuleEntry {
    #[serde(rename = "type")]
    pub rule_type: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_manifest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub umask: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarConfig {
    pub tsnet: TsnetSidecarConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TsnetSidecarConfig {
    pub enabled: bool,
    pub control_url: String,
    pub auth_key: String,
    pub hostname: String,
    pub socks_port: u16,
    pub allow_net: Vec<String>,
}

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

pub fn generate_and_write_config(
    manifest_path: &Path,
    enforcement_override: Option<String>,
    standalone: bool,
) -> Result<PathBuf> {
    let config = generate_config(manifest_path, enforcement_override, standalone)?;
    write_config(manifest_path, &config)
}

pub fn generate_config(
    manifest_path: &Path,
    enforcement_override: Option<String>,
    standalone: bool,
) -> Result<ConfigJson> {
    let loaded = manifest::load_manifest(manifest_path)?;
    let bridge = crate::router::CompatManifestBridge::from_normalized_toml(loaded.raw_text)?;
    let workspace_root = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let compat_input = CompatProjectInput::from_bridge_with_label(
        workspace_root,
        manifest_path.display().to_string(),
        bridge,
    )?;
    let config = build_config_json(&compat_input, enforcement_override, standalone)?;
    validate_config_json(&config)?;
    Ok(config)
}

pub fn generate_config_from_compat_input(
    compat_input: &CompatProjectInput,
    enforcement_override: Option<String>,
    standalone: bool,
) -> Result<ConfigJson> {
    let config = build_config_json(compat_input, enforcement_override, standalone)?;
    validate_config_json(&config)?;
    Ok(config)
}

pub fn generate_config_from_lock(
    lock: &AtoLock,
    resolved: &ResolvedLockRuntimeModel,
    overlay: &LockCompilerOverlay,
    enforcement_override: Option<String>,
    standalone: bool,
) -> Result<ConfigJson> {
    let mut services = HashMap::new();
    for service in &resolved.services {
        services.insert(
            service.name.clone(),
            build_lock_service_spec(service, standalone)?,
        );
    }

    validate_services_dag(&services)?;

    let (egress, allow_domains) = build_lock_egress(
        resolved.network.as_ref(),
        overlay.network_allow_hosts.as_ref(),
    )?;
    let sandbox = SandboxConfig {
        enabled: true,
        filesystem: build_lock_filesystem(overlay),
        network: NetworkConfig {
            enabled: true,
            enforcement: enforcement_override.unwrap_or_else(|| "best_effort".to_string()),
            egress,
        },
        development_mode: None,
    };
    let metadata = MetadataConfig {
        name: resolved.metadata.name.clone(),
        version: resolved.metadata.version.clone(),
        generated_at: None,
        generated_by: Some(format!("ato-cli v{}", env!("CARGO_PKG_VERSION"))),
        source_manifest: lock
            .lock_id
            .as_ref()
            .map(|value| format!("lock_id:{}", value.as_str())),
    };

    let config = ConfigJson {
        version: CONFIG_VERSION.to_string(),
        services,
        sandbox,
        metadata: Some(metadata),
        annotations: None,
        sidecar: build_lock_sidecar_config(resolved.network.as_ref(), &allow_domains),
    };
    validate_config_json(&config)?;
    Ok(config)
}

pub fn write_config(manifest_path: &Path, config: &ConfigJson) -> Result<PathBuf> {
    let manifest_dir = manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    write_config_in_dir(&manifest_dir, config)
}

pub fn write_config_in_dir(output_dir: &Path, config: &ConfigJson) -> Result<PathBuf> {
    let derived_dir = workspace_derived_dir(output_dir);
    std::fs::create_dir_all(&derived_dir).with_context(|| {
        format!(
            "Failed to create derived config dir: {}",
            derived_dir.display()
        )
    })?;
    let output_path = derived_dir.join(CONFIG_FILE_NAME);

    let json = to_stable_json_pretty(config)?;
    std::fs::write(&output_path, json)
        .with_context(|| format!("Failed to write config.json: {}", output_path.display()))?;

    Ok(output_path)
}

fn config_output_path(workspace_root: &Path) -> PathBuf {
    workspace_derived_dir(workspace_root).join(CONFIG_FILE_NAME)
}

pub(crate) fn resolve_existing_config_path(workspace_root: &Path) -> Option<PathBuf> {
    let primary = config_output_path(workspace_root);
    if primary.exists() {
        return Some(primary);
    }

    let legacy = workspace_root.join(CONFIG_FILE_NAME);
    legacy.exists().then_some(legacy)
}

fn to_stable_json_pretty<T: Serialize>(value: &T) -> Result<String> {
    let mut json = serde_json::to_value(value).context("Failed to serialize config.json")?;
    sort_json_object_keys(&mut json);
    serde_json::to_string_pretty(&json).context("Failed to serialize config.json")
}

fn sort_json_object_keys(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, serde_json::Value)> = std::mem::take(map)
                .into_iter()
                .map(|(k, mut v)| {
                    sort_json_object_keys(&mut v);
                    (k, v)
                })
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (key, value) in entries {
                map.insert(key, value);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                sort_json_object_keys(item);
            }
        }
        _ => {}
    }
}

fn build_config_json(
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
            resolve_shell_command(&run_command, manifest)
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
                anyhow::bail!(
                    "services.main conflicts with execution entrypoint; remove services.main or execution"
                );
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
    .with_context(|| {
        format!(
            "Failed to resolve target-based services from {}",
            compat_input.logical_source_label()
        )
    })?;
    let orchestration = decision.resolve_services()?;
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
            anyhow::bail!(
                "target-based config.json generation does not yet support runtime=oci services (service='{}', target='{}')",
                service.name,
                runtime.target
            )
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

fn build_lock_service_spec(service: &LockServiceUnit, standalone: bool) -> Result<ServiceSpec> {
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

fn build_lock_filesystem(overlay: &LockCompilerOverlay) -> Option<FilesystemConfig> {
    if overlay.filesystem_read_only.is_none() && overlay.filesystem_read_write.is_none() {
        return None;
    }

    Some(FilesystemConfig {
        read_only: overlay.filesystem_read_only.clone(),
        read_write: overlay.filesystem_read_write.clone(),
    })
}

fn build_lock_egress(
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

fn build_lock_sidecar_config(
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

fn resolve_language_command(
    program: &str,
    tokens: &[String],
    standalone: bool,
    env: &mut HashMap<String, String>,
    language: Option<&str>,
    synthesize_default_args: bool,
    layout: SourceLayoutHint,
) -> (String, Vec<String>) {
    let args = tokens.get(1..).unwrap_or(&[]).to_vec();
    let default_program_arg = normalized_source_entrypoint(program, layout);
    match language {
        Some("python") => {
            let args = if synthesize_default_args && args.is_empty() {
                vec![default_program_arg.clone()]
            } else {
                args
            };
            env.insert("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string());
            if standalone {
                env.insert("PYTHONHOME".to_string(), "runtime/python".to_string());
                env.insert("PYTHONPATH".to_string(), "source".to_string());
                ("runtime/python/bin/python3".to_string(), args)
            } else {
                let mut uv_args = vec![
                    "run".to_string(),
                    "--offline".to_string(),
                    "python3".to_string(),
                ];
                uv_args.extend(args);
                ("uv".to_string(), uv_args)
            }
        }
        Some("node") => {
            let args = if synthesize_default_args && args.is_empty() {
                vec![default_program_arg.clone()]
            } else {
                args
            };
            if standalone {
                ("runtime/node/bin/node".to_string(), args)
            } else {
                ("node".to_string(), args)
            }
        }
        Some("deno") => {
            let args = if synthesize_default_args && args.is_empty() {
                vec![
                    "run".to_string(),
                    "-A".to_string(),
                    default_program_arg.clone(),
                ]
            } else {
                args
            };
            if standalone {
                ("runtime/deno/bin/deno".to_string(), args)
            } else {
                ("deno".to_string(), args)
            }
        }
        Some("bun") => {
            let args = if synthesize_default_args && args.is_empty() {
                vec![default_program_arg]
            } else {
                args
            };
            if standalone {
                ("runtime/bun/bin/bun".to_string(), args)
            } else {
                ("bun".to_string(), args)
            }
        }
        _ => (
            normalize_program(program, standalone),
            tokens.get(1..).unwrap_or(&[]).to_vec(),
        ),
    }
}

fn merged_manifest_env(manifest: &toml::Value) -> HashMap<String, String> {
    let mut env = selected_target_table(manifest)
        .and_then(|t| t.get("env"))
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

fn resolve_target_command(
    runtime: &ResolvedTargetRuntime,
    standalone: bool,
    layout: SourceLayoutHint,
) -> (String, Vec<String>, Option<HashMap<String, String>>) {
    if let Some(run_command) = runtime
        .run_command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
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

    let entrypoint = runtime.entrypoint.as_str();
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
        standalone,
        &mut env,
        language.as_deref(),
        true,
        layout,
    );

    let (executable, mut args) = if let Some((cmd_executable, cmd_args)) =
        resolve_explicit_cmd_override(&runtime.cmd, standalone, &mut env, layout)
    {
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

fn validate_config_json(config: &ConfigJson) -> Result<()> {
    let schema_json: serde_json::Value =
        serde_json::from_str(include_str!("../schema/config-schema.json"))
            .context("Failed to parse embedded config schema")?;
    let schema_json = Box::leak(Box::new(schema_json));

    let compiled = JSONSchema::options()
        .with_draft(Draft::Draft7)
        .compile(schema_json)
        .context("Failed to compile config schema")?;

    let instance = serde_json::to_value(config).context("Failed to convert config to JSON")?;
    if let Err(errors) = compiled.validate(&instance) {
        let details: Vec<String> = errors.map(|e| e.to_string()).collect();
        anyhow::bail!(
            "config.json schema validation failed: {}",
            details.join("; ")
        );
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
            .and_then(toml::Value::as_str),
    )
}

fn read_run_command(manifest: &toml::Value) -> Option<String> {
    selected_target_table(manifest)
        .and_then(|target| target.get("run_command"))
        .and_then(|value| value.as_str())
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
    let entrypoint = selected_target_table(manifest)
        .and_then(|t| t.get("entrypoint"))
        .and_then(|e| e.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("No entrypoint defined in capsule.toml"))?;

    Ok(entrypoint.to_string())
}

fn resolve_shell_command(command: &str, manifest: &toml::Value) -> CommandResolution {
    let env = merged_manifest_env(manifest);

    (
        "sh".to_string(),
        vec!["-c".to_string(), command.to_string()],
        if env.is_empty() { None } else { Some(env) },
        execution_signals(manifest),
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
                (Some("ip"), Some(val)) => {
                    if seen_ips.insert(val.to_string()) {
                        rules.push(EgressRuleEntry {
                            rule_type: "ip".to_string(),
                            value: val.to_string(),
                        });
                    }
                }
                (Some("cidr"), Some(val)) => {
                    if seen_cidrs.insert(val.to_string()) {
                        rules.push(EgressRuleEntry {
                            rule_type: "cidr".to_string(),
                            value: val.to_string(),
                        });
                    }
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
        standalone,
        &mut env,
        language.as_deref(),
        true,
        layout,
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
    selected_target_table(manifest)
        .and_then(|t| t.get("language"))
        .and_then(|l| l.as_str())
        .map(normalize_language)
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
        standalone,
        env,
        detect_language_from_program(&program).as_deref(),
        false,
        layout,
    ))
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

fn validate_services_dag(services: &HashMap<String, ServiceSpec>) -> Result<()> {
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
            anyhow::bail!("Circular dependency detected: {}", stack.join(" -> "));
        }

        let spec = services
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Unknown service '{}' (depends_on)", name))?;

        visiting.insert(name.to_string());
        stack.push(name.to_string());

        if let Some(deps) = &spec.depends_on {
            for dep in deps {
                if !services.contains_key(dep) {
                    anyhow::bail!("Service '{}' depends on unknown service '{}'", name, dep);
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

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ato_lock::AtoLock;
    use crate::lock_runtime::{resolve_lock_runtime_model, LockCompilerOverlay};
    use serde_json::json;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn sample_lock() -> AtoLock {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "metadata".to_string(),
            json!({"name": "svc-demo", "version": "0.1.0", "default_target": "api"}),
        );
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "api.ts", "cmd": ["deno", "run", "api.ts"]}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([
                {
                    "name": "main",
                    "target": "api",
                    "process": {"entrypoint": "api.ts", "cmd": ["deno", "run", "api.ts"]},
                    "depends_on": ["worker"],
                    "readiness_probe": {"http_get": "/healthz", "port": "8080"}
                },
                {
                    "name": "worker",
                    "target": "worker",
                    "process": {"entrypoint": "worker.ts", "cmd": ["deno", "run", "worker.ts"]}
                }
            ]),
        );
        lock.contract.entries.insert(
            "network".to_string(),
            json!({"egress_allow": ["registry.npmjs.org"]}),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "selected_target": "api"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([
                {
                    "label": "api",
                    "runtime": "source",
                    "driver": "deno",
                    "entrypoint": "api.ts",
                    "cmd": ["deno", "run", "api.ts"],
                    "port": 8080
                },
                {
                    "label": "worker",
                    "runtime": "source",
                    "driver": "deno",
                    "entrypoint": "worker.ts",
                    "cmd": ["deno", "run", "worker.ts"]
                }
            ]),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({"kind": "metadata_only", "status": "incomplete"}),
        );
        lock
    }

    #[test]
    fn generates_valid_config_json() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "demo"
    version = "0.1.0"
    type = "app"
    default_target = "cli"

    [targets.cli]
    runtime = "source"
    language = "python"
    entrypoint = "main.py"

[targets.cli.env]
MODEL = "demo"

[network]
egress_allow = ["1.1.1.1"]
"#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, false).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: serde_json::Value = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(config["version"], CONFIG_VERSION);
        assert!(config["services"].get("main").is_some());
        assert!(config["sandbox"]["network"]["egress"].is_object());
    }

    #[test]
    fn test_python_app_config_generation() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "python-demo"
    version = "0.1.0"
    type = "app"
    default_target = "cli"

    [targets.cli]
    runtime = "source"
    language = "python"
    version = "3.11"
    entrypoint = "main.py"

    [targets.cli.env]
    PORT = "8080"
    "#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, true).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: ConfigJson = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(
            config.services["main"].executable,
            "runtime/python/bin/python3"
        );
        assert_eq!(config.services["main"].args, vec!["source/main.py"]);
        assert_eq!(config.services["main"].cwd, Some("source".to_string()));
        assert_eq!(
            config.services["main"].env.as_ref().unwrap()["PYTHONHOME"],
            "runtime/python"
        );
        assert_eq!(
            config.services["main"].env.as_ref().unwrap()["PYTHONPATH"],
            "source"
        );
        assert_eq!(
            config.services["main"].env.as_ref().unwrap()["PORT"],
            "8080"
        );
    }

    #[test]
    fn generated_config_is_written_under_workspace_derived_dir() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "static-demo"
    version = "0.1.0"
    type = "app"
    default_target = "site"

    [targets.site]
    runtime = "web"
    driver = "static"
    entrypoint = "dist"
    port = 4173
    "#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, false).unwrap();
        assert!(config_path.ends_with(".ato/derived/config.json"));
        assert!(!tmp.path().join("config.json").exists());
    }

    #[test]
    fn test_python_app_config_generation_thin() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "python-demo"
    version = "0.1.0"
    type = "app"
    default_target = "cli"

    [targets.cli]
    runtime = "source"
    language = "python"
    version = "3.11"
    entrypoint = "main.py"

    [targets.cli.env]
    PORT = "8080"
    "#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, false).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: ConfigJson = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(config.services["main"].executable, "uv");
        assert_eq!(
            config.services["main"].args,
            vec!["run", "--offline", "python3", "main.py"]
        );
        assert_eq!(config.services["main"].cwd, Some("source".to_string()));
        assert_eq!(
            config.services["main"].env.as_ref().unwrap()["PYTHONDONTWRITEBYTECODE"],
            "1"
        );
        assert_eq!(
            config.services["main"].env.as_ref().unwrap()["PORT"],
            "8080"
        );
    }

    #[test]
    fn test_single_script_python_config_anchors_entrypoint_for_workspace_cwd() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "python-single-script"
    version = "0.1.0"
    type = "job"
    default_target = "cli"

    [targets.cli]
    runtime = "source"
    language = "python"
    entrypoint = "main.py"
    source_layout = "anchored_entrypoint"
    "#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, false).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: ConfigJson = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(config.services["main"].executable, "uv");
        assert_eq!(
            config.services["main"].args,
            vec!["run", "--offline", "python3", "source/main.py"]
        );
        assert_eq!(config.services["main"].cwd, Some(".".to_string()));
    }

    #[test]
    fn test_node_app_config_generation() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "node-demo"
    version = "0.1.0"
    type = "app"
    default_target = "cli"

    [targets.cli]
    runtime = "source"
    language = "node"
    version = "20"
    entrypoint = "index.js"
    "#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, true).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: ConfigJson = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(config.services["main"].executable, "runtime/node/bin/node");
        assert_eq!(config.services["main"].args, vec!["source/index.js"]);
        assert_eq!(config.services["main"].cwd, Some("source".to_string()));
    }

    #[test]
    fn test_node_app_config_generation_thin() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "node-demo"
    version = "0.1.0"
    type = "app"
    default_target = "cli"

    [targets.cli]
    runtime = "source"
    language = "node"
    version = "20"
    entrypoint = "index.js"
    "#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, false).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: ConfigJson = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(config.services["main"].executable, "node");
        assert_eq!(config.services["main"].args, vec!["index.js"]);
        assert_eq!(config.services["main"].cwd, Some("source".to_string()));
    }

    #[test]
    fn test_target_based_services_config_generation() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
schema_version = "0.2"
name = "svc-demo"
version = "0.1.0"
type = "app"
default_target = "dashboard"

[services.main]
target = "dashboard"
depends_on = ["control_plane"]
readiness_probe = { http_get = "/", port = "4173" }

[services.control_plane]
target = "control_plane"
readiness_probe = { http_get = "/healthz", port = "8081" }

[targets.dashboard]
runtime = "web"
driver = "node"
runtime_version = "20.11.0"
entrypoint = "apps/dashboard/server.js"
port = 4173
working_dir = "."
env = { PORT = "4173" }

[targets.control_plane]
runtime = "source"
driver = "python"
runtime_version = "3.11.10"
entrypoint = "python -m uvicorn control_plane.modal_webhook:app --port 8081"
working_dir = "apps/control-plane"
port = 8081
env = { PYTHONPATH = "src" }
"#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, false).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: ConfigJson = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(config.services["main"].executable, "node");
        assert_eq!(
            config.services["main"].args,
            vec!["apps/dashboard/server.js"]
        );
        assert_eq!(config.services["main"].cwd, Some("source".to_string()));

        assert_eq!(config.services["control_plane"].executable, "uv");
        assert_eq!(
            config.services["control_plane"].args,
            vec![
                "run",
                "--offline",
                "python3",
                "-m",
                "uvicorn",
                "control_plane.modal_webhook:app",
                "--port",
                "8081"
            ]
        );
        assert_eq!(
            config.services["control_plane"].cwd,
            Some("source/apps/control-plane".to_string())
        );
    }

    #[test]
    fn test_v03_run_command_generates_shell_service() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
schema_version = "0.3"
name = "json-server"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "npx json-server --watch db.json --port $PORT"
port = 3000
"#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config = generate_config(&manifest_path, None, false).unwrap();
        assert_eq!(config.services["main"].executable, "sh");
        assert_eq!(
            config.services["main"].args,
            vec![
                "-c".to_string(),
                "npx json-server --watch db.json --port $PORT".to_string()
            ]
        );
    }

    #[test]
    fn test_deno_app_config_generation() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "deno-demo"
    version = "0.1.0"
    type = "app"
    default_target = "cli"

    [targets.cli]
    runtime = "source"
    language = "deno"
    version = "1.40"
    entrypoint = "server.ts"
    "#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, true).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: ConfigJson = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(config.services["main"].executable, "runtime/deno/bin/deno");
        assert_eq!(
            config.services["main"].args,
            vec!["run", "-A", "source/server.ts"]
        );
    }

    #[test]
    fn test_deno_app_config_generation_thin() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "deno-demo"
    version = "0.1.0"
    type = "app"
    default_target = "cli"

    [targets.cli]
    runtime = "source"
    language = "deno"
    version = "1.40"
    entrypoint = "server.ts"
    "#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, false).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: ConfigJson = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(config.services["main"].executable, "deno");
        assert_eq!(config.services["main"].args, vec!["run", "-A", "server.ts"]);
    }

    #[test]
    fn test_bun_app_config_generation() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "bun-demo"
    version = "0.1.0"
    type = "app"
    default_target = "cli"

    [targets.cli]
    runtime = "source"
    language = "bun"
    version = "1.1"
    entrypoint = "main.ts"
    "#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, true).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: ConfigJson = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(config.services["main"].executable, "runtime/bun/bin/bun");
        assert_eq!(config.services["main"].args, vec!["source/main.ts"]);
    }

    #[test]
    fn test_bun_app_config_generation_thin() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "bun-demo"
    version = "0.1.0"
    type = "app"
    default_target = "cli"

    [targets.cli]
    runtime = "source"
    language = "bun"
    version = "1.1"
    entrypoint = "main.ts"
    "#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, false).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: ConfigJson = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(config.services["main"].executable, "bun");
        assert_eq!(config.services["main"].args, vec!["main.ts"]);
    }

    #[test]
    fn test_custom_binary_config_generation() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "custom-demo"
    version = "0.1.0"
    type = "app"
    default_target = "cli"

    [targets.cli]
    runtime = "source"
    language = "binary"
    entrypoint = "./my-app"
    "#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, true).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: ConfigJson = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(config.services["main"].executable, "my-app");
    }

    #[test]
    fn test_explicit_cmd_overrides_bun_inference_for_typescript_entrypoint() {
        let tmp = tempdir().unwrap();
        let manifest_path = tmp.path().join("capsule.toml");

        let manifest = r#"
    schema_version = "0.2"
    name = "fresh-demo"
    version = "0.1.0"
    type = "app"
    default_target = "app"

    [targets.app]
    runtime = "source"
    entrypoint = "main.ts"
    cmd = ["deno", "run", "-A", "--no-lock", "--unstable-kv", "main.ts"]
    "#;

        std::fs::write(&manifest_path, manifest).unwrap();

        let config_path = generate_and_write_config(&manifest_path, None, false).unwrap();
        let config_raw = std::fs::read_to_string(config_path).unwrap();
        let config: ConfigJson = serde_json::from_str(&config_raw).unwrap();

        assert_eq!(config.services["main"].executable, "deno");
        assert_eq!(
            config.services["main"].args,
            vec!["run", "-A", "--no-lock", "--unstable-kv", "main.ts"]
        );
    }

    #[test]
    fn stable_json_serialization_sorts_hashmap_keys() {
        let mut left = HashMap::new();
        left.insert("z".to_string(), "last".to_string());
        left.insert("a".to_string(), "first".to_string());

        let mut right = HashMap::new();
        right.insert("a".to_string(), "first".to_string());
        right.insert("z".to_string(), "last".to_string());

        let mut left_services = HashMap::new();
        left_services.insert(
            "main".to_string(),
            ServiceSpec {
                executable: "echo".to_string(),
                args: vec!["ok".to_string()],
                cwd: Some("source".to_string()),
                env: Some(left),
                user: None,
                signals: None,
                depends_on: None,
                health_check: None,
                ports: None,
            },
        );

        let mut right_services = HashMap::new();
        right_services.insert(
            "main".to_string(),
            ServiceSpec {
                executable: "echo".to_string(),
                args: vec!["ok".to_string()],
                cwd: Some("source".to_string()),
                env: Some(right),
                user: None,
                signals: None,
                depends_on: None,
                health_check: None,
                ports: None,
            },
        );
        let left_config = ConfigJson {
            version: CONFIG_VERSION.to_string(),
            services: left_services,
            sandbox: SandboxConfig {
                enabled: true,
                filesystem: None,
                network: NetworkConfig {
                    enabled: true,
                    enforcement: "best_effort".to_string(),
                    egress: None,
                },
                development_mode: None,
            },
            metadata: Some(MetadataConfig {
                name: Some("demo".to_string()),
                version: Some("0.1.0".to_string()),
                generated_at: None,
                generated_by: Some("ato-cli".to_string()),
                source_manifest: Some("sha256:abc".to_string()),
            }),
            annotations: None,
            sidecar: None,
        };

        let right_config = ConfigJson {
            version: CONFIG_VERSION.to_string(),
            services: right_services,
            sandbox: SandboxConfig {
                enabled: true,
                filesystem: None,
                network: NetworkConfig {
                    enabled: true,
                    enforcement: "best_effort".to_string(),
                    egress: None,
                },
                development_mode: None,
            },
            metadata: Some(MetadataConfig {
                name: Some("demo".to_string()),
                version: Some("0.1.0".to_string()),
                generated_at: None,
                generated_by: Some("ato-cli".to_string()),
                source_manifest: Some("sha256:abc".to_string()),
            }),
            annotations: None,
            sidecar: None,
        };

        let left_json = to_stable_json_pretty(&left_config).expect("left json");
        let right_json = to_stable_json_pretty(&right_config).expect("right json");

        assert_eq!(left_json, right_json);
    }

    #[test]
    fn generate_config_from_lock_preserves_service_coherence() {
        let lock = sample_lock();
        let resolved = resolve_lock_runtime_model(&lock, Some("api")).expect("resolved");
        let config = generate_config_from_lock(
            &lock,
            &resolved,
            &LockCompilerOverlay::default(),
            None,
            false,
        )
        .expect("config");

        assert_eq!(config.services["main"].executable, "deno");
        assert_eq!(
            config.services["main"].depends_on.as_ref().unwrap(),
            &vec!["worker".to_string()]
        );
        assert_eq!(config.services["worker"].executable, "deno");
        assert_eq!(
            config
                .metadata
                .as_ref()
                .and_then(|value| value.name.as_deref()),
            Some("svc-demo")
        );
    }

    #[test]
    fn explicit_overlay_changes_lock_config_egress_without_manifest_inputs() {
        let lock = sample_lock();
        let resolved = resolve_lock_runtime_model(&lock, Some("api")).expect("resolved");
        let config = generate_config_from_lock(
            &lock,
            &resolved,
            &LockCompilerOverlay {
                network_allow_hosts: Some(vec!["example.com".to_string()]),
                ..LockCompilerOverlay::default()
            },
            None,
            false,
        )
        .expect("config");

        let rules = config
            .sandbox
            .network
            .egress
            .and_then(|value| value.rules)
            .unwrap_or_default();
        assert!(!rules.is_empty());
    }

    #[test]
    fn generate_config_from_compat_input_does_not_require_manifest_path() {
        let workspace = tempdir().expect("tempdir");
        let manifest_raw = r#"
schema_version = "0.2"
name = "compat-demo"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "node"
entrypoint = "index.js"
"#;
        let manifest_value: toml::Value = toml::from_str(manifest_raw).expect("manifest value");
        let bridge = crate::router::CompatManifestBridge::from_manifest_value(&manifest_value)
            .expect("bridge");
        let compat_input = crate::router::CompatProjectInput::from_bridge_with_label(
            workspace.path().to_path_buf(),
            "in-memory compat input".to_string(),
            bridge,
        )
        .expect("compat input");

        let config =
            generate_config_from_compat_input(&compat_input, Some("strict".to_string()), false)
                .expect("config");

        assert_eq!(config.services["main"].executable, "node");
        assert_eq!(config.services["main"].cwd.as_deref(), Some("source"));
        assert!(
            !workspace.path().join("capsule.toml").exists(),
            "compat input must not materialize a synthetic manifest path"
        );
    }
}

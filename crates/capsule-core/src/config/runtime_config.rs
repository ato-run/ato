use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{CapsuleError, Result};

use crate::ato_lock::AtoLock;
use crate::common::paths::{manifest_dir, workspace_derived_dir};
use crate::lock_runtime::{LockCompilerOverlay, ResolvedLockRuntimeModel};
use crate::manifest;
use crate::router::CompatProjectInput;

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

mod builder;

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
    let bridge = crate::router::CompatManifestBridge::from_normalized_toml(loaded.raw_text)
        .map_err(|e| CapsuleError::Manifest(manifest_path.to_path_buf(), e.to_string()))?;
    let workspace_root = manifest_dir(manifest_path);
    let compat_input = CompatProjectInput::from_bridge_with_label(
        workspace_root,
        manifest_path.display().to_string(),
        bridge,
    )
    .map_err(|e| CapsuleError::Manifest(manifest_path.to_path_buf(), e.to_string()))?;
    let config = builder::build_config_json(&compat_input, enforcement_override, standalone)?;
    builder::validate_config_json(&config)?;
    Ok(config)
}

pub fn generate_config_from_compat_input(
    compat_input: &CompatProjectInput,
    enforcement_override: Option<String>,
    standalone: bool,
) -> Result<ConfigJson> {
    let config = builder::build_config_json(compat_input, enforcement_override, standalone)?;
    builder::validate_config_json(&config)?;
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
            builder::build_lock_service_spec(service, standalone)?,
        );
    }

    builder::validate_services_dag(&services)?;

    let (egress, allow_domains) = builder::build_lock_egress(
        resolved.network.as_ref(),
        overlay.network_allow_hosts.as_ref(),
    )?;
    let sandbox = SandboxConfig {
        enabled: true,
        filesystem: builder::build_lock_filesystem(overlay),
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
        sidecar: builder::build_lock_sidecar_config(resolved.network.as_ref(), &allow_domains),
    };
    builder::validate_config_json(&config)?;
    Ok(config)
}

pub fn write_config(manifest_path: &Path, config: &ConfigJson) -> Result<PathBuf> {
    write_config_in_dir(&manifest_dir(manifest_path), config)
}

pub fn write_config_in_dir(output_dir: &Path, config: &ConfigJson) -> Result<PathBuf> {
    let derived_dir = workspace_derived_dir(output_dir);
    std::fs::create_dir_all(&derived_dir).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to create derived config dir '{}': {}",
            derived_dir.display(),
            e
        ))
    })?;
    let output_path = derived_dir.join(CONFIG_FILE_NAME);

    let json = to_stable_json_pretty(config)?;
    std::fs::write(&output_path, &json).map_err(|e| {
        CapsuleError::Pack(format!(
            "Failed to write config.json '{}': {}",
            output_path.display(),
            e
        ))
    })?;

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
    let mut json = serde_json::to_value(value)
        .map_err(|e| CapsuleError::Pack(format!("Failed to serialize config.json: {}", e)))?;
    sort_json_object_keys(&mut json);
    serde_json::to_string_pretty(&json)
        .map_err(|e| CapsuleError::Pack(format!("Failed to serialize config.json: {}", e)))
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

    fn sample_python_lock() -> AtoLock {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "metadata".to_string(),
            json!({"name": "python-demo", "version": "0.1.0", "default_target": "cli"}),
        );
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.py", "cmd": ["uv", "run", "python3", "main.py"]}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([
                {
                    "name": "main",
                    "target": "cli",
                    "process": {"entrypoint": "main.py", "cmd": ["uv", "run", "python3", "main.py"]}
                }
            ]),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "python", "selected_target": "cli"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([
                {
                    "label": "cli",
                    "runtime": "source",
                    "driver": "python",
                    "runtime_version": "3.11.10",
                    "entrypoint": "main.py"
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
    schema_version = "0.3"
    name = "demo"
    version = "0.1.0"
    type = "app"

    runtime = "source"
    MODEL = "demo"
    run = "main.py"
    [env]
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
    schema_version = "0.3"
    name = "python-demo"
    version = "0.1.0"
    type = "app"

    runtime = "source/python"
    runtime_version = "3.11.10"
    run = "main.py"
    [env]
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
    schema_version = "0.3"
    name = "static-demo"
    version = "0.1.0"
    type = "app"

    runtime = "web/static"
    port = 4173
    run = "dist""#;

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
    schema_version = "0.3"
    name = "python-demo"
    version = "0.1.0"
    type = "app"

    runtime = "source/python"
    runtime_version = "3.11.10"
    run = "main.py"
    [env]
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
            config.services["main"].env.as_ref().unwrap()["UV_MANAGED_PYTHON"],
            "1"
        );
        assert_eq!(
            config.services["main"].env.as_ref().unwrap()["UV_PYTHON"],
            "3.11.10"
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
    schema_version = "0.3"
    name = "python-single-script"
    version = "0.1.0"
    type = "job"

    runtime = "source"
    source_layout = "anchored_entrypoint"
    run = "main.py""#;

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
    schema_version = "0.3"
    name = "node-demo"
    version = "0.1.0"
    type = "app"

    runtime = "source/node"
    runtime_version = "20"
    run = "index.js""#;

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
    schema_version = "0.3"
    name = "node-demo"
    version = "0.1.0"
    type = "app"

    runtime = "source/node"
    runtime_version = "20"
    run = "index.js""#;

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
schema_version = "0.3"
name = "svc-demo"
version = "0.1.0"
type = "app"

default_target = "dashboard"

[targets.dashboard]
runtime = "web/node"
runtime_version = "20.11.0"
port = 4173
working_dir = "."
env = { PORT = "4173" }
run = "apps/dashboard/server.js"

[targets.control_plane]
runtime = "source/python"
runtime_version = "3.11.10"
working_dir = "apps/control-plane"
port = 8081
env = { PYTHONPATH = "src" }
run = "python -m uvicorn control_plane.modal_webhook:app --port 8081"
[services.main]
target = "dashboard"
depends_on = ["control_plane"]
readiness_probe = { http_get = "/", port = "4173" }

[services.control_plane]
target = "control_plane"
readiness_probe = { http_get = "/healthz", port = "8081" }
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
            config.services["control_plane"].env.as_ref().unwrap()["UV_MANAGED_PYTHON"],
            "1"
        );
        assert_eq!(
            config.services["control_plane"].env.as_ref().unwrap()["UV_PYTHON"],
            "3.11.10"
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
    schema_version = "0.3"
    name = "deno-demo"
    version = "0.1.0"
    type = "app"

    runtime = "source/deno"
    runtime_version = "1.40"
    run = "server.ts""#;

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
    schema_version = "0.3"
    name = "deno-demo"
    version = "0.1.0"
    type = "app"

    runtime = "source/deno"
    runtime_version = "1.40"
    run = "server.ts""#;

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

        // Bun is detected from the .ts entrypoint rather than from a driver
        // suffix, since `bun` is not part of the v0.3 driver allowlist
        // (static|deno|node|python|wasmtime|native).
        let manifest = r#"
    schema_version = "0.3"
    name = "bun-demo"
    version = "0.1.0"
    type = "app"

    runtime = "source"
    runtime_version = "1.1"
    run = "main.ts""#;

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
    schema_version = "0.3"
    name = "bun-demo"
    version = "0.1.0"
    type = "app"

    runtime = "source"
    runtime_version = "1.1"
    run = "main.ts""#;

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
    schema_version = "0.3"
    name = "custom-demo"
    version = "0.1.0"
    type = "app"

    runtime = "source"
    run = "./my-app""#;

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

        // v0.3 expresses an explicit interpreter command via `run = "<interp> <args>"`
        // — the first token is the interpreter (deno here), which short-circuits
        // the .ts → bun inference.
        let manifest = r#"
    schema_version = "0.3"
    name = "fresh-demo"
    version = "0.1.0"
    type = "app"

    runtime = "source"
    run = "deno run -A --no-lock --unstable-kv main.ts""#;

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
    fn generate_config_from_lock_derives_python_selector_env_from_runtime_version() {
        let lock = sample_python_lock();
        let resolved = resolve_lock_runtime_model(&lock, Some("cli")).expect("resolved");
        let config = generate_config_from_lock(
            &lock,
            &resolved,
            &LockCompilerOverlay::default(),
            None,
            false,
        )
        .expect("config");

        assert_eq!(config.services["main"].executable, "uv");
        assert_eq!(
            config.services["main"].env.as_ref().unwrap()["UV_MANAGED_PYTHON"],
            "1"
        );
        assert_eq!(
            config.services["main"].env.as_ref().unwrap()["UV_PYTHON"],
            "3.11.10"
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
schema_version = "0.3"
name = "compat-demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
run = "index.js""#;
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

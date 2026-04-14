use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashMap;

use super::{CapsuleError, CapsuleManifest, RuntimeType};

const DEFAULT_STORAGE_MOUNT_BASE: &str = "/var/lib/gumball/volumes";

fn default_storage_mount_base() -> String {
    std::env::var("GUMBALL_STORAGE_BASE").unwrap_or_else(|_| DEFAULT_STORAGE_MOUNT_BASE.to_string())
}

/// Normalized execution plan produced from manifests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunPlan {
    pub capsule_id: String,
    pub name: String,
    pub version: String,

    #[serde(flatten)]
    pub runtime: RunPlanRuntime,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_profile: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress_allowlist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunPlanRuntime {
    #[serde(rename = "docker")]
    Docker(DockerRuntime),
    #[serde(rename = "native")]
    Native(NativeRuntime),
    #[serde(rename = "source")]
    Source(SourceRuntime),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DockerRuntime {
    pub image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<Port>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<Mount>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeRuntime {
    pub binary_path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceRuntime {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub entrypoint: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cmd: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<Port>,
    #[serde(default)]
    pub dev_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Port {
    pub container_port: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_port: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mount {
    pub source: String,
    pub target: String,
    pub readonly: bool,
}

impl CapsuleManifest {
    /// Convert a validated manifest into a normalized RunPlan.
    pub fn to_run_plan(&self) -> Result<RunPlan, CapsuleError> {
        self.to_run_plan_with_state_overrides(&HashMap::new())
    }

    pub fn to_run_plan_with_state_overrides(
        &self,
        state_source_overrides: &HashMap<String, String>,
    ) -> Result<RunPlan, CapsuleError> {
        let target = self.resolve_default_target()?;
        if target.entrypoint.trim().is_empty() {
            return Err(CapsuleError::ValidationError(format!(
                "targets.{}.entrypoint is required",
                self.default_target
            )));
        }

        let resolved_runtime = self.resolve_default_runtime()?;
        let merged_env = merged_target_env(self, target);
        let ports = port_list(self.targets.as_ref().and_then(|targets| targets.port));
        let env = ordered_env(&merged_env);

        #[allow(deprecated)]
        let runtime = match resolved_runtime {
            RuntimeType::Docker | RuntimeType::Youki | RuntimeType::Oci => {
                // OCI container runtime (Docker, Youki, or new Oci type)
                let mut mounts = Vec::new();
                if !self.storage.volumes.is_empty() {
                    let base = default_storage_mount_base();
                    for vol in &self.storage.volumes {
                        let name = vol.name.trim();
                        let mount_path = vol.mount_path.trim();
                        if name.is_empty()
                            || mount_path.is_empty()
                            || !mount_path.starts_with('/')
                            || mount_path.contains("..")
                        {
                            return Err(CapsuleError::ValidationError(
                                "invalid storage volume (requires name and absolute mount_path)"
                                    .to_string(),
                            ));
                        }

                        mounts.push(Mount {
                            source: format!(
                                "{}/{}/{}",
                                base.trim_end_matches('/'),
                                self.name,
                                name
                            ),
                            target: mount_path.to_string(),
                            readonly: vol.read_only,
                        });
                    }
                }
                mounts.extend(state_mounts(self, state_source_overrides)?);

                RunPlanRuntime::Docker(DockerRuntime {
                    image: target.entrypoint.clone(),
                    digest: None,
                    command: target.cmd.clone(),
                    env: env.clone(),
                    working_dir: target.working_dir.clone(),
                    user: None,
                    ports: ports.clone(),
                    mounts,
                })
            }
            // UARC V1.1.0: Native is deprecated, map to Source runtime
            RuntimeType::Native => RunPlanRuntime::Source(SourceRuntime {
                language: None,
                entrypoint: target.entrypoint.clone(),
                cmd: target.cmd.clone(),
                args: Vec::new(),
                env: env.clone(),
                working_dir: target.working_dir.clone(),
                ports: ports.clone(),
                dev_mode: false,
            }),
            RuntimeType::Source => RunPlanRuntime::Source(SourceRuntime {
                language: None, // Will be set by caller if needed
                entrypoint: target.entrypoint.clone(),
                cmd: target.cmd.clone(),
                args: Vec::new(),
                env: env.clone(),
                working_dir: target.working_dir.clone(),
                ports: ports.clone(),
                dev_mode: false,
            }),
            RuntimeType::Wasm => RunPlanRuntime::Native(NativeRuntime {
                // Wasm components are routed by ato-cli; nacelle does not execute them
                binary_path: target.entrypoint.clone(),
                args: target.cmd.clone(),
                env: env.clone(),
                working_dir: target.working_dir.clone(),
            }),
            RuntimeType::Web => RunPlanRuntime::Source(SourceRuntime {
                // Web targets are served by ato-cli open_web executor.
                language: Some("web".to_string()),
                entrypoint: target.entrypoint.clone(),
                cmd: target.cmd.clone(),
                args: Vec::new(),
                env: env.clone(),
                working_dir: target.working_dir.clone(),
                ports: ports.clone(),
                dev_mode: false,
            }),
        };

        // storage validation is handled by CapsuleManifest::validate(); keep to_run_plan focused.

        let memory_bytes = self.requirements.vram_min_bytes()?;

        Ok(RunPlan {
            capsule_id: self.name.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            runtime,
            cpu_cores: None,
            memory_bytes,
            gpu_profile: None,
            egress_allowlist: Vec::new(),
        })
    }
}

fn state_mounts(
    manifest: &CapsuleManifest,
    state_source_overrides: &HashMap<String, String>,
) -> Result<Vec<Mount>, CapsuleError> {
    let Some(services) = manifest.services.as_ref() else {
        return Ok(Vec::new());
    };
    let Some(main) = services.get("main") else {
        return Ok(Vec::new());
    };

    main.state_bindings
        .iter()
        .map(|binding| {
            let state_name = binding.state.trim();
            let target = binding.target.trim();
            let requirement = manifest.state.get(state_name).ok_or_else(|| {
                CapsuleError::ValidationError(format!(
                    "services.main.state_bindings references unknown state '{}'",
                    state_name
                ))
            })?;

            if !super::is_valid_mount_path(target) {
                return Err(CapsuleError::ValidationError(format!(
                    "services.main.state_bindings target '{}' must be an absolute path",
                    binding.target
                )));
            }

            Ok(Mount {
                source: manifest.state_source_path(
                    state_name,
                    requirement,
                    Some(state_source_overrides),
                )?,
                target: target.to_string(),
                readonly: false,
            })
        })
        .collect()
}

fn ordered_env(env: &HashMap<String, String>) -> BTreeMap<String, String> {
    env.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn merged_target_env(
    manifest: &CapsuleManifest,
    target: &super::NamedTarget,
) -> HashMap<String, String> {
    let mut env = manifest
        .targets
        .as_ref()
        .map(|targets| targets.env.clone())
        .unwrap_or_default();
    for (key, value) in &target.env {
        env.insert(key.clone(), value.clone());
    }
    env
}

fn port_list(port: Option<u16>) -> Vec<Port> {
    port.map(|p| Port {
        container_port: p as u32,
        host_port: None,
        protocol: Some("tcp".to_string()),
    })
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {

    use serde_json;

    use super::CapsuleManifest;

    const SAMPLE_PYTHON_TOML: &str = r#"
schema_version = "0.2"
name = "mlx-qwen3-8b"
version = "1.0.0"
type = "inference"
default_target = "cli"

[targets]
port = 8081

[targets.cli]
runtime = "source"
entrypoint = "server.py"

[targets.cli.env]
GUMBALL_MODEL = "qwen3-8b"

[capabilities]
chat = true
function_calling = true
vision = false
context_length = 8192

[model]
source = "hf:org/model"
"#;

    const SAMPLE_DOCKER_TOML: &str = r#"
schema_version = "0.2"
name = "hello-docker"
version = "0.1.0"
type = "app"
default_target = "container"

[targets]
port = 8080

[targets.container]
runtime = "oci"
entrypoint = "ghcr.io/example/hello:latest"
"#;

    const SAMPLE_DOCKER_STATE_TOML: &str = r#"
schema_version = "0.2"
name = "hello-docker"
version = "0.1.0"
type = "app"
default_target = "container"

[targets.container]
runtime = "oci"
entrypoint = "ghcr.io/example/hello:latest"

[state.data]
kind = "filesystem"
durability = "ephemeral"
purpose = "primary-data"

[services.main]
target = "container"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#;

    const SAMPLE_DOCKER_PERSISTENT_STATE_TOML: &str = r#"
schema_version = "0.2"
name = "hello-docker"
version = "0.1.0"
type = "app"
default_target = "container"

[targets.container]
runtime = "oci"
entrypoint = "ghcr.io/example/hello:latest"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[services.main]
target = "container"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#;

    #[test]
    fn runplan_from_source_manifest() {
        let manifest = CapsuleManifest::from_toml(SAMPLE_PYTHON_TOML).unwrap();
        manifest.validate().unwrap();
        let plan = manifest.to_run_plan().unwrap();

        let json = serde_json::to_value(&plan).unwrap();
        let expected = serde_json::json!({
            "capsule_id": "mlx-qwen3-8b",
            "name": "mlx-qwen3-8b",
            "version": "1.0.0",
            "source": {
                "entrypoint": "server.py",
                "env": {"GUMBALL_MODEL": "qwen3-8b"},
                "ports": [
                    {"container_port": 8081, "protocol": "tcp"}
                ],
                "dev_mode": false
            }
        });

        assert_eq!(json, expected);
    }

    #[test]
    fn runplan_from_docker_manifest() {
        let manifest = CapsuleManifest::from_toml(SAMPLE_DOCKER_TOML).unwrap();
        manifest.validate().unwrap();
        let plan = manifest.to_run_plan().unwrap();

        let json = serde_json::to_value(&plan).unwrap();
        let expected = serde_json::json!({
            "capsule_id": "hello-docker",
            "name": "hello-docker",
            "version": "0.1.0",
            "docker": {
                "image": "ghcr.io/example/hello:latest",
                "ports": [
                    {"container_port": 8080, "protocol": "tcp"}
                ]
            }
        });

        assert_eq!(json, expected);
    }

    #[test]
    fn runplan_from_docker_manifest_with_ephemeral_state_binding() {
        let manifest = CapsuleManifest::from_toml(SAMPLE_DOCKER_STATE_TOML).unwrap();
        manifest.validate().unwrap();
        let plan = manifest.to_run_plan().unwrap();

        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(
            json["docker"]["mounts"],
            serde_json::json!([{
                "source": "/var/lib/ato/state/hello-docker/data",
                "target": "/var/lib/app",
                "readonly": false
            }])
        );
    }

    #[test]
    fn runplan_requires_explicit_bind_for_persistent_state() {
        let manifest = CapsuleManifest::from_toml(SAMPLE_DOCKER_PERSISTENT_STATE_TOML).unwrap();
        manifest.validate().unwrap();
        let err = manifest.to_run_plan().expect_err("missing bind must fail");
        assert!(err
            .to_string()
            .contains("requires an explicit persistent binding"));
    }

    #[test]
    fn runplan_uses_explicit_bind_for_persistent_state() {
        let manifest = CapsuleManifest::from_toml(SAMPLE_DOCKER_PERSISTENT_STATE_TOML).unwrap();
        manifest.validate().unwrap();
        let plan = manifest
            .to_run_plan_with_state_overrides(
                &[(
                    "data".to_string(),
                    "/var/lib/ato/persistent/hello-docker/data".to_string(),
                )]
                .into_iter()
                .collect(),
            )
            .unwrap();

        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(
            json["docker"]["mounts"],
            serde_json::json!([{
                "source": "/var/lib/ato/persistent/hello-docker/data",
                "target": "/var/lib/app",
                "readonly": false
            }])
        );
    }
}

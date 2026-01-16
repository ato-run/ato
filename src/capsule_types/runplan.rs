use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashMap;

use super::capsule_v1::{CapsuleManifestV1, RuntimeType};
use super::error::CapsuleError;

const DEFAULT_STORAGE_MOUNT_BASE: &str = "/var/lib/gumball/volumes";

fn default_storage_mount_base() -> String {
    std::env::var("GUMBALL_STORAGE_BASE").unwrap_or_else(|_| DEFAULT_STORAGE_MOUNT_BASE.to_string())
}

/// Normalized execution plan produced from capsule_v1 manifests.
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

impl CapsuleManifestV1 {
    /// Convert a validated capsule_v1 manifest into a normalized RunPlan.
    pub fn to_run_plan(&self) -> Result<RunPlan, CapsuleError> {
        let ports = port_list(self.execution.port);
        let env = ordered_env(&self.execution.env);

        #[allow(deprecated)]
        let runtime = match self.execution.runtime {
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

                RunPlanRuntime::Docker(DockerRuntime {
                    image: self.execution.entrypoint.clone(),
                    digest: None,
                    command: Vec::new(),
                    env: env.clone(),
                    working_dir: None,
                    user: None,
                    ports: ports.clone(),
                    mounts,
                })
            }
            // UARC V1.1.0: Native is deprecated, map to Source runtime
            RuntimeType::Native => RunPlanRuntime::Source(SourceRuntime {
                language: None,
                entrypoint: self.execution.entrypoint.clone(),
                cmd: Vec::new(),
                args: Vec::new(),
                env: env.clone(),
                working_dir: None,
                ports: ports.clone(),
                dev_mode: false,
            }),
            RuntimeType::Source => RunPlanRuntime::Source(SourceRuntime {
                language: None, // Will be set by caller if needed
                entrypoint: self.execution.entrypoint.clone(),
                cmd: Vec::new(),
                args: Vec::new(),
                env: env.clone(),
                working_dir: None,
                ports: ports.clone(),
                dev_mode: false,
            }),
            RuntimeType::Wasm => RunPlanRuntime::Native(NativeRuntime {
                // Wasm components are routed by capsule-cli; nacelle does not execute them
                binary_path: self.execution.entrypoint.clone(),
                args: Vec::new(),
                env: env.clone(),
                working_dir: None,
            }),
        };

        // storage validation is handled by CapsuleManifestV1::validate(); keep to_run_plan focused.

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

fn ordered_env(env: &HashMap<String, String>) -> BTreeMap<String, String> {
    env.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
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
    use super::*;

    const SAMPLE_PYTHON_TOML: &str = r#"
schema_version = "1.0"
name = "mlx-qwen3-8b"
version = "1.0.0"
type = "inference"

[execution]
runtime = "source"
entrypoint = "server.py"
port = 8081

[execution.env]
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
schema_version = "1.0"
name = "hello-docker"
version = "0.1.0"
type = "app"

[execution]
runtime = "docker"
entrypoint = "ghcr.io/example/hello:latest"
port = 8080
"#;

    #[test]
    fn runplan_from_source_manifest() {
        let manifest = CapsuleManifestV1::from_toml(SAMPLE_PYTHON_TOML).unwrap();
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
        let manifest = CapsuleManifestV1::from_toml(SAMPLE_DOCKER_TOML).unwrap();
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
}

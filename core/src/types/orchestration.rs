use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{manifest::ReadinessProbe, runplan::Mount};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedServiceNetwork {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub publish: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_from: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceConnectionInfo {
    pub dependency: String,
    pub host_env: String,
    pub port_env: String,
    pub container_port: Option<u16>,
    pub default_host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedTargetRuntime {
    pub target: String,
    pub runtime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default)]
    pub entrypoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cmd: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_layout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<Mount>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResolvedServiceRuntime {
    Oci(ResolvedTargetRuntime),
    Managed(ResolvedTargetRuntime),
}

impl ResolvedServiceRuntime {
    pub fn target(&self) -> &str {
        match self {
            Self::Oci(runtime) | Self::Managed(runtime) => &runtime.target,
        }
    }

    pub fn runtime(&self) -> &ResolvedTargetRuntime {
        match self {
            Self::Oci(runtime) | Self::Managed(runtime) => runtime,
        }
    }

    pub fn runtime_mut(&mut self) -> &mut ResolvedTargetRuntime {
        match self {
            Self::Oci(runtime) | Self::Managed(runtime) => runtime,
        }
    }

    pub fn is_oci(&self) -> bool {
        matches!(self, Self::Oci(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedService {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<ServiceConnectionInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness_probe: Option<ReadinessProbe>,
    #[serde(default)]
    pub network: ResolvedServiceNetwork,
    #[serde(flatten)]
    pub runtime: ResolvedServiceRuntime,
}

impl ResolvedService {
    pub fn primary_alias(&self) -> &str {
        self.network
            .aliases
            .first()
            .map(String::as_str)
            .unwrap_or(self.name.as_str())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestrationPlan {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub startup_order: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ResolvedService>,
}

impl OrchestrationPlan {
    pub fn service(&self, name: &str) -> Option<&ResolvedService> {
        self.services.iter().find(|service| service.name == name)
    }
}

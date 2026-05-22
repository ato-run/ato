use serde::{Deserialize, Serialize};

pub const OCI_LAUNCH_ENVELOPE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciPlatform {
    pub os: String,
    pub architecture: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciImageResolution {
    pub declared_ref: String,
    pub resolved_digest: String,
    pub platform: OciPlatform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importer_input_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciProviderSemantics {
    pub kind: OciProviderKind,
    pub mode: OciProviderMode,
    pub substrate: OciProviderSubstrate,
    pub policy_profile: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OciProviderKind {
    Podman,
    DockerCompatible,
    AtoNative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OciProviderMode {
    Rootless,
    Rootful,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OciProviderSubstrate {
    NativeLinux,
    PodmanMachine,
    DockerDesktop,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciPolicyEnvelope {
    pub enforcement_mode: OciPolicyEnforcementMode,
    pub enforcement_level: OciPolicyEnforcementLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_policy_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_policy_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_policy_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported_policy: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OciPolicyEnforcementMode {
    Strict,
    Loose,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OciPolicyEnforcementLevel {
    Enforced,
    Warning,
    BestEffort,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciSecretReferenceShape {
    pub id: String,
    pub delivery: OciSecretDeliveryShape,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum OciSecretDeliveryShape {
    Env { key: String },
    File { target: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciStateMountShape {
    pub state: String,
    pub target: String,
    pub readonly: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciPortExposureShape {
    pub container_port: u16,
    pub protocol: String,
    pub publish: OciPortPublishPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OciPortPublishPolicy {
    None,
    LocalhostDynamic,
    LocalhostDeclared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciServiceLaunchShape {
    pub name: String,
    pub target_label: String,
    pub image: OciImageResolution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_refs: Vec<OciSecretReferenceShape>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_mounts: Vec<OciStateMountShape>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<OciPortExposureShape>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness_probe: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciLaunchEnvelope {
    pub schema_version: u32,
    pub provider: OciProviderSemantics,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<OciServiceLaunchShape>,
    pub policy: OciPolicyEnvelope,
}

impl OciLaunchEnvelope {
    pub fn new(
        provider: OciProviderSemantics,
        services: Vec<OciServiceLaunchShape>,
        policy: OciPolicyEnvelope,
    ) -> Self {
        Self {
            schema_version: OCI_LAUNCH_ENVELOPE_SCHEMA_VERSION,
            provider,
            services,
            policy,
        }
    }
}

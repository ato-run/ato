use serde::{Deserialize, Serialize};

pub const EXECUTION_PLAN_SCHEMA_VERSION: &str = "1";
pub const MOUNT_SET_ALGO_ID: &str = "lockfile_mountset_v1";
pub const MOUNT_SET_ALGO_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionRuntime {
    Web,
    Source,
    Wasm,
    Oci,
}

impl ExecutionRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Source => "source",
            Self::Wasm => "wasm",
            Self::Oci => "oci",
        }
    }

    pub fn from_manifest(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        let runtime = normalized.split('/').next().unwrap_or_default();
        match runtime {
            "web" => Some(Self::Web),
            "source" => Some(Self::Source),
            "wasm" => Some(Self::Wasm),
            "oci" | "docker" | "youki" | "runc" => Some(Self::Oci),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDriver {
    Static,
    Deno,
    Node,
    Python,
    Wasmtime,
    Native,
}

impl ExecutionDriver {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Deno => "deno",
            Self::Node => "node",
            Self::Python => "python",
            Self::Wasmtime => "wasmtime",
            Self::Native => "native",
        }
    }

    pub fn from_manifest(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "static" => Some(Self::Static),
            "deno" => Some(Self::Deno),
            "node" | "nodejs" => Some(Self::Node),
            "python" | "python3" => Some(Self::Python),
            "wasmtime" => Some(Self::Wasmtime),
            "native" => Some(Self::Native),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionTier {
    Tier1,
    Tier2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub schema_version: String,
    pub capsule: CapsuleRef,
    pub target: TargetRef,
    pub provisioning: Provisioning,
    pub runtime: Runtime,
    pub consent: Consent,
    pub reproducibility: Reproducibility,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapsuleRef {
    pub scoped_id: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetRef {
    pub label: String,
    pub runtime: ExecutionRuntime,
    pub driver: ExecutionDriver,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provisioning {
    pub network: ProvisioningNetwork,
    pub lock_required: bool,
    pub integrity_required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_registries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisioningNetwork {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_registry_hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Runtime {
    pub policy: RuntimePolicy,
    pub fail_closed: bool,
    pub non_interactive_behavior: NonInteractiveBehavior,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePolicy {
    pub network: RuntimeNetworkPolicy,
    pub filesystem: RuntimeFilesystemPolicy,
    pub secrets: RuntimeSecretsPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeNetworkPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeFilesystemPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_only: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_write: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSecretsPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_secret_ids: Vec<String>,
    pub delivery: SecretDelivery,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretDelivery {
    Fd,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NonInteractiveBehavior {
    DenyIfUnconsented,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Consent {
    pub key: ConsentKey,
    pub policy_segment_hash: String,
    pub provisioning_policy_hash: String,
    pub mount_set_algo_id: String,
    pub mount_set_algo_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentKey {
    pub scoped_id: String,
    pub version: String,
    pub target_label: String,
}

impl ConsentKey {
    /// PR-4b: canonical projection of `ExecutionPlan.consent.key`.
    /// Used by `consent_store` as the keyed primitive for both
    /// plan-taking and view-taking call sites.
    pub fn from_execution_plan(plan: &ExecutionPlan) -> Self {
        Self {
            scoped_id: plan.consent.key.scoped_id.clone(),
            version: plan.consent.key.version.clone(),
            target_label: plan.consent.key.target_label.clone(),
        }
    }

    /// PR-4b: project from a bundle-derived consent view. The view
    /// carries the same 3 ConsentKey facets, so this is a passthrough.
    pub fn from_derived_consent_view(
        view: &crate::engine::execution_graph::DerivedConsentView,
    ) -> Self {
        Self {
            scoped_id: view.scoped_id.clone(),
            version: view.version.clone(),
            target_label: view.target_label.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reproducibility {
    pub platform: Platform,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub arch: String,
    pub libc: String,
}

//! Capsule Manifest v0.3 Schema
//!
//! Implements the "Everything is a Capsule" paradigm for Gumball v0.3.0.
//! Supports both TOML (human-authored) and JSON (machine-generated) formats.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;
use toml::value::Table;
use url::form_urlencoded;
use walkdir::{DirEntry, WalkDir};

#[path = "manifest_v03.rs"]
mod manifest_v03;
#[path = "manifest_validation.rs"]
mod manifest_validation;

use super::error::CapsuleError;
use super::utils::parse_memory_string;
use crate::orchestration::startup_order_from_dependencies;
use crate::schema_registry::SchemaRegistry;

use manifest_v03::*;
pub(crate) use manifest_validation::is_valid_mount_path;
pub use manifest_validation::ValidationError;
#[cfg(test)]
pub(crate) use manifest_validation::{is_kebab_case, is_semver};

/// Capsule Type - defines the fundamental nature of the Capsule
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CapsuleType {
    /// AI model inference (MLX, vLLM, etc.)
    Inference,
    /// Utility tool (RAG, code interpreter, etc.)
    Tool,
    /// One-shot or batch workload executed to completion.
    Job,
    /// Reusable build-only package in schema v0.3.
    Library,
    /// Application (agent, workflow, etc.)
    #[default]
    App,
}

/// Runtime Type - how the Capsule is executed
///
/// UARC V1.1.0 defines three runtime classes:
/// - `Source`: Interpreted source code (Python, JS, etc.)
/// - `Wasm`: WebAssembly Component Model
/// - `Oci`: OCI Container Image (Docker, Youki, etc.)
///
/// Legacy types (Docker, Native, Youki) are deprecated and mapped to Oci.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeType {
    /// Interpreted source code runtime (Python, Node.js, Ruby, etc.)
    /// UARC V1.1.0: Primary runtime for scripting workloads
    #[default]
    Source,

    /// WebAssembly Component Model runtime
    /// UARC V1.1.0: Portable, sandboxed bytecode for edge/latency-sensitive workloads
    Wasm,

    /// OCI Container Image runtime (youki, runc, containerd)
    /// UARC V1.1.0: Fallback for legacy/GPU applications
    Oci,

    /// Static web runtime for browser sandbox / playground.
    Web,

    // === Legacy types (deprecated, for backward compatibility) ===
    // These will be removed in UARC v0.2.0
    /// Docker container (deprecated: use `oci` instead)
    #[deprecated(since = "1.1.0", note = "Use `oci` runtime type instead")]
    #[serde(rename = "docker")]
    Docker,

    /// Native binary (deprecated: not supported in UARC V1)
    #[deprecated(
        since = "1.1.0",
        note = "Native runtime is not supported in UARC V1 for security reasons"
    )]
    #[serde(rename = "native")]
    Native,

    /// Youki OCI runtime (deprecated: use `oci` instead)
    #[deprecated(since = "1.1.0", note = "Use `oci` runtime type instead")]
    #[serde(rename = "youki")]
    Youki,
}

impl RuntimeType {
    /// Normalize legacy runtime types to UARC V1.1.0 types
    pub fn normalize(&self) -> RuntimeType {
        #[allow(deprecated)]
        match self {
            RuntimeType::Docker => RuntimeType::Oci,
            RuntimeType::Youki => RuntimeType::Oci,
            RuntimeType::Native => RuntimeType::Source, // Best-effort fallback
            other => other.clone(),
        }
    }

    /// Check if this is a legacy (deprecated) runtime type
    #[allow(deprecated)]
    pub fn is_legacy(&self) -> bool {
        matches!(
            self,
            RuntimeType::Docker | RuntimeType::Native | RuntimeType::Youki
        )
    }

    /// Parse a v0.2 named target runtime label.
    #[allow(deprecated)]
    pub fn from_target_runtime(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "source" => Some(RuntimeType::Source),
            "wasm" => Some(RuntimeType::Wasm),
            "oci" => Some(RuntimeType::Oci),
            "web" => Some(RuntimeType::Web),
            "docker" => Some(RuntimeType::Docker),
            "native" => Some(RuntimeType::Native),
            "youki" => Some(RuntimeType::Youki),
            _ => None,
        }
    }
}

/// Routing Weight - determines local vs cloud routing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RouteWeight {
    /// Small models, quick tasks - prefer local
    #[default]
    Light,
    /// Large models, heavy compute - consider cloud
    Heavy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quantization {
    Fp16,
    Bf16,
    #[serde(rename = "8bit")]
    Bit8,
    #[serde(rename = "4bit")]
    Bit4,
}

/// Platform target
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    DarwinArm64,
    DarwinX86_64,
    LinuxAmd64,
    LinuxArm64,
}

/// Transparency enforcement level for source code validation
///
/// Controls how strictly the runtime enforces source code transparency requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TransparencyLevel {
    /// Source code required, no binaries allowed except explicitly allowlisted.
    /// Most restrictive: .pyc, .class, native binaries all forbidden unless allowlisted.
    Strict,
    /// Binaries allowed if in allowlist or are known bytecode (.pyc, .class).
    /// Practical default for most use cases.
    #[default]
    Loose,
    /// No transparency enforcement (legacy/Docker compatibility mode).
    Off,
}

/// Transparency enforcement configuration
///
/// Enforces UARC's "no binary-only" philosophy by validating that capsules
/// contain source code and not just compiled binaries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransparencyConfig {
    /// Enforcement level
    #[serde(default)]
    pub level: TransparencyLevel,

    /// Glob patterns for allowed binary files
    ///
    /// Examples: "lib/**/*.so", "venv/bin/*", "node_modules/**/*.node"
    #[serde(default)]
    pub allowed_binaries: Vec<String>,
}

/// Build configuration (packaging-time behavior)
///
/// These settings affect how capsules are packaged (e.g. bundle/source archive).
/// They do not change runtime behavior directly.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildConfig {
    /// Glob patterns to exclude from packaged artifacts.
    ///
    /// Typical uses:
    /// - Exclude large ML libraries (torch, jaxlib, etc.) for "Thin Capsule on Fat Container"
    /// - Exclude host-provided dynamic libs when using passthrough
    #[serde(default)]
    pub exclude_libs: Vec<String>,

    /// Sugar syntax: GPU-oriented packaging defaults.
    ///
    /// When true, tooling may apply recommended defaults (e.g. docker scaffold template
    /// and optional exclude patterns) but should remain opt-in.
    #[serde(default)]
    pub gpu: bool,

    /// Build task lifecycle for CI/build pipelines.
    #[serde(default)]
    pub lifecycle: Option<BuildLifecycleConfig>,

    /// Build inputs used for reproducibility and provenance.
    #[serde(default)]
    pub inputs: Option<BuildInputsConfig>,

    /// Build outputs expected by registry/store verification.
    #[serde(default)]
    pub outputs: Option<BuildOutputsConfig>,

    /// Publish-time verification policy.
    #[serde(default)]
    pub policy: Option<BuildPolicyConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildLifecycleConfig {
    #[serde(default)]
    pub prepare: Option<String>,
    #[serde(default)]
    pub build: Option<String>,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub verify: Option<String>,
    #[serde(default)]
    pub publish: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildInputsConfig {
    #[serde(default)]
    pub lockfiles: Vec<String>,
    #[serde(default)]
    pub toolchain: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub allow_network: Option<bool>,
    #[serde(default)]
    pub reproducibility: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildOutputsConfig {
    #[serde(default)]
    pub capsule: Option<String>,
    #[serde(default)]
    pub sha256: Option<bool>,
    #[serde(default)]
    pub blake3: Option<bool>,
    #[serde(default)]
    pub attestation: Option<bool>,
    #[serde(default)]
    pub signature: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildPolicyConfig {
    #[serde(default)]
    pub require_attestation: Option<bool>,
    #[serde(default)]
    pub require_did_signature: Option<bool>,
}

/// Packaging filter configuration
///
/// Controls which project files are included in the capsule payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackConfig {
    /// Strict allowlist patterns. When specified, only matched files are included.
    #[serde(default)]
    pub include: Vec<String>,

    /// Exclusion patterns applied after include/default selection.
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Isolation configuration (runtime-time behavior)
///
/// This section controls what host environment data is allowed to pass into the
/// capsule at runtime. This is a security-sensitive opt-in.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IsolationConfig {
    /// Host environment variables to pass through.
    ///
    /// Examples: ["LD_LIBRARY_PATH", "CUDA_HOME", "HF_TOKEN"].
    #[serde(default)]
    pub allow_env: Vec<String>,
}

/// Service specification for Supervisor Mode (multi-process orchestration).
///
/// This is intentionally minimal in Step 1: schema + dependency graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceSpec {
    /// Command line to execute.
    ///
    /// Accept both `entrypoint` (preferred) and `command` (alias) for compatibility
    /// with early drafts.
    #[serde(default)]
    #[serde(alias = "command")]
    pub entrypoint: String,

    /// Reference to a target under [targets.<label>].
    #[serde(default)]
    pub target: Option<String>,

    /// Service dependencies by name.
    #[serde(default)]
    pub depends_on: Option<Vec<String>>,

    /// Placeholders to allocate and inject as ports (Step 2).
    #[serde(default)]
    pub expose: Option<Vec<String>>,

    /// Environment variables to inject into this service.
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,

    /// State requirements bound into this service at runtime.
    #[serde(default)]
    pub state_bindings: Vec<ServiceStateBinding>,

    /// Readiness probe (Step 2/3).
    #[serde(default)]
    pub readiness_probe: Option<ReadinessProbe>,

    /// Service-to-service network exposure controls.
    #[serde(default)]
    pub network: Option<ServiceNetworkSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceStateBinding {
    pub state: String,
    pub target: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceNetworkSpec {
    /// Additional DNS aliases for this service inside the orchestration network.
    #[serde(default)]
    pub aliases: Vec<String>,

    /// Whether this service should be reachable from the host network.
    #[serde(default)]
    pub publish: bool,

    /// Restrict which services may receive connection metadata for this service.
    #[serde(default)]
    pub allow_from: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadinessProbe {
    #[serde(default)]
    pub http_get: Option<String>,

    #[serde(default)]
    pub tcp_connect: Option<String>,

    /// Placeholder name that resolves to a concrete port (e.g., "PORT").
    pub port: String,
}

/// Capsule Manifest v0.3
///
/// The primary configuration format for all Capsules in Gumball v0.3.0+
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapsuleManifest {
    /// Schema version (must be "0.3")
    #[serde(default = "default_schema_version")]
    pub schema_version: String,

    /// Unique capsule identifier (kebab-case)
    pub name: String,

    /// Semantic version. Optional for versionless publish surfaces; empty means unset.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,

    /// Capsule type
    #[serde(rename = "type")]
    pub capsule_type: CapsuleType,

    /// Default target label used when no explicit target is selected.
    #[serde(default)]
    pub default_target: String,

    /// Human-readable metadata
    #[serde(default)]
    pub metadata: CapsuleMetadata,

    /// Capsule capabilities (for inference type)
    #[serde(default)]
    pub capabilities: Option<CapsuleCapabilities>,

    /// System requirements
    #[serde(default)]
    pub requirements: CapsuleRequirements,

    /// Execution configuration
    #[serde(default, skip_serializing)]
    pub execution: CapsuleExecution,

    /// Persistent storage volumes
    #[serde(default)]
    pub storage: CapsuleStorage,

    /// Filesystem-backed application state requirements.
    #[serde(default)]
    pub state: HashMap<String, StateRequirement>,

    /// Optional opaque owner scope used for persistent state registry identity.
    ///
    /// When omitted, `name` remains the default owner scope for backward compatibility.
    #[serde(default)]
    pub state_owner_scope: Option<String>,

    /// Optional opaque owner scope used for host-managed service binding identity.
    ///
    /// When omitted, `name` remains the default owner scope so published ingress and
    /// future cross-capsule bindings inherit a stable default identity.
    #[serde(default)]
    pub service_binding_scope: Option<String>,

    /// Routing configuration
    #[serde(default)]
    pub routing: CapsuleRouting,

    /// Network configuration
    #[serde(default)]
    pub network: Option<NetworkConfig>,

    /// Model configuration (for inference type)
    #[serde(default)]
    pub model: Option<ModelConfig>,

    /// Transparency enforcement configuration
    #[serde(default)]
    pub transparency: Option<TransparencyConfig>,

    /// Pre-warmed container pool configuration
    #[serde(default)]
    pub pool: Option<PoolConfig>,

    /// Build configuration (packaging-time)
    #[serde(default)]
    pub build: Option<BuildConfig>,

    /// Packaging filter configuration
    #[serde(default)]
    pub pack: Option<PackConfig>,

    /// Isolation configuration (runtime-time)
    #[serde(default)]
    pub isolation: Option<IsolationConfig>,

    /// Polymorphism configuration (implements schema hashes)
    #[serde(default)]
    pub polymorphism: Option<PolymorphismConfig>,

    /// Multi-target execution configuration (UARC V1.1.0)
    ///
    /// Allows capsules to specify multiple runtime targets (wasm, source, oci).
    /// Engine performs runtime resolution to select the most appropriate target.
    #[serde(default)]
    pub targets: Option<TargetsConfig>,

    /// Explicit exported surfaces such as one-shot CLI tools.
    #[serde(default)]
    pub exports: Option<CapsuleExports>,

    /// Supervisor Mode: Multi-service definition.
    ///
    /// Optional and dev-first: absence means single-process execution via `execution`.
    #[serde(default)]
    pub services: Option<HashMap<String, ServiceSpec>>,

    /// Workspace-scoped setup authoring surface used by `ato setup`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceSetupSpec>,

    /// Distribution metadata generated at pack/publish time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution: Option<DistributionInfo>,

    /// Foundation conformance requirements (Part I — spec-level, Foundation scope).
    ///
    /// Declares which Foundation-defined runtime profiles and engine versions this capsule
    /// requires.  Absent means no Foundation conformance assertion; the capsule runs on any
    /// conformant ato implementation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foundation_requirements: Option<FoundationRequirements>,
}

/// Foundation conformance requirements (§3.6, Part I of the Capsule Protocol spec).
///
/// Declares which Foundation-approved runtime profile and engine constraints this capsule
/// requires.  A conformant ato implementation MUST reject execution if it cannot satisfy
/// the declared `profile` or if the requested engines are not available in a compatible
/// version.
///
/// All fields are optional; an empty `FoundationRequirements` block is equivalent to
/// omitting the section entirely.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FoundationRequirements {
    /// Foundation-approved runtime profile identifier (e.g. "std.secure", "std.network").
    ///
    /// A runtime profile is an opaque string defined by the Foundation registry.  The ato
    /// implementation MUST verify that the running environment satisfies this profile before
    /// launching the capsule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,

    /// List of runtime tool requirements (name@version-range pairs).
    ///
    /// Examples: `["python@>=3.11", "node@>=20"]`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<String>,

    /// List of engine capability requirements (name@version-range pairs).
    ///
    /// Examples: `["nacelle@>=0.4", "bwrap@>=0.8"]`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub engines: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DistributionInfo {
    pub manifest_hash: String,
    pub merkle_root: String,
    #[serde(default)]
    pub chunk_list: Vec<ChunkDescriptor>,
    #[serde(default)]
    pub signatures: Vec<SignatureEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkDescriptor {
    pub chunk_hash: String,
    pub offset: u64,
    pub length: u64,
    pub codec: String,
    pub compression: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignatureEntry {
    pub signer_did: String,
    pub key_id: String,
    pub algorithm: String,
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpochPointer {
    pub scoped_id: String,
    pub epoch: u64,
    pub manifest_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_epoch_hash: Option<String>,
    pub issued_at: String,
    pub signer_did: String,
    pub key_id: String,
    pub signature: String,
}

/// Polymorphism configuration
///
/// Allows capsules to declare which schema hashes they implement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolymorphismConfig {
    #[serde(default)]
    pub implements: Vec<String>,
}

fn default_schema_version() -> String {
    "0.3".to_string()
}

fn is_supported_schema_version(value: &str) -> bool {
    matches!(value.trim(), "0.3")
}

fn is_v03_schema(raw: &toml::Value) -> bool {
    raw.get("schema_version")
        .and_then(toml::Value::as_str)
        .map(|value| value.trim() == "0.3")
        .unwrap_or(false)
}

fn is_chml_manifest(raw: &toml::Value) -> bool {
    if raw.get("schema_version").is_some() {
        return false;
    }

    let Some(table) = raw.as_table() else {
        return false;
    };

    if table.contains_key("packages") || table.contains_key("workspace") {
        return true;
    }

    table.get("build").and_then(toml::Value::as_str).is_some()
        || table.get("run").and_then(toml::Value::as_str).is_some()
        || table.get("runtime").and_then(toml::Value::as_str).is_some()
        || table.contains_key("outputs")
        || table.contains_key("build_env")
        || table.contains_key("required_env")
        || table.contains_key("runtime_version")
        || table.contains_key("runtime_tools")
        || table.contains_key("readiness_probe")
        || table.contains_key("external_injection")
        || table.contains_key("dependencies")
        || table.contains_key("capsule_path")
}

pub fn is_v03_like_schema(raw: &toml::Value) -> bool {
    is_v03_schema(raw) || is_chml_manifest(raw)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalCapsuleDependency {
    pub alias: String,
    pub source: String,
    pub source_type: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub injection_bindings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalInjectionSpec {
    #[serde(rename = "type")]
    pub injection_type: String,
    #[serde(default = "default_external_injection_required")]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDependencySpec {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAppPersonalizationSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy_mode: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAppSpec {
    #[serde(flatten)]
    pub dependency: WorkspaceDependencySpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personalization: Option<WorkspaceAppPersonalizationSpec>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceServiceSpec {
    #[serde(flatten)]
    pub dependency: WorkspaceDependencySpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSetupSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_app: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub apps: BTreeMap<String, WorkspaceAppSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, WorkspaceDependencySpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub services: BTreeMap<String, WorkspaceServiceSpec>,
}

/// Pre-warmed container pool configuration
///
/// Enables ultra-low latency container startup by maintaining a pool of
/// frozen containers that can be instantly thawed and assigned.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolConfig {
    /// Whether pool is enabled for this capsule
    #[serde(default)]
    pub enabled: bool,

    /// Number of containers to keep pre-warmed (default: 3)
    #[serde(default = "default_pool_size")]
    pub size: u16,

    /// Minimum threshold before triggering replenishment (default: 1)
    #[serde(default = "default_min_threshold")]
    pub min_threshold: u16,

    /// Replenish check interval in milliseconds (default: 5000)
    #[serde(default = "default_replenish_interval_ms")]
    pub replenish_interval_ms: u32,

    /// Maximum time a container can be assigned in seconds (default: 300)
    #[serde(default = "default_max_assignment_duration_secs")]
    pub max_assignment_duration_secs: u32,
}

fn default_pool_size() -> u16 {
    3
}
fn default_min_threshold() -> u16 {
    1
}
fn default_replenish_interval_ms() -> u32 {
    5000
}
fn default_max_assignment_duration_secs() -> u32 {
    300
}

/// Persistent storage configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapsuleStorage {
    #[serde(default)]
    pub volumes: Vec<StorageVolume>,
    /// Use thin provisioning by default for all volumes in this capsule
    #[serde(default)]
    pub use_thin_provisioning: bool,
}

/// A named persistent volume mounted into the container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageVolume {
    pub name: String,
    pub mount_path: String,
    #[serde(default)]
    pub read_only: bool,
    /// Size in bytes (0 = use engine default)
    #[serde(default)]
    pub size_bytes: u64,
    /// Use thin provisioning for this volume (overrides CapsuleStorage.use_thin_provisioning)
    #[serde(default)]
    pub use_thin: Option<bool>,
    /// Enable encryption for this volume
    #[serde(default)]
    pub encrypted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateKind {
    Filesystem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateDurability {
    Ephemeral,
    Persistent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StateAttach {
    #[default]
    Auto,
    Explicit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateRequirement {
    pub kind: StateKind,
    pub durability: StateDurability,
    pub purpose: String,
    #[serde(default)]
    pub producer: Option<String>,
    #[serde(default)]
    pub attach: StateAttach,
    #[serde(default)]
    pub schema_id: Option<String>,
}

/// Human-readable metadata
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapsuleMetadata {
    /// Display name for UI
    #[serde(default)]
    pub display_name: Option<String>,

    /// Description
    #[serde(default)]
    pub description: Option<String>,

    /// Author or organization
    #[serde(default)]
    pub author: Option<String>,

    /// Icon URL
    #[serde(default)]
    pub icon: Option<String>,

    /// Tags for categorization
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Capsule capabilities (for inference type)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapsuleCapabilities {
    /// Supports chat completions
    #[serde(default)]
    pub chat: bool,

    /// Supports function/tool calling
    #[serde(default)]
    pub function_calling: bool,

    /// Supports vision/image input
    #[serde(default)]
    pub vision: bool,

    /// Maximum context window size
    #[serde(default)]
    pub context_length: Option<u32>,
}

/// System requirements
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapsuleRequirements {
    /// Supported platforms
    #[serde(default)]
    pub platform: Vec<Platform>,

    /// Minimum VRAM required (e.g., "6GB")
    #[serde(default)]
    pub vram_min: Option<String>,

    /// Recommended VRAM (e.g., "8GB")
    #[serde(default)]
    pub vram_recommended: Option<String>,

    /// Disk space required (e.g., "5GB")
    #[serde(default)]
    pub disk: Option<String>,

    /// Other Capsule dependencies
    #[serde(default)]
    pub dependencies: Vec<String>,

    /// Optional capability declarations surfaced to registry search and
    /// agent-facing SKILL.md vocab. See
    /// `capsule_core::schema::capabilities::Capabilities`. Absence means
    /// "not declared"; do not infer a default level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<crate::schema::capabilities::Capabilities>,
}

impl CapsuleRequirements {
    /// Parse vram_min into bytes
    pub fn vram_min_bytes(&self) -> Result<Option<u64>, CapsuleError> {
        match &self.vram_min {
            Some(s) => {
                Ok(Some(parse_memory_string(s).map_err(|e| {
                    CapsuleError::InvalidMemoryString(e.to_string())
                })?))
            }
            None => Ok(None),
        }
    }

    /// Parse vram_recommended into bytes
    pub fn vram_recommended_bytes(&self) -> Result<Option<u64>, CapsuleError> {
        match &self.vram_recommended {
            Some(s) => {
                Ok(Some(parse_memory_string(s).map_err(|e| {
                    CapsuleError::InvalidMemoryString(e.to_string())
                })?))
            }
            None => Ok(None),
        }
    }

    /// Parse disk into bytes
    pub fn disk_bytes(&self) -> Result<Option<u64>, CapsuleError> {
        match &self.disk {
            Some(s) => {
                Ok(Some(parse_memory_string(s).map_err(|e| {
                    CapsuleError::InvalidMemoryString(e.to_string())
                })?))
            }
            None => Ok(None),
        }
    }
}

/// Signal configuration for graceful shutdown
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SignalConfig {
    /// Signal for graceful stop (default: SIGTERM)
    #[serde(default = "default_stop_signal")]
    pub stop: String,

    /// Signal for force kill (default: SIGKILL)
    #[serde(default = "default_kill_signal")]
    pub kill: String,
}

fn default_stop_signal() -> String {
    "SIGTERM".to_string()
}

fn default_kill_signal() -> String {
    "SIGKILL".to_string()
}

/// Execution configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapsuleExecution {
    /// Runtime type
    pub runtime: RuntimeType,

    /// Entry point (script, binary, or Docker image)
    pub entrypoint: String,

    /// Port the service listens on
    #[serde(default)]
    pub port: Option<u16>,

    /// Health check endpoint
    #[serde(default)]
    pub health_check: Option<String>,

    /// Startup timeout in seconds
    #[serde(default = "default_startup_timeout")]
    pub startup_timeout: u32,

    /// Environment variables
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Signal configuration
    #[serde(default)]
    pub signals: SignalConfig,
}

fn default_startup_timeout() -> u32 {
    60
}

/// Routing configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapsuleRouting {
    /// Weight for routing decision
    #[serde(default)]
    pub weight: RouteWeight,

    /// Whether to fallback to cloud when local resources are insufficient
    #[serde(default = "default_true")]
    pub fallback_to_cloud: bool,

    /// Cloud Capsule ID to use as fallback
    #[serde(default)]
    pub cloud_capsule: Option<String>,
}

fn default_true() -> bool {
    true
}

pub fn default_ephemeral_state_base() -> String {
    std::env::var("ATO_STATE_EPHEMERAL_BASE").unwrap_or_else(|_| "/var/lib/ato/state".to_string())
}

/// Model configuration (for inference Capsules)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Model source (e.g., "hf:org/model")
    #[serde(default)]
    pub source: Option<String>,

    /// Quantization format
    #[serde(default)]
    pub quantization: Option<Quantization>,
}

/// Network configuration for Egress Control
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// List of allowlisted domains (L7/Proxy)
    #[serde(default)]
    pub egress_allow: Vec<String>,

    /// List of allowlisted IPs/CIDRs (L3/Firewall)
    #[serde(default)]
    pub egress_id_allow: Vec<EgressIdRule>,
}

/// Rule for L3 Egress Control
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressIdRule {
    /// Type of rule (ip, cidr, spiffe - though spiffe might be L7, treating as ID here)
    #[serde(rename = "type")]
    pub rule_type: EgressIdType,

    /// Value (e.g., "192.168.1.1", "10.0.0.0/8")
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EgressIdType {
    Ip,
    Cidr,
    /// SPIFFE ID (future use, currently placeholder for L3 mapping)
    Spiffe,
}

// ============================================================================
// Multi-Target Execution Configuration (UARC V1.1.0)
// ============================================================================

/// Multi-target execution configuration
///
/// Allows capsules to provide multiple runtime targets (wasm, source, oci).
/// The Engine performs runtime resolution to select the most appropriate target
/// based on platform capabilities and the preference order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetsConfig {
    /// Preferred resolution order (e.g., ["wasm", "source", "oci"])
    ///
    /// If not specified, the default order is: wasm → source → oci
    #[serde(default)]
    pub preference: Vec<String>,

    /// SHA256 digest of the source code archive for L1 policy verification (UARC V1.1.0)
    ///
    /// Format: "sha256:<hash>" pointing to the source archive in CAS.
    /// Required when source target is specified.
    /// The Engine verifies this digest against CAS during L1 Source Policy checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,

    /// Port the service listens on (global for all targets)
    #[serde(default)]
    pub port: Option<u16>,

    /// Startup timeout in seconds (global for all targets)
    #[serde(default = "default_startup_timeout")]
    pub startup_timeout: u32,

    /// Environment variables (global for all targets)
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Health check endpoint (global for all targets)
    #[serde(default)]
    pub health_check: Option<String>,

    /// WebAssembly Component Model target
    #[serde(default)]
    pub wasm: Option<WasmTarget>,

    /// Source code target (interpreted languages)
    #[serde(default)]
    pub source: Option<SourceTarget>,

    /// OCI container target
    #[serde(default)]
    pub oci: Option<OciTarget>,

    /// Named target entries for v0.2 (e.g. [targets.cli], [targets.static]).
    #[serde(flatten)]
    pub named: HashMap<String, NamedTarget>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapsuleExports {
    #[serde(default)]
    pub cli: HashMap<String, CliExportSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CliExportSpec {
    pub kind: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// v0.2 named target definition under [targets.<label>].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NamedTarget {
    /// Runtime kind for this target (`source`, `web`, `wasm`, `oci`).
    #[serde(default)]
    pub runtime: String,

    /// Runtime driver (`static`, `deno`, `node`, `python`, `wasmtime`, `native`).
    ///
    /// If omitted, the driver is inferred from runtime and language.
    #[serde(default)]
    pub driver: Option<String>,

    /// Optional source language hint used for driver inference.
    #[serde(default)]
    pub language: Option<String>,

    /// Runtime version pinned for deterministic hermetic execution.
    #[serde(default)]
    pub runtime_version: Option<String>,

    /// Additional hermetic runtime versions required by orchestrators.
    ///
    /// Example:
    /// runtime_tools = { node = "20.11.0", python = "3.11.7" }
    #[serde(default)]
    pub runtime_tools: HashMap<String, String>,

    /// Entrypoint path for the target.
    #[serde(default)]
    pub entrypoint: String,

    /// OCI image reference (preferred for runtime=oci).
    #[serde(default)]
    pub image: Option<String>,

    /// Optional command arguments.
    #[serde(default)]
    pub cmd: Vec<String>,

    /// Optional environment variables.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Required environment variable names.
    #[serde(default)]
    pub required_env: Vec<String>,

    /// Legacy public asset allowlist (deprecated for runtime=web; rejected by validation).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public: Vec<String>,

    /// Optional listening port.
    #[serde(default)]
    pub port: Option<u16>,

    /// Optional working directory.
    #[serde(default)]
    pub working_dir: Option<String>,

    /// Internal source runtime layout hint used by generated manifests.
    #[serde(default)]
    pub source_layout: Option<String>,

    /// Package type preserved from schema v0.3 (`app` or `library`).
    #[serde(default)]
    pub package_type: Option<String>,

    /// Package-specific build command preserved from schema v0.3.
    #[serde(default)]
    pub build_command: Option<String>,

    /// CHML build cache output globs preserved on the normalized target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,

    /// CHML build cache environment keys preserved on the normalized target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build_env: Vec<String>,

    /// Preserved shell-native run command for schema v0.3.
    #[serde(default)]
    pub run_command: Option<String>,

    /// WebAssembly component path for runtime=wasm targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,

    /// Optional readiness probe for top-level target execution.
    #[serde(default)]
    pub readiness_probe: Option<ReadinessProbe>,

    /// v0.3 workspace-local package dependencies flattened to target labels.
    #[serde(default)]
    pub package_dependencies: Vec<String>,

    /// v0.3 external capsule dependencies preserved for lockfile resolution.
    #[serde(default)]
    pub external_dependencies: Vec<ExternalCapsuleDependency>,

    /// v0.3 external data injection contracts.
    #[serde(default)]
    pub external_injection: HashMap<String, ExternalInjectionSpec>,
}

/// WebAssembly Component Model target configuration
///
/// For capsules that can run as Wasm components using the wasi:cli/command world.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmTarget {
    /// CAS digest of the Wasm component binary
    ///
    /// Format: "sha256:<hash>" pointing to the .wasm file in CAS
    pub digest: String,

    /// WIT world interface (e.g., "wasi:cli/command", "uarc:v1/http-handler")
    #[serde(default = "default_wasm_world")]
    pub world: String,

    /// Optional: component-specific configuration as key-value pairs
    #[serde(default)]
    pub config: HashMap<String, String>,
}

fn default_wasm_world() -> String {
    "wasi:cli/command".to_string()
}

/// Source code target configuration
///
/// For capsules that run directly from source code using an interpreter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceTarget {
    /// Language runtime (e.g., "python", "node", "deno")
    pub language: String,

    /// Version constraint (e.g., "^3.11", ">=18.0")
    #[serde(default)]
    pub version: Option<String>,

    /// Entry point file (relative to source root)
    pub entrypoint: String,

    /// Dependencies file (e.g., "requirements.txt", "package.json")
    #[serde(default)]
    pub dependencies: Option<String>,

    /// Optional: runtime-specific arguments
    #[serde(default)]
    pub args: Vec<String>,

    /// Development mode - disables sandboxing for easier debugging.
    /// WARNING: Only honored when Engine's allow_insecure_dev_mode is true.
    /// UARC V1.1.0: (manifest.dev_mode) AND (engine.allow_insecure_dev_mode)
    #[serde(default)]
    pub dev_mode: bool,
}

/// OCI container target configuration
///
/// For capsules that run as Docker/OCI containers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OciTarget {
    /// OCI image reference (e.g., "python:3.11-slim", "ghcr.io/org/image:tag")
    pub image: String,

    /// Image digest for immutability (e.g., "sha256:<hash>")
    #[serde(default)]
    pub digest: Option<String>,

    /// Command to execute (overrides image CMD)
    #[serde(default)]
    pub cmd: Vec<String>,

    /// Environment variables
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl TargetsConfig {
    /// Check if any target is defined
    pub fn has_any_target(&self) -> bool {
        self.wasm.is_some() || self.source.is_some() || self.oci.is_some() || !self.named.is_empty()
    }

    /// Get the preference order, using defaults if not specified
    pub fn preference_order(&self) -> Vec<&str> {
        if self.preference.is_empty() {
            // Default order: wasm → source → oci
            vec!["wasm", "source", "oci"]
        } else {
            self.preference.iter().map(|s| s.as_str()).collect()
        }
    }

    /// Validates that source_digest is present when source target is defined (UARC V1.1.0 L1 requirement)
    pub fn validate_source_digest(&self) -> Result<(), String> {
        if self.source.is_some() && self.source_digest.is_none() {
            return Err(
                "source_digest is required when source target is defined (UARC V1.1.0 L1)"
                    .to_string(),
            );
        }
        if let Some(ref digest) = self.source_digest {
            if !digest.starts_with("sha256:") {
                return Err(format!(
                    "source_digest must start with 'sha256:', got: {}",
                    digest
                ));
            }
            // Validate hex length (SHA256 = 64 hex chars)
            let hash_part = digest.strip_prefix("sha256:").unwrap();
            if hash_part.len() != 64 || !hash_part.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!(
                    "source_digest has invalid SHA256 hash format: {}",
                    digest
                ));
            }
        }
        Ok(())
    }

    /// Returns a v0.2 named target by label.
    pub fn named_target(&self, label: &str) -> Option<&NamedTarget> {
        self.named.get(label)
    }

    /// Returns all named targets.
    pub fn named_targets(&self) -> &HashMap<String, NamedTarget> {
        &self.named
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationMode {
    Strict,
    Preview,
}

impl CapsuleManifest {
    fn from_toml_with_path_internal(
        content: &str,
        manifest_path: Option<&Path>,
    ) -> Result<Self, CapsuleError> {
        let raw: toml::Value = toml::from_str(content)
            .map_err(|e| CapsuleError::ParseError(format!("TOML parse error: {}", e)))?;

        if raw.get("execution").is_some() {
            return Err(CapsuleError::ParseError(
                "legacy [execution] section is not supported in schema_version=0.3".to_string(),
            ));
        }

        let mut visiting = HashSet::new();
        if let Some(manifest_path) = manifest_path {
            let canonical = manifest_path
                .canonicalize()
                .unwrap_or_else(|_| manifest_path.to_path_buf());
            visiting.insert(canonical);
        }

        let normalized = normalize_v03_manifest_value_with_path(raw, manifest_path, &mut visiting)?;
        let normalized_text = toml::to_string(&normalized)
            .map_err(|e| CapsuleError::SerializeError(format!("TOML serialize error: {}", e)))?;

        toml::from_str(&normalized_text)
            .map_err(|e| CapsuleError::ParseError(format!("TOML parse error: {}", e)))
    }

    /// Parse from TOML string
    pub fn from_toml(content: &str) -> Result<Self, CapsuleError> {
        Self::from_toml_with_path_internal(content, None)
    }

    /// Parse from TOML string with file path context for v0.3 delegation.
    pub fn from_toml_with_path<P: AsRef<Path>>(
        content: &str,
        manifest_path: P,
    ) -> Result<Self, CapsuleError> {
        Self::from_toml_with_path_internal(content, Some(manifest_path.as_ref()))
    }

    /// Parse from JSON string
    pub fn from_json(content: &str) -> Result<Self, CapsuleError> {
        let raw: serde_json::Value = serde_json::from_str(content)
            .map_err(|e| CapsuleError::ParseError(format!("JSON parse error: {}", e)))?;
        if raw.get("execution").is_some() {
            return Err(CapsuleError::ParseError(
                "legacy [execution] section is not supported in schema_version=0.3".to_string(),
            ));
        }

        serde_json::from_str(content)
            .map_err(|e| CapsuleError::ParseError(format!("JSON parse error: {}", e)))
    }

    /// Serialize to JSON
    pub fn to_json(&self) -> Result<String, CapsuleError> {
        serde_json::to_string_pretty(self).map_err(|e| CapsuleError::SerializeError(e.to_string()))
    }

    /// Serialize to TOML
    pub fn to_toml(&self) -> Result<String, CapsuleError> {
        toml::to_string_pretty(self).map_err(|e| CapsuleError::SerializeError(e.to_string()))
    }

    /// Returns the intermediate normalized TOML text (with `[targets]` populated) for use in the
    /// compat bridge, without re-running v0.3 validation on the result. This avoids the
    /// round-trip issue where `normalize_v03_target_table` emits `entrypoint` (v0.2-style) which
    /// `reject_v03_legacy_fields` would reject on re-parse.
    pub fn normalize_to_compat_toml(content: &str) -> Result<String, CapsuleError> {
        let raw: toml::Value = toml::from_str(content)
            .map_err(|e| CapsuleError::ParseError(format!("TOML parse error: {}", e)))?;
        let mut visiting = HashSet::new();
        let normalized = normalize_v03_manifest_value_with_path(raw, None, &mut visiting)?;
        toml::to_string(&normalized)
            .map_err(|e| CapsuleError::SerializeError(format!("TOML serialize error: {}", e)))
    }

    pub fn resolve_default_target(&self) -> Result<&NamedTarget, CapsuleError> {
        let targets = self.targets.as_ref().ok_or_else(|| {
            CapsuleError::ValidationError(
                "at least one [targets.<label>] section is required".to_string(),
            )
        })?;
        if self.default_target.trim().is_empty() {
            return Err(CapsuleError::ValidationError(
                "default_target is required".to_string(),
            ));
        }
        targets
            .named_targets()
            .get(self.default_target.trim())
            .ok_or_else(|| {
                CapsuleError::ValidationError(format!(
                    "default_target '{}' does not exist under [targets]",
                    self.default_target
                ))
            })
    }

    /// Resolve runtime from the effective v0.2 target.
    pub fn resolve_default_runtime(&self) -> Result<RuntimeType, CapsuleError> {
        let target = self.resolve_default_target()?;
        RuntimeType::from_target_runtime(&target.runtime)
            .map(|runtime| runtime.normalize())
            .ok_or_else(|| {
                CapsuleError::ValidationError(format!(
                    "Invalid target '{}': runtime and entrypoint are required",
                    self.default_target
                ))
            })
    }

    /// Check whether this capsule implements the given schema identifier.
    ///
    /// The schema identifier may be a sha256 hash or a registry alias.
    pub fn implements_schema(
        &self,
        schema_id: &str,
        registry: &SchemaRegistry,
    ) -> Result<bool, CapsuleError> {
        let Some(poly) = &self.polymorphism else {
            return Ok(false);
        };

        let target = registry
            .resolve_schema_hash(schema_id)
            .map_err(|e| CapsuleError::ValidationError(e.to_string()))?;

        for entry in &poly.implements {
            let resolved = registry
                .resolve_schema_hash(entry)
                .map_err(|e| CapsuleError::ValidationError(e.to_string()))?;
            if resolved == target {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Load from file (auto-detects format by extension)
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, CapsuleError> {
        let path = path.as_ref();
        let content = fs::read_to_string(path).map_err(|e| CapsuleError::IoError(e.to_string()))?;

        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        match ext {
            "toml" => Self::from_toml_with_path(&content, path),
            "json" => Self::from_json(&content),
            _ => {
                // Try TOML first, then JSON
                Self::from_toml_with_path(&content, path).or_else(|_| Self::from_json(&content))
            }
        }
    }
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;

#[cfg(test)]
mod wasm_component_test {
    use super::*;
    
    #[test]
    fn test_wasm_component_preserved() {
        let toml = r#"
schema_version = "0.3"
name = "wasm-hello"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "wasm"
driver = "wasmtime"
run_command = "hello.wasm"
component = "hello.wasm"
"#;
        let model = CapsuleManifest::from_toml(toml).unwrap();
        let serialized = model.to_toml().unwrap();
        eprintln!("Serialized:\n{}", serialized);
        
        let targets = model.targets.as_ref().unwrap();
        let app_target = targets.named.get("app").unwrap();
        eprintln!("component field: {:?}", app_target.component);
        assert_eq!(app_target.component.as_deref(), Some("hello.wasm"));
    }
}

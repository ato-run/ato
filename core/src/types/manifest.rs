//! Capsule Manifest v0.2 Schema
//!
//! Implements the "Everything is a Capsule" paradigm for Gumball v0.3.0.
//! Supports both TOML (human-authored) and JSON (machine-generated) formats.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path};

use super::error::CapsuleError;
use super::utils::parse_memory_string;
use crate::orchestration::startup_order_from_dependencies;
use crate::schema_registry::SchemaRegistry;

/// Capsule Type - defines the fundamental nature of the Capsule
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CapsuleType {
    /// AI model inference (MLX, vLLM, etc.)
    Inference,
    /// Utility tool (RAG, code interpreter, etc.)
    Tool,
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

/// Capsule Manifest v0.2
///
/// The primary configuration format for all Capsules in Gumball v0.3.0+
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapsuleManifest {
    /// Schema version (must be "0.2")
    #[serde(default = "default_schema_version")]
    pub schema_version: String,

    /// Unique capsule identifier (kebab-case)
    pub name: String,

    /// Semantic version
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

    /// Supervisor Mode: Multi-service definition.
    ///
    /// Optional and dev-first: absence means single-process execution via `execution`.
    #[serde(default)]
    pub services: Option<HashMap<String, ServiceSpec>>,

    /// Distribution metadata generated at pack/publish time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution: Option<DistributionInfo>,
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
    "0.2".to_string()
}

fn is_supported_schema_version(value: &str) -> bool {
    matches!(value.trim(), "0.2" | "1")
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
    #[serde(default)]
    pub public: Vec<String>,

    /// Optional listening port.
    #[serde(default)]
    pub port: Option<u16>,

    /// Optional working directory.
    #[serde(default)]
    pub working_dir: Option<String>,
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

impl CapsuleManifest {
    /// Parse from TOML string
    pub fn from_toml(content: &str) -> Result<Self, CapsuleError> {
        let raw: toml::Value = toml::from_str(content)
            .map_err(|e| CapsuleError::ParseError(format!("TOML parse error: {}", e)))?;

        if raw.get("execution").is_some() {
            return Err(CapsuleError::ParseError(
                "legacy [execution] section is not supported in schema_version=0.2".to_string(),
            ));
        }

        toml::from_str(content)
            .map_err(|e| CapsuleError::ParseError(format!("TOML parse error: {}", e)))
    }

    /// Parse from JSON string
    pub fn from_json(content: &str) -> Result<Self, CapsuleError> {
        let raw: serde_json::Value = serde_json::from_str(content)
            .map_err(|e| CapsuleError::ParseError(format!("JSON parse error: {}", e)))?;
        if raw.get("execution").is_some() {
            return Err(CapsuleError::ParseError(
                "legacy [execution] section is not supported in schema_version=0.2".to_string(),
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

    /// Resolve the effective v0.2 target from `default_target`.
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
            "toml" => Self::from_toml(&content),
            "json" => Self::from_json(&content),
            _ => {
                // Try TOML first, then JSON
                Self::from_toml(&content).or_else(|_| Self::from_json(&content))
            }
        }
    }

    /// Validate the manifest
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();

        if self
            .state_owner_scope
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            errors.push(ValidationError::InvalidState(
                "state_owner_scope".to_string(),
                "state_owner_scope cannot be empty".to_string(),
            ));
        }

        if self
            .service_binding_scope
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            errors.push(ValidationError::InvalidService(
                "service_binding_scope".to_string(),
                "service_binding_scope cannot be empty".to_string(),
            ));
        }

        // Schema version must be "0.2"
        if !is_supported_schema_version(&self.schema_version) {
            errors.push(ValidationError::InvalidSchemaVersion(
                self.schema_version.clone(),
            ));
        }

        // Name must be kebab-case
        if !is_kebab_case(&self.name) {
            errors.push(ValidationError::InvalidName(self.name.clone()));
        }

        // Name length bounds (frozen v1.0)
        if !(3..=64).contains(&self.name.len()) {
            errors.push(ValidationError::InvalidName(self.name.clone()));
        }

        // Version must be semver
        if !is_semver(&self.version) {
            errors.push(ValidationError::InvalidVersion(self.version.clone()));
        }

        if let Some(pack) = &self.pack {
            if pack.include.iter().any(|pattern| pattern.trim().is_empty()) {
                errors.push(ValidationError::InvalidTarget(
                    "pack.include must not contain empty patterns".to_string(),
                ));
            }
            if pack.exclude.iter().any(|pattern| pattern.trim().is_empty()) {
                errors.push(ValidationError::InvalidTarget(
                    "pack.exclude must not contain empty patterns".to_string(),
                ));
            }
        }

        // Requirements memory strings must be parseable if present
        if let Some(v) = &self.requirements.vram_min {
            if parse_memory_string(v).is_err() {
                errors.push(ValidationError::InvalidMemoryString {
                    field: "requirements.vram_min",
                    value: v.clone(),
                });
            }
        }
        if let Some(v) = &self.requirements.vram_recommended {
            if parse_memory_string(v).is_err() {
                errors.push(ValidationError::InvalidMemoryString {
                    field: "requirements.vram_recommended",
                    value: v.clone(),
                });
            }
        }
        if let Some(v) = &self.requirements.disk {
            if parse_memory_string(v).is_err() {
                errors.push(ValidationError::InvalidMemoryString {
                    field: "requirements.disk",
                    value: v.clone(),
                });
            }
        }

        // Inference type should have capabilities
        if self.capsule_type == CapsuleType::Inference && self.capabilities.is_none() {
            errors.push(ValidationError::MissingCapabilities);
        }

        // Inference type should have model config
        if self.capsule_type == CapsuleType::Inference && self.model.is_none() {
            errors.push(ValidationError::MissingModelConfig);
        }

        // default_target must point to an existing named target.
        let named_targets = self
            .targets
            .as_ref()
            .map(|t| t.named_targets())
            .cloned()
            .unwrap_or_default();
        if self.default_target.trim().is_empty() {
            errors.push(ValidationError::MissingDefaultTarget);
        }
        if named_targets.is_empty() {
            errors.push(ValidationError::MissingTargets);
        } else if !self.default_target.trim().is_empty()
            && !named_targets.contains_key(self.default_target.trim())
        {
            errors.push(ValidationError::DefaultTargetNotFound(
                self.default_target.clone(),
            ));
        }

        let has_services = self
            .services
            .as_ref()
            .map(|services| !services.is_empty())
            .unwrap_or(false);
        let has_target_services = self
            .services
            .as_ref()
            .map(|services| {
                services.values().any(|service| {
                    service
                        .target
                        .as_ref()
                        .map(|target| !target.trim().is_empty())
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        let mut requires_web_services_validation = false;

        for (label, target) in &named_targets {
            let runtime = target.runtime.trim().to_ascii_lowercase();
            let entrypoint = target.entrypoint.trim();
            if label.trim().is_empty()
                || runtime.is_empty()
                || !matches!(runtime.as_str(), "source" | "wasm" | "oci" | "web")
            {
                errors.push(ValidationError::InvalidTarget(label.clone()));
                continue;
            }

            if runtime == "source" {
                if entrypoint.is_empty() {
                    errors.push(ValidationError::InvalidTarget(label.clone()));
                    continue;
                }
                let effective_driver = infer_source_driver(target, entrypoint);
                if matches!(
                    effective_driver.as_deref(),
                    Some("deno") | Some("node") | Some("python")
                ) && target
                    .runtime_version
                    .as_ref()
                    .map(|v| v.trim().is_empty())
                    .unwrap_or(true)
                {
                    errors.push(ValidationError::MissingRuntimeVersion(
                        label.clone(),
                        effective_driver.unwrap_or_else(|| "unknown".to_string()),
                    ));
                }
            }

            if runtime == "web" {
                if !target.public.is_empty() {
                    errors.push(ValidationError::InvalidWebTarget(
                        label.clone(),
                        "public is no longer supported for runtime=web".to_string(),
                    ));
                }

                if target.port.is_none() {
                    errors.push(ValidationError::InvalidWebTarget(
                        label.clone(),
                        "port is required for runtime=web".to_string(),
                    ));
                } else if target.port == Some(0) {
                    errors.push(ValidationError::InvalidWebTarget(
                        label.clone(),
                        "port must be between 1 and 65535".to_string(),
                    ));
                }

                let mut normalized_driver: Option<String> = None;
                match target.driver.as_ref() {
                    None => errors.push(ValidationError::InvalidWebTarget(
                        label.clone(),
                        "driver is required for runtime=web (static|node|deno|python)".to_string(),
                    )),
                    Some(driver) => {
                        let normalized = driver.trim().to_ascii_lowercase();
                        if matches!(normalized.as_str(), "browser_static" | "browser-static") {
                            errors.push(ValidationError::InvalidWebTarget(
                                label.clone(),
                                "driver 'browser_static' has been removed; use 'static'"
                                    .to_string(),
                            ));
                        } else if !matches!(
                            normalized.as_str(),
                            "static" | "node" | "deno" | "python"
                        ) {
                            errors.push(ValidationError::InvalidTargetDriver(
                                label.clone(),
                                driver.clone(),
                            ));
                        } else {
                            normalized_driver = Some(normalized);
                        }
                    }
                }

                let web_services_mode =
                    matches!(normalized_driver.as_deref(), Some("deno")) && has_services;
                if web_services_mode {
                    requires_web_services_validation = true;
                    if std::path::Path::new(entrypoint)
                        .file_name()
                        .and_then(|v| v.to_str())
                        .map(|v| v.eq_ignore_ascii_case("ato-entry.ts"))
                        .unwrap_or(false)
                    {
                        errors.push(ValidationError::InvalidWebTarget(
                            label.clone(),
                            "entrypoint='ato-entry.ts' is deprecated. Define top-level [services] and remove ato-entry.ts orchestrator."
                                .to_string(),
                        ));
                    }
                } else {
                    if entrypoint.is_empty() {
                        errors.push(ValidationError::InvalidTarget(label.clone()));
                        continue;
                    }
                    if matches!(
                        normalized_driver.as_deref(),
                        Some("node") | Some("deno") | Some("python")
                    ) && entrypoint.split_whitespace().count() > 1
                    {
                        errors.push(ValidationError::InvalidWebTarget(
                            label.clone(),
                            "entrypoint must be a script file path (shell command strings are not allowed)"
                                .to_string(),
                        ));
                    }
                }
                continue;
            }

            if runtime == "oci" {
                let image = target.image.as_deref().map(str::trim).unwrap_or("");
                if entrypoint.is_empty() && image.is_empty() {
                    errors.push(ValidationError::InvalidTarget(label.clone()));
                    continue;
                }
            } else if entrypoint.is_empty() {
                errors.push(ValidationError::InvalidTarget(label.clone()));
                continue;
            }

            if let Some(driver) = target.driver.as_ref() {
                let normalized = driver.trim().to_ascii_lowercase();
                if !matches!(
                    normalized.as_str(),
                    "static" | "deno" | "node" | "python" | "wasmtime" | "native"
                ) {
                    errors.push(ValidationError::InvalidTargetDriver(
                        label.clone(),
                        driver.clone(),
                    ));
                    continue;
                }
                if normalized == "static" {
                    errors.push(ValidationError::InvalidTargetDriver(
                        label.clone(),
                        driver.clone(),
                    ));
                    continue;
                }
            }
        }

        if has_target_services {
            let services = self.services.as_ref().cloned().unwrap_or_default();
            if services.is_empty() {
                errors.push(ValidationError::InvalidService(
                    "main".to_string(),
                    "top-level [services] must define at least one service for orchestration mode"
                        .to_string(),
                ));
            } else {
                if !services.contains_key("main") {
                    errors.push(ValidationError::InvalidService(
                        "main".to_string(),
                        "services.main is required for orchestration mode".to_string(),
                    ));
                }

                let mut dependencies = HashMap::new();
                let mut resolved_runtimes = HashMap::new();

                for (name, service) in &services {
                    let target_name = service.target.as_deref().map(str::trim).unwrap_or("");
                    let has_target = !target_name.is_empty();
                    let has_entrypoint = !service.entrypoint.trim().is_empty();

                    if has_target && has_entrypoint {
                        errors.push(ValidationError::InvalidService(
                            name.to_string(),
                            "target and entrypoint are mutually exclusive".to_string(),
                        ));
                    }

                    let effective_target = if has_target {
                        Some(target_name.to_string())
                    } else if name == "main" && !has_entrypoint {
                        Some(self.default_target.trim().to_string())
                    } else {
                        None
                    };

                    let target_label = match effective_target {
                        Some(target_label) => target_label,
                        None => {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "target is required for orchestration mode".to_string(),
                            ));
                            dependencies.insert(
                                name.to_string(),
                                service.depends_on.clone().unwrap_or_default(),
                            );
                            continue;
                        }
                    };

                    let Some(target) = self
                        .targets
                        .as_ref()
                        .and_then(|targets| targets.named_target(&target_label))
                    else {
                        errors.push(ValidationError::InvalidService(
                            name.to_string(),
                            format!("target '{}' does not exist under [targets]", target_label),
                        ));
                        dependencies.insert(
                            name.to_string(),
                            service.depends_on.clone().unwrap_or_default(),
                        );
                        continue;
                    };

                    let runtime = target.runtime.trim().to_ascii_lowercase();
                    if runtime == "wasm" {
                        errors.push(ValidationError::InvalidService(
                            name.to_string(),
                            "runtime=wasm is not supported in orchestration mode".to_string(),
                        ));
                    }

                    if service
                        .network
                        .as_ref()
                        .map(|network| {
                            network.aliases.iter().any(|alias| alias.trim().is_empty())
                                || network
                                    .allow_from
                                    .iter()
                                    .any(|value| value.trim().is_empty())
                        })
                        .unwrap_or(false)
                    {
                        errors.push(ValidationError::InvalidService(
                            name.to_string(),
                            "network aliases and allow_from must not contain empty values"
                                .to_string(),
                        ));
                    }

                    if let Some(probe) = service.readiness_probe.as_ref() {
                        if probe.port.trim().is_empty() {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe.port must be a non-empty placeholder name"
                                    .to_string(),
                            ));
                        }
                        let has_http_get = probe
                            .http_get
                            .as_ref()
                            .map(|v| !v.trim().is_empty())
                            .unwrap_or(false);
                        let has_tcp_connect = probe
                            .tcp_connect
                            .as_ref()
                            .map(|v| !v.trim().is_empty())
                            .unwrap_or(false);
                        if !has_http_get && !has_tcp_connect {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe must define http_get or tcp_connect".to_string(),
                            ));
                        }
                    }

                    let deps = service.depends_on.clone().unwrap_or_default();
                    for dep in &deps {
                        if !services.contains_key(dep) {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                format!("depends_on references unknown service '{}'", dep),
                            ));
                        }
                    }

                    if let Some(network) = service.network.as_ref() {
                        for allowed in &network.allow_from {
                            if !services.contains_key(allowed) {
                                errors.push(ValidationError::InvalidService(
                                    name.to_string(),
                                    format!("allow_from references unknown service '{}'", allowed),
                                ));
                            }
                        }
                    }

                    dependencies.insert(name.to_string(), deps);
                    resolved_runtimes.insert(name.to_string(), runtime);
                }

                for (name, service) in &services {
                    let Some(runtime) = resolved_runtimes.get(name) else {
                        continue;
                    };
                    for dep in service.depends_on.clone().unwrap_or_default() {
                        let Some(dep_runtime) = resolved_runtimes.get(&dep) else {
                            continue;
                        };
                        if runtime == "oci" && dep_runtime != "oci" {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                format!(
                                    "OCI service '{}' cannot depend on non-OCI service '{}'",
                                    name, dep
                                ),
                            ));
                        }
                        if let Some(network) =
                            services.get(&dep).and_then(|svc| svc.network.as_ref())
                        {
                            if !network.allow_from.is_empty()
                                && !network.allow_from.iter().any(|value| value == name)
                            {
                                errors.push(ValidationError::InvalidService(
                                    name.to_string(),
                                    format!(
                                        "service '{}' is not allowed to connect to '{}'",
                                        name, dep
                                    ),
                                ));
                            }
                        }
                    }
                }

                if let Err(err) = startup_order_from_dependencies(&dependencies) {
                    errors.push(ValidationError::InvalidService(
                        "services".to_string(),
                        err.to_string(),
                    ));
                }
            }
        } else if requires_web_services_validation {
            let services = self.services.as_ref().cloned().unwrap_or_default();
            if services.is_empty() {
                errors.push(ValidationError::InvalidService(
                    "main".to_string(),
                    "top-level [services] must define at least one service for web/deno services mode".to_string(),
                ));
            } else {
                if !services.contains_key("main") {
                    errors.push(ValidationError::InvalidService(
                        "main".to_string(),
                        "services.main is required for web/deno services mode".to_string(),
                    ));
                }

                for (name, service) in &services {
                    if service.entrypoint.trim().is_empty() {
                        errors.push(ValidationError::InvalidService(
                            name.to_string(),
                            "entrypoint is required".to_string(),
                        ));
                    }

                    if service
                        .expose
                        .as_ref()
                        .is_some_and(|ports| !ports.is_empty())
                    {
                        errors.push(ValidationError::InvalidService(
                            name.to_string(),
                            "expose is not supported yet in web/deno services mode".to_string(),
                        ));
                    }

                    if let Some(probe) = service.readiness_probe.as_ref() {
                        if probe.port.trim().is_empty() {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe.port must be a non-empty placeholder name"
                                    .to_string(),
                            ));
                        }
                        let has_http_get = probe
                            .http_get
                            .as_ref()
                            .map(|v| !v.trim().is_empty())
                            .unwrap_or(false);
                        let has_tcp_connect = probe
                            .tcp_connect
                            .as_ref()
                            .map(|v| !v.trim().is_empty())
                            .unwrap_or(false);
                        if !has_http_get && !has_tcp_connect {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe must define http_get or tcp_connect".to_string(),
                            ));
                        }
                    }
                }

                for (name, service) in &services {
                    if let Some(deps) = service.depends_on.as_ref() {
                        for dep in deps {
                            if !services.contains_key(dep) {
                                errors.push(ValidationError::InvalidService(
                                    name.to_string(),
                                    format!("depends_on references unknown service '{}'", dep),
                                ));
                            }
                        }
                    }
                }

                if let Err(cycle) = detect_service_cycle(&services) {
                    errors.push(ValidationError::InvalidService(
                        "services".to_string(),
                        format!("circular dependency detected: {}", cycle),
                    ));
                }
            }
        }

        // Storage volumes (minimal): require at least one OCI target.
        let has_oci_target = self.targets.as_ref().is_some_and(|targets| {
            targets
                .named_targets()
                .values()
                .any(|t| t.runtime.eq_ignore_ascii_case("oci"))
                || targets.oci.is_some()
        });
        if !self.storage.volumes.is_empty() {
            if !has_oci_target {
                errors.push(ValidationError::StorageOnlyForDocker);
            }

            let mut names = std::collections::HashSet::new();
            for vol in &self.storage.volumes {
                if vol.name.trim().is_empty() {
                    errors.push(ValidationError::InvalidStorageVolume);
                    continue;
                }
                if !names.insert(vol.name.trim().to_string()) {
                    errors.push(ValidationError::InvalidStorageVolume);
                }
                let mp = vol.mount_path.trim();
                if mp.is_empty() || !mp.starts_with('/') || mp.contains("..") {
                    errors.push(ValidationError::InvalidStorageVolume);
                }
            }
        }

        if !self.state.is_empty() {
            if self
                .services
                .as_ref()
                .map(|services| {
                    services.is_empty()
                        || !services
                            .values()
                            .any(|service| !service.state_bindings.is_empty())
                })
                .unwrap_or(true)
            {
                errors.push(ValidationError::InvalidState(
                    "state".to_string(),
                    "services.*.state_bindings are required when [state] is declared".to_string(),
                ));
            }

            let mut shared_state_bindings = HashMap::new();
            for (state_name, requirement) in &self.state {
                let trimmed_name = state_name.trim();
                if trimmed_name.is_empty() || !is_kebab_case(trimmed_name) {
                    errors.push(ValidationError::InvalidState(
                        state_name.clone(),
                        "state name must be kebab-case".to_string(),
                    ));
                }

                if requirement.purpose.trim().is_empty() {
                    errors.push(ValidationError::InvalidState(
                        state_name.clone(),
                        "purpose is required".to_string(),
                    ));
                }
                if requirement
                    .producer
                    .as_deref()
                    .is_some_and(|producer| producer.trim().is_empty())
                {
                    errors.push(ValidationError::InvalidState(
                        state_name.clone(),
                        "producer cannot be empty".to_string(),
                    ));
                }

                if requirement.kind != StateKind::Filesystem {
                    errors.push(ValidationError::InvalidState(
                        state_name.clone(),
                        "only kind=\"filesystem\" is supported in this PoC".to_string(),
                    ));
                }

                if requirement.durability == StateDurability::Persistent {
                    if requirement.attach != StateAttach::Explicit {
                        errors.push(ValidationError::InvalidState(
                            state_name.clone(),
                            "persistent state requires attach=\"explicit\"".to_string(),
                        ));
                    }
                    if requirement
                        .schema_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .is_none()
                    {
                        errors.push(ValidationError::InvalidState(
                            state_name.clone(),
                            "persistent state requires schema_id".to_string(),
                        ));
                    }
                }
            }

            if let Some(services) = self.services.as_ref() {
                for (service_name, service) in services {
                    if service.state_bindings.is_empty() {
                        continue;
                    }

                    let Some(target_label) = service
                        .target
                        .as_ref()
                        .map(|value| value.trim())
                        .filter(|value| !value.is_empty())
                    else {
                        errors.push(ValidationError::InvalidStateBinding(
                            service_name.clone(),
                            "state_bindings require target-based services".to_string(),
                        ));
                        continue;
                    };

                    if let Some(target) = named_targets.get(target_label) {
                        if !target.runtime.eq_ignore_ascii_case("oci") {
                            errors.push(ValidationError::InvalidStateBinding(
                                service_name.clone(),
                                format!(
                                    "state_bindings are only supported for OCI targets in this PoC (target '{}')",
                                    target_label
                                ),
                            ));
                        }
                    }

                    let mut bound_states = std::collections::HashSet::new();
                    let mut bound_targets = std::collections::HashSet::new();
                    for binding in &service.state_bindings {
                        let state_name = binding.state.trim();
                        let target = binding.target.trim();

                        if state_name.is_empty() {
                            errors.push(ValidationError::InvalidStateBinding(
                                service_name.clone(),
                                "binding.state is required".to_string(),
                            ));
                        } else {
                            if !bound_states.insert(state_name.to_string()) {
                                errors.push(ValidationError::InvalidStateBinding(
                                    service_name.clone(),
                                    format!("state '{}' is bound more than once", state_name),
                                ));
                            }

                            if let Some(previous_service) = shared_state_bindings
                                .insert(state_name.to_string(), service_name.clone())
                            {
                                if previous_service != *service_name {
                                    errors.push(ValidationError::InvalidStateBinding(
                                        service_name.clone(),
                                        format!(
                                            "state '{}' is already bound by service '{}'; shared mutable state is not supported in this PoC",
                                            state_name, previous_service
                                        ),
                                    ));
                                }
                            }

                            match self.state.get(state_name) {
                                Some(_) => {}
                                None => errors.push(ValidationError::InvalidStateBinding(
                                    service_name.clone(),
                                    format!("state '{}' is not declared under [state]", state_name),
                                )),
                            }
                        }

                        if !is_valid_mount_path(target) {
                            errors.push(ValidationError::InvalidStateBinding(
                                service_name.clone(),
                                format!("target '{}' must be an absolute path", binding.target),
                            ));
                        } else if !bound_targets.insert(target.to_string()) {
                            errors.push(ValidationError::InvalidStateBinding(
                                service_name.clone(),
                                format!("target '{}' is bound more than once", target),
                            ));
                        }
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Check if this Capsule can run on the current platform
    pub fn supports_current_platform(&self) -> bool {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            self.requirements.platform.is_empty()
                || self.requirements.platform.contains(&Platform::DarwinArm64)
        }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            self.requirements.platform.is_empty()
                || self.requirements.platform.contains(&Platform::DarwinX86_64)
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            self.requirements.platform.is_empty()
                || self.requirements.platform.contains(&Platform::LinuxAmd64)
        }
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        {
            self.requirements.platform.is_empty()
                || self.requirements.platform.contains(&Platform::LinuxArm64)
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64")
        )))]
        {
            false
        }
    }

    /// Get effective display name
    pub fn display_name(&self) -> &str {
        self.metadata.display_name.as_deref().unwrap_or(&self.name)
    }

    /// Check if this is an inference Capsule
    pub fn is_inference(&self) -> bool {
        self.capsule_type == CapsuleType::Inference
    }

    /// Check if cloud fallback is enabled
    pub fn can_fallback_to_cloud(&self) -> bool {
        self.routing.fallback_to_cloud && self.routing.cloud_capsule.is_some()
    }

    pub fn ephemeral_state_source_path(&self, state_name: &str) -> Result<String, CapsuleError> {
        let state_name = state_name.trim();
        if !is_kebab_case(state_name) {
            return Err(CapsuleError::ValidationError(format!(
                "state '{}' must be kebab-case before deriving an ephemeral state path",
                state_name
            )));
        }

        Ok(format!(
            "{}/{}/{}",
            default_ephemeral_state_base().trim_end_matches('/'),
            self.name,
            state_name
        ))
    }

    pub fn state_source_path(
        &self,
        state_name: &str,
        requirement: &StateRequirement,
        overrides: Option<&HashMap<String, String>>,
    ) -> Result<String, CapsuleError> {
        if let Some(path) = overrides
            .and_then(|entries| entries.get(state_name.trim()))
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            return Ok(path.to_string());
        }

        match requirement.durability {
            StateDurability::Ephemeral => self.ephemeral_state_source_path(state_name),
            StateDurability::Persistent => Err(CapsuleError::ValidationError(format!(
                "state '{}' requires an explicit persistent binding before it can be attached",
                state_name.trim()
            ))),
        }
    }

    /// Resolve the producer identity for a state requirement.
    ///
    /// When `producer` is omitted on `[state.<name>]`, the manifest name is used as the
    /// default producer identity so persistent attach checks remain fail-closed. When
    /// both are empty, this returns `None`; callers should treat that as a validation
    /// failure and reject the attach.
    pub fn state_producer(&self, state_name: &str) -> Option<String> {
        self.state
            .get(state_name.trim())
            // Prefer an explicit producer on the state requirement; otherwise fall back to the
            // manifest identity so persistent attach compatibility remains fail-closed by default.
            .and_then(|requirement| requirement.producer.as_deref())
            .map(str::trim)
            .filter(|producer| !producer.is_empty())
            .map(str::to_string)
            .or_else(|| {
                let name = self.name.trim();
                (!name.is_empty()).then(|| name.to_string())
            })
    }

    /// Resolve the owner scope used by the local persistent state registry.
    ///
    /// `state_owner_scope` is treated as an opaque, user-controlled identity string. When it
    /// is omitted, the manifest name remains the fail-closed default so existing manifests keep
    /// their current behavior.
    pub fn persistent_state_owner_scope(&self) -> Option<String> {
        self.state_owner_scope
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                let name = self.name.trim();
                (!name.is_empty()).then(|| name.to_string())
            })
    }

    /// Resolve the owner scope used by the host-side service binding registry.
    ///
    /// `service_binding_scope` is treated as an opaque, user-controlled identity string.
    /// When it is omitted, the manifest name remains the fail-closed default so
    /// existing manifests keep a stable binding identity.
    pub fn host_service_binding_scope(&self) -> Option<String> {
        self.service_binding_scope
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                let name = self.name.trim();
                (!name.is_empty()).then(|| name.to_string())
            })
    }
}

/// Validation error types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    InvalidSchemaVersion(String),
    InvalidName(String),
    InvalidMemoryString { field: &'static str, value: String },
    InvalidVersion(String),
    MissingCapabilities,
    MissingModelConfig,
    InvalidPort(u16),
    StorageOnlyForDocker,
    InvalidStorageVolume,
    MissingDefaultTarget,
    MissingTargets,
    DefaultTargetNotFound(String),
    InvalidTarget(String),
    InvalidTargetDriver(String, String),
    MissingRuntimeVersion(String, String),
    InvalidWebTarget(String, String),
    InvalidService(String, String),
    InvalidState(String, String),
    InvalidStateBinding(String, String),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::InvalidSchemaVersion(v) => {
                write!(
                    f,
                    "Invalid schema_version '{}', expected '1' or legacy '0.2'",
                    v
                )
            }
            ValidationError::InvalidName(n) => {
                write!(f, "Invalid name '{}', must be kebab-case", n)
            }
            ValidationError::InvalidMemoryString { field, value } => {
                write!(f, "Invalid memory string for {}: '{}'", field, value)
            }
            ValidationError::InvalidVersion(v) => {
                write!(f, "Invalid version '{}', must be semver (e.g., 1.0.0)", v)
            }
            ValidationError::MissingCapabilities => {
                write!(f, "Inference Capsule must have capabilities defined")
            }
            ValidationError::MissingModelConfig => {
                write!(f, "Inference Capsule must have model config defined")
            }
            ValidationError::InvalidPort(p) => {
                write!(f, "Invalid port {}", p)
            }
            ValidationError::StorageOnlyForDocker => {
                write!(
                    f,
                    "Storage volumes are only supported for execution.runtime=docker"
                )
            }
            ValidationError::InvalidStorageVolume => {
                write!(
                    f,
                    "Invalid storage volume (requires unique name and absolute mount_path)"
                )
            }
            ValidationError::MissingDefaultTarget => {
                write!(f, "default_target is required")
            }
            ValidationError::MissingTargets => {
                write!(f, "At least one [targets.<label>] entry is required")
            }
            ValidationError::DefaultTargetNotFound(target) => {
                write!(
                    f,
                    "default_target '{}' does not exist under [targets]",
                    target
                )
            }
            ValidationError::InvalidTarget(label) => {
                write!(
                    f,
                    "Invalid target '{}': runtime and entrypoint are required",
                    label
                )
            }
            ValidationError::InvalidTargetDriver(label, driver) => {
                write!(
                    f,
                    "Invalid target '{}': unsupported driver '{}' (allowed: static|deno|node|python|wasmtime|native)",
                    label, driver
                )
            }
            ValidationError::MissingRuntimeVersion(label, driver) => {
                write!(
                    f,
                    "Invalid target '{}': runtime_version is required for runtime=source driver='{}'",
                    label, driver
                )
            }
            ValidationError::InvalidWebTarget(label, message) => {
                write!(f, "Invalid web target '{}': {}", label, message)
            }
            ValidationError::InvalidService(name, message) => {
                write!(f, "Invalid service '{}': {}", name, message)
            }
            ValidationError::InvalidState(name, message) => {
                write!(f, "Invalid state '{}': {}", name, message)
            }
            ValidationError::InvalidStateBinding(name, message) => {
                write!(
                    f,
                    "Invalid state binding for service '{}': {}",
                    name, message
                )
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Check if string is kebab-case
fn is_kebab_case(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let chars: Vec<char> = s.chars().collect();
    // Must start and end with alphanumeric
    if !chars[0].is_ascii_lowercase() && !chars[0].is_ascii_digit() {
        return false;
    }
    if !chars.last().unwrap().is_ascii_lowercase() && !chars.last().unwrap().is_ascii_digit() {
        return false;
    }
    // Only lowercase, digits, and hyphens allowed
    chars
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
}

/// Check if string is valid semver
fn is_semver(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let version_part = parts[0];
    let version_nums: Vec<&str> = version_part.split('.').collect();

    if version_nums.len() != 3 {
        return false;
    }

    version_nums.iter().all(|n| n.parse::<u32>().is_ok())
}

pub(crate) fn is_valid_mount_path(path: &str) -> bool {
    let path = Path::new(path);
    path.is_absolute()
        && path.components().all(|component| {
            !matches!(
                component,
                Component::ParentDir | Component::CurDir | Component::Prefix(_)
            )
        })
}

fn infer_source_driver(target: &NamedTarget, entrypoint: &str) -> Option<String> {
    let _ = entrypoint;
    if let Some(driver) = target.driver.as_ref() {
        let normalized = driver.trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }
    None
}

fn detect_service_cycle(services: &HashMap<String, ServiceSpec>) -> Result<(), String> {
    fn visit(
        current: &str,
        services: &HashMap<String, ServiceSpec>,
        visiting: &mut HashSet<String>,
        visited: &mut HashSet<String>,
        stack: &mut Vec<String>,
    ) -> Result<(), String> {
        if visited.contains(current) {
            return Ok(());
        }
        if visiting.contains(current) {
            stack.push(current.to_string());
            return Err(stack.join(" -> "));
        }

        visiting.insert(current.to_string());
        stack.push(current.to_string());
        if let Some(spec) = services.get(current) {
            if let Some(deps) = spec.depends_on.as_ref() {
                for dep in deps {
                    if services.contains_key(dep) {
                        visit(dep, services, visiting, visited, stack)?;
                    }
                }
            }
        }
        stack.pop();
        visiting.remove(current);
        visited.insert(current.to_string());
        Ok(())
    }

    let mut names: Vec<&String> = services.keys().collect();
    names.sort();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for name in names {
        let mut stack = Vec::new();
        visit(name, services, &mut visiting, &mut visited, &mut stack)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_TOML: &str = r#"
schema_version = "0.2"
name = "mlx-qwen3-8b"
version = "1.0.0"
type = "inference"
default_target = "cli"

[metadata]
display_name = "Qwen3 8B (MLX)"
description = "Local inference on Apple Silicon"
author = "gumball-official"
tags = ["llm", "mlx"]

[capabilities]
chat = true
function_calling = true
vision = false
context_length = 128000

[requirements]
platform = ["darwin-arm64"]
vram_min = "6GB"
vram_recommended = "8GB"
disk = "5GB"

[targets]
port = 8081
health_check = "/health"
startup_timeout = 120

[targets.cli]
runtime = "source"
entrypoint = "server.py"

[targets.cli.env]
GUMBALL_MODEL = "qwen3-8b"

[routing]
weight = "light"
fallback_to_cloud = true
cloud_capsule = "vllm-qwen3-8b"

[model]
source = "hf:org/model"
quantization = "4bit"
"#;

    #[test]
    fn test_parse_valid_toml() {
        let manifest = CapsuleManifest::from_toml(VALID_TOML).unwrap();

        assert_eq!(manifest.name, "mlx-qwen3-8b");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.capsule_type, CapsuleType::Inference);
        assert_eq!(manifest.targets.as_ref().and_then(|t| t.port), Some(8081));
        assert_eq!(
            manifest.resolve_default_runtime().unwrap(),
            RuntimeType::Source
        );
        assert!(manifest.capabilities.as_ref().unwrap().chat);
        assert_eq!(manifest.routing.weight, RouteWeight::Light);
    }

    #[test]
    fn test_validate_valid_manifest() {
        let manifest = CapsuleManifest::from_toml(VALID_TOML).unwrap();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_validate_invalid_schema_version() {
        let toml = VALID_TOML.replace("schema_version = \"0.2\"", "schema_version = \"2.0\"");
        let manifest = CapsuleManifest::from_toml(&toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidSchemaVersion(_))));
    }

    #[test]
    fn test_validate_invalid_memory_string() {
        let toml = VALID_TOML.replace("vram_min = \"6GB\"", "vram_min = \"6XB\"");
        let manifest = CapsuleManifest::from_toml(&toml).unwrap();
        let errs = manifest.validate().unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidMemoryString { .. })));
    }

    #[test]
    fn test_validate_invalid_name() {
        let toml = VALID_TOML.replace("name = \"mlx-qwen3-8b\"", "name = \"Invalid Name!\"");
        let manifest = CapsuleManifest::from_toml(&toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidName(_))));
    }

    #[test]
    fn test_validate_invalid_driver() {
        let toml = VALID_TOML.replace(
            "[targets.cli]\nruntime = \"source\"\nentrypoint = \"server.py\"",
            "[targets.cli]\nruntime = \"source\"\ndriver = \"invalid-driver\"\nentrypoint = \"server.py\"",
        );
        let manifest = CapsuleManifest::from_toml(&toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidTargetDriver(_, _))));
    }

    #[test]
    fn test_validate_source_driver_requires_runtime_version() {
        let toml = VALID_TOML.replace(
            "[targets.cli]\nruntime = \"source\"\nentrypoint = \"server.py\"",
            "[targets.cli]\nruntime = \"source\"\ndriver = \"python\"\nentrypoint = \"server.py\"",
        );
        let manifest = CapsuleManifest::from_toml(&toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| matches!(e, ValidationError::MissingRuntimeVersion(_, _))));
    }

    #[test]
    fn test_validate_web_requires_driver_and_port() {
        let toml = r#"
schema_version = "0.2"
name = "web-app"
version = "0.1.0"
type = "app"
default_target = "static"

[targets.static]
runtime = "web"
entrypoint = "dist"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidWebTarget(_, msg) if msg.contains("driver is required")
        )));
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidWebTarget(_, msg) if msg.contains("port is required")
        )));
    }

    #[test]
    fn test_validate_web_rejects_public_and_browser_static() {
        let toml = r#"
schema_version = "0.2"
name = "web-app"
version = "0.1.0"
type = "app"
default_target = "static"

[targets.static]
runtime = "web"
driver = "browser_static"
entrypoint = "dist"
public = ["dist/**"]
port = 8080
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidWebTarget(_, msg) if msg.contains("driver 'browser_static' has been removed")
        )));
    }

    #[test]
    fn test_validate_web_static_accepts_port_and_driver() {
        let toml = r#"
schema_version = "0.2"
name = "web-app"
version = "0.1.0"
type = "app"
default_target = "static"

[targets.static]
runtime = "web"
driver = "static"
entrypoint = "dist"
port = 8080
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_validate_web_dynamic_rejects_shell_style_entrypoint() {
        let toml = r#"
schema_version = "0.2"
name = "web-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "node"
entrypoint = "npm run start"
port = 3000
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidWebTarget(_, msg) if msg.contains("shell command strings are not allowed")
        )));
    }

    #[test]
    fn test_validate_web_deno_services_allows_empty_target_entrypoint() {
        let toml = r#"
schema_version = "0.2"
name = "web-services-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "deno"
port = 4173

[services.main]
entrypoint = "node apps/dashboard/server.js"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_validate_web_deno_services_requires_main_service() {
        let toml = r#"
schema_version = "0.2"
name = "web-services-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "deno"
port = 4173

[services.api]
entrypoint = "python apps/api/main.py"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidService(name, msg)
                if name == "main" && msg.contains("services.main is required")
        )));
    }

    #[test]
    fn test_validate_web_deno_services_rejects_unknown_dependency() {
        let toml = r#"
schema_version = "0.2"
name = "web-services-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "deno"
port = 4173

[services.main]
entrypoint = "node apps/dashboard/server.js"
depends_on = ["api"]
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidService(name, msg)
                if name == "main" && msg.contains("unknown service 'api'")
        )));
    }

    #[test]
    fn test_validate_web_deno_services_rejects_circular_dependencies() {
        let toml = r#"
schema_version = "0.2"
name = "web-services-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "deno"
port = 4173

[services.main]
entrypoint = "node apps/dashboard/server.js"
depends_on = ["api"]

[services.api]
entrypoint = "python apps/api/main.py"
depends_on = ["main"]
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidService(name, msg)
                if name == "services" && msg.contains("circular dependency detected")
        )));
    }

    #[test]
    fn test_validate_web_deno_services_rejects_invalid_readiness_probe() {
        let toml = r#"
schema_version = "0.2"
name = "web-services-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "deno"
port = 4173

[services.main]
entrypoint = "node apps/dashboard/server.js"

[services.api]
entrypoint = "python apps/api/main.py"
readiness_probe = { port = "API_PORT" }
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidService(name, msg)
                if name == "api" && msg.contains("http_get or tcp_connect")
        )));
    }

    #[test]
    fn test_validate_web_deno_services_rejects_expose() {
        let toml = r#"
schema_version = "0.2"
name = "web-services-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "deno"
port = 4173

[services.main]
entrypoint = "node apps/dashboard/server.js"
expose = ["API_PORT"]
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidService(name, msg)
                if name == "main" && msg.contains("expose is not supported")
        )));
    }

    #[test]
    fn test_validate_ephemeral_state_binding_for_oci_service() {
        let toml = r#"
schema_version = "0.2"
name = "stateful-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[state.data]
kind = "filesystem"
durability = "ephemeral"
purpose = "primary-data"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_state_binding_for_non_oci_service() {
        let toml = r#"
schema_version = "0.2"
name = "stateful-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "node"
entrypoint = "server.js"
port = 3000

[state.data]
kind = "filesystem"
durability = "ephemeral"
purpose = "primary-data"

[services.main]
target = "web"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidStateBinding(name, msg)
                if name == "main" && msg.contains("only supported for OCI targets")
        )));
    }

    #[test]
    fn test_validate_accepts_persistent_state_with_explicit_attach() {
        let toml = r#"
schema_version = "0.2"
name = "stateful-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "vaultwarden/data/v1"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_empty_state_owner_scope() {
        let toml = r#"
schema_version = "0.2"
name = "stateful-app"
version = "0.1.0"
type = "app"
default_target = "app"
state_owner_scope = "   "

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "vaultwarden/data/v1"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|error| matches!(
            error,
            ValidationError::InvalidState(name, message)
                if name == "state_owner_scope" && message.contains("cannot be empty")
        )));
    }

    #[test]
    fn test_persistent_state_owner_scope_prefers_explicit_field() {
        let toml = r#"
schema_version = "0.2"
name = "stateful-app"
version = "0.1.0"
type = "app"
default_target = "app"
state_owner_scope = "tenant/acme/prod"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "vaultwarden/data/v1"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        assert_eq!(
            manifest.persistent_state_owner_scope().as_deref(),
            Some("tenant/acme/prod")
        );
    }

    #[test]
    fn test_validate_rejects_empty_service_binding_scope() {
        let toml = r#"
schema_version = "0.2"
name = "stateful-app"
version = "0.1.0"
type = "app"
default_target = "app"
service_binding_scope = "   "

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[services.main]
target = "app"
network = { publish = true }
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|error| matches!(
            error,
            ValidationError::InvalidService(name, message)
                if name == "service_binding_scope" && message.contains("cannot be empty")
        )));
    }

    #[test]
    fn test_host_service_binding_scope_prefers_explicit_field() {
        let toml = r#"
schema_version = "0.2"
name = "stateful-app"
version = "0.1.0"
type = "app"
default_target = "app"
service_binding_scope = "tenant/acme/services"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[services.main]
target = "app"
network = { publish = true }
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        assert_eq!(
            manifest.host_service_binding_scope().as_deref(),
            Some("tenant/acme/services")
        );
    }

    #[test]
    fn test_to_json_roundtrip() {
        let manifest = CapsuleManifest::from_toml(VALID_TOML).unwrap();
        let json = manifest.to_json().unwrap();
        let manifest2 = CapsuleManifest::from_json(&json).unwrap();

        assert_eq!(manifest.name, manifest2.name);
        assert_eq!(manifest.version, manifest2.version);
    }

    #[test]
    fn test_parse_build_and_isolation_sections() {
        let toml = format!(
            "{}\n\n[build]\nexclude_libs = [\"**/site-packages/torch/**\"]\ngpu = true\n\n[build.lifecycle]\nprepare = \"npm ci\"\nbuild = \"npm run build\"\npackage = \"ato pack\"\n\n[build.inputs]\nlockfiles = [\"package-lock.json\"]\ntoolchain = \"node:20\"\n\n[build.outputs]\ncapsule = \"dist/*.capsule\"\nsha256 = true\nblake3 = true\nattestation = true\nsignature = true\n\n[build.policy]\nrequire_attestation = true\nrequire_did_signature = true\n\n[isolation]\nallow_env = [\"LD_LIBRARY_PATH\", \"HF_TOKEN\"]\n",
            VALID_TOML
        );

        let manifest = CapsuleManifest::from_toml(&toml).unwrap();

        let build = manifest.build.as_ref().expect("build section should exist");
        assert!(build.gpu);
        assert_eq!(build.exclude_libs, vec!["**/site-packages/torch/**"]);
        assert_eq!(
            build.lifecycle.as_ref().and_then(|v| v.prepare.as_deref()),
            Some("npm ci")
        );
        assert_eq!(
            build.inputs.as_ref().and_then(|v| v.toolchain.as_deref()),
            Some("node:20")
        );
        assert_eq!(
            build.outputs.as_ref().and_then(|v| v.capsule.as_deref()),
            Some("dist/*.capsule")
        );
        assert_eq!(
            build.policy.as_ref().and_then(|v| v.require_attestation),
            Some(true)
        );

        let isolation = manifest
            .isolation
            .as_ref()
            .expect("isolation section should exist");
        assert_eq!(isolation.allow_env, vec!["LD_LIBRARY_PATH", "HF_TOKEN"]);
    }

    #[test]
    fn test_display_name() {
        let manifest = CapsuleManifest::from_toml(VALID_TOML).unwrap();
        assert_eq!(manifest.display_name(), "Qwen3 8B (MLX)");
    }

    #[test]
    fn test_can_fallback_to_cloud() {
        let manifest = CapsuleManifest::from_toml(VALID_TOML).unwrap();
        assert!(manifest.can_fallback_to_cloud());
    }

    #[test]
    fn test_vram_parsing() {
        let manifest = CapsuleManifest::from_toml(VALID_TOML).unwrap();
        let vram_min = manifest.requirements.vram_min_bytes().unwrap();
        assert_eq!(vram_min, Some(6 * 1024 * 1024 * 1024)); // 6GB
    }

    #[test]
    fn test_rejects_legacy_execution_section_toml() {
        let legacy_manifest = r#"
schema_version = "0.2"
name = "legacy-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets.cli]
runtime = "source"
entrypoint = "main.py"
"#;

        let error = CapsuleManifest::from_toml(legacy_manifest).unwrap_err();
        assert!(error
            .to_string()
            .contains("legacy [execution] section is not supported in schema_version=0.2"));
    }

    #[test]
    fn test_rejects_legacy_execution_section_json() {
        let legacy_manifest = r#"{
  "schema_version": "0.2",
  "name": "legacy-app",
  "version": "0.1.0",
  "type": "app",
  "default_target": "cli",
  "execution": {
    "runtime": "source",
    "entrypoint": "main.py"
  },
  "targets": {
    "cli": {
      "runtime": "source",
      "entrypoint": "main.py"
    }
  }
}"#;

        let error = CapsuleManifest::from_json(legacy_manifest).unwrap_err();
        assert!(error
            .to_string()
            .contains("legacy [execution] section is not supported in schema_version=0.2"));
    }

    #[test]
    fn test_validate_orchestration_services_target_mode() {
        let toml = r#"
schema_version = "0.2"
name = "multi-runtime-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "node"
entrypoint = "server.js"
port = 3000

[targets.db]
runtime = "oci"
image = "mysql:8"
port = 3306

[services.main]
target = "web"
depends_on = ["db"]

[services.db]
target = "db"
network = { allow_from = ["main"] }
"#;

        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_validate_orchestration_rejects_unknown_target() {
        let toml = r#"
schema_version = "0.2"
name = "multi-runtime-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "node"
entrypoint = "server.js"
port = 3000

[services.main]
target = "missing"
"#;

        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidService(name, msg)
                if name == "main" && msg.contains("target 'missing' does not exist")
        )));
    }

    #[test]
    fn test_validate_orchestration_rejects_target_and_entrypoint_mix() {
        let toml = r#"
schema_version = "0.2"
name = "multi-runtime-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "node"
entrypoint = "server.js"
port = 3000

[services.main]
target = "web"
entrypoint = "node server.js"
"#;

        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidService(name, msg)
                if name == "main" && msg.contains("mutually exclusive")
        )));
    }

    #[test]
    fn test_validate_oci_target_accepts_image_without_entrypoint() {
        let toml = r#"
schema_version = "0.2"
name = "oci-app"
version = "0.1.0"
type = "app"
default_target = "db"

[targets.db]
runtime = "oci"
image = "mysql:8"
"#;

        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_validate_orchestration_rejects_unknown_allow_from() {
        let toml = r#"
schema_version = "0.2"
name = "multi-runtime-app"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "node"
entrypoint = "server.js"
port = 3000

[targets.db]
runtime = "oci"
image = "mysql:8"
port = 3306

[services.main]
target = "web"
depends_on = ["db"]

[services.db]
target = "db"
network = { allow_from = ["api"] }
"#;

        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest.validate().unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::InvalidService(name, msg)
                if name == "db" && msg.contains("allow_from references unknown service")
        )));
    }

    #[test]
    fn test_is_kebab_case() {
        assert!(is_kebab_case("valid-name"));
        assert!(is_kebab_case("name123"));
        assert!(is_kebab_case("a1"));
        assert!(!is_kebab_case("Invalid"));
        assert!(!is_kebab_case("-invalid"));
        assert!(!is_kebab_case("invalid-"));
        assert!(!is_kebab_case(""));
    }

    #[test]
    fn test_is_semver() {
        assert!(is_semver("1.0.0"));
        assert!(is_semver("0.1.0"));
        assert!(is_semver("1.0.0-alpha"));
        assert!(!is_semver("1.0"));
        assert!(!is_semver("v1.0.0"));
    }
}

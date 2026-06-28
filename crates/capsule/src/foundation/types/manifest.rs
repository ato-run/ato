//! Capsule Manifest v0.3 Schema
//!
//! Implements the "Everything is a Capsule" paradigm for Gumball v0.3.0.
//! Supports both TOML (human-authored) and JSON (machine-generated) formats.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

use super::command_spec::CommandSpec;

// `ConfigField` / `ConfigKind` were extracted to `protocol` in N2 so
// `ato-desktop` can consume them without linking capsule's heavy
// runtime deps. They are re-exported here so existing
// `capsule::types::{ConfigField, ConfigKind}` import paths
// (`error.rs`, `manifest_tests.rs`, the CLI diagnostics tests) keep
// compiling unchanged.
pub use protocol::config::{ConfigField, ConfigKind};
use std::fs;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;
use toml::value::Table;
use url::form_urlencoded;
use walkdir::{DirEntry, WalkDir};

#[path = "dependency_grammar.rs"]
mod dependency_grammar;
#[path = "manifest_v03.rs"]
mod manifest_v03;
#[path = "manifest_validation.rs"]
mod manifest_validation;

use super::error::CapsuleError;
use super::utils::parse_memory_string;
use crate::orchestration::startup_order_from_dependencies;
use crate::schema_registry::SchemaRegistry;

pub use dependency_grammar::{
    CapsuleUrl, ContractRef, ContractSpec, ContractStateSpec, CredentialSchema, DependencySpec,
    DependencyStateOwnership, DependencyStateSpec, EndpointSpec, ParamSchema, ParamValue,
    ReadyProbe, RuntimeExportSpec, RuntimeExportValue, TemplateExpr, TemplateSegment,
    TemplatedString, ValueType,
};
use manifest_v03::*;
pub use manifest_validation::ValidationError;
pub(crate) use manifest_validation::is_valid_mount_path;
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

fn deserialize_build_config_option<'de, D>(deserializer: D) -> Result<Option<BuildConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<toml::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        toml::Value::String(command) => Ok(Some(BuildConfig {
            lifecycle: Some(BuildLifecycleConfig {
                build: Some(command),
                ..BuildLifecycleConfig::default()
            }),
            ..BuildConfig::default()
        })),
        other => other.try_into().map(Some).map_err(serde::de::Error::custom),
    }
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Name of the service whose container receives this mount. Defaults to
    /// the enclosing service when omitted.
    #[serde(default)]
    pub service_target: Option<String>,
    /// Optional ownership initialization for the bound state directory.
    ///
    /// When present, Ato `chown`s the host-side state source to this
    /// uid/(gid) before the container starts, so a non-root container `user`
    /// can write to a mounted volume. Declaring `owner` is the recipe author's
    /// explicit opt-in: without it, Ato never changes ownership of a bound
    /// path (see #428).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<StateOwner>,
    /// Optional permission bits applied to the host-side state source, as an
    /// octal string (e.g. `"0700"`, `"0755"`). Applied alongside `owner`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

/// Ownership initialization for a bound state directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateOwner {
    /// Numeric user id the container runs as (matches the OCI target `user`).
    pub uid: u32,
    /// Numeric group id; defaults to `uid` when omitted.
    #[serde(default)]
    pub gid: Option<u32>,
    /// Apply ownership recursively to existing directory contents.
    #[serde(default)]
    pub recursive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// Route this service's outbound HTTP(S) through the `ato-netd` egress proxy.
    ///
    /// Defaults to `true`. Set to `false` to opt out of proxy injection for this
    /// service (e.g. for database-only services that never make external requests).
    #[serde(default = "default_egress_proxy")]
    pub egress_proxy: bool,
}

fn default_egress_proxy() -> bool {
    true
}

impl Default for ServiceNetworkSpec {
    fn default() -> Self {
        Self {
            aliases: Vec::new(),
            publish: false,
            allow_from: Vec::new(),
            egress_proxy: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadinessProbe {
    #[serde(default)]
    pub http_get: Option<String>,

    #[serde(default)]
    pub tcp_connect: Option<String>,

    /// Command to run inside the container; success (exit 0) means ready.
    /// Example: `exec = ["pg_isready", "-U", "postgres"]`
    #[serde(default)]
    pub exec: Option<Vec<String>>,

    /// Placeholder name that resolves to a concrete port (e.g., "PORT").
    /// Required for `http_get` and `tcp_connect` probes; ignored for `exec` probes.
    /// Legacy exec recipes that include `port` are accepted but the value is not used.
    #[serde(default)]
    pub port: Option<String>,

    /// Seconds to wait before the first probe attempt (default: 0).
    #[serde(default)]
    pub initial_delay_seconds: u32,

    /// Total seconds before the probe is considered failed (default: 180).
    /// Must be > 0 and >= initial_delay_seconds.
    #[serde(default = "default_readiness_timeout_seconds")]
    pub timeout_seconds: u32,

    /// Seconds between consecutive probe attempts (default: 2).
    /// Must be > 0.
    #[serde(default = "default_readiness_interval_seconds")]
    pub interval_seconds: u32,
}

fn default_readiness_timeout_seconds() -> u32 {
    180
}

fn default_readiness_interval_seconds() -> u32 {
    2
}

/// Host integration capability names.
///
/// These are the only values accepted in `[[host_capabilities]]` blocks.
/// The host validates the `name` field against this enum at session start;
/// unrecognised names are rejected with a manifest-validation error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostCapabilityName {
    /// Open a file or project in the host user's editor (VSCode, Cursor, etc.).
    ///
    /// Schema/protocol shape only: the manifest and bridge types exist for
    /// forward compatibility, but no host implements a production execution
    /// path or consent UI for it yet. Until that lands, this capability is
    /// rejected by [`CapsuleManifest::validate`] so it cannot be declared as a
    /// silently-inert grant. See #468.
    OpenEditor,
    /// Open a file using the host OS default application.
    OpenFile,
    /// Reveal a path in the host OS file manager.
    RevealWorkspace,
}

impl HostCapabilityName {
    /// The kebab-case string used in `[[host_capabilities]] name = "..."`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenEditor => "open-editor",
            Self::OpenFile => "open-file",
            Self::RevealWorkspace => "reveal-workspace",
        }
    }

    /// Whether this Ato build implements a production host execution path
    /// (and the consent UI) for the capability.
    ///
    /// `open-editor` is schema-only — the manifest/protocol types exist but no
    /// host actually launches an editor and there is no consent integration, so
    /// it returns `false` and manifest validation rejects it rather than
    /// granting an inert capability. See #468. Remove this gate once the host
    /// execution path and consent UI exist.
    pub fn is_host_supported(&self) -> bool {
        match self {
            Self::OpenEditor => false,
            Self::OpenFile | Self::RevealWorkspace => true,
        }
    }
}

impl std::fmt::Display for HostCapabilityName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single host integration capability declared by a capsule.
///
/// ## Example `capsule.toml`
///
/// ```toml
/// [[host_capabilities]]
/// name = "open-editor"
/// reason = "Open the generated project in the user's editor after scaffolding."
/// ```
///
/// The `reason` is shown in the host consent UI.  Providing a clear, concise
/// reason is required; an empty reason is rejected during manifest validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostCapabilitySpec {
    /// The capability being requested.
    pub name: HostCapabilityName,
    /// Human-readable explanation shown to the user in the consent prompt.
    pub reason: String,
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
    #[serde(default, deserialize_with = "deserialize_build_config_option")]
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

    /// Platform-specific artifacts for `type = "tool"` capsules.
    ///
    /// Each `[platforms.<os>-<arch>]` entry declares the relocatable archive
    /// ato fetches and verifies when resolving this tool capsule on the
    /// matching host. Only meaningful when `capsule_type == Tool`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub platforms: BTreeMap<String, ToolPlatformArtifact>,

    /// Explicit exported surfaces such as one-shot CLI tools.
    #[serde(default)]
    pub exports: Option<CapsuleExports>,

    /// Supervisor Mode: Multi-service definition.
    ///
    /// Optional and dev-first: absence means single-process execution via `execution`.
    #[serde(default)]
    pub services: Option<HashMap<String, ServiceSpec>>,

    /// Capsule dependency contracts consumed by this capsule.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, DependencySpec>,

    /// Tool-capsule dependencies consumed by this capsule.
    ///
    /// Lifecycle is strictly resolve → materialize → project into sandbox →
    /// inject env. There is no start, stop, or readiness wait — the dependency
    /// artifact is an immutable executable tree, not a running service.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_dependencies: BTreeMap<String, ToolDependencySpec>,

    /// Manifest top-level required environment variable names. Per
    /// `CAPSULE_DEPENDENCY_CONTRACTS.md` §5.2, this is the resolution scope for
    /// `{{env.X}}` template expressions appearing inside `[dependencies.*]`
    /// blocks. Per-target `required_env` is for the target's own env and does
    /// not participate in dependency parameter / credential resolution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_env: Vec<String>,

    /// Contracts exported by this capsule for downstream consumers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contracts: BTreeMap<String, ContractSpec>,

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

    /// Host integration capabilities requested by this capsule.
    ///
    /// Capsule code may invoke host-side editor / file-system operations only when the
    /// corresponding capability is declared here.  The host presents a consent prompt that
    /// shows each capability's `reason` before issuing the grant.
    ///
    /// ## Example
    ///
    /// ```toml
    /// [[host_capabilities]]
    /// name = "open-editor"
    /// reason = "Open the generated project in your editor after scaffolding."
    /// ```
    ///
    /// Undeclared capabilities are rejected with `GuestBridgeResponse::Denied` at
    /// the IPC layer.  See [`crate::types::CapabilityGrant`] for the runtime grants
    /// that correspond to each capability name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_capabilities: Vec<HostCapabilitySpec>,

    /// Local ingress route configuration for multi-service OCI sessions.
    ///
    /// When present, Ato starts a session-scoped reverse proxy that routes
    /// requests by path prefix to upstream container services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress: Option<IngressConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IngressMode {
    Path,
    Host,
}

impl IngressMode {
    pub fn validate_v1(&self) -> Result<(), IngressError> {
        match self {
            IngressMode::Path => Ok(()),
            IngressMode::Host => Err(IngressError::UnsupportedInV1 {
                mode: "host".to_string(),
                message: "hostname-based ingress is deferred to v2".to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngressConfig {
    pub mode: IngressMode,
    pub routes: BTreeMap<String, IngressRoute>,
    #[serde(default)]
    pub env_inject: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngressRoute {
    pub target: String,
    pub port: u16,
    #[serde(default)]
    pub listed: bool,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default = "default_true")]
    pub strip_prefix: bool,
    #[serde(default)]
    pub upstream_path_prefix: Option<String>,
    #[serde(default)]
    pub root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IngressError {
    #[error("ingress mode '{mode}' is unsupported in v1: {message}")]
    UnsupportedInV1 { mode: String, message: String },
    #[error("duplicate ingress alias '{alias}' on routes '{route_a}' and '{route_b}'")]
    DuplicateAlias {
        alias: String,
        route_a: String,
        route_b: String,
    },
    #[error("invalid ingress alias '{alias}': {reason}")]
    InvalidAlias { alias: String, reason: String },
    #[error("root route '{route}' must not set alias")]
    RootWithAlias { route: String },
    #[error("multiple root routes: '{route_a}' and '{route_b}'")]
    MultipleRootRoutes { route_a: String, route_b: String },
    #[error("non-root route '{route}' must have an alias or use the route name as alias")]
    NonRootWithoutAlias { route: String },
    #[error("ingress route '{route}' references missing service '{target}'")]
    MissingService { route: String, target: String },
    #[error("ingress route '{route}' has invalid port {port}")]
    InvalidPort { route: String, port: u16 },
    #[error("ingress route '{route}' declares upstream_path_prefix but strip_prefix is false")]
    UpstreamPrefixWithoutStrip { route: String },
    #[error("ingress route '{route}' upstream_path_prefix '{prefix}' must start with '/'")]
    UpstreamPrefixMissingSlash { route: String, prefix: String },
    #[error("ingress route '{route}' upstream_path_prefix '{prefix}' is invalid: {reason}")]
    InvalidUpstreamPrefix {
        route: String,
        prefix: String,
        reason: String,
    },
    #[error("ingress env_inject target '{target}' does not reference a declared service")]
    EnvInjectTargetMissing { target: String },
    #[error("ingress env_inject template '{template}' references unknown route '{route_name}'")]
    EnvInjectMissingRoute {
        target: String,
        env_name: String,
        route_name: String,
        template: String,
    },
    #[error(
        "ingress env_inject template '{template}' has unsupported field '.{field}' (allowed: url, base_url, path, origin)"
    )]
    EnvInjectUnknownField {
        target: String,
        env_name: String,
        template: String,
        field: String,
    },
    #[error("ingress env_inject has invalid env var name '{name}'")]
    InvalidEnvVarName { name: String },
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub injection_bindings: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, ParamValue>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub credentials: BTreeMap<String, TemplatedString>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StateSharing {
    #[default]
    Exclusive,
    #[serde(alias = "same-capsule")]
    SameCapsule,
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
    #[serde(default)]
    pub sharing: StateSharing,
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
    /// `capsule::schema::capabilities::Capabilities`. Absence means
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

    /// Tool-capsule binary exports (alias → path relative to tool root).
    /// Populated only on `type = "tool"` capsules.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub binaries: BTreeMap<String, String>,

    /// Tool-capsule path exports (alias → path relative to tool root).
    /// Populated only on `type = "tool"` capsules; intended for non-binary
    /// surfaces such as `lib_dir`, `share_dir`, or the tool root itself.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub paths: BTreeMap<String, String>,
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

/// Platform-specific artifact entry for `type = "tool"` capsules.
///
/// Each `[platforms.<os>-<arch>]` table names a relocatable archive ato
/// fetches and verifies when materializing the tool capsule on a matching
/// host. The `<os>-<arch>` key follows the same `<os>-<arch>` form as
/// `requirements.platform` (e.g. `darwin-arm64`, `linux-x86_64`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolPlatformArtifact {
    /// Archive filename or absolute URL (e.g. `postgresql-16.4-darwin-arm64.tar.zst`).
    pub artifact: String,
    /// Hex-encoded SHA-256 of the archive bytes.
    pub sha256: String,
}

/// Consumer-side tool-dependency declaration.
///
/// Tool dependencies share resolution machinery with `[dependencies]`, but
/// their lifecycle is strictly resolve → materialize → project → inject env;
/// there is no start, stop, or readiness wait. The artifact is an immutable
/// executable tree, not a running service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDependencySpec {
    /// Reference to the tool capsule.
    #[serde(rename = "ref")]
    pub capsule_ref: CapsuleUrl,

    /// Optional version constraint that supplements the version pinned in
    /// `ref`. Manifests carry the constraint; the lockfile records the
    /// exact resolved version. Example: `">=16,<17"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Explicit export-name → env-var-name map. Takes precedence over the
    /// default convention `ATO_TOOL_<ALIAS>_<EXPORT>` so providers can avoid
    /// collisions across tool capsules sharing common export names.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bind_env: BTreeMap<String, String>,
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

    /// Provider-side host tool artifacts the orchestrator must
    /// resolve before this target spawns. Each entry is a stable
    /// tool ID (e.g. `"postgresql"`) understood by ato-cli's
    /// built-in registry. The orchestrator downloads, verifies, and
    /// installs each artifact into `<ato_home>/store/tools/...` and
    /// then injects `ATO_TOOL_*` env vars into this target's
    /// process.
    ///
    /// Example:
    /// tool_artifacts = ["postgresql"]
    ///
    /// Replaces capsule-side dependencies on host package managers
    /// (e.g. /opt/homebrew/bin/pg_isready). See #119 for the
    /// downloader contract and #120 for the migration policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_artifacts: Vec<String>,

    /// Entrypoint path for the target.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entrypoint: String,

    /// OCI image reference (preferred for runtime=oci).
    #[serde(default)]
    pub image: Option<String>,

    /// Optional command arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cmd: Vec<String>,

    /// Optional environment variables.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Optional container user for `runtime = "oci"` targets, passed through to
    /// the engine as `--user`. Format: `"uid"`, `"uid:gid"`, or a name the
    /// image resolves (e.g. `"1001:1001"`). See #428.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,

    /// native-inference: inference engine identifier (e.g. `"llama.cpp"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,

    /// native-inference: pinned engine version (e.g. llama.cpp build tag
    /// `"b4231"`). Used to fetch/locate a managed engine when `engine_path` is
    /// not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,

    /// native-inference: managed engine build variant (e.g. `"vulkan"` for a
    /// GPU-accelerated llama.cpp build). Unset = the default CPU/Metal build.
    /// `"cuda"` is not a fetchable Linux prebuilt and fails closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_variant: Option<String>,

    /// native-inference: local filesystem path to the engine server binary
    /// (e.g. `llama-server`). When set, it overrides managed engine fetching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_path: Option<String>,

    /// native-inference: local filesystem path to the model file (e.g. a GGUF).
    /// When set, it overrides managed model fetching (`model_url`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// native-inference: direct download URL for a managed model file. Resolved
    /// and verified against `model_sha256` and cached content-addressed. Used when
    /// `model` (a local path) is not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_url: Option<String>,

    /// native-inference: required SHA-256 (hex, optional `sha256:` prefix) of the
    /// managed model. Both the cache key and the post-download integrity check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_sha256: Option<String>,

    /// native-inference: optional display/cache filename for a managed model
    /// (e.g. `"model.gguf"`). Informational; the cache is keyed by sha256.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_filename: Option<String>,

    /// native-inference: model format hint (e.g. `"gguf"`). Informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_format: Option<String>,

    /// native-inference (multi-file engines, e.g. SGLang): a Hugging Face model
    /// repo id (`"<org>/<name>"`) downloaded as a directory of shards. Mutually
    /// exclusive with `model_url` (single-file). Used when `model` (a local dir)
    /// is not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_repo: Option<String>,

    /// native-inference: the immutable 40-hex Hugging Face commit the repo is
    /// pinned to (reproducibility — a branch like `"main"` is rejected). Required
    /// alongside `model_repo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_revision: Option<String>,

    /// native-inference: the digest-of-digests over the included file set of the
    /// pinned repo (the multi-file analogue of `model_sha256`; both the cache key
    /// and the integrity gate). Required alongside `model_repo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_repo_sha256: Option<String>,

    /// native-inference: optional glob allowlist of repo files to download (e.g.
    /// `["config.json", "*.safetensors", "tokenizer*"]`). Empty = a built-in
    /// default weights+config+tokenizer set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_repo_include: Vec<String>,

    /// native-inference: when `true`, the repo is gated and the download sends an
    /// `HF_TOKEN` bearer credential (read from the environment at fetch time,
    /// never logged or persisted).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub model_repo_gated: bool,

    /// native-inference: extra args appended to the engine server argv, AFTER the
    /// engine's base flags and independent of the launcher-injected `--port`
    /// (e.g. SGLang `["--mem-fraction-static", "0.9", "--context-length",
    /// "8192"]`, llama.cpp `["--ctx-size", "8192", "--n-gpu-layers", "999"]`).
    /// Engine-generic: passed through verbatim to whichever native-inference
    /// engine runs. Launcher/engine-controlled flags (`--port`/`-p`, `--host`,
    /// `--model-path`/`-m`/`--model`) are rejected at manifest validation so a
    /// capsule can't break readiness/app_url or the model wiring.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub server_args: Vec<String>,

    /// Required environment variable names.
    #[serde(default)]
    pub required_env: Vec<String>,

    /// Service dependencies that must be ready before this target is started.
    /// Each entry must be a sibling target label. Accepted as either `needs`
    /// (legacy) or `depends_on` in TOML.
    #[serde(default, skip_serializing_if = "Vec::is_empty", alias = "depends_on")]
    pub needs: Vec<String>,

    /// Optional rich schema for user-facing config inputs. When populated,
    /// consumers (desktop dynamic form, `ato run` preflight error details)
    /// prefer this over the flat `required_env` list. See
    /// `NamedTarget::resolved_config_schema` for the resolution rule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_schema: Vec<ConfigField>,

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
    #[serde(default, alias = "build")]
    pub build_command: Option<String>,

    /// Install command — runs once before building (e.g., package manager install).
    /// Supports string (legacy) and structured `CommandSpec` forms.
    #[serde(default, alias = "install")]
    pub install_command: Option<CommandSpec>,

    /// Pre-start command — runs after build and provider readiness, before main run
    /// (e.g., database migrations). Supports string (legacy) and structured forms.
    #[serde(default, alias = "prestart")]
    pub prestart_command: Option<CommandSpec>,

    /// CHML build cache output globs preserved on the normalized target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,

    /// CHML build cache environment keys preserved on the normalized target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build_env: Vec<String>,

    /// Preserved shell-native run command for schema v0.3.
    #[serde(default, alias = "run")]
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

    /// Phase 8 follow-up: identity-relevant env-key allowlist. When
    /// populated, the v2 environment observer treats this list as the
    /// SOLE identity-relevant set, replacing the intrinsic
    /// PATH/LANG/LC_*/TZ default. Keys outside the list are ignored
    /// for identity (not even surfaced as `ambient_untracked_keys`),
    /// so a Pure-eligible capsule can drop noisy host PATH and reach
    /// `EnvironmentMode::Closed`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_allowlist: Vec<String>,

    /// Opt-in to OCI platform emulation for this target.
    ///
    /// When `true`, the runtime is allowed to pull and run images whose platform
    /// does not match the host (e.g., linux/amd64 on an arm64 macOS host).
    /// Emulation may be slower. Default is `false` (native-only).
    ///
    /// Example: set to `true` for images that are only published as linux/amd64.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_emulation: bool,

    /// One-shot lifecycle: run this OCI container to completion before
    /// starting dependent services.  Exit code 0 = success (dependents
    /// may start).  Non-zero or timeout = typed error, dependents blocked.
    ///
    /// Only supported for OCI targets.  `readiness_probe` and `port` must
    /// not be set together with `run_once`.  `cmd` is required.
    ///
    /// Typical uses: DB migrations, permission init, bucket creation,
    /// bootstrap seed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub run_once: bool,
}

impl NamedTarget {
    /// Returns the authoritative config schema for this target.
    ///
    /// - If `config_schema` is populated (rich form), it wins verbatim.
    /// - Otherwise, derives a default `ConfigKind::Secret` entry per name in
    ///   `required_env` so legacy capsules still drive the desktop dynamic
    ///   form (as masked secret inputs).
    pub fn resolved_config_schema(&self) -> Vec<ConfigField> {
        if !self.config_schema.is_empty() {
            return self.config_schema.clone();
        }
        self.required_env
            .iter()
            .map(|name| ConfigField {
                name: name.clone(),
                label: None,
                description: None,
                kind: ConfigKind::Secret,
                default: None,
                placeholder: None,
            })
            .collect()
    }
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

    /// Optional container user passed to the engine as `--user`
    /// (`"uid"`, `"uid:gid"`, or a name resolvable in the image). See #428.
    #[serde(default)]
    pub user: Option<String>,
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

    /// Convert declared `[[host_capabilities]]` entries into the corresponding
    /// [`crate::types::bridge::CapabilityGrant`] values.
    ///
    /// The returned set is what the host MUST grant at session creation time
    /// (after user consent) for the capsule to operate correctly.
    ///
    /// This is a pure protocol-shape mapping and does not itself gate on host
    /// support: capabilities with no production execution path (currently
    /// `open-editor`, see [`HostCapabilityName::is_host_supported`] and #468)
    /// are rejected earlier by [`CapsuleManifest::validate`], so in production a
    /// manifest carrying such a capability never reaches this point.
    pub fn host_capability_grants(&self) -> Vec<crate::foundation::types::bridge::CapabilityGrant> {
        self.host_capabilities
            .iter()
            .map(|spec| match spec.name {
                HostCapabilityName::OpenEditor => {
                    crate::foundation::types::bridge::CapabilityGrant::OpenEditor
                }
                HostCapabilityName::OpenFile => {
                    crate::foundation::types::bridge::CapabilityGrant::OpenFile
                }
                HostCapabilityName::RevealWorkspace => {
                    crate::foundation::types::bridge::CapabilityGrant::RevealWorkspace
                }
            })
            .collect()
    }

    /// Validate `[[host_capabilities]]` entries.
    ///
    /// Returns an error string when any entry has an empty `reason` field, since
    /// the host consent UI cannot present a meaningful prompt without it.
    pub fn validate_host_capabilities(&self) -> Result<(), String> {
        for spec in &self.host_capabilities {
            if spec.reason.trim().is_empty() {
                return Err(format!(
                    "host_capability '{}' must have a non-empty `reason` field",
                    spec.name
                ));
            }
        }
        Ok(())
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

/// A native-inference `engine_version` is interpolated into download URLs,
/// archive filenames, and toolchain cache paths, so it must be tightly
/// constrained. Allow build tags (`b9754`) and semver-ish ids; reject anything
/// that could traverse paths or alter URLs (`/`, `\`, `..`, `%`, whitespace, …).
pub fn is_safe_engine_version(version: &str) -> bool {
    let v = version.trim();
    !v.is_empty()
        && !v.contains("..")
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Normalize a managed-model `model_sha256` to its canonical 64-char lowercase
/// hex form (accepting an optional `sha256:`/`sha256-` prefix). Returns `None`
/// for anything that is not a valid SHA-256 — it is the content-addressed cache
/// key, so it must be exact.
pub fn normalize_model_sha256(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    let hex = lower
        .strip_prefix("sha256:")
        .or_else(|| lower.strip_prefix("sha256-"))
        .unwrap_or(&lower);
    if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hex.to_string())
    } else {
        None
    }
}

/// Resolve a capsule `model_url` to a plain download URL.
///
/// `hf://<org>/<repo>/<path-to-file>` (a **public** Hugging Face model file) expands
/// to the canonical resolve URL on the `main` revision — a convenience layer over
/// the existing `model_url` + `model_sha256` path: `model_sha256` (required
/// separately) still pins the exact bytes, and the rest of the download/verify/CAS
/// pipeline is unchanged. `http(s)://` URLs pass through verbatim. No auth, no
/// gated/private repos, no revision/quantization auto-selection (use a full
/// `https://…/resolve/<rev>/…` URL for those).
pub fn resolve_model_url(value: &str) -> Result<String, String> {
    let v = value.trim();
    if v.contains(char::is_whitespace) {
        return Err(format!("model_url must not contain whitespace: {value:?}"));
    }
    if let Some(rest) = v.strip_prefix("hf://") {
        // rest = <org>/<repo>/<path/to/file.gguf>
        let parts: Vec<&str> = rest.splitn(3, '/').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
            return Err(format!(
                "invalid hf:// model ref {value:?}; expected hf://<org>/<repo>/<path-to-file>"
            ));
        }
        let (org, repo, path) = (parts[0], parts[1], parts[2]);
        return Ok(format!(
            "https://huggingface.co/{org}/{repo}/resolve/main/{path}"
        ));
    }
    if v.starts_with("https://") || v.starts_with("http://") {
        return Ok(v.to_string());
    }
    Err(format!(
        "unsupported model_url {value:?}; use https:// or hf://<org>/<repo>/<path-to-file>"
    ))
}

/// A managed `model_url` must be a plain `http(s)://` URL or a public
/// `hf://<org>/<repo>/<path>` ref (resolved by [`resolve_model_url`]).
pub fn is_safe_model_url(value: &str) -> bool {
    resolve_model_url(value).is_ok()
}

/// A managed multi-file `model_repo` must be a `<org>/<name>` Hugging Face repo
/// id: exactly one `/`, each segment starting with an alphanumeric and otherwise
/// `[A-Za-z0-9._-]`. The id is interpolated into the HF API/download URLs and the
/// content-addressed cache path, so reject path traversal (`..`), whitespace, and
/// any extra slashes.
pub fn is_safe_hf_repo(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() || v.contains("..") {
        return false;
    }
    let Some((org, name)) = v.split_once('/') else {
        return false;
    };
    if name.contains('/') {
        return false;
    }
    let segment_ok = |s: &str| {
        let mut chars = s.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphanumeric() => {}
            _ => return false,
        }
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    segment_ok(org) && segment_ok(name)
}

/// A managed `model_revision` must be an immutable 40-char lowercase-hex Git
/// commit SHA. A branch/tag (e.g. `"main"`) is deliberately rejected so a
/// `model_repo` pin is reproducible (the bytes can't move under the digest).
pub fn is_safe_hf_revision(value: &str) -> bool {
    let v = value.trim();
    v.len() == 40 && v.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Flags a native-inference `server_args` token that names a launcher- or
/// engine-controlled flag, which a capsule must NOT override:
///  * `--port` / `-p` — the host launcher injects the resolved/allocated port so
///    readiness and the app_url agree; a capsule-set port breaks both.
///  * `--host` — forced to `127.0.0.1` by the launcher (the engine only listens
///    on loopback; the proxy/readiness assume it).
///  * `--model-path` / `-m` / `--model` — the engine sets the model from the
///    resolved `model` / `model_url` / `model_repo`; a second value would fight it.
///
/// Matches both the `--flag value` form (the token is exactly the flag) and the
/// `--flag=value` form (the token starts with `<flag>=`). Returns the offending
/// canonical flag name when `arg` is forbidden, else `None`.
pub fn forbidden_native_inference_server_arg(arg: &str) -> Option<&'static str> {
    // Canonical launcher/engine-controlled flags. Short forms (`-p`, `-m`) are
    // single-dash and must match exactly or as `-x=...`.
    const FORBIDDEN: &[&str] = &["--port", "-p", "--host", "--model-path", "-m", "--model"];
    let token = arg.trim();
    // Compare the flag portion only (everything before a `=`), so `--port=9000`
    // and `--port 9000` are both caught.
    let flag = token.split('=').next().unwrap_or(token);
    FORBIDDEN.iter().copied().find(|&f| f == flag)
}

#[cfg(test)]
mod server_args_guard_tests {
    use super::forbidden_native_inference_server_arg;

    #[test]
    fn rejects_each_forbidden_flag_space_form() {
        // `--flag value` form: the token is exactly the flag.
        assert_eq!(forbidden_native_inference_server_arg("--port"), Some("--port"));
        assert_eq!(forbidden_native_inference_server_arg("-p"), Some("-p"));
        assert_eq!(forbidden_native_inference_server_arg("--host"), Some("--host"));
        assert_eq!(
            forbidden_native_inference_server_arg("--model-path"),
            Some("--model-path")
        );
        assert_eq!(forbidden_native_inference_server_arg("-m"), Some("-m"));
        assert_eq!(forbidden_native_inference_server_arg("--model"), Some("--model"));
    }

    #[test]
    fn rejects_each_forbidden_flag_equals_form() {
        // `--flag=value` form: the token starts with `<flag>=`.
        assert_eq!(
            forbidden_native_inference_server_arg("--port=9000"),
            Some("--port")
        );
        assert_eq!(forbidden_native_inference_server_arg("-p=9000"), Some("-p"));
        assert_eq!(
            forbidden_native_inference_server_arg("--host=0.0.0.0"),
            Some("--host")
        );
        assert_eq!(
            forbidden_native_inference_server_arg("--model-path=/x"),
            Some("--model-path")
        );
        assert_eq!(forbidden_native_inference_server_arg("-m=/x"), Some("-m"));
        assert_eq!(
            forbidden_native_inference_server_arg("--model=/x"),
            Some("--model")
        );
    }

    #[test]
    fn allows_tunable_engine_flags() {
        for ok in [
            "--mem-fraction-static",
            "0.9",
            "--context-length",
            "8192",
            "--quantization",
            "moe_wna16",
            "--reasoning-parser",
            "qwen3",
            "--tp-size",
            "2",
            "--ctx-size",
            "--n-gpu-layers",
            "999",
            // A value that merely contains a forbidden substring is fine — only
            // the flag portion is compared, and only exact flag names are caught.
            "--model-loader-extra-config",
            "--portfolio",
        ] {
            assert_eq!(
                forbidden_native_inference_server_arg(ok),
                None,
                "{ok:?} should be allowed"
            );
        }
    }
}

#[cfg(test)]
mod engine_version_tests {
    use super::is_safe_engine_version;

    #[test]
    fn accepts_build_tags_and_semverish() {
        assert!(is_safe_engine_version("b9754"));
        assert!(is_safe_engine_version("b4231"));
        assert!(is_safe_engine_version("1.2.3"));
        assert!(is_safe_engine_version("v0.1.0-rc1"));
    }

    #[test]
    fn rejects_path_traversal_and_url_unsafe() {
        assert!(!is_safe_engine_version(""));
        assert!(!is_safe_engine_version("../evil"));
        assert!(!is_safe_engine_version("b9754/../../x"));
        assert!(!is_safe_engine_version("..")); // pure traversal
        assert!(!is_safe_engine_version("a/b"));
        assert!(!is_safe_engine_version("a\\b"));
        assert!(!is_safe_engine_version("b97 54")); // whitespace
        assert!(!is_safe_engine_version("b9754%2f")); // url-encoded slash
    }
}

#[cfg(test)]
mod model_ref_tests {
    use super::{is_safe_model_url, normalize_model_sha256, resolve_model_url};

    #[test]
    fn normalizes_valid_sha256_and_strips_prefix() {
        let hex = "a".repeat(64);
        assert_eq!(normalize_model_sha256(&hex), Some(hex.clone()));
        assert_eq!(
            normalize_model_sha256(&format!("sha256:{hex}")),
            Some(hex.clone())
        );
        assert_eq!(
            normalize_model_sha256(&format!("  SHA256-{}  ", "A".repeat(64))),
            Some(hex)
        );
    }

    #[test]
    fn rejects_invalid_sha256() {
        assert_eq!(normalize_model_sha256(""), None);
        assert_eq!(normalize_model_sha256("abc"), None); // too short
        assert_eq!(normalize_model_sha256(&"z".repeat(64)), None); // non-hex
        assert_eq!(normalize_model_sha256(&"a".repeat(63)), None); // off-by-one
        assert_eq!(normalize_model_sha256(&"a".repeat(65)), None);
    }

    #[test]
    fn model_url_accepts_http_s_and_wellformed_hf() {
        assert!(is_safe_model_url("https://example.com/m.gguf"));
        assert!(is_safe_model_url("http://example.com/m.gguf"));
        // Well-formed public hf:// ref (org/repo/path) is accepted (#7).
        assert!(is_safe_model_url("hf://org/repo/model.gguf"));
        assert!(is_safe_model_url("hf://org/repo/sub/dir/model.gguf"));
        // Malformed / unsupported.
        assert!(!is_safe_model_url("hf://repo/model")); // missing <path> segment
        assert!(!is_safe_model_url("hf://org/repo/")); // empty path
        assert!(!is_safe_model_url("file:///etc/passwd"));
        assert!(!is_safe_model_url("ftp://x/y"));
        assert!(!is_safe_model_url("https://e.com/a b")); // whitespace
        assert!(!is_safe_model_url(""));
    }

    #[test]
    fn resolve_model_url_expands_hf_and_passes_http_through() {
        // hf:// → canonical Hugging Face resolve URL on `main`; sub-paths preserved.
        assert_eq!(
            resolve_model_url(
                "hf://Qwen/Qwen2.5-1.5B-Instruct-GGUF/qwen2.5-1.5b-instruct-q4_k_m.gguf"
            )
            .unwrap(),
            "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf"
        );
        assert_eq!(
            resolve_model_url("hf://org/repo/a/b/c.gguf").unwrap(),
            "https://huggingface.co/org/repo/resolve/main/a/b/c.gguf"
        );
        // http(s) passes through verbatim.
        assert_eq!(
            resolve_model_url("https://example.com/m.gguf").unwrap(),
            "https://example.com/m.gguf"
        );
        // Malformed hf:// and unsupported schemes error.
        assert!(resolve_model_url("hf://org/repo").is_err());
        assert!(resolve_model_url("hf://org//file.gguf").is_err());
        assert!(resolve_model_url("s3://bucket/k").is_err());
    }

    #[test]
    fn hf_repo_accepts_org_slash_name_and_rejects_traversal() {
        use super::is_safe_hf_repo;
        assert!(is_safe_hf_repo("Qwen/Qwen3-32B-AWQ"));
        assert!(is_safe_hf_repo("meta-llama/Llama-3.1-8B-Instruct"));
        assert!(is_safe_hf_repo("org/repo.name_v2"));
        // Exactly one `/`, both segments alphanumeric-led.
        assert!(!is_safe_hf_repo("noslash"));
        assert!(!is_safe_hf_repo("a/b/c")); // extra slash
        assert!(!is_safe_hf_repo("org/")); // empty name
        assert!(!is_safe_hf_repo("/repo")); // empty org
        assert!(!is_safe_hf_repo("../evil/repo")); // traversal
        assert!(!is_safe_hf_repo("org/..")); // traversal
        assert!(!is_safe_hf_repo(".hidden/repo")); // leading dot
        assert!(!is_safe_hf_repo("org/repo name")); // whitespace
        assert!(!is_safe_hf_repo(""));
    }

    #[test]
    fn hf_revision_requires_immutable_40_hex_commit() {
        use super::is_safe_hf_revision;
        assert!(is_safe_hf_revision(&"a".repeat(40)));
        assert!(is_safe_hf_revision("0123456789abcdef0123456789abcdef01234567"));
        // Branches/tags and malformed lengths/casing are rejected.
        assert!(!is_safe_hf_revision("main"));
        assert!(!is_safe_hf_revision(&"a".repeat(39)));
        assert!(!is_safe_hf_revision(&"a".repeat(41)));
        assert!(!is_safe_hf_revision(&"A".repeat(40))); // uppercase
        assert!(!is_safe_hf_revision(&"g".repeat(40))); // non-hex
        assert!(!is_safe_hf_revision(""));
    }
}

#[cfg(test)]
mod host_capability_tests {
    use super::*;

    #[test]
    fn host_capabilities_parse_from_toml() {
        let toml = r#"
schema_version = "0.3"
name = "my-scaffold"
type = "app"
default_target = "app"

[[host_capabilities]]
name = "open-editor"
reason = "Open the generated project in your editor after scaffolding."

[[host_capabilities]]
name = "reveal-workspace"
reason = "Show the output directory in the file manager."

[targets.app]
runtime = "source/node"
run = "node index.js"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        assert_eq!(manifest.host_capabilities.len(), 2);
        assert_eq!(
            manifest.host_capabilities[0].name,
            HostCapabilityName::OpenEditor
        );
        assert_eq!(
            manifest.host_capabilities[1].name,
            HostCapabilityName::RevealWorkspace
        );
    }

    #[test]
    fn host_capability_grants_is_pure_protocol_shape_mapping() {
        // `host_capability_grants()` is a pure type-level mapping and does NOT
        // gate on host support — it still maps `open-editor` to its grant so the
        // protocol shape is preserved for future compatibility. This does NOT
        // imply production support: a manifest declaring `open-editor` is
        // rejected by `validate()` (see `open_editor_is_rejected_*` below and
        // #468) before grants are ever computed in production. We call the
        // mapping directly here, bypassing validation, only to assert the
        // protocol shape is retained.
        let toml = r#"
schema_version = "0.3"
name = "editor-capsule"
type = "app"
default_target = "app"

[[host_capabilities]]
name = "open-editor"
reason = "Open generated files."

[targets.app]
runtime = "source/node"
run = "node index.js"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let grants = manifest.host_capability_grants();
        assert_eq!(grants.len(), 1);
        assert!(matches!(
            grants[0],
            crate::foundation::types::bridge::CapabilityGrant::OpenEditor
        ));
        // The same manifest must NOT pass validation, even though the protocol
        // mapping above succeeds.
        assert!(
            manifest.validate().is_err(),
            "open-editor must be rejected by validation despite the protocol mapping"
        );
    }

    #[test]
    fn open_editor_is_rejected_as_unsupported_host_capability() {
        let toml = r#"
schema_version = "0.3"
name = "editor-capsule"
type = "app"
default_target = "app"

[[host_capabilities]]
name = "open-editor"
reason = "Open generated files."

[targets.app]
runtime = "source/node"
run = "node index.js"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest
            .validate()
            .expect_err("open-editor must fail validation as unsupported");
        assert!(
            errors
                .iter()
                .any(|err| matches!(err, ValidationError::UnsupportedHostCapability { .. })),
            "expected an UnsupportedHostCapability error, got: {errors:?}"
        );
        let details = errors
            .iter()
            .map(|err| err.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            details.contains("open-editor"),
            "error must name the capability, got: {details}"
        );
        assert!(
            details.contains("https://github.com/ato-run/ato/issues/468"),
            "error must be actionable, got: {details}"
        );
    }

    #[test]
    fn open_editor_is_rejected_by_load_manifest() {
        let toml = r#"
schema_version = "0.3"
name = "editor-capsule"
type = "app"
default_target = "app"

[[host_capabilities]]
name = "open-editor"
reason = "Open generated files."

[targets.app]
runtime = "source/node"
run = "node index.js"
"#;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("capsule.toml");
        std::fs::write(&path, toml).expect("write manifest");

        let err = crate::contract::manifest::load_manifest(&path)
            .expect_err("load_manifest must reject unsupported open-editor capability");
        assert!(
            err.to_string().contains("open-editor"),
            "error must name the capability, got: {err}"
        );
    }

    #[test]
    fn supported_host_capabilities_still_validate() {
        let toml = r#"
schema_version = "0.3"
name = "supported-capsule"
type = "app"
default_target = "app"

[[host_capabilities]]
name = "open-file"
reason = "Open a generated file with the default application."

[[host_capabilities]]
name = "reveal-workspace"
reason = "Show the output directory in the file manager."

[targets.app]
runtime = "source/node"
run = "node index.js"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        assert!(
            manifest.validate().is_ok(),
            "open-file and reveal-workspace must remain supported: {:?}",
            manifest.validate().err()
        );
    }

    #[test]
    fn host_capability_empty_reason_fails_validation() {
        let toml = r#"
schema_version = "0.3"
name = "bad-capsule"
type = "app"
default_target = "app"

[[host_capabilities]]
name = "open-file"
reason = ""

[targets.app]
runtime = "source/node"
run = "node index.js"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        let errors = manifest
            .validate()
            .expect_err("empty reason must fail validation");
        let details = errors
            .iter()
            .map(|err| err.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            details.contains("host_capability 'open-file' must have a non-empty `reason` field")
        );
    }

    #[test]
    fn host_capability_empty_reason_is_rejected_by_load_manifest() {
        let toml = r#"
schema_version = "0.3"
name = "bad-capsule"
type = "app"
default_target = "app"

[[host_capabilities]]
name = "open-file"
reason = ""

[targets.app]
runtime = "source/node"
run = "node index.js"
"#;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("capsule.toml");
        std::fs::write(&path, toml).expect("write manifest");

        let err = crate::contract::manifest::load_manifest(&path)
            .expect_err("load_manifest must reject empty host capability reason");
        assert!(
            err.to_string()
                .contains("host_capability 'open-file' must have a non-empty `reason` field")
        );
    }

    #[test]
    fn no_host_capabilities_is_valid() {
        let toml = r#"
schema_version = "0.3"
name = "plain-capsule"
type = "app"
default_target = "app"

[targets.app]
runtime = "source/node"
run = "node index.js"
"#;
        let manifest = CapsuleManifest::from_toml(toml).unwrap();
        assert!(manifest.host_capabilities.is_empty());
        assert!(manifest.validate().is_ok());
        assert!(manifest.host_capability_grants().is_empty());
    }
}

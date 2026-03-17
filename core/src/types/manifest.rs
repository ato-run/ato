//! Capsule Manifest v0.2 Schema
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
    matches!(value.trim(), "0.2" | "0.3" | "1")
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

fn is_v03_like_schema(raw: &toml::Value) -> bool {
    is_v03_schema(raw) || is_chml_manifest(raw)
}

fn normalize_v03_capsule_type(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[derive(Debug, Clone, Default, Deserialize)]
struct V03PackageSurface {
    #[serde(rename = "type", default)]
    package_type: Option<String>,
    #[serde(default)]
    runtime: Option<String>,
    #[serde(default)]
    build: Option<String>,
    #[serde(default)]
    outputs: Vec<String>,
    #[serde(default)]
    build_env: Vec<String>,
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    required_env: Vec<String>,
    #[serde(default)]
    runtime_version: Option<String>,
    #[serde(default)]
    runtime_tools: HashMap<String, String>,
    #[serde(default)]
    readiness_probe: Option<toml::Value>,
    #[serde(default)]
    driver: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    public: Vec<String>,
}

fn parse_v03_package_surface(
    package_name: &str,
    table: &Table,
) -> Result<V03PackageSurface, CapsuleError> {
    toml::Value::Table(table.clone())
        .try_into()
        .map_err(|error| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 package '{}' could not be parsed: {}",
                package_name, error
            ))
        })
}

fn reject_v03_legacy_fields(table: &Table, context: &str) -> Result<(), CapsuleError> {
    for field in ["entrypoint", "cmd"] {
        if table.contains_key(field) {
            return Err(CapsuleError::ParseError(format!(
                "schema_version=0.3 {context} must not use legacy field '{}'; use 'run' instead",
                field
            )));
        }
    }

    if let Some(targets) = table.get("targets").and_then(toml::Value::as_table) {
        for (target_name, target_value) in targets {
            let Some(target_table) = target_value.as_table() else {
                continue;
            };
            for field in ["entrypoint", "cmd"] {
                if target_table.contains_key(field) {
                    return Err(CapsuleError::ParseError(format!(
                        "schema_version=0.3 target '{}' must not use legacy field '{}'; use 'run' instead",
                        target_name, field
                    )));
                }
            }
        }
    }

    Ok(())
}

fn shallow_merge_v03_tables(defaults: &Table, package: &Table) -> Table {
    let mut merged = defaults.clone();
    for (key, value) in package {
        match (merged.get_mut(key), value) {
            (Some(toml::Value::Table(base)), toml::Value::Table(overlay)) => {
                for (child_key, child_value) in overlay {
                    base.insert(child_key.clone(), child_value.clone());
                }
            }
            _ => {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    merged
}

fn normalize_v03_runtime_selector(value: &str) -> (String, Option<String>) {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "web/static" => ("web".to_string(), Some("static".to_string())),
        "web/node" | "source/node" => ("source".to_string(), Some("node".to_string())),
        "web/deno" | "source/deno" => ("source".to_string(), Some("deno".to_string())),
        "web/python" | "source/python" => ("source".to_string(), Some("python".to_string())),
        "source/native" => ("source".to_string(), Some("native".to_string())),
        "source/go" => ("source".to_string(), Some("native".to_string())),
        "source" | "web" | "oci" | "wasm" => (normalized, None),
        other => {
            if let Some((runtime, driver)) = other.split_once('/') {
                let runtime = runtime.trim();
                let driver = driver.trim();
                let runtime = if runtime == "web" && driver != "static" {
                    "source"
                } else {
                    runtime
                };
                let driver = (!driver.is_empty()).then(|| driver.to_string());
                (runtime.to_string(), driver)
            } else {
                (other.to_string(), None)
            }
        }
    }
}

fn infer_v03_language_from_driver(driver: Option<&str>) -> Option<String> {
    match driver.map(|value| value.trim().to_ascii_lowercase()) {
        Some(driver) if matches!(driver.as_str(), "node" | "python" | "deno" | "bun") => {
            Some(driver)
        }
        _ => None,
    }
}

fn apply_v03_readiness_probe(target_table: &mut Table, readiness_probe: toml::Value) {
    match readiness_probe {
        toml::Value::String(value) => {
            let mut probe = Table::new();
            probe.insert("http_get".to_string(), toml::Value::String(value.clone()));
            probe.insert("port".to_string(), toml::Value::String("PORT".to_string()));
            target_table.insert("readiness_probe".to_string(), toml::Value::Table(probe));
            target_table.insert("health_check".to_string(), toml::Value::String(value));
        }
        toml::Value::Table(probe_table) => {
            let mut normalized_probe = Table::new();
            if let Some(http_get) = probe_table
                .get("http_get")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                normalized_probe.insert(
                    "http_get".to_string(),
                    toml::Value::String(http_get.to_string()),
                );
                target_table.insert(
                    "health_check".to_string(),
                    toml::Value::String(http_get.to_string()),
                );
            } else if probe_table
                .get("type")
                .and_then(toml::Value::as_str)
                .map(|value| value.eq_ignore_ascii_case("http"))
                .unwrap_or(false)
            {
                if let Some(target) = probe_table
                    .get("target")
                    .and_then(toml::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    normalized_probe.insert(
                        "http_get".to_string(),
                        toml::Value::String(target.to_string()),
                    );
                    target_table.insert(
                        "health_check".to_string(),
                        toml::Value::String(target.to_string()),
                    );
                }
            }
            if let Some(tcp_connect) = probe_table
                .get("tcp_connect")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                normalized_probe.insert(
                    "tcp_connect".to_string(),
                    toml::Value::String(tcp_connect.to_string()),
                );
            }
            if let Some(port) = probe_table
                .get("port")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                normalized_probe.insert("port".to_string(), toml::Value::String(port.to_string()));
            } else {
                normalized_probe
                    .insert("port".to_string(), toml::Value::String("PORT".to_string()));
            }
            if !normalized_probe.is_empty() {
                target_table.insert(
                    "readiness_probe".to_string(),
                    toml::Value::Table(normalized_probe),
                );
            }
        }
        _ => {}
    }
}

fn normalize_v03_target_table(package_name: &str, table: &Table) -> Result<Table, CapsuleError> {
    reject_v03_legacy_fields(table, &format!("package '{}'", package_name))?;
    let V03PackageSurface {
        package_type,
        runtime,
        build,
        outputs,
        build_env,
        run,
        port,
        required_env,
        runtime_version,
        runtime_tools,
        readiness_probe,
        driver,
        language,
        image,
        env,
        public,
    } = parse_v03_package_surface(package_name, table)?;
    let mut target_table = Table::new();

    if let Some(package_type) = package_type.as_deref() {
        target_table.insert(
            "package_type".to_string(),
            toml::Value::String(normalize_v03_capsule_type(package_type)),
        );
    }

    let mut normalized_driver = driver
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    if let Some(runtime_selector) = runtime
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let (runtime, driver) = normalize_v03_runtime_selector(runtime_selector);
        target_table.insert("runtime".to_string(), toml::Value::String(runtime));
        if let Some(driver) = driver {
            normalized_driver = Some(driver.clone());
            target_table.insert("driver".to_string(), toml::Value::String(driver));
        }
    } else if let Some(driver) = normalized_driver.as_ref() {
        target_table.insert("driver".to_string(), toml::Value::String(driver.clone()));
    }

    if let Some(language) = language
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        target_table.insert(
            "language".to_string(),
            toml::Value::String(language.to_string()),
        );
    }

    if let Some(run_command) = run
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        target_table.insert(
            "run_command".to_string(),
            toml::Value::String(run_command.to_string()),
        );
        if let Some(language) = infer_v03_language_from_driver(normalized_driver.as_deref()) {
            target_table
                .entry("language".to_string())
                .or_insert_with(|| toml::Value::String(language));
        }
    }

    if let Some(build_command) = build
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        target_table.insert(
            "build_command".to_string(),
            toml::Value::String(build_command.to_string()),
        );
    }

    if !outputs.is_empty() {
        target_table.insert(
            "outputs".to_string(),
            toml::Value::Array(outputs.into_iter().map(toml::Value::String).collect()),
        );
    }
    if !build_env.is_empty() {
        target_table.insert(
            "build_env".to_string(),
            toml::Value::Array(build_env.into_iter().map(toml::Value::String).collect()),
        );
    }

    if let Some(port) = port {
        target_table.insert("port".to_string(), toml::Value::Integer(i64::from(port)));
    }
    if !required_env.is_empty() {
        target_table.insert(
            "required_env".to_string(),
            toml::Value::Array(required_env.into_iter().map(toml::Value::String).collect()),
        );
    }
    if let Some(runtime_version) = runtime_version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        target_table.insert(
            "runtime_version".to_string(),
            toml::Value::String(runtime_version.to_string()),
        );
    }
    if !runtime_tools.is_empty() {
        target_table.insert(
            "runtime_tools".to_string(),
            toml::Value::try_from(runtime_tools).unwrap(),
        );
    }
    if let Some(readiness_probe) = readiness_probe {
        apply_v03_readiness_probe(&mut target_table, readiness_probe);
    }

    if let Some(image) = image
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        target_table.insert("image".to_string(), toml::Value::String(image.to_string()));
    }
    if !env.is_empty() {
        target_table.insert("env".to_string(), toml::Value::try_from(env).unwrap());
    }
    if !public.is_empty() {
        target_table.insert(
            "public".to_string(),
            toml::Value::Array(public.into_iter().map(toml::Value::String).collect()),
        );
    }

    Ok(target_table)
}

#[derive(Debug, Clone)]
struct V03WorkspaceTarget {
    label: String,
    target_table: Table,
}

#[derive(Debug, Clone, Default)]
struct V03WorkspaceContext {
    package_dirs_by_label: HashMap<String, PathBuf>,
    labels_by_relative_path: HashMap<String, String>,
}

fn seed_v03_workspace_context_labels(packages: &Table) -> V03WorkspaceContext {
    let mut context = V03WorkspaceContext::default();
    for label in packages.keys() {
        context
            .package_dirs_by_label
            .insert(label.clone(), PathBuf::new());
    }
    context
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

fn default_external_injection_required() -> bool {
    true
}

#[derive(Debug, Clone, Default)]
struct V03NormalizedDependencies {
    workspace_dependencies: Vec<String>,
    external_dependencies: Vec<ExternalCapsuleDependency>,
}

fn normalize_workspace_relative_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn validate_workspace_relative_reference(
    package_name: &str,
    alias: &str,
    raw: &str,
) -> Result<String, CapsuleError> {
    let path = Path::new(raw.trim());
    if path.is_absolute() {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} must use a relative workspace path",
            package_name, alias
        )));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} must not escape the workspace root",
            package_name, alias
        )));
    }

    let normalized = normalize_workspace_relative_path(path);
    if normalized.is_empty() {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} must reference a workspace package or path",
            package_name, alias
        )));
    }
    Ok(normalized)
}

fn workspace_members_globset(
    manifest_path: &Path,
    members: &[String],
) -> Result<Option<GlobSet>, CapsuleError> {
    if members.is_empty() {
        return Ok(None);
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in members {
        let glob = Glob::new(pattern).map_err(|err| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 workspace.members contains invalid glob '{}': {}",
                pattern, err
            ))
        })?;
        builder.add(glob);
    }

    builder.build().map(Some).map_err(|err| {
        CapsuleError::ParseError(format!(
            "schema_version=0.3 workspace.members could not build globset for '{}': {}",
            manifest_path.display(),
            err
        ))
    })
}

fn should_prune_workspace_member_walk_entry(workspace_root: &Path, entry: &DirEntry) -> bool {
    const IGNORED_DIR_NAMES: &[&str] = &[
        ".git",
        ".hg",
        ".svn",
        ".next",
        ".nuxt",
        ".output",
        ".svelte-kit",
        ".turbo",
        ".wrangler",
        "node_modules",
        "dist",
        "build",
        "target",
        "coverage",
        ".tmp",
        "tmp",
    ];

    if !entry.file_type().is_dir() {
        return false;
    }

    let Ok(relative) = entry.path().strip_prefix(workspace_root) else {
        return false;
    };
    if relative.as_os_str().is_empty() {
        return false;
    }

    relative.components().any(|component| match component {
        Component::Normal(value) => {
            let name = value.to_string_lossy();
            name.starts_with('.') || IGNORED_DIR_NAMES.contains(&name.as_ref())
        }
        _ => false,
    })
}

fn discover_v03_workspace_member_dirs(
    manifest_path: &Path,
    workspace_members: &[String],
) -> Result<HashMap<String, PathBuf>, CapsuleError> {
    let Some(globset) = workspace_members_globset(manifest_path, workspace_members)? else {
        return Ok(HashMap::new());
    };
    let workspace_root = manifest_path.parent().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "manifest path '{}' has no parent directory",
            manifest_path.display()
        ))
    })?;

    let mut discovered = HashMap::new();
    for entry in WalkDir::new(workspace_root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !should_prune_workspace_member_walk_entry(workspace_root, entry))
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_dir())
    {
        let dir = entry.into_path();
        if !dir.join("capsule.toml").exists() {
            continue;
        }

        let relative = dir.strip_prefix(workspace_root).map_err(|_| {
            CapsuleError::ParseError(format!(
                "workspace member '{}' must stay inside '{}'",
                dir.display(),
                workspace_root.display()
            ))
        })?;
        if !globset.is_match(relative) {
            continue;
        }

        let label = dir
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                CapsuleError::ParseError(format!(
                    "workspace member '{}' must have a terminal directory name",
                    dir.display()
                ))
            })?
            .to_string();
        if let Some(existing) = discovered.insert(label.clone(), dir.clone()) {
            return Err(CapsuleError::ParseError(format!(
                "schema_version=0.3 workspace.members discovered duplicate package label '{}' at '{}' and '{}'",
                label,
                existing.display(),
                dir.display()
            )));
        }
    }

    Ok(discovered)
}

fn augment_v03_packages_from_members(
    manifest_path: &Path,
    packages: &mut Table,
    member_dirs: &HashMap<String, PathBuf>,
) -> Result<(), CapsuleError> {
    let workspace_root = manifest_path.parent().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "manifest path '{}' has no parent directory",
            manifest_path.display()
        ))
    })?;

    for (label, member_dir) in member_dirs {
        if packages.contains_key(label) {
            continue;
        }
        let member_manifest_path = member_dir.join("capsule.toml");
        let claimed_by_explicit_package = packages.values().any(|raw_package| {
            raw_package
                .as_table()
                .and_then(|package_table| package_table.get("capsule_path"))
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .and_then(|capsule_path| {
                    manifest_path_for_capsule_path(manifest_path, capsule_path).ok()
                })
                .map(|path| path == member_manifest_path)
                .unwrap_or(false)
        });
        if claimed_by_explicit_package {
            continue;
        }

        let relative = member_dir.strip_prefix(workspace_root).map_err(|_| {
            CapsuleError::ParseError(format!(
                "workspace member '{}' must stay inside '{}'",
                member_dir.display(),
                workspace_root.display()
            ))
        })?;
        let mut package_table = Table::new();
        package_table.insert(
            "capsule_path".to_string(),
            toml::Value::String(normalize_workspace_relative_path(relative)),
        );
        packages.insert(label.clone(), toml::Value::Table(package_table));
    }

    Ok(())
}

fn build_v03_workspace_context(
    manifest_path: &Path,
    packages: &Table,
    member_dirs: &HashMap<String, PathBuf>,
) -> Result<V03WorkspaceContext, CapsuleError> {
    let workspace_root = manifest_path.parent().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "manifest path '{}' has no parent directory",
            manifest_path.display()
        ))
    })?;
    let mut context = V03WorkspaceContext::default();

    for (label, raw_package) in packages {
        let package_table = raw_package.as_table().ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{} must be a TOML table",
                label
            ))
        })?;

        let package_dir = if let Some(capsule_path) = package_table
            .get("capsule_path")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            manifest_path_for_capsule_path(manifest_path, capsule_path)?
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| workspace_root.to_path_buf())
        } else if let Some(member_dir) = member_dirs.get(label) {
            member_dir.clone()
        } else {
            workspace_root.to_path_buf()
        };

        let relative = package_dir.strip_prefix(workspace_root).map_err(|_| {
            CapsuleError::ParseError(format!(
                "workspace package '{}' resolved outside workspace root '{}'",
                label,
                workspace_root.display()
            ))
        })?;
        let relative = normalize_workspace_relative_path(relative);

        context
            .package_dirs_by_label
            .insert(label.clone(), package_dir.clone());
        if !relative.is_empty() {
            if let Some(existing) = context
                .labels_by_relative_path
                .insert(relative.clone(), label.clone())
            {
                if existing != *label {
                    return Err(CapsuleError::ParseError(format!(
                        "schema_version=0.3 workspace path '{}' maps to both '{}' and '{}'",
                        relative, existing, label
                    )));
                }
            }
        }
    }

    Ok(context)
}

fn normalize_v03_workspace_dependency(
    package_name: &str,
    alias: &str,
    raw_dependency: &toml::Value,
    workspace_context: &V03WorkspaceContext,
) -> Result<String, CapsuleError> {
    let dependency = raw_dependency.as_str().map(str::trim).ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} must be a string",
            package_name, alias
        ))
    })?;

    if dependency.is_empty() {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} must not be empty",
            package_name, alias
        )));
    }

    if let Some(target) = dependency.strip_prefix("workspace:") {
        let target = target.trim();
        if target.is_empty() {
            return Err(CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{}.dependencies.{} must reference a workspace package",
                package_name, alias
            )));
        }
        if workspace_context.package_dirs_by_label.contains_key(target) {
            return Ok(target.to_string());
        }

        let normalized_path = validate_workspace_relative_reference(package_name, alias, target)?;
        if let Some(label) = workspace_context
            .labels_by_relative_path
            .get(&normalized_path)
        {
            return Ok(label.clone());
        }

        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} references unknown workspace package/path '{}'",
            package_name, alias, target
        )));
    }

    if dependency.starts_with("capsule://") {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} external capsule dependencies are not supported yet",
            package_name, alias
        )));
    }

    Err(CapsuleError::ParseError(format!(
        "schema_version=0.3 packages.{}.dependencies.{} must use workspace: references",
        package_name, alias
    )))
}

fn infer_capsule_dependency_source_type(source: &str) -> Option<&'static str> {
    let source = source.trim();
    if source.starts_with("capsule://store/") {
        Some("store")
    } else if source.starts_with("capsule://github.com/") {
        Some("github")
    } else {
        None
    }
}

fn parse_capsule_dependency_source(
    package_name: &str,
    alias: &str,
    raw_source: &str,
) -> Result<(String, BTreeMap<String, String>), CapsuleError> {
    let source = raw_source.trim();
    let (base_source, query) = source.split_once('?').unwrap_or((source, ""));
    let mut injection_bindings = BTreeMap::new();

    if !query.is_empty() {
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if key.is_empty() || value.is_empty() {
                return Err(CapsuleError::ParseError(format!(
                    "schema_version=0.3 packages.{}.dependencies.{} contains an invalid capsule injection query in '{}'",
                    package_name, alias, raw_source
                )));
            }
            if injection_bindings.insert(key.clone(), value).is_some() {
                return Err(CapsuleError::ParseError(format!(
                    "schema_version=0.3 packages.{}.dependencies.{} repeats capsule injection key '{}'",
                    package_name, alias, key
                )));
            }
        }
    }

    Ok((base_source.to_string(), injection_bindings))
}

fn is_valid_external_injection_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn normalize_external_injection_type(
    package_name: &str,
    key: &str,
    raw_type: &str,
) -> Result<String, CapsuleError> {
    let normalized = raw_type.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "file" | "directory" | "string") {
        Ok(normalized)
    } else {
        Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.external_injection.{} type '{}' is unsupported",
            package_name, key, raw_type
        )))
    }
}

fn normalize_v03_external_injection_table(
    package_name: &str,
    table: &Table,
) -> Result<Vec<(String, ExternalInjectionSpec)>, CapsuleError> {
    let Some(raw_external_injection) = table.get("external_injection") else {
        return Ok(Vec::new());
    };

    let external_injection_table = raw_external_injection.as_table().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.external_injection must be a TOML table",
            package_name
        ))
    })?;

    let mut contracts = Vec::new();
    for (key, raw_contract) in external_injection_table {
        if !is_valid_external_injection_key(key) {
            return Err(CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{}.external_injection key '{}' must be an uppercase shell-safe identifier",
                package_name, key
            )));
        }

        let contract = if let Some(raw_type) = raw_contract.as_str() {
            ExternalInjectionSpec {
                injection_type: normalize_external_injection_type(package_name, key, raw_type)?,
                required: true,
                default: None,
            }
        } else {
            let contract_table = raw_contract.as_table().ok_or_else(|| {
                CapsuleError::ParseError(format!(
                    "schema_version=0.3 packages.{}.external_injection.{} must be a string or table",
                    package_name, key
                ))
            })?;
            let injection_type = contract_table
                .get("type")
                .and_then(toml::Value::as_str)
                .map(|value| normalize_external_injection_type(package_name, key, value))
                .transpose()?
                .ok_or_else(|| {
                    CapsuleError::ParseError(format!(
                        "schema_version=0.3 packages.{}.external_injection.{} must include type",
                        package_name, key
                    ))
                })?;
            let required = contract_table
                .get("required")
                .and_then(toml::Value::as_bool)
                .unwrap_or(true);
            let default = contract_table
                .get("default")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            ExternalInjectionSpec {
                injection_type,
                required,
                default,
            }
        };

        contracts.push((key.clone(), contract));
    }

    Ok(contracts)
}

fn normalize_v03_external_dependency(
    package_name: &str,
    alias: &str,
    raw_dependency: &toml::Value,
) -> Result<ExternalCapsuleDependency, CapsuleError> {
    let (source, source_type, injection_bindings) = if let Some(source) = raw_dependency.as_str() {
        let (source, injection_bindings) =
            parse_capsule_dependency_source(package_name, alias, source)?;
        let source_type = infer_capsule_dependency_source_type(&source).ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{}.dependencies.{} uses unsupported capsule source '{}'",
                package_name, alias, source
            ))
        })?;
        (source, source_type.to_string(), injection_bindings)
    } else {
        let table = raw_dependency.as_table().ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{}.dependencies.{} must be a string or table",
                package_name, alias
            ))
        })?;
        let raw_source = table
            .get("source")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                CapsuleError::ParseError(format!(
                    "schema_version=0.3 packages.{}.dependencies.{} table must include non-empty source",
                    package_name, alias
                ))
            })?;
        let (source, mut injection_bindings) =
            parse_capsule_dependency_source(package_name, alias, raw_source)?;
        let inferred_source_type = infer_capsule_dependency_source_type(&source).ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{}.dependencies.{} uses unsupported capsule source '{}'",
                package_name, alias, source
            ))
        })?;
        let source_type = table
            .get("source_type")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(inferred_source_type)
            .to_ascii_lowercase();
        if source_type != inferred_source_type {
            return Err(CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{}.dependencies.{} source_type '{}' does not match source '{}'",
                package_name, alias, source_type, source
            )));
        }

        if let Some(raw_bindings) = table.get("injection_bindings") {
            let binding_table = raw_bindings.as_table().ok_or_else(|| {
                CapsuleError::ParseError(format!(
                    "schema_version=0.3 packages.{}.dependencies.{}.injection_bindings must be a table",
                    package_name, alias
                ))
            })?;
            for (key, value) in binding_table {
                let value = value.as_str().map(str::trim).filter(|value| !value.is_empty()).ok_or_else(|| {
                    CapsuleError::ParseError(format!(
                        "schema_version=0.3 packages.{}.dependencies.{}.injection_bindings.{} must be a non-empty string",
                        package_name, alias, key
                    ))
                })?;
                if injection_bindings
                    .insert(key.clone(), value.to_string())
                    .is_some()
                {
                    return Err(CapsuleError::ParseError(format!(
                        "schema_version=0.3 packages.{}.dependencies.{} repeats capsule injection key '{}'",
                        package_name, alias, key
                    )));
                }
            }
        }

        (source, source_type, injection_bindings)
    };

    Ok(ExternalCapsuleDependency {
        alias: alias.to_string(),
        source,
        source_type,
        injection_bindings,
    })
}

fn normalize_v03_package_dependencies(
    package_name: &str,
    table: &Table,
    workspace_context: &V03WorkspaceContext,
) -> Result<V03NormalizedDependencies, CapsuleError> {
    let Some(raw_dependencies) = table.get("dependencies") else {
        return Ok(V03NormalizedDependencies::default());
    };

    let dependencies_table = raw_dependencies.as_table().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies must be a TOML table",
            package_name
        ))
    })?;

    let mut dependencies = V03NormalizedDependencies::default();
    let mut seen_workspace = HashSet::new();
    let mut seen_external = HashSet::new();
    for (alias, raw_dependency) in dependencies_table {
        let external_source = raw_dependency
            .as_str()
            .map(str::trim)
            .filter(|value| value.starts_with("capsule://"))
            .map(str::to_string)
            .or_else(|| {
                raw_dependency
                    .as_table()
                    .and_then(|table| table.get("source"))
                    .and_then(toml::Value::as_str)
                    .map(str::trim)
                    .filter(|value| value.starts_with("capsule://"))
                    .map(str::to_string)
            });

        if external_source.is_some() {
            let dependency =
                normalize_v03_external_dependency(package_name, alias, raw_dependency)?;
            if seen_external.insert((dependency.alias.clone(), dependency.source.clone())) {
                dependencies.external_dependencies.push(dependency);
            }
            continue;
        }

        let dependency = normalize_v03_workspace_dependency(
            package_name,
            alias,
            raw_dependency,
            workspace_context,
        )?;
        if seen_workspace.insert(dependency.clone()) {
            dependencies.workspace_dependencies.push(dependency);
        }
    }
    Ok(dependencies)
}

fn manifest_path_for_capsule_path(
    base_manifest_path: &Path,
    capsule_path: &str,
) -> Result<PathBuf, CapsuleError> {
    let base_dir = base_manifest_path.parent().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "manifest path '{}' has no parent directory",
            base_manifest_path.display()
        ))
    })?;
    let candidate = base_dir.join(capsule_path);
    let manifest_path = if candidate.is_dir() {
        candidate.join("capsule.toml")
    } else {
        candidate
    };

    if !manifest_path.exists() {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 capsule_path '{}' does not exist",
            manifest_path.display()
        )));
    }
    Ok(manifest_path)
}

fn relative_package_working_dir(
    root_manifest_path: &Path,
    package_dir: &Path,
) -> Result<Option<String>, CapsuleError> {
    let root_dir = root_manifest_path.parent().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "manifest path '{}' has no parent directory",
            root_manifest_path.display()
        ))
    })?;

    if package_dir == root_dir {
        return Ok(None);
    }

    let relative = package_dir.strip_prefix(root_dir).map_err(|_| {
        CapsuleError::ParseError(format!(
            "delegated package directory '{}' must stay inside workspace root '{}'",
            package_dir.display(),
            root_dir.display()
        ))
    })?;

    Ok(Some(relative.to_string_lossy().replace('\\', "/")))
}

fn normalize_workspace_target_from_package(
    root_manifest_path: Option<&Path>,
    package_name: &str,
    package_table: &Table,
    package_dir: Option<&Path>,
    workspace_context: &V03WorkspaceContext,
) -> Result<V03WorkspaceTarget, CapsuleError> {
    let mut target_table = normalize_v03_target_table(package_name, package_table)?;
    let dependencies =
        normalize_v03_package_dependencies(package_name, package_table, workspace_context)?;
    let working_dir = match (root_manifest_path, package_dir) {
        (Some(root_manifest_path), Some(package_dir)) => {
            relative_package_working_dir(root_manifest_path, package_dir)?
        }
        _ => None,
    };

    if let Some(working_dir) = working_dir.as_ref() {
        target_table.insert(
            "working_dir".to_string(),
            toml::Value::String(working_dir.clone()),
        );
    }
    let external_injection = normalize_v03_external_injection_table(package_name, package_table)?;
    if !external_injection.is_empty() {
        let mut table = Table::new();
        for (key, contract) in external_injection {
            table.insert(key, toml::Value::try_from(contract).unwrap());
        }
        target_table.insert("external_injection".to_string(), toml::Value::Table(table));
    }
    if !dependencies.workspace_dependencies.is_empty() {
        target_table.insert(
            "package_dependencies".to_string(),
            toml::Value::Array(
                dependencies
                    .workspace_dependencies
                    .iter()
                    .cloned()
                    .map(toml::Value::String)
                    .collect(),
            ),
        );
    }
    if !dependencies.external_dependencies.is_empty() {
        target_table.insert(
            "external_dependencies".to_string(),
            toml::Value::Array(
                dependencies
                    .external_dependencies
                    .iter()
                    .cloned()
                    .map(|dependency| toml::Value::try_from(dependency).unwrap())
                    .collect(),
            ),
        );
    }

    Ok(V03WorkspaceTarget {
        label: package_name.to_string(),
        target_table,
    })
}

fn normalize_v03_single_manifest_target(
    mut table: Table,
    root_manifest_path: Option<&Path>,
    package_manifest_path: Option<&Path>,
    explicit_label: Option<&str>,
    workspace_context: &V03WorkspaceContext,
) -> Result<V03WorkspaceTarget, CapsuleError> {
    let package_name = explicit_label.map(str::to_string).unwrap_or_else(|| {
        table
            .get("default_target")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("app")
            .to_string()
    });

    if let Some(capsule_type) = table.get("type").and_then(toml::Value::as_str) {
        table.insert(
            "type".to_string(),
            toml::Value::String(normalize_v03_capsule_type(capsule_type)),
        );
    }

    normalize_workspace_target_from_package(
        root_manifest_path,
        &package_name,
        &table,
        package_manifest_path.and_then(Path::parent),
        workspace_context,
    )
}

fn normalize_legacy_manifest_as_workspace_target(
    root_manifest_path: &Path,
    manifest_path: &Path,
    package_name: &str,
) -> Result<V03WorkspaceTarget, CapsuleError> {
    let manifest = CapsuleManifest::load_from_file(manifest_path)?;
    let target = manifest.resolve_default_target()?;
    let mut target_table = toml::Value::try_from(target.clone())
        .map_err(|err| CapsuleError::SerializeError(err.to_string()))?
        .as_table()
        .cloned()
        .ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "legacy delegated manifest '{}' did not normalize to a target table",
                manifest_path.display()
            ))
        })?;

    let working_dir = relative_package_working_dir(
        root_manifest_path,
        manifest_path.parent().unwrap_or(Path::new(".")),
    )?;
    if let Some(working_dir) = working_dir.as_ref() {
        target_table.insert(
            "working_dir".to_string(),
            toml::Value::String(working_dir.clone()),
        );
    }

    Ok(V03WorkspaceTarget {
        label: package_name.to_string(),
        target_table,
    })
}

fn normalize_v03_workspace_targets(
    root_manifest_path: Option<&Path>,
    current_manifest_path: Option<&Path>,
    workspace_defaults: &Table,
    packages: &Table,
    workspace_context: &V03WorkspaceContext,
    visiting: &mut HashSet<PathBuf>,
) -> Result<Vec<V03WorkspaceTarget>, CapsuleError> {
    let mut targets = Vec::new();

    for (package_name, raw_package) in packages {
        let package_table = raw_package.as_table().cloned().ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{} must be a TOML table",
                package_name
            ))
        })?;

        if let Some(capsule_path) = package_table
            .get("capsule_path")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let current_manifest_path = current_manifest_path.ok_or_else(|| {
                CapsuleError::ParseError(format!(
                    "schema_version=0.3 packages.{} capsule_path requires loading from a file path",
                    package_name
                ))
            })?;
            let delegated_manifest_path =
                manifest_path_for_capsule_path(current_manifest_path, capsule_path)?;
            let delegated_canonical = delegated_manifest_path
                .canonicalize()
                .unwrap_or_else(|_| delegated_manifest_path.clone());
            if !visiting.insert(delegated_canonical.clone()) {
                return Err(CapsuleError::ParseError(format!(
                    "circular capsule_path delegation detected at '{}'",
                    delegated_canonical.display()
                )));
            }

            let delegated_text = fs::read_to_string(&delegated_manifest_path).map_err(|err| {
                CapsuleError::ParseError(format!(
                    "failed to read delegated manifest '{}': {}",
                    delegated_manifest_path.display(),
                    err
                ))
            })?;
            let delegated_raw: toml::Value = toml::from_str(&delegated_text).map_err(|err| {
                CapsuleError::ParseError(format!(
                    "failed to parse delegated manifest '{}': {}",
                    delegated_manifest_path.display(),
                    err
                ))
            })?;
            let delegated_table = delegated_raw.as_table().cloned().ok_or_else(|| {
                CapsuleError::ParseError(format!(
                    "delegated manifest '{}' must be a TOML table",
                    delegated_manifest_path.display()
                ))
            })?;

            let delegated_targets = if is_v03_schema(&delegated_raw) {
                if delegated_table.contains_key("packages") {
                    let delegated_defaults = delegated_table
                        .get("workspace")
                        .and_then(|workspace| workspace.get("defaults"))
                        .and_then(toml::Value::as_table)
                        .cloned()
                        .unwrap_or_default();
                    let delegated_packages = delegated_table
                        .get("packages")
                        .and_then(toml::Value::as_table)
                        .cloned()
                        .ok_or_else(|| {
                            CapsuleError::ParseError(format!(
                                "schema_version=0.3 delegated manifest '{}' packages must be a TOML table",
                                delegated_manifest_path.display()
                            ))
                        })?;
                    let mut delegated_workspace_context = workspace_context.clone();
                    let seeded_context = seed_v03_workspace_context_labels(&delegated_packages);
                    for (label, package_dir) in seeded_context.package_dirs_by_label {
                        delegated_workspace_context
                            .package_dirs_by_label
                            .entry(label)
                            .or_insert(package_dir);
                    }
                    normalize_v03_workspace_targets(
                        root_manifest_path,
                        Some(&delegated_manifest_path),
                        &delegated_defaults,
                        &delegated_packages,
                        &delegated_workspace_context,
                        visiting,
                    )?
                } else {
                    vec![normalize_v03_single_manifest_target(
                        delegated_table,
                        root_manifest_path,
                        Some(&delegated_manifest_path),
                        Some(package_name),
                        workspace_context,
                    )?]
                }
            } else {
                vec![normalize_legacy_manifest_as_workspace_target(
                    root_manifest_path.ok_or_else(|| {
                        CapsuleError::ParseError(format!(
                            "schema_version=0.3 packages.{} capsule_path requires a workspace root path",
                            package_name
                        ))
                    })?,
                    &delegated_manifest_path,
                    package_name,
                )?]
            };
            visiting.remove(&delegated_canonical);
            targets.extend(delegated_targets);
            continue;
        }

        let merged = shallow_merge_v03_tables(workspace_defaults, &package_table);
        let package_dir = workspace_context
            .package_dirs_by_label
            .get(package_name)
            .filter(|path| !path.as_os_str().is_empty())
            .map(PathBuf::as_path);
        targets.push(normalize_workspace_target_from_package(
            root_manifest_path,
            package_name,
            &merged,
            package_dir.or_else(|| current_manifest_path.and_then(Path::parent)),
            workspace_context,
        )?);
    }

    Ok(targets)
}

fn normalize_v03_workspace_manifest_with_path(
    mut table: Table,
    manifest_path: Option<&Path>,
    visiting: &mut HashSet<PathBuf>,
) -> Result<toml::Value, CapsuleError> {
    let workspace_config = table
        .get("workspace")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let workspace_defaults = workspace_config
        .get("defaults")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let workspace_members = workspace_config
        .get("members")
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut packages = table
        .remove("packages")
        .and_then(|value| value.as_table().cloned())
        .ok_or_else(|| {
            CapsuleError::ParseError("schema_version=0.3 packages must be a TOML table".to_string())
        })?;
    let member_dirs = manifest_path
        .map(|path| discover_v03_workspace_member_dirs(path, &workspace_members))
        .transpose()?
        .unwrap_or_default();
    if let Some(path) = manifest_path {
        augment_v03_packages_from_members(path, &mut packages, &member_dirs)?;
    }
    let workspace_context = manifest_path
        .map(|path| build_v03_workspace_context(path, &packages, &member_dirs))
        .transpose()?
        .unwrap_or_else(|| seed_v03_workspace_context_labels(&packages));

    let explicit_default_target = table
        .get("default_target")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let mut targets_table = Table::new();
    let mut first_runnable_target: Option<String> = None;
    let targets = normalize_v03_workspace_targets(
        manifest_path,
        manifest_path,
        &workspace_defaults,
        &packages,
        &workspace_context,
        visiting,
    )?;
    for target in targets {
        let target_table = target.target_table;
        if first_runnable_target.is_none()
            && target_table
                .get("package_type")
                .and_then(toml::Value::as_str)
                .map(|value| !value.eq_ignore_ascii_case("library"))
                .unwrap_or(true)
        {
            first_runnable_target = Some(target.label.clone());
        }
        if targets_table.contains_key(&target.label) {
            return Err(CapsuleError::ParseError(format!(
                "schema_version=0.3 duplicate package name '{}' after capsule_path expansion",
                target.label
            )));
        }
        targets_table.insert(target.label, toml::Value::Table(target_table));
    }

    if !table.contains_key("type") {
        table.insert("type".to_string(), toml::Value::String("app".to_string()));
    }
    if !table.contains_key("version") {
        table.insert(
            "version".to_string(),
            toml::Value::String("0.0.0".to_string()),
        );
    }
    table.remove("workspace");
    table.insert("targets".to_string(), toml::Value::Table(targets_table));
    table.insert(
        "default_target".to_string(),
        toml::Value::String(
            explicit_default_target
                .or(first_runnable_target)
                .unwrap_or_else(|| "app".to_string()),
        ),
    );

    Ok(toml::Value::Table(table))
}

fn normalize_v03_manifest_value_with_path(
    raw: toml::Value,
    manifest_path: Option<&Path>,
    visiting: &mut HashSet<PathBuf>,
) -> Result<toml::Value, CapsuleError> {
    if !is_v03_like_schema(&raw) {
        return Ok(raw);
    }

    let mut table = raw.as_table().cloned().ok_or_else(|| {
        CapsuleError::ParseError("schema_version=0.3 manifest must be a TOML table".to_string())
    })?;

    if !table.contains_key("schema_version") {
        table.insert(
            "schema_version".to_string(),
            toml::Value::String("0.3".to_string()),
        );
    }

    if !table.contains_key("version") {
        table.insert(
            "version".to_string(),
            toml::Value::String("0.0.0".to_string()),
        );
    }

    if table.contains_key("execution") {
        return Err(CapsuleError::ParseError(
            "legacy [execution] section is not supported in schema_version=0.3".to_string(),
        ));
    }

    reject_v03_legacy_fields(&table, "manifest")?;

    if table.contains_key("packages") {
        return normalize_v03_workspace_manifest_with_path(table, manifest_path, visiting);
    }

    if let Some(capsule_type) = table.get("type").and_then(toml::Value::as_str) {
        table.insert(
            "type".to_string(),
            toml::Value::String(normalize_v03_capsule_type(capsule_type)),
        );
    }

    let has_top_level_build_command = table
        .get("build")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();

    if table
        .get("runtime")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        || table
            .get("run")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
        || has_top_level_build_command
        || table.contains_key("required_env")
        || table.contains_key("runtime_version")
        || table.contains_key("runtime_tools")
        || table.contains_key("port")
        || table.contains_key("readiness_probe")
        || table.contains_key("outputs")
        || table.contains_key("build_env")
    {
        let default_target = table
            .get("default_target")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("app")
            .to_string();

        let mut targets_table = table
            .remove("targets")
            .and_then(|value| value.as_table().cloned())
            .unwrap_or_default();
        let mut target_table = targets_table
            .remove(&default_target)
            .and_then(|value| value.as_table().cloned())
            .unwrap_or_default();
        let normalized_target = normalize_v03_target_table(&default_target, &table)?;
        target_table = shallow_merge_v03_tables(&target_table, &normalized_target);
        let external_injection = normalize_v03_external_injection_table(&default_target, &table)?;
        if !external_injection.is_empty() {
            let mut normalized_table = Table::new();
            for (key, contract) in external_injection {
                normalized_table.insert(key, toml::Value::try_from(contract).unwrap());
            }
            target_table.insert(
                "external_injection".to_string(),
                toml::Value::Table(normalized_table),
            );
        }
        let dependencies = normalize_v03_package_dependencies(
            &default_target,
            &table,
            &V03WorkspaceContext::default(),
        )?;
        if !dependencies.workspace_dependencies.is_empty() {
            target_table.insert(
                "package_dependencies".to_string(),
                toml::Value::Array(
                    dependencies
                        .workspace_dependencies
                        .into_iter()
                        .map(toml::Value::String)
                        .collect(),
                ),
            );
        }
        if !dependencies.external_dependencies.is_empty() {
            target_table.insert(
                "external_dependencies".to_string(),
                toml::Value::Array(
                    dependencies
                        .external_dependencies
                        .into_iter()
                        .map(|dependency| toml::Value::try_from(dependency).unwrap())
                        .collect(),
                ),
            );
        }

        targets_table.insert(default_target.clone(), toml::Value::Table(target_table));
        table.insert(
            "default_target".to_string(),
            toml::Value::String(default_target),
        );
        table.insert("targets".to_string(), toml::Value::Table(targets_table));

        let build_command = table
            .get("targets")
            .and_then(toml::Value::as_table)
            .and_then(|targets| {
                table
                    .get("default_target")
                    .and_then(toml::Value::as_str)
                    .and_then(|label| targets.get(label))
            })
            .and_then(toml::Value::as_table)
            .and_then(|target| target.get("build_command"))
            .and_then(toml::Value::as_str)
            .map(ToOwned::to_owned);

        table.remove("runtime");
        table.remove("run");
        table.remove("port");
        table.remove("required_env");
        table.remove("runtime_version");
        table.remove("runtime_tools");
        table.remove("readiness_probe");
        table.remove("outputs");
        table.remove("build_env");

        if let Some(build_command) = build_command {
            let mut lifecycle = Table::new();
            lifecycle.insert("build".to_string(), toml::Value::String(build_command));
            let mut build = Table::new();
            build.insert("lifecycle".to_string(), toml::Value::Table(lifecycle));
            table.insert("build".to_string(), toml::Value::Table(build));
        }
    }

    Ok(toml::Value::Table(table))
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public: Vec<String>,

    /// Optional listening port.
    #[serde(default)]
    pub port: Option<u16>,

    /// Optional working directory.
    #[serde(default)]
    pub working_dir: Option<String>,

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

impl CapsuleManifest {
    fn from_toml_with_path_internal(
        content: &str,
        manifest_path: Option<&Path>,
    ) -> Result<Self, CapsuleError> {
        let raw: toml::Value = toml::from_str(content)
            .map_err(|e| CapsuleError::ParseError(format!("TOML parse error: {}", e)))?;

        if raw.get("execution").is_some() {
            return Err(CapsuleError::ParseError(
                "legacy [execution] section is not supported in schema_version=0.2".to_string(),
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
            "toml" => Self::from_toml_with_path(&content, path),
            "json" => Self::from_json(&content),
            _ => {
                // Try TOML first, then JSON
                Self::from_toml_with_path(&content, path).or_else(|_| Self::from_json(&content))
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

        let schema_is_v03 = self.schema_version.trim() == "0.3";

        // Schema version must be supported.
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

        let is_v03_library = schema_is_v03 && self.capsule_type == CapsuleType::Library;

        // default_target must point to an existing named target.
        let named_targets = self
            .targets
            .as_ref()
            .map(|t| t.named_targets())
            .cloned()
            .unwrap_or_default();
        if !is_v03_library && self.default_target.trim().is_empty() {
            errors.push(ValidationError::MissingDefaultTarget);
        }
        if !is_v03_library && named_targets.is_empty() {
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
            let has_run_command = target
                .run_command
                .as_ref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false);
            let target_is_library = schema_is_v03
                && target
                    .package_type
                    .as_deref()
                    .map(|value| value.eq_ignore_ascii_case("library"))
                    .unwrap_or(is_v03_library);

            if target_is_library {
                if has_run_command
                    || !entrypoint.is_empty()
                    || target
                        .image
                        .as_deref()
                        .map(|value| !value.trim().is_empty())
                        .unwrap_or(false)
                    || !target.cmd.is_empty()
                {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "library target '{}' must not define a run command",
                        label
                    )));
                }
                continue;
            }

            if label.trim().is_empty()
                || runtime.is_empty()
                || !matches!(runtime.as_str(), "source" | "wasm" | "oci" | "web")
            {
                errors.push(ValidationError::InvalidTarget(label.clone()));
                continue;
            }

            if runtime == "source" {
                if entrypoint.is_empty() && !has_run_command {
                    errors.push(ValidationError::InvalidTarget(label.clone()));
                    continue;
                }
                let effective_driver = infer_source_driver(target, entrypoint);
                if !schema_is_v03
                    && matches!(
                        effective_driver.as_deref(),
                        Some("deno") | Some("node") | Some("python")
                    )
                    && target
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
                    if entrypoint.is_empty() && !has_run_command {
                        errors.push(ValidationError::InvalidTarget(label.clone()));
                        continue;
                    }
                    if matches!(
                        normalized_driver.as_deref(),
                        Some("node") | Some("deno") | Some("python")
                    ) && !has_run_command
                        && entrypoint.split_whitespace().count() > 1
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
            } else if entrypoint.is_empty() && !has_run_command {
                errors.push(ValidationError::InvalidTarget(label.clone()));
                continue;
            }

            if let Some(probe) = target.readiness_probe.as_ref() {
                if probe.port.trim().is_empty() {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': readiness_probe.port must be a non-empty placeholder name",
                        label
                    )));
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
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': readiness_probe must define http_get or tcp_connect",
                        label
                    )));
                }
            }

            for (key, contract) in &target.external_injection {
                if !is_valid_external_injection_key(key) {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': external_injection key '{}' must be an uppercase shell-safe identifier",
                        label, key
                    )));
                }
                if !matches!(
                    contract.injection_type.as_str(),
                    "file" | "directory" | "string"
                ) {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': external_injection.{} type '{}' is unsupported",
                        label, key, contract.injection_type
                    )));
                }
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

        if schema_is_v03 {
            let package_dependencies = named_targets
                .iter()
                .map(|(label, target)| (label.clone(), target.package_dependencies.clone()))
                .collect::<HashMap<_, _>>();

            for (label, dependencies) in &package_dependencies {
                for dependency in dependencies {
                    if dependency == label {
                        errors.push(ValidationError::InvalidTarget(format!(
                            "target '{}' must not depend on itself",
                            label
                        )));
                    } else if !named_targets.contains_key(dependency) {
                        errors.push(ValidationError::InvalidTarget(format!(
                            "target '{}' depends on unknown workspace package '{}'",
                            label, dependency
                        )));
                    }
                }

                let target = named_targets
                    .get(label)
                    .expect("package_dependencies keys must exist in named_targets");
                if target.outputs.iter().any(|value| value.trim().is_empty()) {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': outputs must not contain empty patterns",
                        label
                    )));
                }
                if target.build_env.iter().any(|value| value.trim().is_empty()) {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': build_env must not contain empty variable names",
                        label
                    )));
                }
            }

            if let Err(err) = startup_order_from_dependencies(&package_dependencies) {
                errors.push(ValidationError::InvalidTarget(err.to_string()));
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
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("Invalid schema_version '{0}', expected '1' or legacy '0.2'/'0.3'")]
    InvalidSchemaVersion(String),
    #[error("Invalid name '{0}', must be kebab-case")]
    InvalidName(String),
    #[error("Invalid memory string for {field}: '{value}'")]
    InvalidMemoryString { field: &'static str, value: String },
    #[error("Invalid version '{0}', must be semver (e.g., 1.0.0)")]
    InvalidVersion(String),
    #[error("Inference Capsule must have capabilities defined")]
    MissingCapabilities,
    #[error("Inference Capsule must have model config defined")]
    MissingModelConfig,
    #[error("Invalid port {0}")]
    InvalidPort(u16),
    #[error("Storage volumes are only supported for execution.runtime=docker")]
    StorageOnlyForDocker,
    #[error("Invalid storage volume (requires unique name and absolute mount_path)")]
    InvalidStorageVolume,
    #[error("default_target is required")]
    MissingDefaultTarget,
    #[error("At least one [targets.<label>] entry is required")]
    MissingTargets,
    #[error("default_target '{0}' does not exist under [targets]")]
    DefaultTargetNotFound(String),
    #[error("Invalid target: {0}")]
    InvalidTarget(String),
    #[error("Invalid target '{0}': unsupported driver '{1}' (allowed: static|deno|node|python|wasmtime|native)")]
    InvalidTargetDriver(String, String),
    #[error("Invalid target '{0}': runtime_version is required for runtime=source driver='{1}'")]
    MissingRuntimeVersion(String, String),
    #[error("Invalid web target '{0}': {1}")]
    InvalidWebTarget(String, String),
    #[error("Invalid service '{0}': {1}")]
    InvalidService(String, String),
    #[error("Invalid state '{0}': {1}")]
    InvalidState(String, String),
    #[error("Invalid state binding for service '{0}': {1}")]
    InvalidStateBinding(String, String),
}

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
    fn test_from_toml_accepts_v03_single_package_manifest() {
        let toml = r#"
schema_version = "0.3"
name = "v03-demo"
version = "0.1.0"
type = "app"
runtime = "source/node"
build = "npm run build"
run = "npm start"
port = 3000
required_env = ["DATABASE_URL"]
"#;

        let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 manifest");
        assert_eq!(manifest.schema_version, "0.3");
        assert_eq!(manifest.default_target, "app");

        let target = manifest.resolve_default_target().expect("default target");
        assert_eq!(target.runtime, "source");
        assert_eq!(target.driver.as_deref(), Some("node"));
        assert!(target.entrypoint.is_empty());
        assert!(target.cmd.is_empty());
        assert_eq!(target.run_command.as_deref(), Some("npm start"));
        assert_eq!(target.port, Some(3000));
        assert_eq!(target.required_env, vec!["DATABASE_URL".to_string()]);
        assert_eq!(
            manifest
                .build
                .as_ref()
                .and_then(|build| build.lifecycle.as_ref())
                .and_then(|lifecycle| lifecycle.build.as_deref()),
            Some("npm run build")
        );
    }

    #[test]
    fn test_from_toml_accepts_chml_single_package_manifest() {
        let toml = r#"
name = "chml-demo"
type = "app"
runtime = "source/node"
build = "npm run build"
outputs = ["dist/**"]
build_env = ["NODE_ENV", "API_BASE_URL"]
run = "npm start"
port = 3000
required_env = ["DATABASE_URL"]

[external_injection]
MODEL_DIR = "directory"
"#;

        let manifest = CapsuleManifest::from_toml(toml).expect("parse CHML manifest");
        assert_eq!(manifest.schema_version, "0.3");
        assert_eq!(manifest.version, "0.0.0");
        assert_eq!(manifest.default_target, "app");

        let target = manifest.resolve_default_target().expect("default target");
        assert_eq!(target.runtime, "source");
        assert_eq!(target.driver.as_deref(), Some("node"));
        assert_eq!(target.run_command.as_deref(), Some("npm start"));
        assert_eq!(target.outputs, vec!["dist/**".to_string()]);
        assert_eq!(
            target.build_env,
            vec!["NODE_ENV".to_string(), "API_BASE_URL".to_string()]
        );
        assert_eq!(
            target.external_injection["MODEL_DIR"].injection_type,
            "directory"
        );
    }

    #[test]
    fn test_from_toml_preserves_v03_run_command_without_splitting() {
        let toml = r#"
schema_version = "0.3"
name = "json-server"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "node src/bin.ts fixtures/db.json"
"#;

        let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 manifest");
        let target = manifest.resolve_default_target().expect("default target");

        assert!(target.entrypoint.is_empty());
        assert_eq!(target.driver.as_deref(), Some("node"));
        assert_eq!(target.language.as_deref(), Some("node"));
        assert_eq!(
            target.run_command.as_deref(),
            Some("node src/bin.ts fixtures/db.json")
        );
        assert!(target.cmd.is_empty());
    }

    #[test]
    fn test_from_toml_preserves_v03_readiness_probe_table() {
        let toml = r#"
schema_version = "0.3"
name = "probe-demo"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "npm start -- --port $PORT"
port = 3000
readiness_probe = { http_get = "/healthz", port = "PORT" }
"#;

        let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 manifest");
        let target = manifest.resolve_default_target().expect("default target");

        assert_eq!(
            target.run_command.as_deref(),
            Some("npm start -- --port $PORT")
        );
        assert_eq!(
            target
                .readiness_probe
                .as_ref()
                .and_then(|probe| probe.http_get.as_deref()),
            Some("/healthz")
        );
        assert_eq!(
            target
                .readiness_probe
                .as_ref()
                .map(|probe| probe.port.as_str()),
            Some("PORT")
        );
    }

    #[test]
    fn test_validate_v03_library_without_run_is_ok() {
        let toml = r#"
schema_version = "0.3"
name = "shared-ui"
version = "0.1.0"
type = "library"
build = "npm run build"
"#;

        let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 library");
        assert_eq!(manifest.capsule_type, CapsuleType::Library);
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_validate_v03_library_rejects_run_command() {
        let toml = r#"
schema_version = "0.3"
name = "shared-ui"
version = "0.1.0"
type = "library"
runtime = "source/node"
run = "npm start"
"#;

        let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 library");
        let errors = manifest.validate().expect_err("library run must fail");
        assert!(errors.iter().any(|error| {
            matches!(error, ValidationError::InvalidTarget(message) if message.contains("must not define a run command"))
        }));
    }

    #[test]
    fn test_from_toml_accepts_v03_workspace_packages_as_named_targets() {
        let toml = r#"
schema_version = "0.3"
name = "workspace-demo"

[workspace]
members = ["apps/*"]

[workspace.defaults]
runtime = "source/node"
required_env = ["DATABASE_URL"]

[packages.web]
type = "app"
build = "pnpm --filter web build"
run = "pnpm --filter web start"
port = 3000

    [packages.web.dependencies]
    ui = "workspace:ui"

[packages.ui]
type = "library"
build = "pnpm --filter ui build"
"#;

        let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 workspace");
        assert_eq!(manifest.default_target, "web");
        assert_eq!(manifest.version, "0.0.0");

        let web = manifest
            .targets
            .as_ref()
            .and_then(|targets| targets.named_target("web"))
            .expect("web target");
        assert_eq!(web.package_type.as_deref(), Some("app"));
        assert_eq!(
            web.build_command.as_deref(),
            Some("pnpm --filter web build")
        );
        assert_eq!(web.run_command.as_deref(), Some("pnpm --filter web start"));
        assert_eq!(web.required_env, vec!["DATABASE_URL".to_string()]);
        assert_eq!(web.package_dependencies, vec!["ui".to_string()]);

        let ui = manifest
            .targets
            .as_ref()
            .and_then(|targets| targets.named_target("ui"))
            .expect("ui target");
        assert_eq!(ui.package_type.as_deref(), Some("library"));
        assert_eq!(ui.build_command.as_deref(), Some("pnpm --filter ui build"));
    }

    #[test]
    fn test_from_toml_accepts_chml_workspace_packages_as_named_targets() {
        let toml = r#"
name = "workspace-demo"

[workspace]
members = ["apps/*"]

[workspace.defaults]
runtime = "source/node"
required_env = ["DATABASE_URL"]

[packages.web]
type = "app"
build = "pnpm --filter web build"
outputs = ["apps/web/dist/**"]
build_env = ["NODE_ENV"]
run = "pnpm --filter web start"
port = 3000

    [packages.web.dependencies]
    ui = "workspace:ui"

[packages.ui]
type = "library"
build = "pnpm --filter ui build"
outputs = ["packages/ui/dist/**"]
"#;

        let manifest = CapsuleManifest::from_toml(toml).expect("parse CHML workspace");
        assert_eq!(manifest.schema_version, "0.3");
        assert_eq!(manifest.version, "0.0.0");
        assert_eq!(manifest.default_target, "web");

        let web = manifest
            .targets
            .as_ref()
            .and_then(|targets| targets.named_target("web"))
            .expect("web target");
        assert_eq!(web.outputs, vec!["apps/web/dist/**".to_string()]);
        assert_eq!(web.build_env, vec!["NODE_ENV".to_string()]);
        assert_eq!(web.required_env, vec!["DATABASE_URL".to_string()]);

        let ui = manifest
            .targets
            .as_ref()
            .and_then(|targets| targets.named_target("ui"))
            .expect("ui target");
        assert_eq!(ui.outputs, vec!["packages/ui/dist/**".to_string()]);
    }

    #[test]
    fn test_validate_v03_workspace_rejects_dependency_cycles() {
        let toml = r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.web]
type = "app"
runtime = "source/node"
run = "pnpm --filter web start"

  [packages.web.dependencies]
  ui = "workspace:ui"

[packages.ui]
type = "library"
runtime = "source/node"
build = "pnpm --filter ui build"

  [packages.ui.dependencies]
  web = "workspace:web"
"#;

        let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 workspace");
        let errors = manifest.validate().expect_err("cycle must fail");
        assert!(errors.iter().any(|error| {
            matches!(error, ValidationError::InvalidTarget(message) if message.contains("circular dependency detected"))
        }));
    }

    #[test]
    fn test_from_toml_rejects_v03_top_level_legacy_entrypoint() {
        let toml = r#"
schema_version = "0.3"
name = "legacy-v03"
version = "0.1.0"
type = "app"
runtime = "source/node"
entrypoint = "server.js"
"#;

        let error = CapsuleManifest::from_toml(toml).expect_err("v0.3 entrypoint must fail");
        assert!(error
            .to_string()
            .contains("must not use legacy field 'entrypoint'"));
    }

    #[test]
    fn test_from_toml_rejects_v03_target_legacy_cmd() {
        let toml = r#"
schema_version = "0.3"
name = "legacy-v03"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"
cmd = ["python", "app.py"]
"#;

        let error = CapsuleManifest::from_toml(toml).expect_err("v0.3 cmd must fail");
        assert!(error
            .to_string()
            .contains("must not use legacy field 'cmd'"));
    }

    #[test]
    fn test_load_from_file_supports_v03_capsule_path_single_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root_manifest = tmp.path().join("capsule.toml");
        let package_dir = tmp.path().join("apps").join("api");
        fs::create_dir_all(&package_dir).expect("create package dir");

        fs::write(
            &root_manifest,
            r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.api]
capsule_path = "./apps/api"
"#,
        )
        .expect("write root manifest");

        fs::write(
            package_dir.join("capsule.toml"),
            r#"
schema_version = "0.3"
name = "api"
type = "app"
runtime = "source/node"
run = "pnpm start"
"#,
        )
        .expect("write delegated manifest");

        let manifest = CapsuleManifest::load_from_file(&root_manifest).expect("load manifest");
        let api = manifest
            .targets
            .as_ref()
            .and_then(|targets| targets.named_target("api"))
            .expect("api target");

        assert_eq!(manifest.default_target, "api");
        assert_eq!(api.run_command.as_deref(), Some("pnpm start"));
        assert_eq!(api.working_dir.as_deref(), Some("apps/api"));
    }

    #[test]
    fn test_load_from_file_ignores_generated_workspace_member_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root_manifest = tmp.path().join("capsule.toml");
        let control_plane_dir = tmp.path().join("apps").join("control-plane");
        let dashboard_dir = tmp.path().join("apps").join("dashboard");
        let generated_duplicate_dir = dashboard_dir
            .join(".next")
            .join("standalone")
            .join("apps")
            .join("control-plane");
        fs::create_dir_all(&control_plane_dir).expect("create control-plane dir");
        fs::create_dir_all(&dashboard_dir).expect("create dashboard dir");
        fs::create_dir_all(&generated_duplicate_dir).expect("create generated duplicate dir");

        fs::write(
            &root_manifest,
            r#"
name = "file2api"

[workspace]
members = ["apps/*"]

[packages.control-plane]
type = "app"
runtime = "source/python"
run = "uvicorn control_plane.modal_webhook:app --host 0.0.0.0 --port $PORT"
port = 8000

[packages.dashboard]
type = "app"
runtime = "source/node"
build = "npm run build"
run = "npm start"
port = 3000
"#,
        )
        .expect("write root manifest");

        fs::write(
            control_plane_dir.join("capsule.toml"),
            "name = \"control-plane\"\ntype = \"app\"\nruntime = \"source/python\"\nrun = \"python main.py\"\n",
        )
        .expect("write control-plane manifest");
        fs::write(
            dashboard_dir.join("capsule.toml"),
            "name = \"dashboard\"\ntype = \"app\"\nruntime = \"source/node\"\nrun = \"npm start\"\n",
        )
        .expect("write dashboard manifest");
        fs::write(
            generated_duplicate_dir.join("capsule.toml"),
            "name = \"control-plane\"\ntype = \"app\"\nruntime = \"source/python\"\nrun = \"python generated.py\"\n",
        )
        .expect("write generated duplicate manifest");

        let manifest = CapsuleManifest::load_from_file(&root_manifest).expect("load manifest");
        let targets = manifest.targets.expect("targets");
        assert!(targets.named_target("control-plane").is_some());
        assert!(targets.named_target("dashboard").is_some());
    }

    #[test]
    fn test_load_from_file_supports_v03_capsule_path_workspace_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root_manifest = tmp.path().join("capsule.toml");
        let delegated_dir = tmp.path().join("packages").join("shared");
        fs::create_dir_all(&delegated_dir).expect("create delegated dir");

        fs::write(
            &root_manifest,
            r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.shared]
capsule_path = "./packages/shared"
"#,
        )
        .expect("write root manifest");

        fs::write(
            delegated_dir.join("capsule.toml"),
            r#"
schema_version = "0.3"
name = "shared-workspace"

[packages.ui]
type = "library"
runtime = "source/node"
build = "pnpm --filter ui build"

[packages.web]
type = "app"
runtime = "source/node"
run = "pnpm --filter web start"

  [packages.web.dependencies]
  ui = "workspace:ui"
"#,
        )
        .expect("write delegated workspace manifest");

        let manifest = CapsuleManifest::load_from_file(&root_manifest).expect("load manifest");
        let targets = manifest.targets.as_ref().expect("targets");
        let ui = targets.named_target("ui").expect("ui target");
        let web = targets.named_target("web").expect("web target");

        assert_eq!(manifest.default_target, "web");
        assert_eq!(ui.working_dir.as_deref(), Some("packages/shared"));
        assert_eq!(web.working_dir.as_deref(), Some("packages/shared"));
        assert_eq!(web.package_dependencies, vec!["ui".to_string()]);
    }

    #[test]
    fn test_load_from_file_expands_workspace_members_and_resolves_workspace_path_dependencies() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root_manifest = tmp.path().join("capsule.toml");
        let web_dir = tmp.path().join("apps").join("web");
        let ui_dir = tmp.path().join("packages").join("ui");
        let api_dir = tmp.path().join("apps").join("api_gateway");
        fs::create_dir_all(&web_dir).expect("create web dir");
        fs::create_dir_all(&ui_dir).expect("create ui dir");
        fs::create_dir_all(&api_dir).expect("create api dir");

        fs::write(web_dir.join("capsule.toml"), "name = 'web-marker'\n")
            .expect("write web marker manifest");
        fs::write(
            ui_dir.join("capsule.toml"),
            r#"
schema_version = "0.3"
name = "ui"
type = "library"
build = "pnpm --filter ui build"
"#,
        )
        .expect("write ui manifest");
        fs::write(
            api_dir.join("capsule.toml"),
            r#"
schema_version = "0.3"
name = "api-gateway"
type = "app"
runtime = "source/node"
run = "pnpm --filter api start"
"#,
        )
        .expect("write api manifest");
        fs::write(
            &root_manifest,
            r#"
schema_version = "0.3"
name = "workspace-demo"

[workspace]
members = ["apps/*", "packages/*"]

[workspace.defaults]
runtime = "source/node"

[packages.web]
type = "app"
run = "pnpm --filter web start"

  [packages.web.dependencies]
  ui = "workspace:packages/ui"

[packages.api_gateway]
capsule_path = "./apps/api_gateway"
"#,
        )
        .expect("write root manifest");

        let manifest = CapsuleManifest::load_from_file(&root_manifest).expect("load manifest");
        let targets = manifest.targets.as_ref().expect("targets");
        let web = targets.named_target("web").expect("web target");
        let ui = targets.named_target("ui").expect("ui target");
        let api = targets
            .named_target("api_gateway")
            .expect("api_gateway target");

        assert_eq!(web.working_dir.as_deref(), Some("apps/web"));
        assert_eq!(web.package_dependencies, vec!["ui".to_string()]);
        assert_eq!(ui.working_dir.as_deref(), Some("packages/ui"));
        assert_eq!(api.working_dir.as_deref(), Some("apps/api_gateway"));
    }

    #[test]
    fn test_workspace_path_dependency_resolves_to_explicit_package_label() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root_manifest = tmp.path().join("capsule.toml");
        let ui_dir = tmp.path().join("packages").join("ui");
        fs::create_dir_all(&ui_dir).expect("create ui dir");
        fs::write(
            ui_dir.join("capsule.toml"),
            r#"
schema_version = "0.3"
name = "ui"
type = "library"
build = "pnpm --filter ui build"
"#,
        )
        .expect("write ui manifest");
        fs::write(
            &root_manifest,
            r#"
schema_version = "0.3"
name = "workspace-demo"

[workspace]
members = ["packages/*"]

[packages.web]
type = "app"
runtime = "source/node"
run = "pnpm --filter web start"

  [packages.web.dependencies]
  ui = "workspace:packages/ui"

[packages.shared-ui]
capsule_path = "./packages/ui"
"#,
        )
        .expect("write root manifest");

        let manifest = CapsuleManifest::load_from_file(&root_manifest).expect("load manifest");
        let web = manifest
            .targets
            .as_ref()
            .and_then(|targets| targets.named_target("web"))
            .expect("web target");

        assert_eq!(web.package_dependencies, vec!["shared-ui".to_string()]);
    }

    #[test]
    fn test_load_from_file_preserves_external_capsule_dependencies() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.web]
type = "app"
runtime = "source/node"
run = "node server.js"

  [packages.web.dependencies]
  auth = "capsule://store/acme/auth-svc"
"#,
        )
        .expect("write manifest");

        let manifest = CapsuleManifest::load_from_file(&manifest_path).expect("load manifest");
        let web = manifest
            .targets
            .as_ref()
            .and_then(|targets| targets.named_target("web"))
            .expect("web target");

        assert_eq!(web.external_dependencies.len(), 1);
        assert_eq!(web.external_dependencies[0].alias, "auth");
        assert_eq!(
            web.external_dependencies[0].source,
            "capsule://store/acme/auth-svc"
        );
        assert_eq!(web.external_dependencies[0].source_type, "store");
    }

    #[test]
    fn test_load_from_file_preserves_external_capsule_dependency_query_bindings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.web]
type = "app"
runtime = "source/node"
run = "npm start"

  [packages.web.dependencies]
  auth = "capsule://store/acme/auth-svc?MODEL_DIR=https%3A%2F%2Fdata.tld%2Fweights.zip&CONFIG_FILE=file%3A%2F%2F.%2Fconfig.json"
"#,
        )
        .expect("write manifest");

        let manifest = CapsuleManifest::load_from_file(&manifest_path).expect("load manifest");
        let web = manifest
            .targets
            .as_ref()
            .and_then(|targets| targets.named_target("web"))
            .expect("web target");

        assert_eq!(web.external_dependencies.len(), 1);
        assert_eq!(
            web.external_dependencies[0].source,
            "capsule://store/acme/auth-svc"
        );
        assert_eq!(
            web.external_dependencies[0]
                .injection_bindings
                .get("MODEL_DIR")
                .map(String::as_str),
            Some("https://data.tld/weights.zip")
        );
        assert_eq!(
            web.external_dependencies[0]
                .injection_bindings
                .get("CONFIG_FILE")
                .map(String::as_str),
            Some("file://./config.json")
        );
    }

    #[test]
    fn test_load_from_file_preserves_external_injection_contracts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.worker]
type = "app"
runtime = "source/python"
run = "python main.py --config $CONFIG_FILE"

  [packages.worker.external_injection]
  MODEL_DIR = "directory"
  CONFIG_FILE = { type = "file", required = false, default = "https://example.test/config.json" }
"#,
        )
        .expect("write manifest");

        let manifest = CapsuleManifest::load_from_file(&manifest_path).expect("load manifest");
        let worker = manifest
            .targets
            .as_ref()
            .and_then(|targets| targets.named_target("worker"))
            .expect("worker target");

        assert_eq!(
            worker.external_injection["MODEL_DIR"].injection_type,
            "directory"
        );
        assert!(worker.external_injection["MODEL_DIR"].required);
        assert_eq!(
            worker.external_injection["CONFIG_FILE"].injection_type,
            "file"
        );
        assert!(!worker.external_injection["CONFIG_FILE"].required);
        assert_eq!(
            worker.external_injection["CONFIG_FILE"].default.as_deref(),
            Some("https://example.test/config.json")
        );
    }

    #[test]
    fn test_load_from_file_rejects_invalid_external_injection_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.worker]
type = "app"
runtime = "source/python"
run = "python main.py"

  [packages.worker.external_injection]
  model_dir = "directory"
"#,
        )
        .expect("write manifest");

        let err = CapsuleManifest::load_from_file(&manifest_path).expect_err("must reject");
        assert!(err
            .to_string()
            .contains("external_injection key 'model_dir'"));
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

#[path = "env_origin.rs"]
mod env_origin;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::error::{CapsuleError, Result};
pub use env_origin::{default_env_origin, EnvOrigin};

pub const EXECUTION_IDENTITY_SCHEMA_VERSION: u32 = 1;
pub const EXECUTION_IDENTITY_SCHEMA_VERSION_V2_EXPERIMENTAL: u32 = 2;
pub const EXECUTION_IDENTITY_CANONICALIZATION: &str = "jcs";
pub const EXECUTION_IDENTITY_HASH_ALGORITHM: &str = "blake3-256";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrackingStatus {
    Known,
    Unknown,
    Untracked,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracked<T> {
    pub status: TrackingStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl<T> Tracked<T> {
    pub fn known(value: T) -> Self {
        Self {
            status: TrackingStatus::Known,
            value: Some(value),
            reason: None,
        }
    }

    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            status: TrackingStatus::Unknown,
            value: None,
            reason: Some(reason.into()),
        }
    }

    pub fn untracked(reason: impl Into<String>) -> Self {
        Self {
            status: TrackingStatus::Untracked,
            value: None,
            reason: Some(reason.into()),
        }
    }

    pub fn not_applicable() -> Self {
        Self {
            status: TrackingStatus::NotApplicable,
            value: None,
            reason: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentityInput {
    pub schema_version: u32,
    pub canonicalization: String,
    pub hash_algorithm: String,
    pub source: SourceIdentity,
    pub dependencies: DependencyIdentity,
    pub runtime: RuntimeIdentity,
    pub environment: EnvironmentIdentity,
    pub filesystem: FilesystemIdentity,
    pub policy: PolicyIdentity,
    pub launch: LaunchIdentity,
    pub reproducibility: ReproducibilityIdentity,
}

impl ExecutionIdentityInput {
    // Each argument corresponds to one of the canonical execution-identity
    // facets pinned by the v1 schema; they don't generalize into a builder
    // without obscuring which facet is which at the call site.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: SourceIdentity,
        dependencies: DependencyIdentity,
        runtime: RuntimeIdentity,
        environment: EnvironmentIdentity,
        filesystem: FilesystemIdentity,
        policy: PolicyIdentity,
        launch: LaunchIdentity,
        reproducibility: ReproducibilityIdentity,
    ) -> Self {
        Self {
            schema_version: EXECUTION_IDENTITY_SCHEMA_VERSION,
            canonicalization: EXECUTION_IDENTITY_CANONICALIZATION.to_string(),
            hash_algorithm: EXECUTION_IDENTITY_HASH_ALGORITHM.to_string(),
            source,
            dependencies,
            runtime,
            environment,
            filesystem,
            policy,
            launch,
            reproducibility,
        }
    }

    pub fn compute_id(&self) -> Result<ExecutionIdentityDigest> {
        compute_execution_id(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentityDigest {
    pub execution_id: String,
    pub input_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub schema_version: u32,
    pub execution_id: String,
    pub computed_at: String,
    pub identity: ExecutionIdentityMetadata,
    pub source: SourceIdentity,
    pub dependencies: DependencyIdentity,
    pub runtime: RuntimeIdentity,
    pub environment: EnvironmentIdentity,
    pub filesystem: FilesystemIdentity,
    pub policy: PolicyIdentity,
    pub launch: LaunchIdentity,
    pub reproducibility: ReproducibilityIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentityMetadata {
    pub canonicalization: String,
    pub hash_algorithm: String,
    pub input_hash: String,
}

impl ExecutionReceipt {
    pub fn from_input(input: ExecutionIdentityInput, computed_at: String) -> Result<Self> {
        let digest = input.compute_id()?;
        Ok(Self {
            schema_version: input.schema_version,
            execution_id: digest.execution_id,
            computed_at,
            identity: ExecutionIdentityMetadata {
                canonicalization: input.canonicalization.clone(),
                hash_algorithm: input.hash_algorithm.clone(),
                input_hash: digest.input_hash,
            },
            source: input.source,
            dependencies: input.dependencies,
            runtime: input.runtime,
            environment: input.environment,
            filesystem: input.filesystem,
            policy: input.policy,
            launch: input.launch,
            reproducibility: input.reproducibility,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub source_ref: Tracked<String>,
    pub source_tree_hash: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentityInputV2 {
    pub schema_version: u32,
    pub canonicalization: String,
    pub hash_algorithm: String,
    pub source: SourceIdentityV2,
    pub source_provenance: SourceProvenance,
    pub dependencies: DependencyIdentityV2,
    pub runtime: RuntimeIdentityV2,
    pub environment: EnvironmentIdentityV2,
    pub filesystem: FilesystemIdentityV2,
    pub policy: PolicyIdentityV2,
    pub launch: LaunchIdentityV2,
    pub local: Option<LocalExecutionLocator>,
    pub reproducibility: ReproducibilityIdentity,
}

impl ExecutionIdentityInputV2 {
    // V2 adds source_provenance + local on top of v1's eight facets; like
    // the v1 constructor these are all canonical schema fields, not a place
    // for builder-style indirection.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: SourceIdentityV2,
        source_provenance: SourceProvenance,
        dependencies: DependencyIdentityV2,
        runtime: RuntimeIdentityV2,
        environment: EnvironmentIdentityV2,
        filesystem: FilesystemIdentityV2,
        policy: PolicyIdentityV2,
        launch: LaunchIdentityV2,
        local: Option<LocalExecutionLocator>,
        reproducibility: ReproducibilityIdentity,
    ) -> Self {
        Self {
            schema_version: EXECUTION_IDENTITY_SCHEMA_VERSION_V2_EXPERIMENTAL,
            canonicalization: EXECUTION_IDENTITY_CANONICALIZATION.to_string(),
            hash_algorithm: EXECUTION_IDENTITY_HASH_ALGORITHM.to_string(),
            source,
            source_provenance,
            dependencies,
            runtime,
            environment,
            filesystem,
            policy,
            launch,
            local,
            reproducibility,
        }
    }

    pub fn compute_id(&self) -> Result<ExecutionIdentityDigest> {
        compute_execution_id_v2(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceiptV2 {
    pub schema_version: u32,
    pub execution_id: String,
    pub computed_at: String,
    pub identity: ExecutionIdentityMetadata,
    pub source: SourceIdentityV2,
    pub source_provenance: SourceProvenance,
    pub dependencies: DependencyIdentityV2,
    pub runtime: RuntimeIdentityV2,
    pub environment: EnvironmentIdentityV2,
    pub filesystem: FilesystemIdentityV2,
    pub policy: PolicyIdentityV2,
    pub launch: LaunchIdentityV2,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalExecutionLocator>,
    pub reproducibility: ReproducibilityIdentity,
}

impl ExecutionReceiptV2 {
    pub fn from_input(input: ExecutionIdentityInputV2, computed_at: String) -> Result<Self> {
        let digest = input.compute_id()?;
        Ok(Self {
            schema_version: input.schema_version,
            execution_id: digest.execution_id,
            computed_at,
            identity: ExecutionIdentityMetadata {
                canonicalization: input.canonicalization.clone(),
                hash_algorithm: input.hash_algorithm.clone(),
                input_hash: digest.input_hash,
            },
            source: input.source,
            source_provenance: input.source_provenance,
            dependencies: input.dependencies,
            runtime: input.runtime,
            environment: input.environment,
            filesystem: input.filesystem,
            policy: input.policy,
            launch: input.launch,
            local: input.local,
            reproducibility: input.reproducibility,
        })
    }
}

// Variants differ in size (V1 ~1.1KB, V2 ~2.4KB) but the enum is the
// canonical receipt envelope and is held by-value across many call
// sites that pattern-match `&doc`. Boxing V2 would force a `&**r` /
// `*Box::new(...)` ceremony at every call site (see ato-cli/src/cli/
// commands/inspect.rs and application/execution_replay.rs) for a few
// stack-bytes saved per receipt — not worth the churn.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema", rename_all = "kebab-case")]
pub enum ExecutionReceiptDocument {
    V1(ExecutionReceipt),
    V2(ExecutionReceiptV2),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceiptView {
    pub schema_version: u32,
    pub execution_id: String,
    pub portable: PortableExecutionIdentityView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalExecutionLocator>,
    pub reproducibility: ReproducibilityIdentity,
}

// V2 is already boxed; V1 is left inline because it's the smaller
// variant. Symmetry isn't worth the call-site churn (see the
// ExecutionReceiptDocument note above for the same trade-off).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortableExecutionIdentityView {
    V1(ExecutionIdentityInput),
    V2(Box<ExecutionIdentityInputV2>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentityV2 {
    pub source_tree_hash: Tracked<String>,
    pub manifest_path_role: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceProvenance {
    pub kind: SourceProvenanceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceProvenanceKind {
    Local,
    Git,
    Registry,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyIdentity {
    pub derivation_hash: Tracked<String>,
    pub output_hash: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyIdentityV2 {
    pub derivation_hash: Tracked<String>,
    pub output_hash: Tracked<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation_inputs: Option<DependencyDerivationInputsV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyDerivationInputsV2 {
    pub package_manager: String,
    pub package_manager_version: Tracked<String>,
    pub runtime_resolved_ref: Tracked<String>,
    pub platform_abi: PlatformIdentity,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependency_manifest_digests: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lockfile_digests: BTreeMap<String, String>,
    pub install_command: Vec<String>,
    pub package_manager_config_hash: Tracked<String>,
    pub lifecycle_script_policy_hash: Tracked<String>,
    pub registry_policy_hash: Tracked<String>,
    pub network_policy_hash: Tracked<String>,
    pub environment_allowlist_hash: Tracked<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared_system_build_inputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    pub binary_hash: Tracked<String>,
    pub dynamic_linkage: Tracked<String>,
    pub platform: PlatformIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeIdentityV2 {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared: Option<String>,
    pub resolved_ref: Tracked<String>,
    pub binary_hash: Tracked<String>,
    pub dynamic_linkage: Tracked<String>,
    pub completeness: RuntimeCompleteness,
    pub platform: PlatformIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeCompleteness {
    DeclaredOnly,
    ResolvedBinary,
    BinaryWithDynamicClosure,
    BestEffort,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformIdentity {
    pub os: String,
    pub arch: String,
    pub libc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentIdentity {
    pub closure_hash: Tracked<String>,
    pub mode: EnvironmentMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracked_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redacted_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentIdentityV2 {
    pub entries: Vec<EnvironmentEntry>,
    pub fd_layout: Tracked<FdLayoutIdentity>,
    pub umask: Tracked<String>,
    pub ulimits: Tracked<UlimitIdentity>,
    pub mode: EnvironmentMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambient_untracked_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentEntry {
    pub key: String,
    pub value_hash: Tracked<String>,
    pub normalization: ValueNormalizationStatus,
    #[serde(default = "default_env_origin", skip_serializing, skip_deserializing)]
    pub origin: EnvOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValueNormalizationStatus {
    Normalized,
    NoHostPath,
    SecretReferenceRequired,
    UnnormalizedHostPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FdLayoutIdentity {
    pub stdin: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UlimitIdentity {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub limits: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvironmentMode {
    Closed,
    Partial,
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemIdentity {
    pub view_hash: Tracked<String>,
    pub projection_strategy: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable_dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persistent_state: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known_readonly_layers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemIdentityV2 {
    pub view_hash: Tracked<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_view_hash: Option<String>,
    pub source_root: Tracked<String>,
    pub working_directory: Tracked<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readonly_layers: Vec<ReadonlyLayerIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable_dirs: Vec<WritableDirIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persistent_state: Vec<StateBindingIdentity>,
    pub semantics: FilesystemSemantics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadonlyLayerIdentity {
    pub role: String,
    pub identity: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WritableDirIdentity {
    pub role: String,
    pub lifecycle: WritableDirLifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WritableDirLifecycle {
    SessionLocal,
    PersistentState,
    HostPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateBindingIdentity {
    pub name: String,
    pub kind: StateBindingKind,
    pub identity: Tracked<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateBindingKind {
    ContentSnapshot,
    AtoStateRef,
    HostPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemSemantics {
    pub case_sensitivity: Tracked<CaseSensitivity>,
    pub symlink_policy: Tracked<SymlinkPolicy>,
    pub tmp_policy: Tracked<TmpPolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaseSensitivity {
    Sensitive,
    Insensitive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymlinkPolicy {
    Preserve,
    Resolve,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TmpPolicy {
    SessionLocal,
    HostTmp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyIdentity {
    pub network_policy_hash: Tracked<String>,
    pub capability_policy_hash: Tracked<String>,
    pub sandbox_policy_hash: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyIdentityV2 {
    pub network_policy_hash: Tracked<String>,
    pub capability_policy_hash: Tracked<String>,
    pub sandbox_policy_hash: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchIdentity {
    pub entry_point: String,
    pub argv: Vec<String>,
    pub working_directory: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchIdentityV2 {
    pub entry_point: LaunchEntryPoint,
    pub argv: Vec<LaunchArg>,
    pub working_directory: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum LaunchEntryPoint {
    RuntimeManaged { resolved_ref: String },
    WorkspaceRelative { path: String },
    Command { name: String },
    Untracked { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchArg {
    pub value_hash: Tracked<String>,
    pub normalization: ValueNormalizationStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalExecutionLocator {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_resolved_path: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub state_paths: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_point_raw: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv_raw: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePathCanonicalizer {
    workspace_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum CanonicalPath {
    WorkspaceRoot,
    WorkspaceRelative(String),
    OutsideWorkspace(String),
}

impl WorkspacePathCanonicalizer {
    pub fn new(workspace_root: impl AsRef<str>) -> Self {
        Self {
            workspace_root: normalize_host_path(workspace_root.as_ref()),
        }
    }

    pub fn canonicalize(&self, path: impl AsRef<str>) -> CanonicalPath {
        let path = normalize_host_path(path.as_ref());
        if path == self.workspace_root {
            return CanonicalPath::WorkspaceRoot;
        }
        let prefix = format!("{}/", self.workspace_root);
        if let Some(relative) = path.strip_prefix(&prefix) {
            return CanonicalPath::WorkspaceRelative(relative.to_string());
        }
        CanonicalPath::OutsideWorkspace(path)
    }

    pub fn role_string(&self, path: impl AsRef<str>) -> Tracked<String> {
        match self.canonicalize(path) {
            CanonicalPath::WorkspaceRoot => Tracked::known("workspace:.".to_string()),
            CanonicalPath::WorkspaceRelative(relative) => {
                Tracked::known(format!("workspace:{relative}"))
            }
            CanonicalPath::OutsideWorkspace(_) => Tracked::untracked("path is outside workspace"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRoleNormalizer {
    roles: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedValue {
    pub value: String,
    pub status: ValueNormalizationStatus,
}

impl PathRoleNormalizer {
    pub fn new<I, K, V>(roles: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut roles: Vec<(String, String)> = roles
            .into_iter()
            .map(|(token, path)| (token.into(), normalize_host_path(&path.into())))
            .collect();
        roles.sort_by_key(|(_, root)| std::cmp::Reverse(root.len()));
        Self { roles }
    }

    pub fn normalize_value(&self, value: &str) -> NormalizedValue {
        let mut normalized = normalize_host_path(value);
        let had_host_path = contains_absolute_path_like(&normalized);
        for (token, root) in &self.roles {
            normalized = normalized.replace(root, token);
        }
        let status = if !had_host_path {
            ValueNormalizationStatus::NoHostPath
        } else if contains_absolute_path_like(&normalized) {
            ValueNormalizationStatus::UnnormalizedHostPath
        } else {
            ValueNormalizationStatus::Normalized
        };
        NormalizedValue {
            value: normalized,
            status,
        }
    }

    pub fn tracked_hash(&self, value: &str) -> (Tracked<String>, ValueNormalizationStatus) {
        let normalized = self.normalize_value(value);
        match normalized.status {
            ValueNormalizationStatus::UnnormalizedHostPath => (
                Tracked::untracked("value contains unnormalized host path"),
                normalized.status,
            ),
            _ => (
                Tracked::known(hash_normalized_value(&normalized.value)),
                normalized.status,
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproducibilityIdentity {
    pub class: ReproducibilityClass,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes: Vec<ReproducibilityCause>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReproducibilityClass {
    Pure,
    HostBound,
    StateBound,
    TimeBound,
    NetworkBound,
    BestEffort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReproducibilityCause {
    HostBound,
    StateBound,
    TimeBound,
    NetworkBound,
    UnknownDependencyOutput,
    UnknownRuntimeIdentity,
    UntrackedEnvironment,
    UntrackedFilesystemView,
    UntrackedDynamicDependency,
    LifecycleUnknown,
}

#[derive(Serialize)]
struct IdentityProjection<'a> {
    schema_version: u32,
    canonicalization: &'a str,
    hash_algorithm: &'a str,
    source: SourceProjection<'a>,
    dependencies: DependencyProjection<'a>,
    runtime: RuntimeProjection<'a>,
    environment: EnvironmentProjection<'a>,
    filesystem: FilesystemProjection<'a>,
    policy: PolicyProjection<'a>,
    launch: &'a LaunchIdentity,
}

#[derive(Serialize)]
struct SourceProjection<'a> {
    source_ref: TrackedProjection<'a, String>,
    source_tree_hash: TrackedProjection<'a, String>,
}

#[derive(Serialize)]
struct DependencyProjection<'a> {
    derivation_hash: TrackedProjection<'a, String>,
    output_hash: TrackedProjection<'a, String>,
}

#[derive(Serialize)]
struct RuntimeProjection<'a> {
    declared: &'a Option<String>,
    resolved: &'a Option<String>,
    binary_hash: TrackedProjection<'a, String>,
    dynamic_linkage: TrackedProjection<'a, String>,
    platform: &'a PlatformIdentity,
}

#[derive(Serialize)]
struct EnvironmentProjection<'a> {
    closure_hash: TrackedProjection<'a, String>,
    mode: EnvironmentMode,
    tracked_keys: &'a [String],
    redacted_keys: &'a [String],
    unknown_keys: &'a [String],
}

#[derive(Serialize)]
struct FilesystemProjection<'a> {
    view_hash: TrackedProjection<'a, String>,
    projection_strategy: &'a str,
    writable_dirs: &'a [String],
    persistent_state: &'a [String],
    known_readonly_layers: &'a [String],
}

#[derive(Serialize)]
struct PolicyProjection<'a> {
    network_policy_hash: TrackedProjection<'a, String>,
    capability_policy_hash: TrackedProjection<'a, String>,
    sandbox_policy_hash: TrackedProjection<'a, String>,
}

#[derive(Serialize)]
struct IdentityProjectionV2<'a> {
    schema_version: u32,
    canonicalization: &'a str,
    hash_algorithm: &'a str,
    source: SourceProjectionV2<'a>,
    dependencies: DependencyProjectionV2<'a>,
    runtime: RuntimeProjectionV2<'a>,
    environment: EnvironmentProjectionV2<'a>,
    filesystem: FilesystemProjectionV2<'a>,
    policy: PolicyProjectionV2<'a>,
    launch: LaunchProjectionV2<'a>,
}

#[derive(Serialize)]
struct SourceProjectionV2<'a> {
    source_tree_hash: TrackedProjection<'a, String>,
    manifest_path_role: TrackedProjection<'a, String>,
}

#[derive(Serialize)]
struct DependencyProjectionV2<'a> {
    derivation_hash: TrackedProjection<'a, String>,
    output_hash: TrackedProjection<'a, String>,
    derivation_inputs: &'a Option<DependencyDerivationInputsV2>,
}

#[derive(Serialize)]
struct RuntimeProjectionV2<'a> {
    declared: &'a Option<String>,
    resolved_ref: TrackedProjection<'a, String>,
    binary_hash: TrackedProjection<'a, String>,
    dynamic_linkage: TrackedProjection<'a, String>,
    completeness: RuntimeCompleteness,
    platform: &'a PlatformIdentity,
}

#[derive(Serialize)]
struct EnvironmentProjectionV2<'a> {
    entries: &'a [EnvironmentEntry],
    fd_layout: TrackedProjection<'a, FdLayoutIdentity>,
    umask: TrackedProjection<'a, String>,
    ulimits: TrackedProjection<'a, UlimitIdentity>,
    mode: EnvironmentMode,
}

#[derive(Serialize)]
struct FilesystemProjectionV2<'a> {
    view_hash: TrackedProjection<'a, String>,
    source_root: TrackedProjection<'a, String>,
    working_directory: TrackedProjection<'a, String>,
    readonly_layers: &'a [ReadonlyLayerIdentity],
    writable_dirs: &'a [WritableDirIdentity],
    persistent_state: &'a [StateBindingIdentity],
    semantics: FilesystemSemanticsProjection<'a>,
}

#[derive(Serialize)]
struct FilesystemSemanticsProjection<'a> {
    case_sensitivity: TrackedProjection<'a, CaseSensitivity>,
    symlink_policy: TrackedProjection<'a, SymlinkPolicy>,
    tmp_policy: TrackedProjection<'a, TmpPolicy>,
}

#[derive(Serialize)]
struct PolicyProjectionV2<'a> {
    network_policy_hash: TrackedProjection<'a, String>,
    capability_policy_hash: TrackedProjection<'a, String>,
    sandbox_policy_hash: TrackedProjection<'a, String>,
}

#[derive(Serialize)]
struct LaunchProjectionV2<'a> {
    entry_point: LaunchEntryPointProjection<'a>,
    argv: &'a [LaunchArg],
    working_directory: TrackedProjection<'a, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
enum LaunchEntryPointProjection<'a> {
    RuntimeManaged { resolved_ref: &'a str },
    WorkspaceRelative { path: &'a str },
    Command { name: &'a str },
    Untracked { gap: &'static str },
}

#[derive(Serialize)]
struct TrackedProjection<'a, T> {
    status: TrackingStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<&'a T>,
}

impl<'a, T> From<&'a Tracked<T>> for TrackedProjection<'a, T> {
    fn from(value: &'a Tracked<T>) -> Self {
        Self {
            status: value.status,
            value: value.value.as_ref(),
        }
    }
}

pub fn compute_execution_id(input: &ExecutionIdentityInput) -> Result<ExecutionIdentityDigest> {
    validate_identity_header(input)?;
    let projection = identity_projection(input);
    let canonical = serde_jcs::to_vec(&projection).map_err(|err| {
        CapsuleError::Config(format!(
            "Failed to canonicalize execution identity input: {err}"
        ))
    })?;
    let digest = format!("blake3:{}", blake3::hash(&canonical).to_hex());
    Ok(ExecutionIdentityDigest {
        execution_id: digest.clone(),
        input_hash: digest,
    })
}

pub fn compute_execution_id_v2(
    input: &ExecutionIdentityInputV2,
) -> Result<ExecutionIdentityDigest> {
    validate_identity_header_v2(input)?;
    let projection = identity_projection_v2(input);
    let canonical = serde_jcs::to_vec(&projection).map_err(|err| {
        CapsuleError::Config(format!(
            "Failed to canonicalize execution identity v2 input: {err}"
        ))
    })?;
    let digest = format!("blake3:{}", blake3::hash(&canonical).to_hex());
    Ok(ExecutionIdentityDigest {
        execution_id: digest.clone(),
        input_hash: digest,
    })
}

fn validate_identity_header(input: &ExecutionIdentityInput) -> Result<()> {
    if input.schema_version != EXECUTION_IDENTITY_SCHEMA_VERSION {
        return Err(CapsuleError::Config(format!(
            "unsupported execution identity schema_version {}; expected {}",
            input.schema_version, EXECUTION_IDENTITY_SCHEMA_VERSION
        )));
    }
    if input.canonicalization != EXECUTION_IDENTITY_CANONICALIZATION {
        return Err(CapsuleError::Config(format!(
            "unsupported execution identity canonicalization {}; expected {}",
            input.canonicalization, EXECUTION_IDENTITY_CANONICALIZATION
        )));
    }
    if input.hash_algorithm != EXECUTION_IDENTITY_HASH_ALGORITHM {
        return Err(CapsuleError::Config(format!(
            "unsupported execution identity hash_algorithm {}; expected {}",
            input.hash_algorithm, EXECUTION_IDENTITY_HASH_ALGORITHM
        )));
    }
    Ok(())
}

fn validate_identity_header_v2(input: &ExecutionIdentityInputV2) -> Result<()> {
    if input.schema_version != EXECUTION_IDENTITY_SCHEMA_VERSION_V2_EXPERIMENTAL {
        return Err(CapsuleError::Config(format!(
            "unsupported execution identity v2 schema_version {}; expected {}",
            input.schema_version, EXECUTION_IDENTITY_SCHEMA_VERSION_V2_EXPERIMENTAL
        )));
    }
    if input.canonicalization != EXECUTION_IDENTITY_CANONICALIZATION {
        return Err(CapsuleError::Config(format!(
            "unsupported execution identity v2 canonicalization {}; expected {}",
            input.canonicalization, EXECUTION_IDENTITY_CANONICALIZATION
        )));
    }
    if input.hash_algorithm != EXECUTION_IDENTITY_HASH_ALGORITHM {
        return Err(CapsuleError::Config(format!(
            "unsupported execution identity v2 hash_algorithm {}; expected {}",
            input.hash_algorithm, EXECUTION_IDENTITY_HASH_ALGORITHM
        )));
    }
    Ok(())
}

fn identity_projection(input: &ExecutionIdentityInput) -> IdentityProjection<'_> {
    IdentityProjection {
        schema_version: input.schema_version,
        canonicalization: input.canonicalization.as_str(),
        hash_algorithm: input.hash_algorithm.as_str(),
        source: SourceProjection {
            source_ref: (&input.source.source_ref).into(),
            source_tree_hash: (&input.source.source_tree_hash).into(),
        },
        dependencies: DependencyProjection {
            derivation_hash: (&input.dependencies.derivation_hash).into(),
            output_hash: (&input.dependencies.output_hash).into(),
        },
        runtime: RuntimeProjection {
            declared: &input.runtime.declared,
            resolved: &input.runtime.resolved,
            binary_hash: (&input.runtime.binary_hash).into(),
            dynamic_linkage: (&input.runtime.dynamic_linkage).into(),
            platform: &input.runtime.platform,
        },
        environment: EnvironmentProjection {
            closure_hash: (&input.environment.closure_hash).into(),
            mode: input.environment.mode,
            tracked_keys: &input.environment.tracked_keys,
            redacted_keys: &input.environment.redacted_keys,
            unknown_keys: &input.environment.unknown_keys,
        },
        filesystem: FilesystemProjection {
            view_hash: (&input.filesystem.view_hash).into(),
            projection_strategy: input.filesystem.projection_strategy.as_str(),
            writable_dirs: &input.filesystem.writable_dirs,
            persistent_state: &input.filesystem.persistent_state,
            known_readonly_layers: &input.filesystem.known_readonly_layers,
        },
        policy: PolicyProjection {
            network_policy_hash: (&input.policy.network_policy_hash).into(),
            capability_policy_hash: (&input.policy.capability_policy_hash).into(),
            sandbox_policy_hash: (&input.policy.sandbox_policy_hash).into(),
        },
        launch: &input.launch,
    }
}

fn identity_projection_v2(input: &ExecutionIdentityInputV2) -> IdentityProjectionV2<'_> {
    IdentityProjectionV2 {
        schema_version: input.schema_version,
        canonicalization: input.canonicalization.as_str(),
        hash_algorithm: input.hash_algorithm.as_str(),
        source: SourceProjectionV2 {
            source_tree_hash: (&input.source.source_tree_hash).into(),
            manifest_path_role: (&input.source.manifest_path_role).into(),
        },
        dependencies: DependencyProjectionV2 {
            derivation_hash: (&input.dependencies.derivation_hash).into(),
            output_hash: (&input.dependencies.output_hash).into(),
            derivation_inputs: &input.dependencies.derivation_inputs,
        },
        runtime: RuntimeProjectionV2 {
            declared: &input.runtime.declared,
            resolved_ref: (&input.runtime.resolved_ref).into(),
            binary_hash: (&input.runtime.binary_hash).into(),
            dynamic_linkage: (&input.runtime.dynamic_linkage).into(),
            completeness: input.runtime.completeness,
            platform: &input.runtime.platform,
        },
        environment: EnvironmentProjectionV2 {
            entries: &input.environment.entries,
            fd_layout: (&input.environment.fd_layout).into(),
            umask: (&input.environment.umask).into(),
            ulimits: (&input.environment.ulimits).into(),
            mode: input.environment.mode,
        },
        filesystem: FilesystemProjectionV2 {
            view_hash: (&input.filesystem.view_hash).into(),
            source_root: (&input.filesystem.source_root).into(),
            working_directory: (&input.filesystem.working_directory).into(),
            readonly_layers: &input.filesystem.readonly_layers,
            writable_dirs: &input.filesystem.writable_dirs,
            persistent_state: &input.filesystem.persistent_state,
            semantics: FilesystemSemanticsProjection {
                case_sensitivity: (&input.filesystem.semantics.case_sensitivity).into(),
                symlink_policy: (&input.filesystem.semantics.symlink_policy).into(),
                tmp_policy: (&input.filesystem.semantics.tmp_policy).into(),
            },
        },
        policy: PolicyProjectionV2 {
            network_policy_hash: (&input.policy.network_policy_hash).into(),
            capability_policy_hash: (&input.policy.capability_policy_hash).into(),
            sandbox_policy_hash: (&input.policy.sandbox_policy_hash).into(),
        },
        launch: LaunchProjectionV2 {
            entry_point: (&input.launch.entry_point).into(),
            argv: &input.launch.argv,
            working_directory: (&input.launch.working_directory).into(),
        },
    }
}

impl<'a> From<&'a LaunchEntryPoint> for LaunchEntryPointProjection<'a> {
    fn from(value: &'a LaunchEntryPoint) -> Self {
        match value {
            LaunchEntryPoint::RuntimeManaged { resolved_ref } => {
                Self::RuntimeManaged { resolved_ref }
            }
            LaunchEntryPoint::WorkspaceRelative { path } => Self::WorkspaceRelative { path },
            LaunchEntryPoint::Command { name } => Self::Command { name },
            LaunchEntryPoint::Untracked { .. } => Self::Untracked {
                gap: "untracked-entry-point",
            },
        }
    }
}

fn normalize_host_path(value: &str) -> String {
    let mut normalized = value.replace('\\', "/");
    if let Some(stripped) = normalized.strip_prefix("//?/") {
        normalized = stripped.to_string();
    }
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

fn contains_absolute_path_like(value: &str) -> bool {
    let bytes = value.as_bytes();
    if value.starts_with('/') || value.starts_with("//") {
        return true;
    }
    bytes
        .windows(3)
        .any(|window| window[0].is_ascii_alphabetic() && window[1] == b':' && window[2] == b'/')
        || value.contains(":/")
        || value.contains(";//")
        || value.contains("://")
}

fn hash_normalized_value(value: &str) -> String {
    format!("blake3:{}", blake3::hash(value.as_bytes()).to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_input() -> ExecutionIdentityInput {
        ExecutionIdentityInput::new(
            SourceIdentity {
                source_ref: Tracked::known("github.com/acme/app@abc123".to_string()),
                source_tree_hash: Tracked::known("blake3:source".to_string()),
            },
            DependencyIdentity {
                derivation_hash: Tracked::unknown("dependency derivation observer not enabled"),
                output_hash: Tracked::unknown("dependency output not observed"),
            },
            RuntimeIdentity {
                declared: Some("node@20".to_string()),
                resolved: Some("node@20.10.0".to_string()),
                binary_hash: Tracked::unknown("runtime binary hash not observed"),
                dynamic_linkage: Tracked::untracked("not implemented"),
                platform: PlatformIdentity {
                    os: "macos".to_string(),
                    arch: "aarch64".to_string(),
                    libc: "unknown".to_string(),
                },
            },
            EnvironmentIdentity {
                closure_hash: Tracked::known("blake3:env".to_string()),
                mode: EnvironmentMode::Closed,
                tracked_keys: vec!["LANG".to_string(), "PATH".to_string()],
                redacted_keys: vec!["OPENAI_API_KEY".to_string()],
                unknown_keys: Vec::new(),
            },
            FilesystemIdentity {
                view_hash: Tracked::known("blake3:fs".to_string()),
                projection_strategy: "direct".to_string(),
                writable_dirs: Vec::new(),
                persistent_state: Vec::new(),
                known_readonly_layers: Vec::new(),
            },
            PolicyIdentity {
                network_policy_hash: Tracked::known("blake3:network".to_string()),
                capability_policy_hash: Tracked::known("blake3:capability".to_string()),
                sandbox_policy_hash: Tracked::known("blake3:sandbox".to_string()),
            },
            LaunchIdentity {
                entry_point: "npm".to_string(),
                argv: vec!["run".to_string(), "dev".to_string()],
                working_directory: "/app".to_string(),
            },
            ReproducibilityIdentity {
                class: ReproducibilityClass::BestEffort,
                causes: vec![
                    ReproducibilityCause::UnknownDependencyOutput,
                    ReproducibilityCause::UnknownRuntimeIdentity,
                ],
            },
        )
    }

    fn sample_input_v2() -> ExecutionIdentityInputV2 {
        let normalizer = PathRoleNormalizer::new([
            ("${WORKSPACE}", "/Users/alice/proj"),
            ("${ATO_HOME}", "/Users/alice/.ato"),
            ("${ATO_RUNTIME}", "/Users/alice/.ato/runtimes"),
        ]);
        let (path_hash, path_status) = normalizer.tracked_hash("/Users/alice/proj/config/app.toml");

        ExecutionIdentityInputV2::new(
            SourceIdentityV2 {
                source_tree_hash: Tracked::known("blake3:source".to_string()),
                manifest_path_role: Tracked::known("workspace:capsule.toml".to_string()),
            },
            SourceProvenance {
                kind: SourceProvenanceKind::Local,
                git_remote: None,
                git_commit: None,
                registry_ref: None,
            },
            DependencyIdentityV2 {
                derivation_hash: Tracked::not_applicable(),
                output_hash: Tracked::not_applicable(),
                derivation_inputs: None,
            },
            RuntimeIdentityV2 {
                declared: Some("node@20".to_string()),
                resolved_ref: Tracked::known("node@20.10.0".to_string()),
                binary_hash: Tracked::known("blake3:runtime".to_string()),
                dynamic_linkage: Tracked::known("blake3:dyn".to_string()),
                completeness: RuntimeCompleteness::BinaryWithDynamicClosure,
                platform: PlatformIdentity {
                    os: "macos".to_string(),
                    arch: "aarch64".to_string(),
                    libc: "unknown".to_string(),
                },
            },
            EnvironmentIdentityV2 {
                entries: vec![EnvironmentEntry {
                    key: "CONFIG".to_string(),
                    value_hash: path_hash,
                    normalization: path_status,
                    origin: EnvOrigin::ManifestStatic,
                }],
                fd_layout: Tracked::known(FdLayoutIdentity {
                    stdin: "inherited".to_string(),
                    stdout: "inherited".to_string(),
                    stderr: "inherited".to_string(),
                }),
                umask: Tracked::known("022".to_string()),
                ulimits: Tracked::known(UlimitIdentity {
                    limits: BTreeMap::new(),
                }),
                mode: EnvironmentMode::Closed,
                ambient_untracked_keys: vec!["SHELL".to_string()],
            },
            FilesystemIdentityV2 {
                view_hash: Tracked::known("blake3:fs".to_string()),
                partial_view_hash: Some("blake3:diagnostic".to_string()),
                source_root: Tracked::known("workspace:.".to_string()),
                working_directory: Tracked::known("workspace:.".to_string()),
                readonly_layers: vec![ReadonlyLayerIdentity {
                    role: "source".to_string(),
                    identity: Tracked::known("blake3:source".to_string()),
                }],
                writable_dirs: vec![WritableDirIdentity {
                    role: "tmp".to_string(),
                    lifecycle: WritableDirLifecycle::SessionLocal,
                }],
                persistent_state: Vec::new(),
                semantics: FilesystemSemantics {
                    case_sensitivity: Tracked::known(CaseSensitivity::Sensitive),
                    symlink_policy: Tracked::known(SymlinkPolicy::Preserve),
                    tmp_policy: Tracked::known(TmpPolicy::SessionLocal),
                },
            },
            PolicyIdentityV2 {
                network_policy_hash: Tracked::known("blake3:network".to_string()),
                capability_policy_hash: Tracked::known("blake3:capability".to_string()),
                sandbox_policy_hash: Tracked::known("blake3:sandbox".to_string()),
            },
            LaunchIdentityV2 {
                entry_point: LaunchEntryPoint::Command {
                    name: "node".to_string(),
                },
                argv: vec![LaunchArg {
                    value_hash: Tracked::known(hash_normalized_value("server.js")),
                    normalization: ValueNormalizationStatus::NoHostPath,
                }],
                working_directory: Tracked::known("workspace:.".to_string()),
            },
            Some(LocalExecutionLocator {
                manifest_path: Some("/Users/alice/proj/capsule.toml".to_string()),
                workspace_root: Some("/Users/alice/proj".to_string()),
                working_directory_path: Some("/Users/alice/proj".to_string()),
                runtime_resolved_path: Some("/Users/alice/.ato/runtimes/node/bin/node".to_string()),
                state_paths: BTreeMap::new(),
                entry_point_raw: Some("/Users/alice/.ato/runtimes/node/bin/node".to_string()),
                argv_raw: vec!["server.js".to_string()],
            }),
            ReproducibilityIdentity {
                class: ReproducibilityClass::Pure,
                causes: Vec::new(),
            },
        )
    }

    #[test]
    fn execution_id_is_stable_for_identical_inputs() {
        let left = sample_input().compute_id().expect("left id").execution_id;
        let right = sample_input().compute_id().expect("right id").execution_id;
        assert_eq!(left, right);
        assert!(left.starts_with("blake3:"));
    }

    #[test]
    fn execution_id_changes_when_launch_argv_changes() {
        let before = sample_input().compute_id().expect("before id").execution_id;
        let mut input = sample_input();
        input.launch.argv.push("--port=3000".to_string());
        let after = input.compute_id().expect("after id").execution_id;
        assert_ne!(before, after);
    }

    #[test]
    fn execution_identity_drift_matrix_covers_launch_envelope_components() {
        type Perturbation = Box<dyn Fn(&mut ExecutionIdentityInput)>;
        let baseline = sample_input().compute_id().expect("baseline").execution_id;
        let mut perturbations: Vec<(&str, Perturbation)> = vec![
            (
                "source",
                Box::new(|input| {
                    input.source.source_tree_hash = Tracked::known("blake3:source2".to_string());
                }),
            ),
            (
                "dependencies",
                Box::new(|input| {
                    input.dependencies.output_hash = Tracked::known("blake3:deps2".to_string());
                }),
            ),
            (
                "runtime",
                Box::new(|input| {
                    input.runtime.binary_hash = Tracked::known("blake3:runtime2".to_string());
                }),
            ),
            (
                "environment",
                Box::new(|input| {
                    input.environment.closure_hash = Tracked::known("blake3:env2".to_string());
                }),
            ),
            (
                "filesystem",
                Box::new(|input| {
                    input.filesystem.view_hash = Tracked::known("blake3:fs2".to_string());
                }),
            ),
            (
                "policy",
                Box::new(|input| {
                    input.policy.network_policy_hash =
                        Tracked::known("blake3:network2".to_string());
                }),
            ),
            (
                "launch",
                Box::new(|input| {
                    input.launch.working_directory = "/different".to_string();
                }),
            ),
        ];

        for (component, perturb) in perturbations.drain(..) {
            let mut input = sample_input();
            perturb(&mut input);
            let changed = input.compute_id().expect(component).execution_id;
            assert_ne!(
                baseline, changed,
                "{component} drift must change execution_id"
            );
        }
    }

    #[test]
    fn execution_id_changes_when_tracking_status_changes() {
        let before = sample_input().compute_id().expect("before id").execution_id;
        let mut input = sample_input();
        input.dependencies.output_hash = Tracked::untracked("not in scope");
        let after = input.compute_id().expect("after id").execution_id;
        assert_ne!(before, after);
    }

    #[test]
    fn execution_id_ignores_tracking_reason_text() {
        let before = sample_input().compute_id().expect("before id").execution_id;
        let mut input = sample_input();
        input.dependencies.output_hash =
            Tracked::unknown("different wording for the same missing observation");
        let after = input.compute_id().expect("after id").execution_id;
        assert_eq!(before, after);
    }

    #[test]
    fn execution_id_ignores_reproducibility_classification_metadata() {
        let before = sample_input().compute_id().expect("before id").execution_id;
        let mut input = sample_input();
        input.reproducibility = ReproducibilityIdentity {
            class: ReproducibilityClass::Pure,
            causes: Vec::new(),
        };
        let after = input.compute_id().expect("after id").execution_id;
        assert_eq!(before, after);
    }

    #[test]
    fn v2_local_locator_does_not_affect_execution_id() {
        let before = sample_input_v2().compute_id().expect("before").execution_id;
        let mut input = sample_input_v2();
        input.local = Some(LocalExecutionLocator {
            manifest_path: Some("/home/bob/proj/capsule.toml".to_string()),
            workspace_root: Some("/home/bob/proj".to_string()),
            working_directory_path: Some("/home/bob/proj".to_string()),
            runtime_resolved_path: Some("/opt/ato/runtimes/node/bin/node".to_string()),
            state_paths: BTreeMap::from([(
                "data".to_string(),
                "/home/bob/.ato/state/data".to_string(),
            )]),
            entry_point_raw: Some("/opt/ato/runtimes/node/bin/node".to_string()),
            argv_raw: vec!["server.js".to_string()],
        });
        let after = input.compute_id().expect("after").execution_id;
        assert_eq!(before, after);
    }

    #[test]
    fn v2_source_provenance_does_not_affect_execution_id() {
        let before = sample_input_v2().compute_id().expect("before").execution_id;
        let mut input = sample_input_v2();
        input.source_provenance = SourceProvenance {
            kind: SourceProvenanceKind::Git,
            git_remote: Some("https://example.com/acme/app.git".to_string()),
            git_commit: Some("deadbeef".to_string()),
            registry_ref: None,
        };
        let after = input.compute_id().expect("after").execution_id;
        assert_eq!(before, after);
    }

    #[test]
    fn v2_filesystem_partial_hash_is_diagnostic_only() {
        let before = sample_input_v2().compute_id().expect("before").execution_id;
        let mut input = sample_input_v2();
        input.filesystem.partial_view_hash = Some("blake3:other-diagnostic".to_string());
        let after = input.compute_id().expect("after").execution_id;
        assert_eq!(before, after);
    }

    #[test]
    fn v2_launch_untracked_reason_text_is_not_hashed() {
        let mut before_input = sample_input_v2();
        before_input.launch.entry_point = LaunchEntryPoint::Untracked {
            reason: "absolute path outside workspace".to_string(),
        };
        let before = before_input.compute_id().expect("before").execution_id;
        let mut after_input = sample_input_v2();
        after_input.launch.entry_point = LaunchEntryPoint::Untracked {
            reason: "different operator wording".to_string(),
        };
        let after = after_input.compute_id().expect("after").execution_id;
        assert_eq!(before, after);
    }

    #[test]
    fn v2_policy_identity_changes_execution_id() {
        let before = sample_input_v2().compute_id().expect("before").execution_id;
        let mut input = sample_input_v2();
        input.policy.sandbox_policy_hash = Tracked::known("blake3:sandbox2".to_string());
        let after = input.compute_id().expect("after").execution_id;
        assert_ne!(before, after);
    }

    #[test]
    fn workspace_path_canonicalizer_removes_unix_and_windows_roots() {
        let unix = WorkspacePathCanonicalizer::new("/Users/alice/proj");
        assert_eq!(
            unix.role_string("/Users/alice/proj/backend")
                .value
                .as_deref(),
            Some("workspace:backend")
        );

        let windows = WorkspacePathCanonicalizer::new(r"C:\Users\alice\proj");
        assert_eq!(
            windows
                .role_string(r"C:\Users\alice\proj\backend")
                .value
                .as_deref(),
            Some("workspace:backend")
        );
    }

    #[test]
    fn path_role_normalizer_hashes_role_tokens_not_host_roots() {
        let alice = PathRoleNormalizer::new([("${WORKSPACE}", "/Users/alice/proj")]);
        let bob = PathRoleNormalizer::new([("${WORKSPACE}", "/home/bob/proj")]);
        let (alice_hash, alice_status) = alice.tracked_hash("/Users/alice/proj/config.toml");
        let (bob_hash, bob_status) = bob.tracked_hash("/home/bob/proj/config.toml");

        assert_eq!(alice_status, ValueNormalizationStatus::Normalized);
        assert_eq!(bob_status, ValueNormalizationStatus::Normalized);
        assert_eq!(alice_hash, bob_hash);
    }

    #[test]
    fn path_role_normalizer_detects_unnormalized_host_paths() {
        let normalizer = PathRoleNormalizer::new([("${WORKSPACE}", "/Users/alice/proj")]);
        let (value_hash, status) = normalizer.tracked_hash("/private/other/config.toml");
        assert_eq!(status, ValueNormalizationStatus::UnnormalizedHostPath);
        assert_eq!(value_hash.status, TrackingStatus::Untracked);
    }

    #[test]
    fn receipt_preserves_reason_metadata() {
        let receipt = ExecutionReceipt::from_input(sample_input(), "2026-05-03T00:00:00Z".into())
            .expect("receipt");
        assert_eq!(receipt.schema_version, EXECUTION_IDENTITY_SCHEMA_VERSION);
        assert_eq!(receipt.execution_id, receipt.identity.input_hash);
        assert_eq!(
            receipt.dependencies.output_hash.reason.as_deref(),
            Some("dependency output not observed")
        );
    }
}

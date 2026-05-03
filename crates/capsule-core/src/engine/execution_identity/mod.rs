use serde::{Deserialize, Serialize};

use crate::error::{CapsuleError, Result};

pub const EXECUTION_IDENTITY_SCHEMA_VERSION: u32 = 1;
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
pub struct DependencyIdentity {
    pub derivation_hash: Tracked<String>,
    pub output_hash: Tracked<String>,
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
pub struct PolicyIdentity {
    pub network_policy_hash: Tracked<String>,
    pub capability_policy_hash: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchIdentity {
    pub entry_point: String,
    pub argv: Vec<String>,
    pub working_directory: String,
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
    Bounded,
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
        },
        launch: &input.launch,
    }
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

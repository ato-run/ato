//! Installed-app lifecycle layer.
//!
//! # Modules
//!
//! - [`ids`]: typed identifiers (`InstalledAppId`, `ProfileId`, `InstallProfileKey`, …)
//! - [`store`]: [`InstallInstanceStore`] — filesystem layout for instances and revisions
//! - [`finalizer`]: [`InstallRevisionFinalizer`] — promotes producer output into a revision
//! - [`launch_inputs`]: [`ValidatedInstallReusableInputs`] — read-validates finalized
//!   records into a safe input boundary for a future launch-template wave

pub mod finalizer;
pub mod hashing;
pub mod ids;
pub mod launch_inputs;
pub mod launch_materialization_builder;
pub mod launch_reuse;
pub mod launch_template;
pub mod launch_template_builder;
pub mod launch_template_reuse;
pub mod materialization;
pub mod records;
pub mod requirement_graph;
pub mod store;

pub use finalizer::{FinalizerInput, FinalizerOutput, InstallBuildFacts, InstallRevisionFinalizer};
pub use hashing::canonical_hash;
pub use ids::{
    ArtifactBuildId, CapsuleInstanceKey, ExecutionId, InstallProfileKey, InstallRevisionId,
    InstalledAppId, ProfileId, derive_capsule_instance_key, derive_install_profile_key,
    path_safe_app_id, revision_id_for_build,
};
pub use launch_inputs::{
    InstallReusableInputValidationError, LaunchTemplateReadiness, LaunchTemplateReadinessReason,
    ValidatedInstallReusableInputs,
};
pub use launch_materialization_builder::{
    LaunchMaterializationBuildError, LaunchMaterializationBuildInput,
    LaunchMaterializationBuildOutput, build_launch_materialization,
};
pub use launch_reuse::{
    LaunchReuseDecision, LaunchReuseInputs, RevalidationFailure, RevalidationFailureKind,
    RevalidationOutcome, VolatileRevalidation, evaluate_launch_reuse,
};
pub use launch_template::{
    BindingAssignmentSet, BindingAssignmentSource, CompatibilityIndex,
    CompatibilityIndexCompleteness, CompatibilityIndexCompletenessReason, LaunchTemplate,
    LaunchTemplateIntegrityError, LaunchTemplateKey, LaunchTemplateKeyInputs, RequirementBinding,
    RequirementBindingKind, RunnerClass, RunnerCompatibilityClass,
    RunnerCompatibilityClassParseError,
};
pub use launch_template_builder::{
    LaunchTemplateBuildError, LaunchTemplateBuildInput, LaunchTemplateBuildOutput,
    build_launch_template,
};
pub use launch_template_reuse::{
    LaunchTemplateReuseInput, PersistedLaunchTemplateReuseBlocker,
    PersistedLaunchTemplateReuseDecision, evaluate_persisted_launch_template_reuse,
    validate_launch_template_for_reuse,
};
pub use materialization::{
    LaunchMaterializationRecord, ProjectionDigest, ProjectionDigestInvalidReason,
};
pub use records::{
    ArtifactBuild, ArtifactBuildIdentityInputs, InstallReceipt, InstallRevision, RequirementGraph,
    RequirementGraphCompleteness, RequirementGraphCompletenessPolicy,
    RequirementGraphCompletenessReason, RequirementGraphEdge, RequirementGraphNode,
    RequirementGraphSnapshot, RequirementGraphSnapshotHash, RequirementGraphSnapshotIdentityError,
    RequirementKind, RequirementRelation, StateContractSnapshot, combined_state_contract_hash,
    compute_requirement_graph_snapshot_hash, derive_artifact_build_id,
};
pub use requirement_graph::{
    ManifestRequirementFacts, NetworkRequirementFact, NormalizedProfile,
    RequirementGraphCompileInput, RequirementGraphCompileOutput, RuntimeRequirementFact,
    SecretRequirementFact, StorageRequirementFact, compile_requirement_graph,
};
pub use store::{AppRecord, InstallInstanceStore, LaunchProfile};

//! Installed-app lifecycle layer.
//!
//! # Modules
//!
//! - [`ids`]: typed identifiers (`InstalledAppId`, `ProfileId`, `InstallProfileKey`, …)
//! - [`store`]: [`InstallInstanceStore`] — filesystem layout for instances and revisions
//! - [`finalizer`]: [`InstallRevisionFinalizer`] — promotes producer output into a revision

pub mod finalizer;
pub mod hashing;
pub mod ids;
pub mod launch_reuse;
pub mod launch_template;
pub mod materialization;
pub mod records;
pub mod store;

pub use finalizer::{FinalizerInput, FinalizerOutput, InstallRevisionFinalizer};
pub use hashing::canonical_hash;
pub use ids::{
    ArtifactBuildId, CapsuleInstanceKey, ExecutionId, InstallProfileKey, InstallRevisionId,
    InstalledAppId, ProfileId, derive_capsule_instance_key, derive_install_profile_key,
    path_safe_app_id, revision_id_for_build,
};
pub use launch_reuse::{
    LaunchReuseDecision, LaunchReuseInputs, RevalidationFailure, RevalidationFailureKind,
    RevalidationOutcome, VolatileRevalidation, evaluate_launch_reuse,
};
pub use launch_template::{
    BindingAssignmentSet, BindingAssignmentSource, CompatibilityIndex, LaunchTemplate,
    LaunchTemplateKey, RequirementBinding, RequirementBindingKind, RunnerClass,
    RunnerCompatibilityClass,
};
pub use materialization::{LaunchMaterializationRecord, ProjectionDigest};
pub use records::{
    ArtifactBuild, ArtifactBuildIdentityInputs, InstallReceipt, InstallRevision, RequirementGraph,
    RequirementGraphEdge, RequirementGraphNode, RequirementGraphSnapshot, RequirementKind,
    RequirementRelation, StateContractSnapshot, combined_state_contract_hash,
    derive_artifact_build_id,
};
pub use store::{AppRecord, InstallInstanceStore, LaunchProfile};

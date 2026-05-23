//! Installed-app lifecycle layer.
//!
//! # Modules
//!
//! - [`ids`]: typed identifiers (`InstalledAppId`, `ProfileId`, `InstallProfileKey`, …)
//! - [`store`]: [`InstallInstanceStore`] — filesystem layout for instances and revisions
//! - [`finalizer`]: [`InstallRevisionFinalizer`] — promotes producer output into a revision

pub mod finalizer;
pub mod ids;
pub mod store;

pub use finalizer::{FinalizerInput, FinalizerOutput, InstallRevisionFinalizer};
pub use ids::{
    derive_capsule_instance_key, derive_install_profile_key, ArtifactBuildId, CapsuleInstanceKey,
    ExecutionId, InstallProfileKey, InstallRevisionId, InstalledAppId, ProfileId,
};
pub use store::{AppRecord, InstallInstanceStore, LaunchProfile};

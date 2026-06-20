//! Cross-device placement index — first minimal slice (umbrella #502, issue
//! #509; Capsule Core Model RFC).
//!
//! This module answers one question fast:
//!
//! ```text
//! capsule requirements + redacted provider snapshots  =>  candidate providers
//! ```
//!
//! It is a **fast map, not an authority.** A query narrows the field to
//! candidate providers and explains every rejection with a typed reason, but a
//! candidate is *not* an admission. Every selected candidate carries
//! `requires_final_local_admission: true`; the chosen provider's local
//! installed-state DB performs the real admission/reservation later. The
//! two-phase contract is:
//!
//! ```text
//! 1. Cross-device index narrows candidates.        (this module)
//! 2. Selected provider's local Installed-State DB   (#508)
//!    performs final admission / reservation.
//! ```
//!
//! ## Redaction
//!
//! The model is **redacted by construction**. No type here has a field that
//! can hold a secret *value* or a raw sensitive local *path*:
//! - secrets are carried as [`model::RedactedSecretRef`] (reference name only);
//! - materialized objects are carried as content hashes and counts, never
//!   local cache paths.
//!
//! If a real local path is ever needed, it belongs in the provider-local #508
//! installed-state DB, never in this cross-device index.
//!
//! ## Scope of this slice
//!
//! In: the redacted snapshot/request/receipt model, the in-memory index, the
//! deterministic candidate filter, [`model::PlacementDecisionReceipt`], and the
//! [`publisher`] boundary that mints normalized, redacted snapshots from
//! provider-local facts.
//!
//! Out (later PRs): #501 provider projection vocabulary, #508 installed-state
//! DB integration (the DB becomes an *optional* summary input to the
//! publisher), real desktop/cloud/mobile networking and sync, actual host
//! probing, secret projection, and any install/launch wiring. Nothing in
//! production calls this module yet.

mod index;
mod model;
mod publisher;
#[cfg(test)]
mod tests;

pub use index::{PlacementIndex, build_decision};
pub use model::{
    DeviceId, DeviceRole, GpuRequirement, GpuSummary, GpuVendor, MaterializedObjectSummary,
    NetworkCapabilitySummary, NetworkRequirement, OnlineStatus, PlacementCandidate,
    PlacementDecisionReceipt, PlacementHints, PlacementQueryResult, PlacementRejectionReason,
    PlacementRequest, PlatformSummary, ProviderCapabilityId, ProviderCapabilitySnapshot,
    ProviderId, ProviderKind, RedactedSecretRef, RejectedPlacementCandidate, RequiredProjection,
    ResourceSummary, RuntimeRequirement, RuntimeSummary, SecretProjectionSummary,
};
pub use publisher::{
    ProviderSnapshotInput, SnapshotBuildError, build_provider_capability_snapshot,
};

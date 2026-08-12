//! Ato-specific runtime semantics layered above the Capsule Protocol Core.
//!
//! This crate intentionally keeps Session, Branch, Checkpoint, WAL, Driver,
//! and attachment concepts out of `capsule-protocol`.

pub mod attachment;
pub mod boundary;
pub mod domain;
pub mod driver_binding;
pub mod effect;
pub mod recovery;
pub mod supervisor;
pub mod wal;

pub use attachment::{
    AttachmentEndpoint, AttachmentMechanism, AttachmentPlan, AttachmentRequirement,
    PortableEligibility, StateRuntimeCapabilities,
};
pub use boundary::{
    BoundaryDeliveryLedger, BoundaryDeliveryState, BoundaryOperationId, BoundaryProtocolError,
};
pub use domain::{
    ConnectorMode, HistoricalReplayVerdict, RecordFrontier, SessionBlockReason, SessionFailure,
    SessionLifecycle,
};
pub use effect::{
    EffectClass, EffectIntent, EffectOperationDigest, EffectState, EffectTransaction,
    EffectTransitionError,
};
pub use recovery::{
    ConnectorCheckpoint, ConnectorRecoveryPoint, ConnectorRecoveryStrategy, RecoveryPlan,
    RecoveryPlanError, ResumeFidelity, SessionCheckpoint, StateRecoveryPoint,
};
pub use supervisor::{
    BarrierError, BarrierId, BoundaryCoordinator, BoundaryDriver, DriverBoundaryError,
    DriverQuiesceReport, FrontierBarrier, JournalCommit,
};
pub use wal::{RecoveredJournal, SessionWal, WalEntry, WalError, WalRecord};

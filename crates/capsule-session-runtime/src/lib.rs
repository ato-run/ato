//! Ato-specific runtime semantics layered above the Capsule Protocol Core.
//!
//! This crate intentionally keeps Session, Branch, Checkpoint, WAL, Driver,
//! and attachment concepts out of `capsule-protocol`.

pub mod attachment;
pub mod boundary;
pub mod domain;
pub mod driver_binding;
pub mod driver_registry;
pub mod effect;
pub mod recovery;
pub mod runtime;
pub mod session_store;
pub mod supervisor;
pub mod wal;

pub use attachment::{
    AttachmentEndpoint, AttachmentMechanism, AttachmentPlan, AttachmentPlanError,
    AttachmentRequirement, PortableEligibility, StateRuntimeCapabilities,
};
pub use boundary::{
    BoundaryDeliveryLedger, BoundaryDeliveryState, BoundaryOperationId, BoundaryProtocolError,
};
pub use domain::{
    ConnectorMode, DurableFrontier, HistoricalReplayVerdict, JournalLsn, RecordFrontier,
    SessionBlockReason, SessionFailure, SessionLifecycle,
};
pub use driver_registry::{
    DriverBindingProfile, DriverExecutable, DriverRegistration, DriverRegistry,
    DriverRegistryError, DriverTrust,
};
pub use effect::{
    EffectClass, EffectIntent, EffectOperationDigest, EffectState, EffectTransaction,
    EffectTransitionError,
};
pub use recovery::{
    ConnectorCheckpoint, ConnectorRecoveryPoint, ConnectorRecoveryStrategy, RecoveryPlan,
    RecoveryPlanError, ResumeFidelity, SessionCheckpoint, StateRecoveryPoint,
};
pub use runtime::{
    ConnectorDriverRuntime, DurableFrontierSource, PausedComputationRuntime, RunningSessionRuntime,
    RuntimeBoundaryError, SessionBootstrap, StateRuntime,
};
pub use session_store::{
    CapsuleProtocolSessionStore, ControlAuthorizationError, NewStoredProtocolSession,
    NewSupervisorIdentity, SessionId, SessionStoreError, StoredComputationOrigin,
    StoredConnectorCheckpoint, StoredLegacyV1Materialization, StoredLegacyV1Recovery,
    StoredLocalCheckpoint, StoredProtocolSession, StoredReplayVerification, StoredRuntimeProfile,
    SupervisorIdentity,
};
pub use supervisor::{
    BarrierError, BarrierId, BoundaryCoordinator, BoundaryDriver, DriverBoundaryError,
    DriverQuiesceReport, FrontierBarrier, JournalCommit,
};
pub use wal::{
    RecoveredJournal, SessionWal, SharedSessionWal, WalEntry, WalError, WalPayload, WalRecord,
};

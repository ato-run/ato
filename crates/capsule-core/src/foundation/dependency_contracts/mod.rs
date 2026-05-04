//! Lock-time verifier for capsule dependency contracts.
//!
//! This module is a **pure verifier** per the brick boundary defined in
//! `CAPSULE_DEPENDENCY_CONTRACTS.md` §9 and the implementation plan. It
//! receives the consumer's parsed manifest plus a map of already-resolved
//! provider manifests (their immutable hashes and parsed manifest bodies)
//! and returns either a fully-verified `DependencyLock` or a `LockError`
//! that maps directly to one of the 13 verification rules in §9.1.
//!
//! It does NOT touch the network, filesystem, or any registry — those are
//! the orchestrator (`ato-cli`) responsibility. The orchestrator is
//! expected to fetch each provider capsule, parse its manifest, and pass
//! the result via [`DependencyLockInput::providers`].

pub mod error;
pub mod lock;

pub use error::LockError;
pub use lock::{
    verify_and_lock, DependencyLock, DependencyLockInput, LockedDependencyEntry,
    LockedDependencyState, ResolvedProviderManifest,
};

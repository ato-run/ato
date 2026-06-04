//! Device/provider-local installed-state database and storage admission.
//!
//! This is the first minimal slice of the installed-state DB (umbrella #502,
//! issue #508; Capsule Core Model RFC §10). It records what has been
//! materialized on this device and the resources installed capsules claim, so
//! install/relaunch can be decided **up front** instead of failing
//! mid-download.
//!
//! Scope of this slice:
//! - the SQLite schema for `materialized_objects` and `resource_claims`;
//! - a **storage admission dry-run** ([`InstalledStateDb::check_storage_admission`])
//!   that answers, before download/build, whether a capsule's required disk fits
//!   in the space left after existing installed claims, returning a typed
//!   [`StorageAdmission`].
//!
//! Out of scope here (later under #508/#509): port/secret/state claims, fast
//! relaunch, GC/ref-count maintenance, install-flow integration, and the
//! cross-device placement index.

mod admission;
mod db;

pub use admission::{
    StorageAdmission, available_space, available_space_for_target, evaluate_storage_admission,
};
pub use db::{InstalledStateDb, MaterializedObject, StorageClaim};

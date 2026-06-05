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
//! ## Source of truth for installed-app launch conditions
//!
//! `launch_condition_claims` (see [`launch_condition`]) is the canonical
//! per-installed-app **launch condition ledger**: for installed apps it is the
//! device/provider-local source of truth for the conditions required to launch /
//! relaunch. `capsule.toml` is the app-owned declaration and `ato.lock` is a
//! portable resolved description — both are **inputs / provenance**, not the
//! runtime query layer for relaunch.
//!
//! The specialized tables `resource_claims` (storage) and `port_claims` remain
//! **query-optimized projections** for fast admission; they are not the full
//! condition model and are not removed.
//!
//! Out of scope here (later under #508/#509): full per-kind condition
//! extraction (env/secret/state/provider/network/hardware), fast relaunch via
//! the ledger, GC/ref-count maintenance, and the cross-device placement index.

mod admission;
mod db;
mod launch_condition;
mod port;
mod relaunch_admission;

pub use admission::{
    StorageAdmission, available_space, available_space_for_target, evaluate_storage_admission,
};
pub use db::{InstalledStateDb, MaterializedObject, StorageClaim};
pub use launch_condition::{
    ALL_LAUNCH_CONDITION_KINDS, ENV_DETAIL_ALLOWED_KEYS, LEDGER_EXTRACTION_STATUS_KEY,
    LOCAL_PROVIDER_ID, LaunchConditionClaim, LaunchConditionKind, LaunchConditionSource,
    LaunchConditionStatus, SECRET_DETAIL_ALLOWED_KEYS, app_service_endpoint,
    launch_condition_extraction_status, launch_condition_from_env_projection,
    launch_condition_from_port_claim, launch_condition_from_port_declaration,
    launch_condition_from_secret_requirement, launch_condition_from_state_binding,
    launch_condition_from_storage_claim, validate_redacted_detail_json,
};
pub use port::{
    ConflictPolicy, PortAdmission, PortClaim, evaluate_port_admission, os_port_is_free,
};
pub use relaunch_admission::{
    RelaunchAdmission, RelaunchAdmissionInput, RelaunchAdmissionReason, evaluate_relaunch_admission,
};

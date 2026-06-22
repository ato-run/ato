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
mod launch_input;
mod launch_input_apply;
mod launch_prompt_plan;
mod port;
mod relaunch_admission;
mod relaunch_resolution;
mod state_binding_resolve;

pub use admission::{
    StorageAdmission, available_space, available_space_for_target, evaluate_storage_admission,
};
pub use db::{
    InstalledStateDb, MaterializedObject, SecretGrantRefRecord, StateBindingRefRecord,
    StateBindingTargetRecord, StorageClaim,
};
pub use launch_condition::{
    ALL_LAUNCH_CONDITION_KINDS, ENV_DETAIL_ALLOWED_KEYS, LEDGER_EXTRACTION_STATUS_KEY,
    LOCAL_PROVIDER_ID, LaunchConditionClaim, LaunchConditionKind, LaunchConditionSource,
    LaunchConditionStatus, SECRET_DETAIL_ALLOWED_KEYS, app_service_endpoint,
    launch_condition_extraction_status, launch_condition_from_env_projection,
    launch_condition_from_port_claim, launch_condition_from_port_declaration,
    launch_condition_from_secret_requirement, launch_condition_from_state_binding,
    launch_condition_from_storage_claim, validate_redacted_detail_json,
};
pub use launch_input::{
    CapsuleLaunchInput, LaunchConditionInput, LaunchConditionInputKind, LaunchConditionInputValue,
    condition_key_kind, parse_capsule_launch_input, validate_condition_key,
};
pub use launch_input_apply::apply_capsule_launch_inputs_to_claims;
pub use launch_prompt_plan::{LaunchConditionPromptRequest, plan_launch_condition_prompts};
pub use port::{
    ConflictPolicy, PortAdmission, PortClaim, evaluate_port_admission, os_port_is_free,
};
pub use relaunch_admission::{
    RelaunchAdmission, RelaunchAdmissionInput, RelaunchAdmissionReason, evaluate_relaunch_admission,
};
pub use relaunch_resolution::{
    LaunchConditionUpdate, RelaunchResolution, RelaunchResolutionContext, RelaunchResolutionInput,
    RelaunchResolutionSource, RelaunchResolutionWarning, resolve_relaunch_conditions,
};
pub use state_binding_resolve::{
    ResolvedStateBinding, resolve_state_binding_from_path,
    resolve_state_binding_from_path_with_location,
};

//! `capsule` — session record schema + validation helpers shared
//! by `ato-cli` (which writes records when a capsule session starts) and
//! `ato-desktop` (which reads records to skip the resolve / session-start
//! subprocesses on warm clicks).
//!
//! ## Why a dedicated crate
//!
//! `ato-cli` carries heavy dependencies (clap, tokio, axum, reqwest,
//! wasmtime, …) that the Desktop binary should not absorb. `protocol`
//! is constitutionally pure-DTO and refuses runtime/network helpers.
//! Validation needs both the DTO **and** small OS helpers (PID alive,
//! process start time, HTTP healthcheck) — so a shared crate sits between
//! the two and keeps the Desktop's dependency graph clean.
//!
//! See `apps/ato/docs/rfcs/draft/SURFACE_MATERIALIZATION.md` §3.2 for the
//! Phase 1 fast-path design that consumes this crate.

pub mod healthcheck;
pub mod materialized;
pub mod process;
pub mod record;
pub mod store;
pub mod sweep;
pub mod validate;

pub use materialized::{
    MATERIALIZED_LAUNCH_RECORD_SCHEMA_VERSION, MaterializedLaunchRecord,
    MaterializedLaunchStaleReason, MaterializedLaunchValidationOutcome, compute_run_config_hash,
    current_platform_tag, launch_cache_root, materialized_launch_record_path,
    read_materialized_launch_record, read_materialized_launch_records,
    validate_materialized_launch_record, write_materialized_launch_record_atomic,
};
pub use record::{
    GuestSessionDisplay, SCHEMA_VERSION_V2, ServiceBackgroundDisplay, StoredDependencyContracts,
    StoredDependencyProvider, StoredExecutionGraph, StoredGraphEdge, StoredGraphNode,
    StoredOrchestrationService, StoredOrchestrationServices, StoredSessionInfo,
    TerminalSessionDisplay, WebSessionDisplay,
};
pub use store::{
    read_session_records, session_record_path, session_root, write_session_record_atomic,
};
pub use validate::{
    RecordValidationOutcome, RecordValidationParams, handle_matches_record,
    validate_record_for_install_profile_key, validate_record_only,
};

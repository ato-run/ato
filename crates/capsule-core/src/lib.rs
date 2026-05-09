//! # capsule-core
//!
//! Core library for the `ato` capsule runtime system.
//!
//! ## Responsibilities
//!
//! - **Manifest loading and validation** (`manifest`, `types`): parse `capsule.toml` and
//!   enforce schema constraints.
//! - **Lockfile generation and verification** (`lockfile`, `lock_runtime`): resolve and pin
//!   runtime versions, toolchain hashes, and platform targets.
//! - **Execution orchestration** (`engine`, `runner`, `orchestration`): discover the nacelle
//!   engine binary, build ordered startup plans, and manage process lifecycles.
//! - **Artifact packing** (`packers`): produce `.ato` capsule archives (PAX TAR + zstd payload).
//! - **Runtime adapters** (`executors`, `runtime`): abstract over OCI, Wasm, and native runtimes.
//! - **Security utilities** (`security`, `signing`, `isolation`): path validation, Ed25519
//!   signing, and host environment isolation.
//!
//! ## Error types
//!
//! Two distinct error types are used throughout the crate:
//!
//! - [`CapsuleError`]: internal propagation type (`?`-compatible via `thiserror`).
//! - [`AtoError`]: structured diagnostic output type (serializable to JSON with `code`, `phase`,
//!   and `hint` fields for end-user display).
//!
//! These two types are **not** merged; see `error.rs` for the full rationale.

// ── Layer 1: Foundation ───────────────────────────────────────────────────
pub mod foundation;
pub use foundation::attestation;
pub use foundation::blob;
pub use foundation::common;
pub use foundation::dependency_contracts;
pub use foundation::error;
pub use foundation::hardware;
pub use foundation::interactive_resolution;
pub use foundation::metrics;
pub use foundation::reporter;
pub use foundation::types;

// ── Layer 2: Contract ─────────────────────────────────────────────────────
pub mod contract;
pub use contract::ato_lock;
pub use contract::lock_runtime;
pub use contract::lockfile;
pub use contract::manifest;
pub use contract::tools;

// ── Layer 3: Routing ──────────────────────────────────────────────────────
pub mod routing;
pub use routing::discovery;
pub use routing::handle;
pub use routing::handle_store;
pub use routing::importer;
pub use routing::input_resolver;
pub use routing::launch_spec;
pub use routing::router;

// ── Layer 4: Engine ───────────────────────────────────────────────────────
pub mod engine;
pub use engine::execution_identity;
pub use engine::execution_plan;
pub use engine::executors;
pub use engine::lifecycle;
pub use engine::orchestration;
pub use engine::runner;
pub use engine::runtime;
pub use engine::share;

// ── Layer 5: Packers ──────────────────────────────────────────────────────
pub mod packers;

// ── Layer 6: Security ─────────────────────────────────────────────────────
pub mod security;
pub use security::isolation;
pub use security::policy;
pub use security::schema;
pub use security::signing;
pub use security::trust_store;
pub use security::validation;

// ── Layer 7: Adapters ─────────────────────────────────────────────────────
pub mod adapters;
pub use adapters::capsule;
pub use adapters::resource;
pub use adapters::tsnet;

// ── Layer 8: Config ───────────────────────────────────────────────────────
pub mod config;

// ── Layer 9: Wire protocols (cross-crate) ─────────────────────────────────
//
// Schema/tolerance for the Capsule Control Protocol. Originally lived in
// capsule-core (M4); extracted to `capsule-wire` (N2) so `ato-desktop` can
// link the wire surface without pulling in capsule-core's runtime deps.
// Re-exported here so existing `capsule_core::ccp::*` paths in the CLI
// keep working unchanged. See `docs/monorepo-consolidation-plan.md` §N2.
pub use capsule_wire::ccp;
pub use config::bootstrap;
pub use config::diagnostics;
pub use config::python_runtime;
pub use config::runtime_config;
pub use config::schema_registry;
pub use config::smoke;

// ── Re-exports (public API — unchanged) ───────────────────────────────────
pub use error::{AtoError, AtoErrorPhase, CapsuleError, Result};
pub use metrics::{MetricsSession, ResourceStats, RuntimeMetadata, UnifiedMetrics};
pub use reporter::{CapsuleReporter, NoOpReporter, UsageReporter};
pub use runner::{SessionRunner, SessionRunnerConfig};
pub use runtime::native::NativeHandle;
pub use runtime::oci::OciHandle;
pub use runtime::wasm::WasmHandle;
pub use runtime::{Measurable, RuntimeHandle};
pub use tsnet::{
    discover_sidecar, spawn_sidecar, wait_for_ready, SidecarBaseConfig, SidecarRequest,
    SidecarSpawnConfig, TsnetClient, TsnetConfig, TsnetEndpoint, TsnetHandle, TsnetServeStatus,
    TsnetState, TsnetStatus, TsnetWaitConfig,
};

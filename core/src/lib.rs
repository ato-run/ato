pub mod capsule_v3;
pub mod common;
pub mod config;
pub mod diagnostics;
pub mod discovery;
pub mod engine;
pub mod error;
pub mod execution_plan;
pub mod executors;
pub mod hardware;
pub mod lockfile;
pub mod mag_uri;
pub mod manifest;
pub mod metrics;
pub mod orchestration;
pub mod packers;
pub mod policy;
pub mod r3_config;
pub mod reporter;
pub mod resource;
pub mod router;
pub mod runner;
pub mod runtime;
pub mod schema_registry;
pub mod security;
pub mod signing;
pub mod smoke;
pub mod trust_store;
pub mod tsnet;
pub mod types;
pub mod validation;

pub use error::{CapsuleError, Result};
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

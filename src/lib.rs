pub mod adep;
pub mod api_server;
pub mod artifact;
pub mod auth;
pub mod billing;
#[allow(dead_code)]
pub mod capsule_capnp; // Cap'n Proto generated code
pub mod capsule_manager;
pub mod capnp_to_manifest; // Cap'n Proto ↔ CapsuleManifestV1 conversion (UARC V1.1.0)
pub mod cas; // CAS client abstraction (UARC V1.1.0)
pub mod cloud;
pub mod config;
// pub mod coordinator_service;  // Disabled: proto definitions not present in capsuled/proto
pub mod downloader; // Enabled for Phase 2
pub mod failure_codes;
pub mod grpc_server; // Enabled for Phase 2
pub mod hardware;
pub mod job_history; // Job history persistence (UARC V1.1.0)
pub mod logs;
pub mod manifest;
pub mod metrics;
pub mod model_fetcher;
pub mod network;
pub mod oci;
pub mod pool_registry;
pub mod process_supervisor;
pub mod proto;
pub mod runplan;
pub mod runtime;
pub mod security;
// pub mod status_reporter;
pub mod storage;
pub mod wasm_host;
pub mod workload;

// TODO: Re-enable when capnp proto generation is set up
// #[cfg(test)]
// mod capnp_roundtrip_test;

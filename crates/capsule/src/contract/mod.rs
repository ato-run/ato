//! Layer 2: Contract — manifest, lockfile, and capsule.lock (canonical lock).
pub mod capsule_lock;
pub mod capsule_program_contract;
pub mod execution_contract;
pub mod execution_contract_finalize;
pub mod lock_runtime;
pub mod lockfile; // lockfile_runtime/support/tests are resolved via #[path] inside lockfile.rs
pub mod manifest;
pub mod oci_compose_lock;
pub mod program_manifest_input;
pub mod program_source_projection;
pub mod snapshot_manifest;
pub mod tools;

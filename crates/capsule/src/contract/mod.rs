//! Layer 2: Contract — manifest, lockfile, and ato.lock.
pub mod ato_lock;
pub mod execution_contract;
pub mod execution_contract_finalize;
pub mod lock_runtime;
pub mod lockfile; // lockfile_runtime/support/tests are resolved via #[path] inside lockfile.rs
pub mod manifest;
pub mod oci_compose_lock;
pub mod snapshot_manifest;
pub mod tools;

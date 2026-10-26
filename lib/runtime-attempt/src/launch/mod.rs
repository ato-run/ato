//! Launching a process under the Runtime's containment: the resolved launch
//! context, the bwrap/Landlock command, the in-sandbox shim, and the
//! executor that starts, probes and stops the process group.
//!
//! Moved here from the Runner (`connected-realization-worker`) so a Run and
//! a Formation attempt launch through the same code. OCI containers and
//! service groups ([`oci`]) followed in stage 2e-c.

pub mod executable_gate;
pub mod host_boundary;
pub mod oci;
mod oci_component_inputs;
mod oci_data;
pub mod process_abi;
pub mod process_executor;
pub mod resolved;
pub mod sandbox;
pub mod sandbox_exec;

pub mod session;
pub mod state_artifact;
pub mod storage_capacity;
pub mod volume;

/// Security-sensitive mount fixtures need a real parent: macOS `/var` and
/// operator-supplied temporary roots may themselves be symlinks. Tests create
/// explicit child symlinks separately when checking their rejection.
#[cfg(test)]
fn test_temp_dir() -> tempfile::TempDir {
    let parent = std::env::temp_dir()
        .canonicalize()
        .expect("canonical test temporary parent");
    tempfile::tempdir_in(parent).expect("create test temporary directory")
}

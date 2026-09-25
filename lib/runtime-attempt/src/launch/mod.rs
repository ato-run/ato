//! Launching a process under the Runtime's containment: the resolved launch
//! context, the bwrap/Landlock command, the in-sandbox shim, and the
//! executor that starts, probes and stops the process group.
//!
//! Moved here from the Runner (`connected-realization-worker`) so a Run and
//! a Formation attempt launch through the same code. OCI containers and
//! service groups ([`oci`]) followed in stage 2e-c.

pub mod oci;
pub mod process_executor;
pub mod resolved;
pub mod sandbox;
pub mod sandbox_exec;

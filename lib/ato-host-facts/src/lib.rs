//! What this host can do, measured without starting a workload.
//!
//! This crate is the one place that answers "can this host run that route, and
//! if not, why". It is deliberately small and side-effect free apart from its
//! own probes:
//!
//! * [`reason`] — the stable vocabulary every refusal uses.
//! * [`docker`] — the local daemon as observed facts (CLI present, daemon
//!   reachable, OS/architecture, rootless, endpoint kind, same kernel, file
//!   sharing, platform support).
//! * [`capability`] — the decision, over those facts, for one declared route.
//!
//! The CLI and the adapters both use it: `ato pack` reports it, `ato run`
//! admits on it, and the OCI adapter picks its Port connector from it. The
//! control plane's lease-kind advertisement stays in the worker (it is a
//! Runner concern), but it re-exports this vocabulary so there is one set of
//! reason codes rather than two that drift.

pub mod capability;
pub mod docker;
pub mod reason;

pub use capability::{ConnectorChoice, ExecutionRequirements, HostFacts, execution_status};
pub use docker::{
    DockerFacts, DockerProbeInput, EndpointKind, PlatformSupport, WorkspaceSharing, facts_from,
    parse_platform, probe,
};
pub use reason::{CapabilityState, CapabilityStatus, Reason, ReasonCode};

/// The execution ABI that requires Docker access.
pub const OCI_EXECUTION_CAPABILITY: &str = "execution_abi:oci";

//! The Formation worker: claim a job, build it under isolation, publish a
//! result.
//!
//! It owns nothing a tenant executes. No ComputeInstance, no Run, no lease, no
//! state revision — a build produces an artifact, and what happens to that
//! artifact afterwards is somebody else's decision.
//!
//! Executing a candidate — admission, the start record, the build, the
//! realization, observation and the receipt — is the shared Runtime crate's
//! (`ato-runtime-attempt`). This crate keeps the Formation side: the hosted
//! job lifecycle, the local search driver, the Runtime Network client, and
//! publication. The modules below re-export the Runtime's under their
//! previous paths.

pub use ato_runtime_attempt::{
    admission, attempt, browser_sandbox, browser_verify, build, ephemeral, executor, journal,
    static_lane,
};

/// The build sandbox, under its previous path.
pub mod sandbox {
    pub use ato_runtime_attempt::build_sandbox::*;
}

/// The build sandbox's in-sandbox entry point, under its previous path.
pub mod sandbox_exec {
    pub use ato_runtime_attempt::build_sandbox_exec::*;
}

pub mod api;
pub mod decision_provider;
pub mod generation_provider;
pub mod job;
pub mod local;
pub mod pack;
pub mod runtime_network;

pub mod operations;

pub mod retained;

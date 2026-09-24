//! The Formation worker: claim a job, build it under isolation, publish a
//! result.
//!
//! It owns nothing a tenant executes. No ComputeInstance, no Run, no lease, no
//! state revision — a build produces an artifact, and what happens to that
//! artifact afterwards is somebody else's decision.

pub mod admission;
pub mod api;
pub mod attempt;
pub mod browser_sandbox;
pub mod browser_verify;
pub mod build;
pub mod ephemeral;
pub mod executor;
pub mod job;
pub mod journal;
pub mod local;
pub mod pack;
pub mod runtime_network;
pub mod sandbox;
pub mod sandbox_exec;
pub mod static_lane;

pub mod operations;

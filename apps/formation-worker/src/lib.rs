//! The Formation worker: claim a job, build it under isolation, publish a
//! result.
//!
//! It owns nothing a tenant executes. No ComputeInstance, no Run, no lease, no
//! state revision — a build produces an artifact, and what happens to that
//! artifact afterwards is somebody else's decision.

pub mod api;
pub mod build;
pub mod job;
pub mod pack;
pub mod sandbox;
pub mod static_lane;

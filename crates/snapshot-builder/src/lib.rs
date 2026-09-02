//! Pure producer components shared by future builder dispatch wiring.
//!
//! This library intentionally owns no claim-loop, Firecracker, Docker, R2, or
//! control-plane code. The current daemon keeps its existing VM Snapshot path;
//! a later caller may choose this explicit output plan as a second materialized
//! artifact without changing how the Vite image is built.

pub mod static_web_bundle;
pub mod static_web_emit;
pub mod static_web_instance_state;
pub mod static_web_output;
pub mod static_web_replay_bridge;
pub mod static_web_transport;

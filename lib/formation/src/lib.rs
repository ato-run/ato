//! The Formation domain: pure, and deliberately so.
//!
//! Everything here takes bytes or a tree and returns a fact. Nothing here
//! opens a socket, spawns a process, installs a dependency or touches a
//! database — those belong to the worker (`apps/formation-worker`) and the
//! control plane. Keeping the domain pure is what makes a build plan testable
//! without a build.

pub mod detect;
pub mod intent;
pub mod preset;
pub mod source;

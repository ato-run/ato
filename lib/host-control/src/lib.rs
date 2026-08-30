//! Host-agnostic execution supervision.
//!
//! The `ato` CLI is the SOLE owner of Capsule execution, isolation, and
//! runner control. This crate does NOT execute Capsules — it *supervises*: it
//! drives the CLI as a child process behind a host-agnostic seam so any host
//! (the desktop shell today; others later) can drive it without re-implementing
//! process supervision.
//!
//! ## Shape
//!
//! - [`RunnerHost`] — the host-specific primitives (spawn / liveness / group
//!   teardown / binary resolution). One impl per host kind.
//! - [`ProcessSupervisor`] — generic over `RunnerHost`; owns spawn, reap, and
//!   whole-process-group teardown.
//! - [`native`] — [`NativeHost`], the concrete `RunnerHost` for a real
//!   operating system.
//!
//! This crate stays dependency-light and introduces no new wire types; the
//! DTOs it needs come from `ato-ipc`.

pub mod backend;
pub mod native;
pub mod supervisor;

pub use backend::{
    ChildId, CommandSpec, CompletedCommand, HostError, ManagedChild, OutputSink, RunnerHost,
    SpawnSpec,
};
pub use native::{
    CommandNoWindowExt, NativeChild, NativeHost, mark_process_group_leader, resolve_on_path,
    terminate_process_group, terminate_process_group_gracefully,
};
pub use supervisor::ProcessSupervisor;

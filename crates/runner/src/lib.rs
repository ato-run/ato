//! Host-agnostic execution supervision.
//!
//! The `ato` CLI is the SOLE owner of capsule execution, isolation, and
//! runner-control. This crate does NOT execute capsules — it *supervises*: it
//! drives the CLI as a child process, tracks session lifecycle, and exposes a
//! typed surface that any **host** can drive. Desktop is the first host; the
//! abstraction is deliberately host-agnostic so future hosts (IoT devices,
//! mobile / tablet, EV, …) load by implementing one trait rather than forking
//! the supervision logic.
//!
//! ## Shape
//!
//! - [`RunnerHost`] — the host-specific primitives (spawn / liveness / group
//!   teardown / binary resolution). One impl per host kind.
//! - [`ProcessSupervisor`] — generic over `RunnerHost`; consolidates the spawn +
//!   process-group-kill + reap logic that is currently triplicated across the
//!   desktop shell, `cli::runner_agent`, and `desktop_run_agent`.
//! - [`os`] — the `(d)` OS receptacle: [`NativeHost`], the concrete
//!   `RunnerHost` for a real operating system. Consolidates the process-group
//!   spawn / whole-group teardown / console-window-suppression primitives that
//!   were triplicated across the desktop shell.
//! - [`session`] / [`control`] — session lifecycle (launch/stop/restart/list +
//!   retention) and runner-control clients (loopback Runtime Control, netd
//!   ingress). Module boundaries today; the concrete logic migrates in from the
//!   desktop crate during the Phase 1 redistribution.
//!
//! Wire / DTO types come from [`protocol`] (the DAG-root type crate). This crate
//! introduces no new wire types of its own.

pub mod backend;
pub mod control;
pub mod os;
pub mod session;
pub mod supervisor;

pub use backend::{ChildId, HostError, ManagedChild, OutputSink, RunnerHost, SpawnSpec};
pub use os::{CommandNoWindowExt, NativeChild, NativeHost, resolve_on_path, terminate_process_group};
pub use session::{SessionId, SessionState, SessionSupervisor};
pub use supervisor::ProcessSupervisor;

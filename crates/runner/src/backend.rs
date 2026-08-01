//! `RunnerHost` — the host-specific primitives every execution host must provide.
//!
//! Everything above this trait ([`crate::supervisor`], [`crate::session`]) is
//! generic over it, so adding a new host (IoT / mobile / EV) means "implement
//! `RunnerHost`", not "re-implement supervision". A host knows how to spawn,
//! observe, and tear down processes on its platform and how to resolve the
//! ato-family binaries — nothing about capsule semantics (that stays in the CLI).

use std::path::PathBuf;

/// Opaque handle to a child process the host is supervising. Unique within one
/// [`crate::supervisor::ProcessSupervisor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChildId(pub u64);

/// Failures a host can surface while spawning or tearing down processes.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// An ato-family binary could not be resolved on this host.
    #[error("binary not found: {0}")]
    BinaryNotFound(String),
    /// The host failed to spawn the requested process.
    #[error("spawn failed: {0}")]
    Spawn(String),
    /// The host failed to tear down a process group.
    #[error("process-group teardown failed: {0}")]
    Teardown(String),
    /// Underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A spawned child under host supervision.
///
/// `terminate_group` MUST tear down the whole process group (not just the direct
/// child) — this is the invariant the three current desktop/cli kill sites each
/// re-implement; centralising it here is the point.
pub trait ManagedChild: Send {
    /// This child's handle.
    fn id(&self) -> ChildId;
    /// Whether the process is still alive (host-specific liveness check).
    fn is_alive(&self) -> bool;
    /// Terminate the whole process group. Idempotent — safe to call on an
    /// already-dead child.
    fn terminate_group(&mut self) -> Result<(), HostError>;
}

/// Where a host should direct a supervised child's stdout/stderr.
#[derive(Debug, Clone)]
pub enum OutputSink {
    /// Append to a log file. This is the desktop's installed-launch pattern —
    /// it avoids the pipe-buffer stall a long-running child hits when nothing
    /// drains its pipe; the supervisor tails the file instead.
    LogFile(PathBuf),
    /// Inherit the parent's stdio.
    Inherit,
    /// Discard output.
    Null,
}

/// A host-agnostic description of what to spawn. The host decides *how* to
/// realise it (process group setup, no-console-window on Windows, etc.).
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    /// The program to run (typically resolved via [`RunnerHost::resolve_binary`]).
    pub program: PathBuf,
    /// Arguments passed to the program.
    pub args: Vec<String>,
    /// Extra environment variables for the child.
    pub env: Vec<(String, String)>,
    /// Where the child's output should go.
    pub output: OutputSink,
}

/// The host-specific execution surface. One implementation per host kind
/// (desktop today; IoT / mobile / EV later). Must be `Send + Sync` so a
/// supervisor can be shared across the host's tasks.
pub trait RunnerHost: Send + Sync {
    /// The concrete supervised-child type this host produces.
    type Child: ManagedChild;

    /// Resolve an ato-family binary (e.g. `"ato"`, `"nacelle"`, `"ato-netd"`)
    /// to an absolute path on this host.
    fn resolve_binary(&self, name: &str) -> Result<PathBuf, HostError>;

    /// Spawn a supervised child from `spec`.
    fn spawn(&self, spec: &SpawnSpec) -> Result<Self::Child, HostError>;
}

//! `ProcessSupervisor` — generic over [`RunnerHost`].
//!
//! Consolidates the spawn + process-group-kill + reap logic that is currently
//! triplicated across `desktop::window::launch_window` (the canonical
//! `kill_installed_launch_process_group`), `cli::application::runner_agent`, and
//! `desktop_run_agent` (groundwork §1 finding #5). Those three become thin
//! policy wrappers over this once the Phase 1 redistribution lands.

use crate::backend::{ChildId, HostError, ManagedChild, RunnerHost, SpawnSpec};

/// Tracks the set of children spawned on one host and owns their teardown.
pub struct ProcessSupervisor<H: RunnerHost> {
    host: H,
    children: Vec<H::Child>,
}

impl<H: RunnerHost> ProcessSupervisor<H> {
    /// Create a supervisor bound to `host`.
    pub fn new(host: H) -> Self {
        Self {
            host,
            children: Vec::new(),
        }
    }

    /// Spawn a supervised child and track it for later teardown.
    pub fn spawn(&mut self, spec: &SpawnSpec) -> Result<ChildId, HostError> {
        let child = self.host.spawn(spec)?;
        let id = child.id();
        self.children.push(child);
        Ok(id)
    }

    /// Drop any children that have exited. Returns how many were reaped.
    pub fn reap(&mut self) -> usize {
        self.reap_with_status().len()
    }

    /// Reap exited children and return their ids plus captured exit codes.
    /// Hosts use this to distinguish successful completion from a failed
    /// cancellable operation without surrendering process-group ownership.
    pub fn reap_with_status(&mut self) -> Vec<(ChildId, Option<i32>)> {
        let mut running = Vec::with_capacity(self.children.len());
        let mut completed = Vec::new();
        for child in self.children.drain(..) {
            if child.is_alive() {
                running.push(child);
            } else {
                completed.push((child.id(), child.exit_code()));
            }
        }
        self.children = running;
        completed
    }

    /// Number of children currently supervised.
    pub fn supervised_count(&self) -> usize {
        self.children.len()
    }

    /// Whether a child is still owned by this supervisor. Call [`Self::reap`]
    /// first when the caller needs a current liveness snapshot.
    pub fn contains(&self, id: ChildId) -> bool {
        self.children.iter().any(|child| child.id() == id)
    }

    /// Terminate one supervised process group and remove it from ownership.
    /// Returns `Ok(false)` when the id is already absent, making cancellation
    /// idempotent across UI retries and completion races.
    pub fn terminate(&mut self, id: ChildId) -> Result<bool, HostError> {
        let Some(index) = self.children.iter().position(|child| child.id() == id) else {
            return Ok(false);
        };
        let mut child = self.children.remove(index);
        child.terminate_group()?;
        Ok(true)
    }

    /// Terminate every supervised child's process group and forget them.
    /// Idempotent. Returns the first teardown error if any occurred, after
    /// attempting to tear down all children.
    pub fn shutdown(&mut self) -> Result<(), HostError> {
        let mut first_err = None;
        for child in self.children.iter_mut() {
            if let Err(e) = child.terminate_group() {
                first_err.get_or_insert(e);
            }
        }
        self.children.clear();
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Borrow the underlying host (e.g. to resolve a binary before spawning).
    pub fn host(&self) -> &H {
        &self.host
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{
        ChildId, CommandSpec, CompletedCommand, HostError, ManagedChild, OutputSink, RunnerHost,
        SpawnSpec,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A minimal in-memory host — proves the abstraction is usable end-to-end
    /// and demonstrates that "a new host = implement RunnerHost".
    struct FakeChild {
        id: ChildId,
        alive: bool,
    }
    impl ManagedChild for FakeChild {
        fn id(&self) -> ChildId {
            self.id
        }
        fn is_alive(&self) -> bool {
            self.alive
        }
        fn terminate_group(&mut self) -> Result<(), HostError> {
            self.alive = false;
            Ok(())
        }
    }

    struct FakeHost {
        next: AtomicU64,
    }
    impl RunnerHost for FakeHost {
        type Child = FakeChild;
        fn resolve_binary(&self, name: &str) -> Result<PathBuf, HostError> {
            Ok(PathBuf::from(format!("/fake/bin/{name}")))
        }
        fn spawn(&self, _spec: &SpawnSpec) -> Result<FakeChild, HostError> {
            Ok(FakeChild {
                id: ChildId(self.next.fetch_add(1, Ordering::Relaxed)),
                alive: true,
            })
        }
        fn run_to_completion(&self, _spec: &CommandSpec) -> Result<CompletedCommand, HostError> {
            Ok(CompletedCommand {
                exit_code: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }

    fn spec() -> SpawnSpec {
        SpawnSpec {
            program: PathBuf::from("/fake/bin/ato"),
            args: vec!["launch".into(), "demo".into()],
            env: vec![],
            output: OutputSink::Null,
        }
    }

    #[test]
    fn resolves_binary_via_host() {
        let host = FakeHost {
            next: AtomicU64::new(0),
        };
        let sup = ProcessSupervisor::new(host);
        assert_eq!(
            sup.host().resolve_binary("nacelle").unwrap(),
            PathBuf::from("/fake/bin/nacelle")
        );
    }

    #[test]
    fn spawns_tracks_and_shuts_down() {
        let host = FakeHost {
            next: AtomicU64::new(0),
        };
        let mut sup = ProcessSupervisor::new(host);
        let a = sup.spawn(&spec()).unwrap();
        let b = sup.spawn(&spec()).unwrap();
        assert_ne!(a, b);
        assert_eq!(sup.supervised_count(), 2);
        // Both alive → nothing reaped.
        assert_eq!(sup.reap(), 0);
        // Shutdown tears down the groups and forgets them; idempotent.
        sup.shutdown().unwrap();
        assert_eq!(sup.supervised_count(), 0);
        sup.shutdown().unwrap();
        assert_eq!(sup.supervised_count(), 0);
    }

    #[test]
    fn terminates_only_the_requested_child_and_is_idempotent() {
        let host = FakeHost {
            next: AtomicU64::new(10),
        };
        let mut sup = ProcessSupervisor::new(host);
        let first = sup.spawn(&spec()).unwrap();
        let second = sup.spawn(&spec()).unwrap();

        assert!(sup.terminate(first).unwrap());
        assert!(!sup.contains(first));
        assert!(sup.contains(second));
        assert!(!sup.terminate(first).unwrap());
        assert_eq!(sup.supervised_count(), 1);
    }
}

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
        let before = self.children.len();
        self.children.retain(|c| c.is_alive());
        before - self.children.len()
    }

    /// Number of children currently supervised.
    pub fn supervised_count(&self) -> usize {
        self.children.len()
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
    use crate::backend::{ChildId, HostError, ManagedChild, OutputSink, RunnerHost, SpawnSpec};
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
}

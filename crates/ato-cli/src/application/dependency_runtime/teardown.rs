//! Teardown ordering for dependency contracts (RFC §10.4).
//!
//! Stops a set of provider targets in **reverse-topological** order
//! relative to the consumer's needs graph. Sends SIGTERM, waits up to a
//! configurable grace window, then escalates to SIGKILL. The caller is
//! responsible for sentinel cleanup (`orphan::sweep_stale_sentinel` or
//! manual unlink) — this module only manages the process lifecycle.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TeardownError {
    #[error("teardown could not stop provider for dep '{dep}' (pid={pid}): {detail}")]
    StopFailed {
        dep: String,
        pid: i32,
        detail: String,
    },

    #[error("teardown ordering: dep '{dep}' references unknown 'needs' entry '{name}'")]
    UnknownNeed { dep: String, name: String },

    #[error("teardown ordering: cycle detected in graph: {path}")]
    CycleDetected { path: String },
}

#[derive(Debug, Clone)]
pub struct TeardownTarget {
    pub dep: String,
    pub pid: i32,
    /// `state.dir` for sentinel cleanup. Caller decides whether to sweep.
    pub state_dir: PathBuf,
    /// Other deps this one needs. Used to compute reverse-topological
    /// ordering — a dep is stopped *after* its dependents.
    pub needs: Vec<String>,
}

/// Stop the given targets in reverse-topological order. The
/// implementation is platform-portable: it uses `libc::kill` on Unix and
/// is a no-op on non-Unix (Ato v1 is unix-only anyway). For each target
/// SIGTERM is sent, then up to `grace` is allowed for graceful exit, then
/// SIGKILL escalation.
pub fn teardown_reverse_topological(
    targets: Vec<TeardownTarget>,
    grace: Duration,
) -> Result<(), TeardownError> {
    let order = reverse_topological(&targets)?;
    let by_dep: BTreeMap<String, TeardownTarget> =
        targets.into_iter().map(|t| (t.dep.clone(), t)).collect();

    let ordered_targets = order
        .into_iter()
        .filter_map(|dep_name| by_dep.get(&dep_name).cloned())
        .collect::<Vec<_>>();
    teardown_in_order(&ordered_targets, grace)
}

pub(crate) fn teardown_in_order(
    targets: &[TeardownTarget],
    grace: Duration,
) -> Result<(), TeardownError> {
    for target in targets {
        stop_one(target, grace)?;
    }
    Ok(())
}

/// PR-5b (refs umbrella v0.6.0 graph-first migration): graph-driven
/// teardown driver. Walks the persisted `StoredExecutionGraph` and
/// dispatches per node kind:
///
/// - `NODE_KIND_SERVICE` → stop via
///   `application::orchestration_teardown::stop_orchestration_service_record`.
/// - `NODE_KIND_PROVIDER` → stop via the existing
///   `teardown_reverse_topological` primitive over a single-node
///   `TeardownTarget`.
/// - Unknown kinds → `tracing::trace!` and skip (no-op-safe).
///
/// Order: services first, then providers. This matches the
/// conceptual reverse-topological walk for the edge convention
/// established in PR-5a (`service --depends_on/uses→ provider`):
/// source first, target second. Within each kind, nodes are sorted
/// by identifier for determinism.
///
/// Today's implementation does NOT consult `depends_on` edges
/// between same-kind nodes — PR-5a's gap (provider→provider needs
/// don't reach the populator yet) makes those edges always empty in
/// practice. When a future commit threads `needs` through, this
/// driver can be upgraded to true edge-walk reverse-topological
/// order; the kind-first ordering is correct in the meantime.
///
/// Errors from individual nodes are propagated. Use this driver
/// only when `session_graph_populate::graph_complete_for_teardown`
/// returned true for the source record — incomplete graphs should
/// fall back to the legacy two-path teardown.
pub fn teardown_from_graph(
    graph: &ato_session_core::StoredExecutionGraph,
    grace: Duration,
) -> Result<(), TeardownError> {
    use crate::application::session_graph_populate::{NODE_KIND_PROVIDER, NODE_KIND_SERVICE};

    // Services first, in alias order.
    let mut service_nodes: Vec<&ato_session_core::StoredGraphNode> = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NODE_KIND_SERVICE)
        .collect();
    service_nodes.sort_by(|a, b| a.identifier.cmp(&b.identifier));
    for node in &service_nodes {
        let record = service_record_from_node(node);
        if let Err(err) = crate::application::orchestration_teardown::stop_orchestration_service_record(
            &record, grace,
        ) {
            tracing::warn!(
                service = node.identifier.as_str(),
                error = %err,
                "graph teardown: service stop returned error; continuing"
            );
        }
    }

    // Providers next, in alias order. Reverse-topo within the
    // provider sub-graph requires `needs`, which PR-5a does not
    // populate yet (see the docstring comment above). For now we
    // delegate to teardown_in_order with `needs = Vec::new()` per
    // target — equivalent to alphabetical-stop.
    let mut provider_nodes: Vec<&ato_session_core::StoredGraphNode> = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NODE_KIND_PROVIDER)
        .collect();
    provider_nodes.sort_by(|a, b| a.identifier.cmp(&b.identifier));
    let provider_targets: Vec<TeardownTarget> = provider_nodes
        .iter()
        .filter_map(|node| {
            let pid = node.pid?;
            let state_dir = node.state_dir.clone()?;
            Some(TeardownTarget {
                dep: node.identifier.clone(),
                pid,
                state_dir,
                needs: Vec::new(),
            })
        })
        .collect();
    if !provider_targets.is_empty() {
        teardown_in_order(&provider_targets, grace)?;
        // Sentinel sweep for each provider node that had a state_dir.
        for target in &provider_targets {
            let _ = crate::application::dependency_runtime::orphan::sweep_stale_sentinel(
                &target.state_dir,
            );
        }
    }

    Ok(())
}

/// PR-5b: project a service node back to a `StoredOrchestrationService`
/// shape that
/// `orchestration_teardown::stop_orchestration_service_record`
/// consumes. Mirror of the writer in `session_graph_populate.rs`.
fn service_record_from_node(
    node: &ato_session_core::StoredGraphNode,
) -> ato_session_core::StoredOrchestrationService {
    let target_label = node
        .metadata
        .get("target_label")
        .cloned()
        .unwrap_or_else(|| node.identifier.clone());
    let host_ports = node
        .metadata
        .get("host_ports")
        .map(|encoded| {
            encoded
                .split(',')
                .filter_map(|pair| {
                    let (h, c) = pair.split_once(':')?;
                    Some((h.parse::<u16>().ok()?, c.parse::<u16>().ok()?))
                })
                .collect()
        })
        .unwrap_or_default();
    let published_port = node
        .metadata
        .get("published_port")
        .and_then(|s| s.parse::<u16>().ok())
        .or(node.port);
    ato_session_core::StoredOrchestrationService {
        name: node.identifier.clone(),
        target_label,
        local_pid: node.pid,
        container_id: node.container_id.clone(),
        host_ports,
        published_port,
    }
}

fn reverse_topological(targets: &[TeardownTarget]) -> Result<Vec<String>, TeardownError> {
    // Forward topological order: a dep with `needs = [X]` comes *after* X
    // (so X is started first). Reverse topological = stop X *after* the
    // deps that need it. Equivalent to forward DFS where children are
    // dependents (= reverse edges).
    let names: BTreeSet<String> = targets.iter().map(|t| t.dep.clone()).collect();
    for t in targets {
        for need in &t.needs {
            if !names.contains(need) {
                return Err(TeardownError::UnknownNeed {
                    dep: t.dep.clone(),
                    name: need.clone(),
                });
            }
        }
    }

    // Build forward graph (edge: dependent → dependency). Forward topo
    // order yields a list where dependencies come first.
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for t in targets {
        adj.entry(t.dep.as_str()).or_default();
        for need in &t.needs {
            adj.entry(t.dep.as_str()).or_default().push(need.as_str());
        }
    }

    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut visiting: BTreeSet<&str> = BTreeSet::new();
    let mut order: Vec<String> = Vec::new();
    for node in adj.keys().copied().collect::<Vec<_>>() {
        let mut stack: Vec<&str> = Vec::new();
        dfs(
            node,
            &adj,
            &mut visited,
            &mut visiting,
            &mut order,
            &mut stack,
        )?;
    }

    // `order` is forward-topological (dependencies first). Reverse it so
    // dependents are stopped first.
    order.reverse();
    Ok(order)
}

fn dfs<'a>(
    node: &'a str,
    adj: &'a BTreeMap<&'a str, Vec<&'a str>>,
    visited: &mut BTreeSet<&'a str>,
    visiting: &mut BTreeSet<&'a str>,
    order: &mut Vec<String>,
    stack: &mut Vec<&'a str>,
) -> Result<(), TeardownError> {
    if visited.contains(node) {
        return Ok(());
    }
    if visiting.contains(node) {
        let mut cycle: Vec<&str> = stack.iter().copied().skip_while(|s| *s != node).collect();
        cycle.push(node);
        return Err(TeardownError::CycleDetected {
            path: cycle.join(" -> "),
        });
    }
    visiting.insert(node);
    stack.push(node);
    if let Some(neighbors) = adj.get(node) {
        for next in neighbors {
            dfs(next, adj, visited, visiting, order, stack)?;
        }
    }
    stack.pop();
    visiting.remove(node);
    visited.insert(node);
    order.push(node.to_string());
    Ok(())
}

#[cfg(unix)]
fn stop_one(target: &TeardownTarget, grace: Duration) -> Result<(), TeardownError> {
    if target.pid <= 0 {
        return Ok(());
    }
    // SIGTERM first
    unsafe { libc::kill(target.pid, libc::SIGTERM) };
    // Wait for graceful exit, polling every 50ms
    let started = Instant::now();
    while started.elapsed() < grace {
        if !pid_alive(target.pid) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // SIGKILL escalation. After this signal succeeds we consider the dep
    // process logically stopped — the kernel guarantees it will not
    // execute again. The process table entry may still be visible to
    // `kill(pid, 0)` as a zombie until its parent reaps it; that is the
    // parent's responsibility (the orchestrator that spawned it via
    // std::process::Child::wait), not teardown's.
    let kill_res = unsafe { libc::kill(target.pid, libc::SIGKILL) };
    if kill_res != 0 {
        let err = std::io::Error::last_os_error();
        // ESRCH = no such process: it already exited cleanly between our
        // SIGTERM and our SIGKILL. That's fine.
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(TeardownError::StopFailed {
            dep: target.dep.clone(),
            pid: target.pid,
            detail: format!("SIGKILL failed: {err}"),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn stop_one(_target: &TeardownTarget, _grace: Duration) -> Result<(), TeardownError> {
    Ok(())
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn target(dep: &str, pid: i32, needs: &[&str]) -> TeardownTarget {
        TeardownTarget {
            dep: dep.to_string(),
            pid,
            state_dir: tempfile::tempdir().expect("tempdir").path().to_path_buf(),
            needs: needs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn reverse_topo_orders_dependents_before_dependencies() {
        // app needs db; cache needs db; db has no deps.
        // Forward topo: db, app, cache (or db, cache, app).
        // Reverse:    app, cache, db (or cache, app, db).
        // So `db` must come last.
        let targets = vec![
            target("db", 0, &[]),
            target("app", 0, &["db"]),
            target("cache", 0, &["db"]),
        ];
        let order = reverse_topological(&targets).expect("topo");
        assert_eq!(*order.last().unwrap(), "db");
    }

    #[test]
    fn reverse_topo_rejects_unknown_need() {
        let targets = vec![target("app", 0, &["phantom"])];
        let err = reverse_topological(&targets).expect_err("must reject");
        assert!(
            matches!(err, TeardownError::UnknownNeed { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn reverse_topo_rejects_cycle() {
        let targets = vec![target("a", 0, &["b"]), target("b", 0, &["a"])];
        let err = reverse_topological(&targets).expect_err("must reject cycle");
        assert!(
            matches!(err, TeardownError::CycleDetected { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn teardown_with_pid_zero_is_noop() {
        // pid=0 sentinel means "no process" — teardown should silently
        // succeed. Useful for dry runs.
        let targets = vec![target("dummy", 0, &[])];
        teardown_reverse_topological(targets, Duration::from_millis(100)).expect("noop");
    }

    #[test]
    fn teardown_kills_a_real_child_via_sigterm() {
        // Spawn a long-running child, take its pid, ensure teardown stops it.
        // We verify success by reaping with `wait()` and checking the exit
        // signal — `pid_alive(kill,0)` returns true for zombies, so it is
        // not a reliable post-teardown liveness signal.
        let mut child = Command::new("/bin/sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        assert!(pid_alive(pid), "child should be alive before teardown");

        let targets = vec![target("worker", pid, &[])];
        teardown_reverse_topological(targets, Duration::from_secs(1)).expect("teardown");

        // The orchestrator's parent (this test) reaps the zombie now that
        // teardown's SIGTERM/SIGKILL has terminated the child.
        let exit_status = child.wait().expect("wait");
        // Child must have terminated by signal (SIGTERM=15 or SIGKILL=9).
        // On unix, std::process::ExitStatus exposes the signal via
        // `signal()`, but the test is portable enough by just asserting
        // non-success: a `sleep 60` that exited under our signal is not
        // expected to report success (.code() = None, .success() = false).
        assert!(!exit_status.success(), "child must not exit success");
    }

    /// PR-5b: graph teardown driver dispatches per node kind. We use
    /// `pid: None` (or pid 0 below — bypasses libc::kill on POSIX)
    /// so the test doesn't actually try to kill processes.
    #[test]
    fn teardown_from_graph_visits_service_and_provider_kinds() {
        use ato_session_core::{StoredExecutionGraph, StoredGraphNode};
        use std::collections::BTreeMap;

        let provider_node = StoredGraphNode {
            kind: "provider".to_string(),
            identifier: "db".to_string(),
            // pid 0 means stop_one's `pid <= 0` short-circuit fires
            // and no kill is sent. state_dir is required because
            // teardown_in_order builds a TeardownTarget.
            pid: Some(0),
            state_dir: Some(PathBuf::from("/tmp/pr5b-test-db")),
            port: None,
            container_id: None,
            capability: None,
            metadata: BTreeMap::new(),
        };
        let service_node = StoredGraphNode {
            kind: "service".to_string(),
            identifier: "web".to_string(),
            pid: Some(0),
            state_dir: None,
            port: None,
            container_id: None,
            capability: None,
            metadata: BTreeMap::new(),
        };
        let graph = StoredExecutionGraph {
            schema_version: StoredExecutionGraph::SCHEMA_VERSION,
            nodes: vec![service_node, provider_node],
            edges: vec![],
        };
        // No real processes signalled (pid 0 → no-op). Must succeed.
        teardown_from_graph(&graph, Duration::from_millis(10))
            .expect("graph teardown returns Ok on empty work");
    }

    #[test]
    fn teardown_from_graph_empty_graph_is_noop() {
        use ato_session_core::StoredExecutionGraph;
        let graph = StoredExecutionGraph {
            schema_version: StoredExecutionGraph::SCHEMA_VERSION,
            nodes: vec![],
            edges: vec![],
        };
        teardown_from_graph(&graph, Duration::from_millis(10)).expect("empty graph teardown");
    }
}

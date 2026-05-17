//! PR-5b (refs umbrella v0.6.0 graph-first migration): single-service
//! orchestration teardown primitive. Extracted out of
//! `app_control::session` so `dependency_runtime::teardown` can call
//! it without creating a circular module dependency.
//!
//! Scope deliberately narrow: this module stops a SINGLE
//! `StoredOrchestrationService` (managed or OCI) with a SIGTERM →
//! grace → SIGKILL escalation on its `local_pid`, plus container kill
//! for OCI services. It does NOT do orphan listener cleanup or
//! descendant-pid sweeping — those richer behaviors stay on the
//! legacy `stop_recorded_orchestration_services` path in `session.rs`.
//!
//! `teardown_from_graph` uses this primitive once per service node
//! when the graph is `graph_complete_for_teardown` (so the legacy
//! fallback is implicitly the safety net for incomplete graphs).

use std::time::{Duration, Instant};

use anyhow::Result;
use ato_session_core::StoredOrchestrationService;

/// PR-5b: stop a single orchestration service record. Returns
/// `Ok(true)` if a process was actually signalled, `Ok(false)` if
/// the record had no `local_pid` and no `container_id` (nothing to
/// stop).
///
/// Errors are non-fatal: callers (the graph teardown driver) typically
/// log + continue so one stuck service doesn't block the rest.
pub(crate) fn stop_orchestration_service_record(
    service: &StoredOrchestrationService,
    grace: Duration,
) -> Result<bool> {
    let mut signalled = false;

    if let Some(pid) = service.local_pid {
        if pid > 0 {
            stop_local_pid(pid, grace);
            signalled = true;
        }
    }

    if let Some(container_id) = service.container_id.as_deref() {
        if !container_id.is_empty() {
            stop_container_best_effort(container_id);
            signalled = true;
        }
    }

    Ok(signalled)
}

#[cfg(unix)]
fn stop_local_pid(pid: i32, grace: Duration) {
    // SIGTERM first.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let started = Instant::now();
    while started.elapsed() < grace {
        if !pid_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe { libc::kill(pid, libc::SIGKILL) };
}

#[cfg(not(unix))]
fn stop_local_pid(_pid: i32, _grace: Duration) {
    // Non-unix is not a v1 deployment target.
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe {
        if libc::kill(pid, 0) == 0 {
            return true;
        }
        matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        )
    }
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    false
}

/// Best-effort `docker stop` (or `podman stop` if available). PR-5b
/// keeps this minimal — the legacy orchestration teardown has richer
/// OCI handling; this primitive is a fallback for graph-driven
/// teardown of complete records.
fn stop_container_best_effort(container_id: &str) {
    use std::process::Command;
    let _ = Command::new("docker")
        .args(["stop", container_id])
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn empty_record_signals_nothing() {
        let service = StoredOrchestrationService {
            name: "empty".to_string(),
            target_label: "empty".to_string(),
            local_pid: None,
            container_id: None,
            host_ports: BTreeMap::new(),
            published_port: None,
        };
        // Should return Ok(false) — nothing to signal.
        let signalled = stop_orchestration_service_record(&service, Duration::from_secs(0))
            .expect("ok");
        assert!(!signalled);
    }

    #[test]
    fn pid_zero_signals_nothing() {
        let service = StoredOrchestrationService {
            name: "pid-zero".to_string(),
            target_label: "x".to_string(),
            local_pid: Some(0),
            container_id: None,
            host_ports: BTreeMap::new(),
            published_port: None,
        };
        // local_pid is Some(0); stop_local_pid is gated on `pid > 0`
        // and signalled is only set inside that gate. Pid 0 is
        // never a valid kill target on POSIX — return false.
        let signalled = stop_orchestration_service_record(&service, Duration::from_secs(0))
            .expect("ok");
        assert!(!signalled);
    }
}

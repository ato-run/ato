//! PR-5b (refs umbrella v0.6.0 graph-first migration): single-service
//! orchestration teardown primitive. Extracted out of
//! `app_control::session` so `dependency_runtime::teardown` can call
//! it without creating a circular module dependency.
//!
//! PR-5b-fix: this primitive is now legacy-equivalent — it performs
//! all four cleanup steps that the legacy
//! `stop_recorded_orchestration_services` did inline:
//!
//!   1. Process-group kill when the recorded `local_pid` is a pgroup
//!      leader (`getpgid(pid) == pid`). The nacelle supervisor sets
//!      this via `cmd.process_group(0)`, so `kill(-pgid, sig)` reaps
//!      the wrapper AND every descendant atomically.
//!   2. Descendant pid walk via `pgrep -P` (bounded depth/count) when
//!      the recorded pid is NOT a pgroup leader — captures wrapper
//!      subtrees that would otherwise reparent to init when the
//!      recorded pid is signalled.
//!   3. `published_port` listener fallback via `lsof -nP -iTCP:<port>`:
//!      belt-and-suspenders for any listener still bound after the
//!      pid/descendant kills (#108).
//!   4. OCI `stop_container` + `remove_container` for service records
//!      that carry a `container_id`.
//!
//! Both `app_control::session::stop_recorded_orchestration_services`
//! (legacy iteration) and `dependency_runtime::teardown::teardown_from_graph`
//! (graph-driven) call this primitive so behavior is identical
//! regardless of which path picks the service.

use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ato_session_core::StoredOrchestrationService;

/// PR-5b: stop a single orchestration service record. Returns
/// `Ok(true)` if a process or container was actually signalled,
/// `Ok(false)` if the record had no `local_pid` and no `container_id`
/// (nothing to stop).
///
/// `grace == Duration::ZERO` means "force stop" — SIGKILL escalation
/// fires immediately after SIGTERM with no wait window. Non-zero
/// grace allows the target up to that window for clean shutdown.
///
/// Errors are non-fatal; per-step failures (failed signal, failed
/// container stop) emit `ATO-WARN` lines and the function continues
/// with the remaining steps so one stuck primitive doesn't block the
/// rest of teardown.
pub(crate) fn stop_orchestration_service_record(
    service: &StoredOrchestrationService,
    grace: Duration,
) -> Result<bool> {
    let mut signalled = false;

    if let Some(container_id) = service.container_id.as_deref() {
        if !container_id.is_empty() {
            // OCI services: stop + remove via bollard. Build a local
            // tokio runtime + client for this call. The runtime is
            // cheap (current-thread) and only constructed when a
            // container_id is actually present.
            match stop_container_via_bollard(container_id, &service.name, grace) {
                Ok(true) => signalled = true,
                Ok(false) => {}
                Err(err) => {
                    eprintln!(
                        "ATO-WARN failed to stop OCI container {} for service '{}': {}",
                        container_id, service.name, err
                    );
                }
            }
            // OCI path: container_id-bearing services don't carry a
            // local pid, so skip the pid/listener fallbacks below.
            return Ok(signalled);
        }
    }

    if let Some(pid) = service.local_pid {
        #[cfg(unix)]
        {
            if pid > 0 {
                let force = grace == Duration::ZERO;
                let signal = if force { libc::SIGKILL } else { libc::SIGTERM };

                // Strategy in order of preference:
                //
                //   1. Process-group kill when the recorded
                //      `local_pid` is currently a pgroup leader
                //      (`getpgid(pid) == pid`). The
                //      `nacelle::manager::supervisor` spawn path sets
                //      this via `cmd.process_group(0)`, so a
                //      `kill(-pgid, sig)` reaps the wrapper AND every
                //      descendant atomically.
                //
                //   2. Descendant walk + per-pid kill when (1) doesn't
                //      apply — the typical orchestration session:
                //      ato-cli spawns nacelle (pid recorded as
                //      `local_pid`), nacelle internally launches `uv
                //      run` / `npm run dev` wrappers via the
                //      direct/sandbox-exec launchers (which inherit
                //      ato-cli's pgroup, not their own). A plain
                //      per-pid SIGKILL on the recorded pid kills
                //      nacelle but leaves the wrappers it spawned
                //      alive as init-reparented orphans (#92 AODD
                //      Phase 2 → #111). Capture descendants via
                //      `pgrep -P` recursively BEFORE signaling so we
                //      don't lose them when reparenting happens, then
                //      signal recorded pid, then signal each
                //      descendant. Idempotent on stale/dead pids
                //      (ESRCH is silently swallowed).
                //
                //   3. The lsof-by-published-port fallback (#109)
                //      stays as a belt-and-suspenders for any
                //      listener we still missed (e.g. a service that
                //      spawned outside the recorded subtree).
                let mut signaled_via_pgroup = false;
                let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
                if pgid > 0 && pgid == pid as libc::pid_t {
                    let ret = unsafe { libc::kill(-pgid, signal) };
                    if ret == 0 {
                        signalled = true;
                        signaled_via_pgroup = true;
                    } else {
                        let err = std::io::Error::last_os_error();
                        if err.raw_os_error() != Some(libc::ESRCH) {
                            eprintln!(
                                "ATO-WARN failed to signal process group {} for service '{}': {}",
                                pgid, service.name, err
                            );
                        }
                    }
                }

                if !signaled_via_pgroup {
                    // Capture descendants BEFORE signaling — once the
                    // recorded pid is killed, its children are
                    // reparented to init and `pgrep -P recorded`
                    // returns nothing, leaking the wrappers.
                    let descendants = collect_descendant_pids(pid as u32, &service.name);

                    // Per-pid kill on the recorded pid first.
                    let ret = unsafe { libc::kill(pid as libc::pid_t, signal) };
                    if ret == 0 {
                        signalled = true;
                    } else {
                        let err = std::io::Error::last_os_error();
                        if err.raw_os_error() != Some(libc::ESRCH) {
                            eprintln!(
                                "ATO-WARN failed to signal local service '{}' (pid {}): {}",
                                service.name, pid, err
                            );
                        }
                    }

                    // Then signal every descendant we captured. Each
                    // signal is idempotent — ESRCH means the process
                    // already died, which is the desired end state.
                    for child_pid in descendants {
                        let ret = unsafe { libc::kill(child_pid as libc::pid_t, signal) };
                        if ret == 0 {
                            signalled = true;
                        } else {
                            let err = std::io::Error::last_os_error();
                            if err.raw_os_error() != Some(libc::ESRCH) {
                                eprintln!(
                                    "ATO-WARN failed to signal descendant {} (under recorded pid {}, service '{}'): {}",
                                    child_pid, pid, service.name, err
                                );
                            }
                        }
                    }
                }

                // Non-zero grace: poll for graceful exit, then SIGKILL
                // escalation. With grace=0 we already sent SIGKILL
                // above so this is a no-op.
                if !force {
                    let started = Instant::now();
                    while started.elapsed() < grace {
                        if !pid_alive(pid) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    if pid_alive(pid) {
                        let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
                    }
                }
            }

            // Belt-and-suspenders for the wrapper-vs-workload PID gap
            // (#108): even with the pgroup kill above, older session
            // records (no pgroup, or pgid != recorded pid) and any
            // spawn mode that drops out of the recorded pgroup land
            // here. Look up the current listener via `lsof` and
            // signal anything that's still bound to `published_port`.
            // Idempotent (returns false when the port is already free
            // or the resolved pid matches what we just signaled).
            if let Some(port) = service.published_port {
                if kill_listeners_on_published_port(port, pid, grace, &service.name) {
                    signalled = true;
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (pid, grace);
            eprintln!(
                "ATO-WARN local orchestration service teardown is unix-only; service '{}' (pid {}) was left running",
                service.name, pid
            );
        }
    }

    Ok(signalled)
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

/// Walk the descendant tree of `root_pid` via `pgrep -P` (BFS) and
/// return every transitive child's pid. Used by
/// `stop_orchestration_service_record` to capture the wrapper subtree
/// BEFORE killing the recorded pid (#111). Once the recorded pid
/// dies, its children get reparented to init and `pgrep -P` no longer
/// finds them — by capturing first, we keep an explicit list of pids
/// to follow up on.
///
/// Best-effort: failures (missing `pgrep`, malformed output, fork
/// races) yield an empty / partial list and a debug-level message.
/// The caller still has the lsof-by-published-port fallback (#109)
/// for any listener we miss here.
///
/// Bounded depth (32 levels) and bounded total pids (256) so a
/// pathological process tree can't make teardown loop forever or
/// allocate without limit.
#[cfg(unix)]
pub(crate) fn collect_descendant_pids(root_pid: u32, service_name: &str) -> Vec<u32> {
    use std::collections::VecDeque;

    const MAX_DEPTH: usize = 32;
    const MAX_PIDS: usize = 256;

    let mut collected: Vec<u32> = Vec::new();
    let mut frontier: VecDeque<(u32, usize)> = VecDeque::new();
    frontier.push_back((root_pid, 0));

    while let Some((parent, depth)) = frontier.pop_front() {
        if depth >= MAX_DEPTH || collected.len() >= MAX_PIDS {
            break;
        }
        let output = match Command::new("pgrep")
            .args(["-P", &parent.to_string()])
            .output()
        {
            Ok(o) => o,
            Err(err) => {
                tracing::debug!(
                    parent,
                    service = service_name,
                    error = %err,
                    "collect_descendant_pids: pgrep -P failed"
                );
                continue;
            }
        };
        // pgrep exits 1 when the parent has no children — not an error.
        if !output.status.success() && output.status.code() != Some(1) {
            tracing::debug!(
                parent,
                service = service_name,
                exit = ?output.status.code(),
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "collect_descendant_pids: pgrep returned non-success"
            );
            continue;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        for token in stdout.split_whitespace() {
            let Ok(child) = token.parse::<u32>() else {
                continue;
            };
            if child == 0 || child == parent || collected.contains(&child) {
                continue;
            }
            collected.push(child);
            frontier.push_back((child, depth + 1));
            if collected.len() >= MAX_PIDS {
                break;
            }
        }
    }

    collected
}

#[cfg(not(unix))]
pub(crate) fn collect_descendant_pids(_root_pid: u32, _service_name: &str) -> Vec<u32> {
    Vec::new()
}

/// Kill any process currently bound to `port` on `127.0.0.1` whose pid
/// differs from `recorded_pid` (which the caller already attempted to
/// signal). Used as the wrapper-vs-workload fallback (#108): when ato
/// spawned the service via `npm run dev` / `uv run` / a shell wrapper,
/// the recorded `local_pid` is the wrapper and the actual listener is
/// its child.
///
/// `grace == Duration::ZERO` → SIGKILL; otherwise SIGTERM.
///
/// Returns `true` iff at least one previously-unsignaled pid was
/// successfully killed.
#[cfg(unix)]
pub(crate) fn kill_listeners_on_published_port(
    port: u16,
    recorded_pid: i32,
    grace: Duration,
    service_name: &str,
) -> bool {
    let listener_pids = match listener_pids_on_port(port) {
        Ok(pids) => pids,
        Err(err) => {
            eprintln!(
                "ATO-WARN failed to enumerate listeners on port {} for service '{}': {}",
                port, service_name, err
            );
            return false;
        }
    };
    let signal = if grace == Duration::ZERO {
        libc::SIGKILL
    } else {
        libc::SIGTERM
    };
    let mut killed = false;
    for pid in listener_pids {
        if pid as i32 == recorded_pid {
            // Already handled by the recorded-pid kill above.
            continue;
        }
        let ret = unsafe { libc::kill(pid as libc::pid_t, signal) };
        if ret == 0 {
            killed = true;
        } else {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                eprintln!(
                    "ATO-WARN failed to signal port-{} listener (pid {}) for service '{}': {}",
                    port, pid, service_name, err
                );
            }
        }
    }
    killed
}

#[cfg(not(unix))]
pub(crate) fn kill_listeners_on_published_port(
    _port: u16,
    _recorded_pid: i32,
    _grace: Duration,
    _service_name: &str,
) -> bool {
    false
}

/// Best-effort resolve "which pids are listening on TCP `port` on the
/// loopback right now?" using `lsof`. Returns the parsed pid list
/// (may be empty if nothing is bound). Limited to TCP / IPv4 LISTEN to
/// match how managed services bind their sockets — the orchestrator's
/// readiness probe only ever waits on TCP listeners on 127.0.0.1.
#[cfg(unix)]
pub(crate) fn listener_pids_on_port(port: u16) -> Result<Vec<u32>> {
    let output = Command::new("lsof")
        .args(["-nP", "-t", &format!("-iTCP:{}", port), "-sTCP:LISTEN"])
        .output()
        .with_context(|| format!("failed to invoke lsof for port {}", port))?;
    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "lsof exited {:?} for port {}: {}",
            output.status.code(),
            port,
            stderr.trim()
        );
    }
    let mut pids = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(pid) = trimmed.parse::<u32>() {
            pids.push(pid);
        }
    }
    Ok(pids)
}

/// Stop + remove an OCI container by id via bollard. Builds a
/// current-thread tokio runtime locally; cheap when only called for
/// services that actually have a `container_id`. Returns true if the
/// stop call succeeded.
fn stop_container_via_bollard(
    container_id: &str,
    service_name: &str,
    grace: Duration,
) -> Result<bool> {
    use capsule_core::runtime::oci::OciRuntimeClient as _;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .with_context(|| "failed to build tokio runtime for OCI teardown")?;
    let client = capsule_core::runtime::oci::BollardOciRuntimeClient::connect_default()
        .with_context(|| "failed to connect to OCI engine for container stop")?;

    // Stop timeout matches the orchestrator's
    // `OCI_STOP_TIMEOUT_SECS` constant when force; otherwise honor
    // the caller's grace window (clamped to a u16 second value).
    let stop_timeout_secs: u16 = if grace == Duration::ZERO {
        0
    } else {
        grace.as_secs().min(u16::MAX as u64) as u16
    };
    let stop_result = rt.block_on(client.stop_container(container_id, stop_timeout_secs.into()));
    let stopped = match stop_result {
        Ok(()) => true,
        Err(err) => {
            eprintln!(
                "ATO-WARN OCI stop_container({}) for service '{}' failed: {}",
                container_id, service_name, err
            );
            false
        }
    };

    // Always attempt remove (force=true when grace=0) — leftover
    // containers occupy port mappings and complicate next-launch.
    let force_remove = grace == Duration::ZERO;
    if let Err(err) = rt.block_on(client.remove_container(container_id, force_remove)) {
        eprintln!(
            "ATO-WARN OCI remove_container({}) for service '{}' failed: {}",
            container_id, service_name, err
        );
    }

    Ok(stopped)
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
        // local_pid is Some(0); the pid>0 gate skips kill. published_port
        // is None so the lsof fallback is also skipped. No container.
        let signalled = stop_orchestration_service_record(&service, Duration::from_secs(0))
            .expect("ok");
        assert!(!signalled);
    }
}

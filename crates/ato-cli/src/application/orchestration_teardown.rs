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

    if let Some(container_id) = service.container_id.as_deref()
        && !container_id.is_empty()
    {
        // OCI services: stop + remove via bollard. Build a local
        // tokio runtime + client for this call. The runtime is
        // cheap (current-thread) and only constructed when a
        // container_id is actually present.
        match stop_container_via_provider_cli(container_id, &service.name, grace) {
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
            if let Some(port) = service.published_port
                && kill_listeners_on_published_port(port, pid, grace, &service.name)
            {
                signalled = true;
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

/// Stop + remove an OCI container by id via the provider CLI. Session
/// orchestration uses the Podman provider, so teardown must not route
/// through a stale Docker-compatible socket and hang before cleanup.
/// Returns the ordered list of OCI CLI tools to try for container stop/rm.
///
/// `podman` is always first. `docker` is appended when `DOCKER_HOST` is set
/// (an explicit engine endpoint was configured, so no hang risk) or when
/// `ATO_ENABLE_DOCKER=1`. We do NOT add `docker` unconditionally: on
/// podman-only hosts with the Docker CLI installed but daemon down,
/// `docker stop` can hang waiting on the unix socket indefinitely.
fn container_stop_cli_candidates() -> Vec<&'static str> {
    let docker_host_set = std::env::var("DOCKER_HOST").is_ok();
    let docker_enabled =
        std::env::var(ato_session_core::process::ATO_ENABLE_DOCKER_ENV).as_deref() == Ok("1");
    if docker_host_set || docker_enabled {
        vec!["podman", "docker"]
    } else {
        vec!["podman"]
    }
}

fn stop_container_via_provider_cli(
    container_id: &str,
    service_name: &str,
    grace: Duration,
) -> Result<bool> {
    let stop_timeout_secs = if grace == Duration::ZERO {
        0
    } else {
        grace.as_secs().min(u16::MAX as u64) as u16
    };

    // Try each CLI in order. The first one that successfully reaches its
    // daemon and stops (or confirms already-gone for) the container wins;
    // remaining CLIs are skipped. A CLI that cannot be executed (binary
    // absent) or whose daemon is unreachable is silently skipped so that
    // the next candidate gets a chance. This allows a session that was
    // started via the Docker-compatible bollard path to be torn down
    // correctly even when Podman is also installed but not running.
    for cli in container_stop_cli_candidates() {
        let stop_out = Command::new(cli)
            .args([
                "stop",
                "--time",
                &stop_timeout_secs.to_string(),
                container_id,
            ])
            .output();

        match stop_out {
            Err(_) => {
                // Binary not found or could not be executed — try next CLI.
                continue;
            }
            Ok(out) if out.status.success() => {
                // Container stopped via this CLI. Remove it with the same CLI
                // and return.
                let rm_out = Command::new(cli)
                    .args(["rm", "--force", container_id])
                    .output();
                if let Ok(rm) = rm_out
                    && !rm.status.success()
                {
                    let rm_err = String::from_utf8_lossy(&rm.stderr);
                    eprintln!(
                        "ATO-WARN {cli} rm({container_id}) for service \
                             '{service_name}' failed: {}",
                        rm_err.trim()
                    );
                }
                return Ok(true);
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let stderr_str = stderr.trim();
                // "Container not found" in this runtime means it either never
                // existed here or was already removed — try the next CLI in
                // case it lives under a different engine (e.g., the session
                // was started via Docker but Podman is also installed).
                if is_container_not_found(stderr_str) {
                    continue;
                }
                // Any other failure (daemon unavailable, permission error,
                // …): emit a warning and try the next CLI.
                eprintln!(
                    "ATO-WARN {cli} stop({container_id}) for service \
                     '{service_name}' failed: {stderr_str}"
                );
            }
        }
    }

    // No CLI successfully stopped the container (already gone or all
    // unreachable). Treat as not-signalled so the caller can decide.
    Ok(false)
}

/// Outcome of a `remove_network_if_present` call. Returned to the
/// caller so `stop_session` can surface failures in its output
/// rather than silently losing the cleanup result.
#[derive(Debug, PartialEq)]
pub(crate) enum NetworkRemovalOutcome {
    /// Network was found and removed successfully.
    Removed,
    /// Network was not found — already gone (no-op, counts as success).
    AlreadyGone,
    /// Network name did not match the `ato-` prefix; removal skipped
    /// to avoid accidentally removing unrelated user networks.
    SkippedNotAtoManaged,
    /// Removal was attempted but failed. The contained string
    /// describes the last error seen across bollard + subprocess attempts.
    Failed(String),
}

/// Returns `true` when `name` looks like an Ato-managed OCI network.
/// Ato orchestrator always names networks `ato-{sanitize(name)}-{hash8}-{pid}`,
/// so an `ato-` prefix is a sufficient guard.
pub(crate) fn is_ato_managed_network(name: &str) -> bool {
    name.starts_with("ato-")
}

/// Argument vector for `<cmd> network rm [...] <name>`.
///
/// Podman gets `--force` so a network that still has an attached endpoint at
/// removal time is force-disconnected and removed rather than left orphaned
/// (#450). Docker has no `--force` for `network rm`, so it stays plain to avoid
/// an "unknown flag" error.
fn network_rm_args<'a>(cmd: &str, network_name: &'a str) -> Vec<&'a str> {
    if cmd == "podman" {
        vec!["network", "rm", "--force", network_name]
    } else {
        vec!["network", "rm", network_name]
    }
}

/// Returns `true` when `msg` indicates the network is already gone
/// (not present) rather than a real removal failure.
fn is_network_not_found(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("no such network")
        || lower.contains("not found")
        || lower.contains("does not exist")
        || lower.contains("network not found")
}

fn is_container_not_found(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("no such container")
        || lower.contains("not found")
        || lower.contains("does not exist")
        || lower.contains("container not known")
}

/// Remove a Docker/Podman network by name after all containers in the
/// session have been stopped.
///
/// Returns a [`NetworkRemovalOutcome`] so the caller can decide
/// whether to surface the failure to the user.
///
/// * Only removes networks whose name starts with `ato-` (guard against
///   accidentally removing unrelated user networks).
/// * Tries bollard first (consistent with container stop), then falls back
///   to a subprocess call (`podman network rm` / `docker network rm`).
pub(crate) fn remove_network_if_present(network_name: &str) -> NetworkRemovalOutcome {
    if network_name.is_empty() || !is_ato_managed_network(network_name) {
        return NetworkRemovalOutcome::SkippedNotAtoManaged;
    }

    try_remove_network_subprocess(network_name)
}

fn try_remove_network_subprocess(network_name: &str) -> NetworkRemovalOutcome {
    let mut last_error = String::new();
    // Try podman first, then docker — but only consult docker when the
    // user has explicitly opted in (`ATO_ENABLE_DOCKER=1`). On podman-only
    // hosts where the docker CLI is installed but its daemon is down,
    // `docker network rm` hangs on the unix socket with no internal
    // timeout, blocking session teardown indefinitely.
    for cmd in ato_session_core::process::oci_probe_runtimes() {
        // `--force` (podman only) disconnects any lingering endpoints before
        // removing the network. Without it, `network rm` fails with "network
        // is being used" when a container is still attached at removal time —
        // e.g. a teardown race, or a sidecar/proxy endpoint not in the service
        // set — leaving the `ato-*` network orphaned (#450). `docker network rm`
        // has no such flag, so it stays plain.
        let result = Command::new(cmd)
            .args(network_rm_args(cmd, network_name))
            .output();
        match result {
            Ok(out) if out.status.success() => return NetworkRemovalOutcome::Removed,
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let msg = stderr.trim().to_string();
                if is_network_not_found(&msg) {
                    return NetworkRemovalOutcome::AlreadyGone;
                }
                if !msg.is_empty() {
                    last_error = format!("{cmd} network rm: {msg}");
                }
            }
            Err(err) => {
                // binary not found — record and try next
                last_error = format!("{cmd}: {err}");
            }
        }
    }
    if last_error.is_empty() {
        NetworkRemovalOutcome::Failed("unknown error".to_string())
    } else {
        NetworkRemovalOutcome::Failed(last_error)
    }
}

/// Outcome of a [`remove_volume_if_present`] call (#444). Mirrors
/// [`NetworkRemovalOutcome`] so `stop_session` can surface failures.
#[derive(Debug, PartialEq)]
pub(crate) enum VolumeRemovalOutcome {
    /// Volume was found and removed (or was already gone — `--force` makes
    /// removing a missing volume a success).
    Removed,
    /// Name did not match the `ato-state-` prefix; removal skipped to avoid
    /// touching volumes Ato did not create.
    SkippedNotAtoManaged,
    /// Removal was attempted but failed across all candidate CLIs.
    Failed(String),
}

/// Returns `true` when `name` looks like an Ato-managed engine state volume.
/// [`engine_state_volume_name`](capsule::runtime::oci::engine_state_volume_name)
/// always prefixes `ato-state-`, so that prefix is a sufficient guard.
fn is_ato_managed_volume(name: &str) -> bool {
    name.starts_with("ato-state-")
}

/// Remove an ephemeral engine-managed state volume by name after its
/// containers have stopped (#444).
///
/// * Only removes volumes whose name starts with `ato-state-` (guard against
///   touching unrelated user volumes).
/// * Tries each opted-in OCI CLI (`podman`, and `docker` when enabled),
///   matching the network/container teardown candidate policy.
/// * `--force` so an already-gone volume counts as removed (idempotent).
pub(crate) fn remove_volume_if_present(volume_name: &str) -> VolumeRemovalOutcome {
    if volume_name.is_empty() || !is_ato_managed_volume(volume_name) {
        return VolumeRemovalOutcome::SkippedNotAtoManaged;
    }

    let mut last_error = String::new();
    for cmd in ato_session_core::process::oci_probe_runtimes() {
        let result = Command::new(cmd)
            .args(["volume", "rm", "--force", volume_name])
            .output();
        match result {
            Ok(out) if out.status.success() => return VolumeRemovalOutcome::Removed,
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let msg = stderr.trim().to_string();
                if !msg.is_empty() {
                    last_error = format!("{cmd} volume rm: {msg}");
                }
            }
            Err(err) => {
                last_error = format!("{cmd}: {err}");
            }
        }
    }
    if last_error.is_empty() {
        VolumeRemovalOutcome::Failed("unknown error".to_string())
    } else {
        VolumeRemovalOutcome::Failed(last_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn is_ato_managed_volume_requires_ato_state_prefix() {
        assert!(is_ato_managed_volume("ato-state-deadbeef0000-pgdata"));
        assert!(!is_ato_managed_volume("postgres-data"));
        assert!(!is_ato_managed_volume("ato-net-abc")); // network-ish, not state
        assert!(!is_ato_managed_volume(""));
    }

    #[test]
    fn remove_volume_skips_non_ato_managed_names() {
        // Guard: never shells out to remove a volume Ato did not create.
        assert_eq!(
            remove_volume_if_present("user-postgres-data"),
            VolumeRemovalOutcome::SkippedNotAtoManaged
        );
        assert_eq!(
            remove_volume_if_present(""),
            VolumeRemovalOutcome::SkippedNotAtoManaged
        );
    }

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
        let signalled =
            stop_orchestration_service_record(&service, Duration::from_secs(0)).expect("ok");
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
        let signalled =
            stop_orchestration_service_record(&service, Duration::from_secs(0)).expect("ok");
        assert!(!signalled);
    }

    // --- #273 network guard tests ---

    #[test]
    fn is_ato_managed_network_accepts_orchestrator_names() {
        assert!(is_ato_managed_network("ato-excalidraw-ad4fe71f-81568"));
        assert!(is_ato_managed_network("ato-affine-12345678-99999"));
        assert!(is_ato_managed_network("ato-dify-abcdef00-12345"));
        assert!(is_ato_managed_network("ato-"));
    }

    #[test]
    fn is_ato_managed_network_rejects_non_ato_names() {
        assert!(!is_ato_managed_network("default"));
        assert!(!is_ato_managed_network("bridge"));
        assert!(!is_ato_managed_network("host"));
        assert!(!is_ato_managed_network("my-random-network"));
        assert!(!is_ato_managed_network(""));
    }

    #[test]
    fn remove_network_skips_non_ato_managed() {
        let outcome = remove_network_if_present("my-random-network");
        assert_eq!(outcome, NetworkRemovalOutcome::SkippedNotAtoManaged);
    }

    // --- #450 network rm --force argument tests ---

    #[test]
    fn network_rm_args_podman_uses_force() {
        // `--force` disconnects lingering endpoints so a still-attached network
        // does not leak (#450).
        assert_eq!(
            network_rm_args("podman", "ato-blinko-abc12345-42"),
            vec!["network", "rm", "--force", "ato-blinko-abc12345-42"]
        );
    }

    #[test]
    fn network_rm_args_docker_stays_plain() {
        // `docker network rm` has no `--force`; adding it would be an unknown flag.
        assert_eq!(
            network_rm_args("docker", "ato-blinko-abc12345-42"),
            vec!["network", "rm", "ato-blinko-abc12345-42"]
        );
    }

    #[test]
    fn remove_network_skips_empty_name() {
        let outcome = remove_network_if_present("");
        assert_eq!(outcome, NetworkRemovalOutcome::SkippedNotAtoManaged);
    }

    #[test]
    fn is_network_not_found_detects_common_messages() {
        assert!(is_network_not_found("Error: no such network: foo"));
        assert!(is_network_not_found("network not found"));
        assert!(is_network_not_found("Network does not exist"));
        assert!(is_network_not_found("Error response: not found"));
        assert!(!is_network_not_found("permission denied"));
        assert!(!is_network_not_found(""));
    }

    // --- #406 container_stop_cli_candidates tests ---
    //
    // These tests mutate process-global env vars, so they must not run
    // concurrently with each other. A module-level Mutex serializes them
    // without requiring a proc-macro dependency (serial_test, etc.).
    static ENV_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

    #[test]
    fn stop_cli_candidates_podman_only_without_docker_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        // When neither DOCKER_HOST nor ATO_ENABLE_DOCKER is set, only podman
        // is tried so we don't accidentally hang on a down Docker daemon.
        let _g1 = EnvGuard::remove("DOCKER_HOST");
        let _g2 = EnvGuard::remove(ato_session_core::process::ATO_ENABLE_DOCKER_ENV);
        assert_eq!(container_stop_cli_candidates(), vec!["podman"]);
    }

    #[test]
    fn stop_cli_candidates_includes_docker_when_docker_host_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        // DOCKER_HOST means the user has an explicit endpoint — no hang risk.
        let _g1 = EnvGuard::set("DOCKER_HOST", "unix:///tmp/docker.sock");
        let _g2 = EnvGuard::remove(ato_session_core::process::ATO_ENABLE_DOCKER_ENV);
        assert_eq!(container_stop_cli_candidates(), vec!["podman", "docker"]);
    }

    #[test]
    fn stop_cli_candidates_includes_docker_when_ato_enable_docker_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g1 = EnvGuard::remove("DOCKER_HOST");
        let _g2 = EnvGuard::set(ato_session_core::process::ATO_ENABLE_DOCKER_ENV, "1");
        assert_eq!(container_stop_cli_candidates(), vec!["podman", "docker"]);
    }

    #[test]
    fn stop_cli_candidates_podman_only_when_ato_enable_docker_not_one() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g1 = EnvGuard::remove("DOCKER_HOST");
        let _g2 = EnvGuard::set(ato_session_core::process::ATO_ENABLE_DOCKER_ENV, "0");
        assert_eq!(container_stop_cli_candidates(), vec!["podman"]);
    }

    /// RAII guard that restores an env var to its previous value on drop.
    struct EnvGuard {
        key: String,
        prev: Option<String>,
    }
    impl EnvGuard {
        fn set(key: &str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key: key.to_string(),
                prev,
            }
        }
        fn remove(key: &str) -> Self {
            let prev = std::env::var(key).ok();
            unsafe {
                std::env::remove_var(key);
            }
            Self {
                key: key.to_string(),
                prev,
            }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => unsafe { std::env::set_var(&self.key, v) },
                None => unsafe { std::env::remove_var(&self.key) },
            }
        }
    }
}

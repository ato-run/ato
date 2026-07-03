//! Desktop Runner local run agent — spawn `ato run` on the Desktop Runner
//! cold-OCI path when the embedded PWA fires `ato://run`, and surface the
//! outcome (session receipt or structured placement error) in the activity log.
//!
//! Mirrors `runner_agent`'s foreground-only model. The `ato run` child is short
//! — it returns after starting the container (or fails fast at the placement
//! gate), so a background waiter thread owns the child, waits for exit, reads
//! the stdout/stderr log files, and posts a rendered result to a thread-safe
//! pending-activity queue that the render loop drains into `AppState`.
//!
//! Trust boundary: the privileged `ato://run` intent is origin-gated by
//! `crate::intent` before it ever reaches here — only a trusted Ato Home pane
//! can request a local run. This module performs the privileged local spawn and
//! honest result surfacing; it never re-validates the origin.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::orchestrator;
use crate::state::{ActivityEntry, ActivityTone};

/// Pending activity entries produced by background run-waiter threads. Drained
/// into `AppState` by the render loop, next to `bridge.drain_activity()`.
static PENDING_ACTIVITY: Mutex<Vec<ActivityEntry>> = Mutex::new(Vec::new());

/// PID of the in-flight run child, mirrored so the cx-less [`shutdown`] hook
/// can group-kill it without locking the waiter thread's owned child. `0` =
/// no run in flight.
static CURRENT_PID: AtomicU32 = AtomicU32::new(0);

fn push_pending(tone: ActivityTone, message: impl Into<String>) {
    let mut q = PENDING_ACTIVITY
        .lock()
        .expect("desktop_run_agent: pending activity lock poisoned");
    q.push(ActivityEntry {
        tone,
        message: message.into(),
    });
}

/// Drain pending run-result activity entries (called from the render loop).
pub fn drain_pending_activity() -> Vec<ActivityEntry> {
    std::mem::take(
        &mut *PENDING_ACTIVITY
            .lock()
            .expect("desktop_run_agent: pending activity lock poisoned"),
    )
}

/// Launch a capsule on the Desktop Runner local cold-OCI path. Returns
/// immediately; the run outcome is surfaced in the activity log asynchronously
/// via [`drain_pending_activity`]. The caller pushes the "run started" activity
/// synchronously so the user gets instant feedback; this function posts the
/// success/failure result when the `ato run` child exits.
///
/// `ready_state_enabled` forwards to the CLI via `ATO_READY_STATE_ENABLED=1`;
/// pass `false` for the M3 cold-OCI path (local Ready-State restore is not
/// supported, and the CLI's placement gate would refuse to cold-start with
/// Ready-State on).
pub fn launch(source: &str, run_id: Option<&str>, ready_state_enabled: bool) -> Result<(), String> {
    let run = orchestrator::spawn_desktop_runner_run(source, ready_state_enabled)
        .map_err(|e| format!("could not start Desktop Runner run for {source}: {e:#}"))?;
    CURRENT_PID.store(run.child.id(), Ordering::SeqCst);

    let source = source.to_string();
    let run_id = run_id.map(str::to_string);
    let stdout_log = run.stdout_log.clone();
    let stderr_log = run.stderr_log.clone();
    std::thread::spawn(move || {
        let mut child = run.child;
        let exit = child.wait();
        // Clear the in-flight PID only if it still points at this child; a
        // later run may have already swapped in a new PID.
        let _ = CURRENT_PID.compare_exchange(child.id(), 0, Ordering::SeqCst, Ordering::SeqCst);
        let stdout = std::fs::read_to_string(&stdout_log).unwrap_or_default();
        let stderr = std::fs::read_to_string(&stderr_log).unwrap_or_default();
        let (tone, message) = match exit {
            Ok(status) => render_run_result(
                status.success(),
                &stdout,
                &stderr,
                &source,
                run_id.as_deref(),
            ),
            Err(err) => (
                ActivityTone::Error,
                format!("Run failed for {source}: wait failed: {err}"),
            ),
        };
        push_pending(tone, message);
    });
    Ok(())
}

/// Pure renderer for a completed Desktop Runner run. Unit-tested.
///
/// - success + parseable session receipt → `Info` with the session id + guest
///   class (the user sees the run is live and where it is running).
/// - failure → `Error` surfacing the structured placement error the CLI printed
///   to stderr (PR 1: `platform` / `local backend` / `reasons` / `next action`),
///   so the user can tell "host requirement unmet" from any other failure and
///   gets an actionable next step.
pub fn render_run_result(
    exit_ok: bool,
    stdout: &str,
    stderr: &str,
    source: &str,
    run_id: Option<&str>,
) -> (ActivityTone, String) {
    let detail = run_id.map(|id| format!(" (run {id})")).unwrap_or_default();
    if exit_ok {
        if let Some(session) = parse_session_receipt(stdout) {
            return (
                ActivityTone::Info,
                format!(
                    "Run ready{detail}: session {} for {source} (guest {}/{}, image {})",
                    session.session_id, session.guest_os, session.guest_arch, session.image
                ),
            );
        }
        // Exit 0 but no parseable receipt — report the last stdout line without
        // claiming a session id we cannot confirm.
        let tail = stdout.trim().lines().last().unwrap_or("").trim();
        return (
            ActivityTone::Info,
            if tail.is_empty() {
                format!("Run completed{detail} for {source}.")
            } else {
                format!("Run completed{detail} for {source}: {tail}")
            },
        );
    }
    // Failure: prefer the structured placement error block (PR 1) when present,
    // else fall back to a short stderr tail.
    let reason = extract_placement_error(stderr).unwrap_or_else(|| {
        let tail = tail_lines(stderr, 6);
        if tail.is_empty() {
            format!("Run failed{detail} for {source}.")
        } else {
            format!("Run failed{detail} for {source}:\n{tail}")
        }
    });
    (ActivityTone::Error, reason)
}

/// Parsed Desktop Runner cold-OCI session receipt (the subset we surface).
#[derive(Debug, Default, PartialEq, Eq)]
struct SessionReceipt {
    session_id: String,
    guest_os: String,
    guest_arch: String,
    image: String,
}

/// Parse the session receipt JSON the CLI prints to stdout on a successful
/// cold-OCI run. Tolerates leading non-JSON lines by parsing from the first
/// `{`. Returns `None` if any surfaced field is missing or the stdout is not
/// valid JSON — the caller then reports a generic success.
fn parse_session_receipt(stdout: &str) -> Option<SessionReceipt> {
    let start = stdout.find('{')?;
    let json = &stdout[start..];
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    Some(SessionReceipt {
        session_id: v.get("session_id")?.as_str()?.to_string(),
        guest_os: v.get("guest_os")?.as_str()?.to_string(),
        guest_arch: v.get("guest_arch")?.as_str()?.to_string(),
        image: v.get("image")?.as_str()?.to_string(),
    })
}

/// Extract the structured placement error block the CLI prints to stderr on a
/// placement-gate failure (PR 1): the line starting "Desktop Runner will not
/// run this capsule locally" plus the following indented `platform:` /
/// `local backend:` / `reasons:` / `next action:` lines. Returns the whole
/// block so the activity log shows every reason and the actionable next step.
fn extract_placement_error(stderr: &str) -> Option<String> {
    const MARKER: &str = "Desktop Runner will not run this capsule locally";
    let start = stderr.find(MARKER)?;
    let block: String = stderr[start..]
        .lines()
        .take_while(|line| {
            line.starts_with(MARKER)
                || line.trim_start().starts_with("platform:")
                || line.trim_start().starts_with("local backend:")
                || line.trim_start().starts_with("reasons:")
                || line.trim_start().starts_with("next action:")
        })
        .collect::<Vec<_>>()
        .join("\n");
    if block.is_empty() { None } else { Some(block) }
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Best-effort teardown: kill any in-flight run child's process group so a
/// local cold-OCI run does not outlive the Desktop. Safe to call without a GPUI
/// context (invoked from `window::begin_shutdown`).
pub fn shutdown() {
    let pid = CURRENT_PID.swap(0, Ordering::SeqCst);
    if pid > 1 {
        kill_process_tree(pid);
        tracing::info!(pid, "desktop_run_agent: run child terminated on shutdown");
    }
}

fn kill_process_tree(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: kill(2) with a negative pid signals the whole process group
        // led by `pid`. The run child is its own group leader (see
        // `spawn_desktop_runner_run`), so this reaps the `ato run` wrapper and
        // any container it spawned.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        use crate::proc_util::CommandNoWindowExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .no_console_window()
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLACEMENT_ERR: &str = "\
DESKTOP-RUNNER: probe complete
Desktop Runner will not run this capsule locally (suggest_managed_runner): no local Desktop Runner backend on macos/aarch64; use a managed runner
  platform: macos/aarch64
  local backend: unavailable
  reasons: [macos_too_old, apple_container_missing]
  next action: upgrade macOS to 26+ / install Apple `container` from https://github.com/apple/container, or use a managed runner / use a managed runner
Error: ...";

    const SESSION_STDOUT: &str = "\
DESKTOP-RUNNER: placement selected: local_cold_oci_candidate (guest linux/aarch64)
{
  \"session_id\": \"ato-desktop-demo-app-123-456\",
  \"provider_kind\": \"desktop\",
  \"substrate\": \"apple_containerization\",
  \"host_os\": \"macos\",
  \"host_arch\": \"aarch64\",
  \"guest_os\": \"linux\",
  \"guest_arch\": \"aarch64\",
  \"isolation_boundary\": \"vm_wrapped_container\",
  \"ready_state_kind\": \"cold_oci\",
  \"image\": \"docker.io/library/python:3-alpine\",
  \"container_name\": \"ato-desktop-demo-app-123-456\",
  \"port\": 8080,
  \"health_status\": \"running\",
  \"binding_required\": false,
  \"binding_leases\": 0,
  \"cleanup_ok\": true
}";

    #[test]
    fn success_with_receipt_surfaces_session_id_and_guest_class() {
        let (tone, msg) =
            render_run_result(true, SESSION_STDOUT, "", "community/hello", Some("run_7"));
        assert_eq!(tone, ActivityTone::Info);
        assert!(
            msg.contains("session ato-desktop-demo-app-123-456"),
            "{msg}"
        );
        assert!(msg.contains("guest linux/aarch64"), "{msg}");
        assert!(msg.contains("community/hello"), "{msg}");
        assert!(msg.contains("(run run_7)"), "{msg}");
    }

    #[test]
    fn success_without_receipt_reports_generic_completion() {
        let (tone, msg) = render_run_result(true, "started ok\n", "", "acme/app", None);
        assert_eq!(tone, ActivityTone::Info);
        assert!(msg.contains("Run completed"), "{msg}");
        assert!(msg.contains("acme/app"), "{msg}");
        assert!(!msg.contains("session"), "{msg}");
    }

    #[test]
    fn placement_failure_surfaces_structured_reasons_and_next_action() {
        let (tone, msg) = render_run_result(false, "", PLACEMENT_ERR, "acme/app", None);
        assert_eq!(tone, ActivityTone::Error);
        assert!(msg.contains("will not run this capsule locally"), "{msg}");
        assert!(msg.contains("macos_too_old"), "{msg}");
        assert!(msg.contains("apple_container_missing"), "{msg}");
        assert!(msg.contains("upgrade macOS"), "{msg}");
        assert!(msg.contains("platform: macos/aarch64"), "{msg}");
        assert!(msg.contains("local backend: unavailable"), "{msg}");
    }

    #[test]
    fn generic_failure_without_placement_block_falls_back_to_stderr_tail() {
        let stderr = "pulling image...\nError: network unreachable";
        let (tone, msg) = render_run_result(false, "", stderr, "acme/app", Some("run_1"));
        assert_eq!(tone, ActivityTone::Error);
        assert!(msg.contains("Run failed"), "{msg}");
        assert!(msg.contains("network unreachable"), "{msg}");
        assert!(msg.contains("(run run_1)"), "{msg}");
    }

    #[test]
    fn empty_failure_stderr_reports_generic_failure() {
        let (tone, msg) = render_run_result(false, "", "", "acme/app", None);
        assert_eq!(tone, ActivityTone::Error);
        assert!(msg.contains("Run failed"), "{msg}");
        assert!(msg.contains("acme/app"), "{msg}");
    }

    #[test]
    fn parse_session_receipt_tolerates_leading_non_json() {
        let s = "DESKTOP-RUNNER: placement selected: local_cold_oci_candidate\n\
                 {\"session_id\":\"s1\",\"guest_os\":\"linux\",\"guest_arch\":\"aarch64\",\"image\":\"img\"}";
        let parsed = parse_session_receipt(s).expect("parses");
        assert_eq!(parsed.session_id, "s1");
        assert_eq!(parsed.guest_os, "linux");
        assert_eq!(parsed.guest_arch, "aarch64");
        assert_eq!(parsed.image, "img");
    }

    #[test]
    fn parse_session_receipt_returns_none_for_missing_fields() {
        // Missing guest_os → None (the caller falls back to generic success).
        let s = "{\"session_id\":\"s1\",\"guest_arch\":\"aarch64\",\"image\":\"img\"}";
        assert!(parse_session_receipt(s).is_none());
        // Not JSON at all.
        assert!(parse_session_receipt("not json").is_none());
        // Empty.
        assert!(parse_session_receipt("").is_none());
    }

    #[test]
    fn extract_placement_error_returns_none_without_marker() {
        assert!(extract_placement_error("some other error\nError: boom").is_none());
        assert!(extract_placement_error("").is_none());
    }

    #[test]
    fn drain_pending_activity_returns_and_clears() {
        // Static lock — serialize against other tests in this module that push.
        push_pending(ActivityTone::Info, "test drain");
        let drained = drain_pending_activity();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].message, "test drain");
        assert!(drain_pending_activity().is_empty(), "drain must clear");
    }
}

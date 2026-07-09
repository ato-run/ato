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
//!
//! ## Run history
//!
//! Every completed run (success or failure) is also persisted to
//! [`DesktopRunHistoryStore`] (`~/.ato/desktop-runner-run-history.json`) by the
//! same waiter thread that renders the activity-log message, so a run's
//! outcome survives past the transient activity feed. See that type's doc
//! comment for why this is a sibling of
//! `system_capsule::ato_start::StartPageHistoryStore` rather than an extension
//! of it, and for the current (non-)status of UI surfacing.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

use crate::orchestrator;
use crate::state::{ActivityEntry, ActivityTone};

/// Pending activity entries produced by background run-waiter threads. Drained
/// into `AppState` by the render loop, next to `bridge.drain_activity()`.
static PENDING_ACTIVITY: Mutex<Vec<ActivityEntry>> = Mutex::new(Vec::new());

/// Single-flight guard: at most one Desktop Runner local cold-OCI run may be
/// in flight at a time. Acquired before spawn; released by the waiter thread
/// after the child exits (and by [`shutdown`] on Desktop exit). This is the
/// M3 policy choice — see PR 2 review: "single-flight is safer than allowing
/// multiple in-flight local cold-OCI runs in the initial version." A second
/// `ato://run` while a run is in flight is rejected with an activity warning
/// rather than spawning a second child.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// PID of the in-flight run child, mirrored so the cx-less [`shutdown`] hook
/// can group-kill it without locking the waiter thread's owned child. `0` =
/// no run in flight. Holds a meaningful value only while [`IN_FLIGHT`] is
/// `true`; the single-flight invariant guarantees at most one PID.
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

/// Try to acquire the single-flight slot. Returns `true` if acquired (caller
/// must release it via [`release_inflight`] when done), `false` if a run is
/// already in flight. Pure over the atomic; testable with serial tests.
fn try_acquire_inflight() -> bool {
    IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

/// Release the single-flight slot. Called by the waiter after the child exits,
/// and by [`shutdown`] after reaping the child.
fn release_inflight() {
    IN_FLIGHT.store(false, Ordering::SeqCst);
}

/// Whether a Desktop Runner local run is currently in flight. Exposed for
/// tests and for the dispatcher to decide whether to surface a "wait" hint.
pub fn is_in_flight() -> bool {
    IN_FLIGHT.load(Ordering::SeqCst)
}

/// Launch a capsule on the Desktop Runner local cold-OCI path. Returns
/// immediately; the run outcome is surfaced in the activity log asynchronously
/// via [`drain_pending_activity`]. The caller pushes the "run started" activity
/// synchronously so the user gets instant feedback; this function posts the
/// success/failure result when the `ato run` child exits.
///
/// **Single-flight:** at most one run may be in flight. A second call while a
/// run is in flight is rejected with an activity warning pushed to the pending
/// queue; no second child is spawned. This is the deliberate M3 policy choice
/// — duplicate `ato://run` intents (double-click / re-click while busy) must
/// not spawn overlapping children whose per-run logs could race.
///
/// `ready_state_enabled` forwards to the CLI via `ATO_READY_STATE_ENABLED=1`;
/// pass `false` for the M3 cold-OCI path (local Ready-State restore is not
/// supported, and the CLI's placement gate would refuse to cold-start with
/// Ready-State on).
pub fn launch(source: &str, run_id: Option<&str>, ready_state_enabled: bool) -> Result<(), String> {
    // Acquire the single-flight slot BEFORE spawning so a failed spawn releases
    // the slot cleanly without ever publishing a PID.
    if !try_acquire_inflight() {
        push_pending(
            ActivityTone::Warning,
            "A local Desktop Runner run is already starting/running; wait for it to finish before \
             starting another."
                .to_string(),
        );
        return Err(
            "a local Desktop Runner run is already in flight; refusing to spawn a second child"
                .to_string(),
        );
    }
    let run = match orchestrator::spawn_desktop_runner_run(source, ready_state_enabled) {
        Ok(r) => r,
        Err(e) => {
            release_inflight();
            return Err(format!(
                "could not start Desktop Runner run for {source}: {e:#}"
            ));
        }
    };
    CURRENT_PID.store(run.child.id(), Ordering::SeqCst);

    let source = source.to_string();
    let run_id = run_id.map(str::to_string);
    // Capture this run's unique log paths by value so the waiter reads its
    // OWN logs even if a later run (after the slot is released) writes to
    // different per-run paths.
    let stdout_log = run.stdout_log.clone();
    let stderr_log = run.stderr_log.clone();
    let my_pid = run.child.id();
    std::thread::spawn(move || {
        let mut child = run.child;
        let exit = child.wait();
        // Clear the in-flight PID only if it still points at this child, then
        // release the single-flight slot so the next intent may start.
        let _ = CURRENT_PID.compare_exchange(my_pid, 0, Ordering::SeqCst, Ordering::SeqCst);
        release_inflight();
        let stdout = std::fs::read_to_string(&stdout_log).unwrap_or_default();
        let stderr = std::fs::read_to_string(&stderr_log).unwrap_or_default();
        let completed_at = now_unix_secs();
        let (tone, message, history_entry) = match exit {
            Ok(status) => {
                let exit_ok = status.success();
                let (tone, message) =
                    render_run_result(exit_ok, &stdout, &stderr, &source, run_id.as_deref());
                let entry = build_history_entry(
                    exit_ok,
                    &stdout,
                    &stderr,
                    &source,
                    run_id.as_deref(),
                    completed_at,
                );
                (tone, message, entry)
            }
            Err(err) => {
                let message = format!("Run failed for {source}: wait failed: {err}");
                let entry = DesktopRunHistoryEntry {
                    source: source.clone(),
                    run_id: run_id.clone(),
                    completed_at,
                    success: false,
                    session_id: None,
                    summary: message.clone(),
                };
                (ActivityTone::Error, message, entry)
            }
        };
        push_pending(tone, message);
        // Persist regardless of outcome — a history-write failure is logged
        // and swallowed; it must never affect the (already-surfaced) run
        // outcome above.
        record_run_history(history_entry);
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
/// local cold-OCI run does not outlive the Desktop, then release the
/// single-flight slot so a future session (after a restart) can start a fresh
/// run. Safe to call without a GPUI context (invoked from
/// `window::begin_shutdown`). The single-flight invariant guarantees at most
/// one in-flight child, so one kill reaps everything.
pub fn shutdown() {
    let pid = CURRENT_PID.swap(0, Ordering::SeqCst);
    if pid > 1 {
        kill_process_tree(pid);
        tracing::info!(pid, "desktop_run_agent: run child terminated on shutdown");
    }
    release_inflight();
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

// ─── Desktop Runner run history ──────────────────────────────────────────
//
// Persisted history of completed Desktop Runner (`ato://run`) runs, kept as
// a sibling of `system_capsule::ato_start::StartPageHistoryStore` rather than
// folded into it: `StartHistoryEntry` is shaped around *webview capsule
// opens* (a `handle` re-opened through the consent / installed-launch flow —
// see `open_capsule_from_start`) and has no notion of a run outcome. An
// `ato://run` entry is a fire-and-forget CLI spawn on the Desktop Runner with
// a distinct relaunch path (`desktop_run_agent::launch`) and a
// success/failure outcome the capsule-open schema has no field for. Reusing
// the exact same JSON-file persistence pattern (load/save via serde_json,
// most-recent-first, bounded cap) keeps the two stores consistent without
// conflating their semantics.
//
// NOTE (UI surfacing): the Start page reads `StartPageHistoryStore` today
// (`recent_capsules` in the injected snapshot) and a click posts
// `open_capsule`, which routes through the webview consent / installed-launch
// flow — wiring an `ato://run` entry into that same list would make clicking
// it incorrectly attempt to *open a webview* for a source the Desktop Runner
// ran headlessly, instead of re-invoking `desktop_run_agent::launch`. Because
// of that, this store's data does NOT show up anywhere in the UI yet by
// construction; surfacing it (e.g. a "recent runs" list wired to `launch()`)
// is left to a follow-up PR.

/// A single completed Desktop Runner (`ato://run`) run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DesktopRunHistoryEntry {
    /// The capsule ref passed to `ato run <source>` — also the relaunch key:
    /// a follow-up UI can call
    /// `desktop_run_agent::launch(&entry.source, entry.run_id.as_deref(), false)`.
    /// (`ready_state_enabled` is hardcoded `false` at the only current call
    /// site — M3 cold-OCI-only policy — so it is not persisted here; if that
    /// ever becomes dynamic, add it to this entry then.)
    pub source: String,
    /// The `ato://run?source=...&run_id=<id>` id, when the triggering intent
    /// supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Unix timestamp (seconds) the run finished.
    pub completed_at: u64,
    /// `true` on a clean exit (with or without a parseable session receipt);
    /// `false` on any failure (placement-gate rejection, non-zero exit, or a
    /// `wait()` failure).
    pub success: bool,
    /// Session id parsed from a successful run's receipt, when the CLI
    /// printed one. `None` on failure or when stdout had no parseable
    /// receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The same human-readable summary shown in the activity log, so a
    /// future history view can explain the outcome without re-reading log
    /// files.
    pub summary: String,
}

/// Persistent store for completed Desktop Runner run history.
///
/// Stored at `~/.ato/desktop-runner-run-history.json`, matching
/// `StartPageHistoryStore`'s persistence approach (serde_json pretty bytes,
/// load tolerant of a missing/corrupt file). At most [`MAX_RUN_HISTORY`]
/// entries, most-recently-completed first.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DesktopRunHistoryStore {
    pub entries: Vec<DesktopRunHistoryEntry>,
}

/// Cap on persisted run-history entries — same bound convention as
/// `StartPageHistoryStore::MAX_HISTORY`.
const MAX_RUN_HISTORY: usize = 20;

impl DesktopRunHistoryStore {
    /// Load from `~/.ato/desktop-runner-run-history.json`. Returns an empty
    /// store if the file does not exist or cannot be parsed (non-fatal).
    pub fn load() -> Self {
        let path = match run_history_path() {
            Ok(p) => p,
            Err(_) => return Self::default(),
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => return Self::default(),
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// Persist to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = run_history_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// Record a completed run, most-recent first, capped at
    /// [`MAX_RUN_HISTORY`]. Unlike `StartPageHistoryStore::upsert`, this
    /// never dedups by source: repeated runs of the same capsule are
    /// distinct history events (each with its own outcome), not a single
    /// "recently opened" row.
    pub fn record(&mut self, entry: DesktopRunHistoryEntry) {
        self.entries.push(entry);
        self.entries
            .sort_by_key(|e| std::cmp::Reverse(e.completed_at));
        self.entries.truncate(MAX_RUN_HISTORY);
    }
}

fn run_history_path() -> anyhow::Result<PathBuf> {
    capsule::common::paths::ato_path("desktop-runner-run-history.json").map_err(anyhow::Error::from)
}

/// Build the persisted history entry for a completed run. Pure (no I/O) —
/// mirrors [`render_run_result`]'s inputs but produces the durable record
/// instead of the transient activity message; both derive the session id /
/// summary from the same stdout/stderr, so what is recorded stays consistent
/// with what the user saw live.
pub fn build_history_entry(
    exit_ok: bool,
    stdout: &str,
    stderr: &str,
    source: &str,
    run_id: Option<&str>,
    completed_at: u64,
) -> DesktopRunHistoryEntry {
    let session_id = if exit_ok {
        parse_session_receipt(stdout).map(|r| r.session_id)
    } else {
        None
    };
    let (_, summary) = render_run_result(exit_ok, stdout, stderr, source, run_id);
    DesktopRunHistoryEntry {
        source: source.to_string(),
        run_id: run_id.map(str::to_string),
        completed_at,
        success: exit_ok,
        session_id,
        summary,
    }
}

/// Load, append, cap, and persist a completed run's history entry. Errors
/// are logged and swallowed — a history-write failure must never affect run
/// outcome surfacing (which has already happened via `push_pending` by the
/// time this is called).
fn record_run_history(entry: DesktopRunHistoryEntry) {
    let mut store = DesktopRunHistoryStore::load();
    store.record(entry);
    if let Err(err) = store.save() {
        tracing::warn!(error = %err, "desktop_run_agent: failed to save run history");
    }
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

    // ── shared-static tests (must serialize: PENDING_ACTIVITY / IN_FLIGHT /
    // CURRENT_PID are process-wide) ────────────────────────────────────────
    //
    // Every test in this group touches process-wide statics, so we serialize
    // them with `serial_test::serial` to keep the invariants honest. Pure
    // renderer/parser tests above are NOT serialized — they do not touch the
    // statics.

    #[serial_test::serial]
    #[test]
    fn drain_pending_activity_returns_and_clears() {
        // Static lock — serialize against other tests in this module that push.
        push_pending(ActivityTone::Info, "test drain");
        let drained = drain_pending_activity();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].message, "test drain");
        assert!(drain_pending_activity().is_empty(), "drain must clear");
    }

    // ── single-flight guard ────────────────────────────────────────────────
    //
    // The single-flight statics (`IN_FLIGHT`, `CURRENT_PID`) are process-wide
    // and shared across tests, so every test in this group MUST leave them in
    // the released (false / 0) state. We serialize by always acquiring at the
    // start and force-releasing at the end (via `shutdown`) so a panic between
    // acquire and release does not poison the slot for the next test.

    fn reset_inflight_state() {
        let _ = CURRENT_PID.swap(0, Ordering::SeqCst);
        release_inflight();
    }

    #[serial_test::serial]
    #[test]
    fn try_acquire_inflight_succeeds_when_idle() {
        reset_inflight_state();
        assert!(
            try_acquire_inflight(),
            "first acquire must succeed when idle"
        );
        // Clean up — do not leave the slot held.
        release_inflight();
        assert!(!is_in_flight(), "release must clear the in-flight flag");
    }

    #[serial_test::serial]
    #[test]
    fn second_acquire_while_in_flight_is_rejected() {
        reset_inflight_state();
        assert!(try_acquire_inflight(), "first acquire must succeed");
        assert!(
            !try_acquire_inflight(),
            "second acquire while in flight must be rejected (single-flight invariant)"
        );
        release_inflight();
    }

    #[serial_test::serial]
    #[test]
    fn release_then_acquire_allows_next_run() {
        reset_inflight_state();
        assert!(try_acquire_inflight());
        release_inflight();
        // After release, the next caller must be able to start a run — this is
        // the invariant that lets the waiter's release unblock the next intent.
        assert!(
            try_acquire_inflight(),
            "acquire must succeed after a release"
        );
        release_inflight();
    }

    #[serial_test::serial]
    #[test]
    fn shutdown_releases_inflight_slot_and_clears_pid() {
        reset_inflight_state();
        // Simulate the spawn path: acquire, then publish a sentinel PID.
        assert!(try_acquire_inflight());
        CURRENT_PID.store(42, Ordering::SeqCst);
        assert!(is_in_flight());
        assert_eq!(CURRENT_PID.load(Ordering::SeqCst), 42);

        shutdown();

        assert!(
            !is_in_flight(),
            "shutdown must release the single-flight slot"
        );
        assert_eq!(
            CURRENT_PID.load(Ordering::SeqCst),
            0,
            "shutdown must clear the in-flight PID"
        );
    }

    #[serial_test::serial]
    #[test]
    fn launch_while_in_flight_rejects_without_spawning() {
        // Drive the single-flight guard into the in-flight state, then call
        // `launch`. It must reject with an error AND push a Warning activity
        // — without ever spawning an `ato run` child (the spawn would shell
        // out to the real CLI, which unit tests must not do). We detect the
        // rejection by observing the pending activity queue and the Err
        // return; a successful spawn would have pushed no pending activity
        // from `launch` itself (only the waiter does, and no waiter is
        // running here).
        reset_inflight_state();
        assert!(try_acquire_inflight(), "precondition: slot is held");

        let _ = drain_pending_activity(); // clean slate
        let result = launch("community/test", None, false);
        assert!(
            result.is_err(),
            "launch must error while a run is in flight"
        );

        let drained = drain_pending_activity();
        assert_eq!(
            drained.len(),
            1,
            "exactly one warning activity must be pushed: {:?}",
            drained
        );
        assert_eq!(drained[0].tone, ActivityTone::Warning);
        assert!(
            drained[0].message.contains("already starting/running"),
            "warning must explain the in-flight reason: {:?}",
            drained[0].message
        );

        // The slot must still be held by the original acquirer (launch did not
        // release it), and no PID must have been published.
        assert!(is_in_flight(), "rejected launch must not release the slot");
        assert_eq!(
            CURRENT_PID.load(Ordering::SeqCst),
            0,
            "rejected launch must not publish a PID"
        );

        release_inflight();
    }

    // ── DesktopRunHistoryStore / build_history_entry ────────────────────────

    #[test]
    fn build_history_entry_success_captures_session_id_and_summary() {
        let entry = build_history_entry(
            true,
            SESSION_STDOUT,
            "",
            "community/hello",
            Some("run_7"),
            1_700_000_000,
        );
        assert_eq!(entry.source, "community/hello");
        assert_eq!(entry.run_id.as_deref(), Some("run_7"));
        assert_eq!(entry.completed_at, 1_700_000_000);
        assert!(entry.success);
        assert_eq!(
            entry.session_id.as_deref(),
            Some("ato-desktop-demo-app-123-456")
        );
        assert!(entry.summary.contains("session ato-desktop-demo-app-123-456"));
    }

    #[test]
    fn build_history_entry_success_without_receipt_has_no_session_id() {
        let entry = build_history_entry(true, "started ok\n", "", "acme/app", None, 42);
        assert!(entry.success);
        assert!(entry.session_id.is_none());
        assert!(entry.summary.contains("Run completed"));
    }

    #[test]
    fn build_history_entry_failure_has_no_session_id_and_records_reason() {
        let entry = build_history_entry(
            false,
            "",
            PLACEMENT_ERR,
            "acme/app",
            None,
            1_700_000_100,
        );
        assert!(!entry.success);
        assert!(entry.session_id.is_none());
        assert!(entry.summary.contains("macos_too_old"));
        assert_eq!(entry.completed_at, 1_700_000_100);
    }

    #[test]
    fn history_store_records_most_recent_first() {
        let mut store = DesktopRunHistoryStore::default();
        store.record(DesktopRunHistoryEntry {
            source: "acme/one".to_string(),
            run_id: None,
            completed_at: 100,
            success: true,
            session_id: Some("s1".to_string()),
            summary: "ok".to_string(),
        });
        store.record(DesktopRunHistoryEntry {
            source: "acme/two".to_string(),
            run_id: None,
            completed_at: 200,
            success: false,
            session_id: None,
            summary: "failed".to_string(),
        });
        assert_eq!(store.entries.len(), 2);
        assert_eq!(store.entries[0].source, "acme/two", "newest first");
        assert_eq!(store.entries[1].source, "acme/one");
    }

    #[test]
    fn history_store_does_not_dedup_repeated_source_runs() {
        // Unlike StartPageHistoryStore, two runs of the same source are two
        // distinct history events, not one upserted row.
        let mut store = DesktopRunHistoryStore::default();
        for i in 0..3 {
            store.record(DesktopRunHistoryEntry {
                source: "acme/repeat".to_string(),
                run_id: None,
                completed_at: i,
                success: true,
                session_id: None,
                summary: "ok".to_string(),
            });
        }
        assert_eq!(store.entries.len(), 3);
    }

    #[test]
    fn history_store_caps_at_max() {
        let mut store = DesktopRunHistoryStore::default();
        for i in 0..(MAX_RUN_HISTORY as u64 + 5) {
            store.record(DesktopRunHistoryEntry {
                source: format!("acme/run-{i}"),
                run_id: None,
                completed_at: i,
                success: true,
                session_id: None,
                summary: "ok".to_string(),
            });
        }
        assert_eq!(store.entries.len(), MAX_RUN_HISTORY);
        // The newest entries (highest completed_at) must survive the cap.
        assert_eq!(
            store.entries[0].completed_at,
            MAX_RUN_HISTORY as u64 + 4,
            "newest entry must be kept after truncation"
        );
    }

    #[test]
    fn history_entry_round_trips_through_serde_and_omits_none_fields() {
        let entry = DesktopRunHistoryEntry {
            source: "acme/app".to_string(),
            run_id: None,
            completed_at: 12345,
            success: false,
            session_id: None,
            summary: "Run failed".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(
            !json.contains("\"run_id\":null"),
            "None run_id must be omitted, not serialized as null: {json}"
        );
        assert!(
            !json.contains("\"session_id\":null"),
            "None session_id must be omitted, not serialized as null: {json}"
        );
        let back: DesktopRunHistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn history_store_round_trips_through_serde() {
        let mut store = DesktopRunHistoryStore::default();
        store.record(DesktopRunHistoryEntry {
            source: "acme/app".to_string(),
            run_id: Some("run_1".to_string()),
            completed_at: 555,
            success: true,
            session_id: Some("sess-1".to_string()),
            summary: "Run ready".to_string(),
        });
        let json = serde_json::to_string(&store).unwrap();
        let back: DesktopRunHistoryStore = serde_json::from_str(&json).unwrap();
        assert_eq!(back.entries, store.entries);
    }
}

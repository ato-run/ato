//! Dock system capsule IPC handler.
//!
//! Handles commands sent from the `ato-dock` WebView page. The `Login`
//! command spawns `ato login --desktop` as a child process. The CLI opens
//! the OS default browser itself (reusing the same `try_open_browser`
//! helper the plain `ato login` command already uses) and polls the
//! auth_bridge for completion exactly as it does for a plain interactive
//! login — this module's only job is to watch the child's NDJSON stdout,
//! forward anything the user needs to see (e.g. a fallback login URL if the
//! automatic browser launch failed) into the Dock's toast channel, and
//! refresh the Dock when a terminal event arrives.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use gpui::{AnyWindowHandle, App};
use serde::Deserialize;
use serde_json::{Value, json};

use super::broker::Capability;
use crate::orchestrator::resolve_ato_binary;
use crate::proc_util::CommandNoWindowExt;
use crate::window::dock::dock_event_queue;

/// Source-of-truth shape for a developer-imported capsule project.
/// Drives both the cloning/validation step and how the inferred
/// manifest's `name` slug seed is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockSourceKind {
    GithubRepo,
    LocalPath,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DockCommand {
    Login,
}

impl DockCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            DockCommand::Login => Capability::LaunchSystemCapsule,
        }
    }
}

pub fn dispatch(cx: &mut App, _host: AnyWindowHandle, command: DockCommand) -> Result<()> {
    match command {
        DockCommand::Login => trigger_login(cx),
    }
}

// ── NDJSON event types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DesktopLoginEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    publisher_handle: Option<String>,
    #[serde(default)]
    message: Option<String>,
    /// Present on `desktop_login_started` and `desktop_browser_launch_failed`.
    /// Carried through so a failed automatic browser launch can still show
    /// the user something actionable instead of failing silently.
    #[serde(default)]
    login_url: Option<String>,
    /// Present on some `desktop_login_failed` events sourced from a live
    /// ato-api response (see `sanitize_bridge_failure` in `ato-cli`'s
    /// `store.rs`): the raw HTTP status + response body text. Round-4
    /// review finding (Major, information-disclosure-ux) — this must be
    /// logged via `tracing::warn!` for debugging only and must NEVER be
    /// forwarded into the Dock's user-facing toast; `message` is the only
    /// field safe to show a user.
    #[serde(default)]
    detail: Option<String>,
}

// ── Single-flight guard ───────────────────────────────────────────────────────

/// Single-flight guard against overlapping `Login` invocations.
///
/// The embedded-WebView flow this replaced (`auth_login_window.rs`, removed
/// by ato#1077) tracked an `AuthLoginWindowSlot` global and re-activated the
/// existing window instead of spawning a second child process for a second
/// click. `trigger_login` needs the same guarantee even though it no longer
/// opens any window of its own: without it, two rapid Login clicks would
/// spawn two independent `ato login --desktop` child processes racing on
/// the same on-disk age identity and opening two bridge sessions (round-2
/// review finding).
///
/// `SeqCst` is stronger than strictly necessary — GPUI dispatch of Dock
/// commands always runs on the foreground thread, so plain access would
/// likely suffice — but the cost is negligible and it removes any doubt if
/// that assumption ever changes.
static LOGIN_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Attempts to acquire the single-flight login guard. Returns `true` if
/// acquired (the caller should proceed), or `false` if a login is already
/// in flight (the caller must not start a second one). Kept free of any
/// GPUI/process dependency so it can be unit-tested directly.
fn try_begin_login() -> bool {
    !LOGIN_IN_FLIGHT.swap(true, Ordering::SeqCst)
}

/// Releases the single-flight login guard. Must be called exactly once for
/// every `try_begin_login()` call that returned `true`, on *every* exit
/// path — including an early error before the child process is even
/// spawned — or a single transient failure would permanently wedge the
/// Dock's Login command until the app restarts.
fn end_login() {
    LOGIN_IN_FLIGHT.store(false, Ordering::SeqCst);
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Guarded entry point for `DockCommand::Login`. Rejects a second
/// invocation while one is already in flight, then delegates to
/// `trigger_login_inner`. Any error from the inner call — including one
/// that happens before a child process/watcher is ever spawned — releases
/// the guard and pushes a `desktop_login_failed` toast, so a Login button
/// disabled while "signing in" on the JS side never gets stuck forever
/// with no explanation.
fn trigger_login(cx: &mut App) -> Result<()> {
    if !try_begin_login() {
        tracing::info!("ato_dock: login already in flight; ignoring duplicate Login command");
        if let Ok(queue) = dock_event_queue(cx)
            && let Ok(mut events) = queue.lock()
        {
            events.push(json!({
                "kind": "desktop_login_in_progress",
                "message": "A sign-in is already in progress.",
            }));
        }
        return Ok(());
    }

    if let Err(error) = trigger_login_inner(cx) {
        end_login();
        if let Ok(queue) = dock_event_queue(cx)
            && let Ok(mut events) = queue.lock()
        {
            events.push(json!({
                "kind": "desktop_login_failed",
                "message": format!("Could not start sign-in: {error}"),
            }));
        }
        return Err(error);
    }

    Ok(())
}

/// Spawn `ato login --desktop` and watch its NDJSON stdout for completion.
/// The CLI process opens the system browser itself and polls the
/// auth_bridge on its own; this function does not open any window — it
/// forwards any user-facing progress/failure events into the Dock's toast
/// channel and notices when the child is done to refresh the Dock.
fn trigger_login_inner(cx: &mut App) -> Result<()> {
    let ato_bin = resolve_ato_binary().context("ato binary not found")?;
    tracing::info!(ato_bin = %ato_bin.display(), "ato_dock: spawning ato login --desktop");

    // Best-effort: if the Dock window/runtime isn't available for some
    // reason, we still run the login flow — the user just won't see
    // intermediate toasts (they'll still see the terminal outcome via
    // `open_dock_window` below).
    let event_queue = match dock_event_queue(cx) {
        Ok(queue) => Some(queue),
        Err(error) => {
            tracing::warn!(
                ?error,
                "ato_dock: dock event queue unavailable; login progress/failure toasts will be skipped"
            );
            None
        }
    };

    let mut child: Child = Command::new(&ato_bin)
        .no_console_window()
        .arg("login")
        .arg("--desktop")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .context("failed to spawn ato login --desktop")?;

    let stdout = child.stdout.take().context("no stdout from child")?;
    let reader = BufReader::new(stdout);

    // ── Background watcher using GPUI executors ────────────────────────────────
    // `be.spawn` runs the blocking I/O on background threads (requires Send).
    // `fe.spawn` schedules the UI update on the main GPUI thread (non-Send ok).
    // `aa` (AsyncApp, non-Send) is only used inside `fe.spawn`.
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();

    fe.spawn(async move {
        let completion = be
            .spawn(async move { watch_login_completion(reader, child, event_queue) })
            .await;
        aa.update(|cx| {
            on_login_completion(cx, completion);
        });
    })
    .detach();

    Ok(())
}

// ── Completion result ─────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum LoginCompletion {
    Success {
        publisher_handle: Option<String>,
    },
    /// `detail` (when present) is the raw ato-api diagnostic behind this
    /// failure (HTTP status + response body) — logged via `tracing::warn!`
    /// only. `message` is the sole field ever forwarded into the Dock's
    /// user-facing toast (round-4 review finding, Major,
    /// information-disclosure-ux).
    Failure {
        message: String,
        detail: Option<String>,
    },
}

/// Result of classifying a single NDJSON line from the child's stdout.
#[derive(Debug, PartialEq, Eq)]
enum ParsedLoginLine {
    /// A terminal event: the watcher loop should stop and report this.
    Terminal(LoginCompletion),
    /// A non-terminal event that should be surfaced to the user (e.g. a
    /// fallback login URL because the automatic browser launch failed), but
    /// does not end the watch loop.
    Forward(Value),
    /// Anything else: unrecognized event kind, or a line that isn't valid
    /// `DesktopLoginEvent` JSON (tolerated — stdout may interleave benign
    /// non-JSON noise from dependencies).
    Ignore,
}

/// Pure classification of one NDJSON line. Kept free of any process/IO
/// dependency so it can be unit-tested with plain string literals.
fn classify_ndjson_line(line: &str) -> ParsedLoginLine {
    let Ok(event) = serde_json::from_str::<DesktopLoginEvent>(line.trim()) else {
        return ParsedLoginLine::Ignore;
    };
    match event.kind.as_str() {
        "desktop_login_completed" => ParsedLoginLine::Terminal(LoginCompletion::Success {
            publisher_handle: event.publisher_handle,
        }),
        "desktop_login_failed" => ParsedLoginLine::Terminal(LoginCompletion::Failure {
            message: event.message.unwrap_or_else(|| "login failed".to_string()),
            detail: event.detail,
        }),
        "desktop_browser_launch_failed" => {
            let message = event
                .message
                .unwrap_or_else(|| "Could not open your browser automatically.".to_string());
            // Keep `login_url` as its own JSON field rather than baking it
            // into `message` text: App.jsx renders it as a clickable
            // link + a "Copy link" button and keeps the toast open until
            // the user dismisses it, instead of a plain-text URL inside a
            // toast bubble that auto-dismisses in under 3 seconds — round-2
            // review finding (the URL is unusable if the user can't act on
            // it before it vanishes).
            let mut event_json = json!({
                "kind": "desktop_browser_launch_failed",
                "message": message,
            });
            if let Some(url) = &event.login_url {
                event_json["login_url"] = json!(url);
            }
            ParsedLoginLine::Forward(event_json)
        }
        _ => ParsedLoginLine::Ignore,
    }
}

/// Reads stdout from the child, forwarding non-terminal events into
/// `event_queue` as they arrive, and waits for it to exit.
fn watch_login_completion(
    reader: BufReader<impl std::io::Read>,
    mut child: Child,
    event_queue: Option<Arc<Mutex<Vec<Value>>>>,
) -> LoginCompletion {
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        match classify_ndjson_line(&line) {
            ParsedLoginLine::Terminal(completion) => {
                let _ = child.wait();
                return completion;
            }
            ParsedLoginLine::Forward(event) => {
                if let Some(queue) = &event_queue
                    && let Ok(mut events) = queue.lock()
                {
                    events.push(event);
                }
            }
            ParsedLoginLine::Ignore => {}
        }
    }

    // Process exited without a completion event.
    let exit_status = child.wait();
    match exit_status {
        Ok(s) if s.success() => LoginCompletion::Success {
            publisher_handle: None,
        },
        Ok(s) => LoginCompletion::Failure {
            message: format!("ato login exited with status {}", s),
            detail: None,
        },
        Err(e) => LoginCompletion::Failure {
            message: format!("waiting for ato login failed: {}", e),
            detail: None,
        },
    }
}

/// Called on the GPUI thread after the child process finishes. Always
/// releases the single-flight login guard first — this is the only path
/// that reaches a terminal outcome for a login that made it past
/// `trigger_login_inner`'s early `?`s, so it is the single place that must
/// clear `LOGIN_IN_FLIGHT` for that case.
fn on_login_completion(cx: &mut App, result: LoginCompletion) {
    end_login();
    match result {
        LoginCompletion::Success { publisher_handle } => {
            tracing::info!(
                publisher_handle = publisher_handle.as_deref().unwrap_or("(unknown)"),
                "Desktop login completed successfully"
            );
            crate::window::dock::notify_login_success(cx);
        }
        LoginCompletion::Failure { message, detail } => {
            // `detail` (when present) carries the raw ato-api diagnostic
            // behind this failure — logged here for debugging only. It
            // must never be added to the JSON pushed below: that queue
            // feeds directly into the Dock's user-facing toast, and
            // `message` is the only field `sanitize_bridge_failure`
            // (ato-cli's store.rs) guarantees is safe to show a user
            // (round-4 review finding, Major, information-disclosure-ux).
            tracing::warn!(
                message = %message,
                detail = detail.as_deref().unwrap_or(""),
                "Desktop login failed or was cancelled"
            );
            // Surface the failure as a toast in the Dock (App.jsx renders any
            // event whose `kind` contains "failed" as a warning toast), so a
            // denied/cancelled/timed-out/errored login isn't silently
            // invisible to the user.
            if let Ok(queue) = dock_event_queue(cx)
                && let Ok(mut events) = queue.lock()
            {
                events.push(json!({
                    "kind": "desktop_login_failed",
                    "message": message,
                }));
            }
            // Bring the existing dock to front (it still shows the login page).
            let _ = crate::window::dock::open_dock_window(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_completed_event_as_success() {
        let line =
            r#"{"type":"desktop_login_completed","publisher_handle":"koh","storage":"age_file"}"#;
        assert_eq!(
            classify_ndjson_line(line),
            ParsedLoginLine::Terminal(LoginCompletion::Success {
                publisher_handle: Some("koh".to_string()),
            })
        );
    }

    #[test]
    fn classifies_completed_event_without_handle() {
        let line = r#"{"type":"desktop_login_completed"}"#;
        assert_eq!(
            classify_ndjson_line(line),
            ParsedLoginLine::Terminal(LoginCompletion::Success {
                publisher_handle: None,
            })
        );
    }

    #[test]
    fn classifies_failed_event_with_message() {
        let line = r#"{"type":"desktop_login_failed","message":"Authentication timed out after 300 seconds"}"#;
        assert_eq!(
            classify_ndjson_line(line),
            ParsedLoginLine::Terminal(LoginCompletion::Failure {
                message: "Authentication timed out after 300 seconds".to_string(),
                detail: None,
            })
        );
    }

    #[test]
    fn classifies_failed_event_missing_message_with_fallback() {
        let line = r#"{"type":"desktop_login_failed"}"#;
        assert_eq!(
            classify_ndjson_line(line),
            ParsedLoginLine::Terminal(LoginCompletion::Failure {
                message: "login failed".to_string(),
                detail: None,
            })
        );
    }

    #[test]
    fn classifies_failed_event_carries_detail_separately_from_message() {
        // Round-4 review finding (Major, information-disclosure-ux):
        // `detail` (the raw ato-api diagnostic) must be threaded through
        // classification distinctly from `message` (the sanitized,
        // user-facing text) so `on_login_completion` can log the former
        // without ever forwarding it into the Dock's toast queue.
        let line = r#"{"type":"desktop_login_failed","message":"Sign-in failed to complete. Run `ato login` from a terminal for more detail.","detail":"Bridge auth exchange failed (500): internal error xyz"}"#;
        assert_eq!(
            classify_ndjson_line(line),
            ParsedLoginLine::Terminal(LoginCompletion::Failure {
                message:
                    "Sign-in failed to complete. Run `ato login` from a terminal for more detail."
                        .to_string(),
                detail: Some("Bridge auth exchange failed (500): internal error xyz".to_string()),
            })
        );
    }

    #[test]
    fn forwards_browser_launch_failure_with_login_url_as_a_separate_field() {
        // Round-2 review finding: baking the URL into the message string
        // left the Dock UI with no way to render it as anything other than
        // plain text. `login_url` must travel as its own field so App.jsx
        // can render a clickable link + copy button instead.
        let line = r#"{"type":"desktop_browser_launch_failed","login_url":"https://ato.run/auth?next=abc","message":"Could not open your browser automatically: no handler"}"#;
        match classify_ndjson_line(line) {
            ParsedLoginLine::Forward(event) => {
                let message = event.get("message").and_then(Value::as_str).unwrap();
                assert!(message.contains("no handler"));
                assert!(
                    !message.contains("https://ato.run/auth?next=abc"),
                    "the URL must not be baked into the message text, got: {message}"
                );
                assert_eq!(
                    event.get("login_url").and_then(Value::as_str),
                    Some("https://ato.run/auth?next=abc")
                );
                assert_eq!(
                    event.get("kind").and_then(Value::as_str),
                    Some("desktop_browser_launch_failed")
                );
            }
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    #[test]
    fn forwards_browser_launch_failure_without_login_url_omits_the_field() {
        let line = r#"{"type":"desktop_browser_launch_failed","message":"Could not open your browser automatically: no handler"}"#;
        match classify_ndjson_line(line) {
            ParsedLoginLine::Forward(event) => {
                assert!(event.get("login_url").is_none());
            }
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    #[test]
    fn ignores_unrecognized_event_kind() {
        let line = r#"{"type":"desktop_login_started","login_url":"https://ato.run/auth?next=abc","user_code":"AB12"}"#;
        assert_eq!(classify_ndjson_line(line), ParsedLoginLine::Ignore);
    }

    #[test]
    fn ignores_malformed_json_line() {
        assert_eq!(
            classify_ndjson_line("not json at all"),
            ParsedLoginLine::Ignore
        );
        assert_eq!(classify_ndjson_line(""), ParsedLoginLine::Ignore);
    }

    #[test]
    fn login_guard_rejects_concurrent_acquire_until_released() {
        // Regression guard for the round-2 review finding: a second `Login`
        // dispatch while one is already in flight must not be allowed to
        // spawn a second `ato login --desktop` child process. `LOGIN_IN_FLIGHT`
        // is a module-level static, so start and end on a known-clean state
        // to stay independent of other tests' execution order.
        end_login();

        assert!(try_begin_login(), "first acquire must succeed");
        assert!(
            !try_begin_login(),
            "a second concurrent acquire must be rejected"
        );
        assert!(
            !try_begin_login(),
            "must still be rejected while still held"
        );

        end_login();
        assert!(
            try_begin_login(),
            "acquire must succeed again once released"
        );
        end_login();
    }
}

//! LaunchTracker — desktop-side ownership of `/v1/launches` launch ids
//! handed off from the PWA (plan: launch-unification v3, Phase 3).
//!
//! The PWA hands the Desktop a **launch_id only** (via the injected
//! `window.__ATO_DESKTOP__.launch()` IPC bridge, or the external-browser
//! `ato://launch?launch_id=` fallback). The tracker then polls
//! `GET {api}/v1/launches/:id` with the desktop-auth-handoff credentials
//! ([`crate::source_import_api::discover`]) — the API owner-verifies the
//! launch, so a launch_id by itself can never open someone else's app and
//! no app_url / token ever rides the handoff channel.
//!
//! While a launch is `starting` the poll loop ticks every ~3s; on the
//! transition to `ready` the launch's `app_url` is dispatched as
//! [`crate::app::NavigateToUrl`] (opening the independent WebAppView
//! window — the existing same-origin focus-dedupe applies). Terminal
//! states stop the poll loop but keep the entry so the Shell Icon Bar tab
//! can keep showing the launch's fate. Snapshot updates go through the
//! GPUI global (same pattern as [`crate::remote_runs::RemoteRunsSnapshot`])
//! so observers re-render.

use std::time::Duration;

use serde::Deserialize;

use crate::source_import_api::ApiCreds;

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// Safety cap on poll iterations (~30 min at 3s) so an unknown non-terminal
/// state introduced by a future API can never leave a hot loop running
/// forever. The server expires stale launches well before this.
const MAX_POLLS: u32 = 600;

pub const MAX_LAUNCH_ID_LEN: usize = 128;
pub const MAX_CAPSULE_REF_LEN: usize = 256;

/// One launch the desktop has accepted visible ownership of.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackedLaunch {
    pub launch_id: String,
    /// `publisher/slug` display hint from the handoff payload; may be
    /// empty (the tab then falls back to the launch id).
    pub capsule_ref: String,
    /// Raw API state (`starting|ready|failed|cancelled|expired` today).
    /// Unknown future states are kept verbatim and rendered as non-ready.
    pub state: String,
    pub app_url: Option<String>,
    /// True once the ready `app_url` has been dispatched — the window is
    /// opened exactly once per launch.
    pub opened: bool,
    /// True while a poll loop is live for this launch (guards duplicate
    /// loops when the same launch_id is registered twice).
    pub polling: bool,
}

/// Latest tracked-launches snapshot; a GPUI global observed by the Shell
/// Icon Bar. Installed empty at startup by [`init`].
#[derive(Default)]
pub struct LaunchTrackerSnapshot {
    pub launches: Vec<TrackedLaunch>,
}

impl gpui::Global for LaunchTrackerSnapshot {}

/// Install the (empty) snapshot global. Call once from app bootstrap.
pub fn init(cx: &mut gpui::App) {
    cx.set_global(LaunchTrackerSnapshot::default());
}

/// Launch-id shape check for ids arriving over the IPC bridge or the
/// `ato://launch` intent. Deliberately strict: identifier charset only, so
/// URLs, tokens-with-dots, or path traversal can never ride this channel.
pub fn is_valid_launch_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_LAUNCH_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Capsule-ref display hint check (`publisher/slug` style). Rejects
/// anything URL-shaped — the ref is a label, never a target.
pub fn is_valid_capsule_ref(capsule_ref: &str) -> bool {
    !capsule_ref.is_empty()
        && capsule_ref.len() <= MAX_CAPSULE_REF_LEN
        && !capsule_ref.contains("://")
        && capsule_ref
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/'))
}

/// States after which polling stops. `ready` is terminal for the poller
/// (the app window is open; live run state is the remote-runs poller's
/// job); unknown future states (`stopping`, `blocked`, …) keep polling —
/// they may still transition — bounded by [`MAX_POLLS`].
pub fn is_terminal_state(state: &str) -> bool {
    matches!(state, "ready" | "failed" | "cancelled" | "expired" | "stopped")
}

/// Track `launch_id` and start (or wake) its poll loop. Invalid ids are
/// dropped with a warning — callers pre-validate, this is the backstop.
/// An invalid `capsule_ref` degrades to the empty display hint.
pub fn register_launch(cx: &mut gpui::App, launch_id: String, capsule_ref: String) {
    if !is_valid_launch_id(&launch_id) {
        tracing::warn!("launch tracker: invalid launch_id shape — ignored");
        return;
    }
    let capsule_ref = if is_valid_capsule_ref(&capsule_ref) {
        capsule_ref
    } else {
        String::new()
    };
    let needs_poll = {
        let snapshot = cx.global_mut::<LaunchTrackerSnapshot>();
        match snapshot
            .launches
            .iter_mut()
            .find(|launch| launch.launch_id == launch_id)
        {
            Some(existing) => {
                if existing.capsule_ref.is_empty() && !capsule_ref.is_empty() {
                    existing.capsule_ref = capsule_ref;
                }
                // Wake: re-poll an idle entry (re-registered after a
                // terminal state — fetch fresh truth from the API).
                if existing.polling {
                    false
                } else {
                    existing.polling = true;
                    true
                }
            }
            None => {
                snapshot.launches.push(TrackedLaunch {
                    launch_id: launch_id.clone(),
                    capsule_ref,
                    state: "starting".to_string(),
                    app_url: None,
                    opened: false,
                    polling: true,
                });
                true
            }
        }
    };
    if needs_poll {
        tracing::info!(launch_id = %launch_id, "launch tracker: tracking launch");
        spawn_poll_loop(cx, launch_id);
    }
}

/// `GET /v1/launches/:id` response — the fields the tracker consumes.
/// (`launchResponse` DTO: `{ ok, launch_id, state, app_url?, ... }`.)
#[derive(Debug, Deserialize)]
struct LaunchStatusResponse {
    state: String,
    #[serde(default)]
    app_url: Option<String>,
}

fn fetch_launch_status(creds: &ApiCreds, launch_id: &str) -> anyhow::Result<LaunchStatusResponse> {
    let url = format!(
        "{}/v1/launches/{}",
        creds.api_base_url.trim_end_matches('/'),
        launch_id
    );
    let body = ureq::get(&url)
        .set(
            "Authorization",
            &format!("Bearer {}", creds.session_token),
        )
        .timeout(HTTP_TIMEOUT)
        .call()?
        .into_string()?;
    Ok(serde_json::from_str(&body)?)
}

/// Only http(s) app URLs may leave the tracker — a hostile `app_url` in an
/// API response must not be able to re-enter the ato:// intent surface.
fn is_dispatchable_app_url(url: &str) -> bool {
    crate::window::web_app_view::is_web_navigation(url)
        && url::Url::parse(url)
            .map(|parsed| matches!(parsed.scheme(), "http" | "https"))
            .unwrap_or(false)
}

/// Apply one polled status to the snapshot; returns true when the state
/// is now terminal. Dispatches the ready `app_url` exactly once.
fn apply_status(cx: &mut gpui::App, launch_id: &str, status: LaunchStatusResponse) -> bool {
    let mut open_url: Option<String> = None;
    {
        let snapshot = cx.global_mut::<LaunchTrackerSnapshot>();
        let Some(entry) = snapshot
            .launches
            .iter_mut()
            .find(|launch| launch.launch_id == launch_id)
        else {
            return true; // entry vanished — stop the loop
        };
        if entry.state != status.state {
            tracing::info!(
                launch_id = %launch_id,
                from = %entry.state,
                to = %status.state,
                "launch tracker: state transition"
            );
        }
        entry.state = status.state;
        entry.app_url = status.app_url.filter(|url| !url.is_empty());
        if entry.state == "ready"
            && !entry.opened
            && let Some(url) = entry.app_url.as_deref().filter(|url| is_dispatchable_app_url(url))
        {
            entry.opened = true;
            open_url = Some(url.to_string());
        }
    }
    if let Some(url) = open_url {
        dispatch_navigate(cx, url);
    }
    let state = cx
        .global::<LaunchTrackerSnapshot>()
        .launches
        .iter()
        .find(|launch| launch.launch_id == launch_id)
        .map(|launch| launch.state.clone())
        .unwrap_or_default();
    is_terminal_state(&state)
}

/// Route the ready URL through the app-level `NavigateToUrl` action (the
/// same path the Shell Icon Bar and `ato://open` use) via any live window;
/// the handler dedupes against an already-open same-origin window.
fn dispatch_navigate(cx: &mut gpui::App, url: String) {
    for handle in cx.windows() {
        let dispatched = handle.update(cx, |_, window, cx| {
            window.dispatch_action(
                Box::new(crate::app::NavigateToUrl { url: url.clone() }),
                cx,
            );
        });
        if dispatched.is_ok() {
            return;
        }
    }
    tracing::warn!(url = %url, "launch tracker: no live window to dispatch NavigateToUrl");
}

fn mark_not_polling(cx: &mut gpui::App, launch_id: &str) {
    let snapshot = cx.global_mut::<LaunchTrackerSnapshot>();
    if let Some(entry) = snapshot
        .launches
        .iter_mut()
        .find(|launch| launch.launch_id == launch_id)
    {
        entry.polling = false;
    }
}

/// Background poll loop for one launch — same executor split as the
/// remote-runs poller: HTTP on the background executor, snapshot swap on
/// the GPUI main thread. Credentials are cached across ticks and dropped
/// on failure so an expired session re-discovers.
fn spawn_poll_loop(cx: &mut gpui::App, launch_id: String) {
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        let mut creds: Option<ApiCreds> = None;
        let mut polls: u32 = 0;
        loop {
            let attempt_creds = creds.clone();
            let id = launch_id.clone();
            let (next_creds, status) = be
                .spawn(async move {
                    let creds = match attempt_creds.or_else(crate::source_import_api::discover) {
                        Some(creds) => creds,
                        None => return (None, None),
                    };
                    match fetch_launch_status(&creds, &id) {
                        Ok(status) => (Some(creds), Some(status)),
                        Err(error) => {
                            tracing::debug!(
                                launch_id = %id,
                                error = %error,
                                "launch poll failed; will re-discover next tick"
                            );
                            (None, None)
                        }
                    }
                })
                .await;
            creds = next_creds;
            let mut done = false;
            aa.update(|cx| {
                if let Some(status) = status {
                    done = apply_status(cx, &launch_id, status);
                }
                if done {
                    mark_not_polling(cx, &launch_id);
                }
            });
            if done {
                break;
            }
            polls += 1;
            if polls >= MAX_POLLS {
                tracing::warn!(
                    launch_id = %launch_id,
                    "launch tracker: poll cap reached without a terminal state — giving up"
                );
                aa.update(|cx| mark_not_polling(cx, &launch_id));
                break;
            }
            be.timer(POLL_INTERVAL).await;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_id_shape_rejects_urls_and_tokens() {
        assert!(is_valid_launch_id("lch_01KW12MFHY"));
        assert!(is_valid_launch_id("abc-123_DEF"));
        assert!(!is_valid_launch_id(""));
        assert!(!is_valid_launch_id("https://evil.example/x"));
        assert!(!is_valid_launch_id("id with spaces"));
        assert!(!is_valid_launch_id("../../etc/passwd"));
        assert!(!is_valid_launch_id("a.b")); // dots excluded — no JWTs
        assert!(!is_valid_launch_id(&"x".repeat(MAX_LAUNCH_ID_LEN + 1)));
    }

    #[test]
    fn capsule_ref_shape_allows_scoped_ids_only() {
        assert!(is_valid_capsule_ref("community/hello-capsule"));
        assert!(is_valid_capsule_ref("acme/app.v2"));
        assert!(!is_valid_capsule_ref(""));
        assert!(!is_valid_capsule_ref("https://evil.example/x"));
        assert!(!is_valid_capsule_ref("a b"));
        assert!(!is_valid_capsule_ref(&"x".repeat(MAX_CAPSULE_REF_LEN + 1)));
    }

    #[test]
    fn terminal_states_stop_polling_and_unknown_states_do_not() {
        for state in ["ready", "failed", "cancelled", "expired", "stopped"] {
            assert!(is_terminal_state(state), "{state} should be terminal");
        }
        for state in ["starting", "stopping", "blocked", "queued", "wat"] {
            assert!(!is_terminal_state(state), "{state} should keep polling");
        }
    }

    #[test]
    fn launch_status_response_parses_api_shape() {
        let body = r#"{"ok":true,"launch_id":"lch_1","state":"ready",
            "selected_provider":"fly","status_url":"/v1/launches/lch_1",
            "cancel_url":"/v1/launches/lch_1/cancel",
            "app_url":"https://abc.app.ato.run/","embed_policy":"embedded"}"#;
        let parsed: LaunchStatusResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.state, "ready");
        assert_eq!(parsed.app_url.as_deref(), Some("https://abc.app.ato.run/"));

        let starting = r#"{"ok":true,"launch_id":"lch_1","state":"starting",
            "status_url":"/v1/launches/lch_1","cancel_url":"/v1/launches/lch_1/cancel"}"#;
        let parsed: LaunchStatusResponse = serde_json::from_str(starting).unwrap();
        assert_eq!(parsed.state, "starting");
        assert!(parsed.app_url.is_none());
    }

    #[test]
    fn only_http_app_urls_are_dispatchable() {
        assert!(is_dispatchable_app_url("https://abc.app.ato.run/"));
        assert!(is_dispatchable_app_url("http://127.0.0.1:8420/"));
        assert!(!is_dispatchable_app_url("ato://open?handle=x"));
        assert!(!is_dispatchable_app_url("capsule://community/hello"));
        assert!(!is_dispatchable_app_url("data:text/html,hi"));
        assert!(!is_dispatchable_app_url("javascript:alert(1)"));
    }
}

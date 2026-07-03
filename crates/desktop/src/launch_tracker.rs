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
//! can keep showing the launch's fate. An API owner-check rejection
//! (403/404/…) instead removes the entry outright — a foreign or bogus
//! launch_id must not pin a poll loop or a ghost tab — and the tracker
//! holds at most [`MAX_TRACKED_LAUNCHES`] entries. Snapshot updates go
//! through the GPUI global (same pattern as
//! [`crate::remote_runs::RemoteRunsSnapshot`]) so observers re-render.

use std::time::Duration;

use serde::Deserialize;

use crate::source_import_api::ApiCreds;

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// Safety cap on poll iterations (~30 min at 3s) so an unknown non-terminal
/// state introduced by a future API can never leave a hot loop running
/// forever. The server expires stale launches well before this.
const MAX_POLLS: u32 = 600;
/// Consecutive poll failures (network errors, 401s, missing creds) after
/// which the loop gives up. Bounds the `discover()` subprocess re-spawn to
/// a handful of attempts instead of one per 3s tick for half an hour.
const MAX_CONSECUTIVE_FAILURES: u32 = 10;
/// Hard cap on simultaneously tracked launches. A page can only hand off
/// ids over the (origin-gated) bridge, but even a buggy trusted PWA must
/// not be able to spawn unbounded poll loops / bar tabs.
pub const MAX_TRACKED_LAUNCHES: usize = 16;

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

/// Outcome of applying a registration to the tracked-launch list.
#[derive(Debug, PartialEq)]
enum RegisterOutcome {
    /// Entry added or woken — a poll loop must (re)start.
    NeedsPoll,
    /// A poll loop is already live for this id — nothing to do.
    AlreadyPolling,
    /// Tracker is at [`MAX_TRACKED_LAUNCHES`] with no evictable entry —
    /// the registration was refused (caller should surface the rejection).
    Rejected,
}

/// Pure registration policy over the tracked-launch list (unit-testable
/// without GPUI). At capacity, idle terminal entries (oldest first) are
/// evicted to make room; if every slot is still busy the new launch is
/// refused — fail-closed rather than unbounded growth.
fn register_in(
    launches: &mut Vec<TrackedLaunch>,
    launch_id: &str,
    capsule_ref: String,
) -> RegisterOutcome {
    if let Some(existing) = launches
        .iter_mut()
        .find(|launch| launch.launch_id == launch_id)
    {
        if existing.capsule_ref.is_empty() && !capsule_ref.is_empty() {
            existing.capsule_ref = capsule_ref;
        }
        // Wake: re-poll an idle entry (re-registered after a
        // terminal state — fetch fresh truth from the API).
        if existing.polling {
            return RegisterOutcome::AlreadyPolling;
        }
        existing.polling = true;
        return RegisterOutcome::NeedsPoll;
    }
    if launches.len() >= MAX_TRACKED_LAUNCHES {
        // Evict the oldest idle terminal entry (its tab has served its
        // purpose); live/polling entries are never evicted.
        match launches
            .iter()
            .position(|launch| !launch.polling && is_terminal_state(&launch.state))
        {
            Some(index) => {
                launches.remove(index);
            }
            None => return RegisterOutcome::Rejected,
        }
    }
    launches.push(TrackedLaunch {
        launch_id: launch_id.to_string(),
        capsule_ref,
        state: "starting".to_string(),
        app_url: None,
        opened: false,
        polling: true,
    });
    RegisterOutcome::NeedsPoll
}

/// Track `launch_id` and start (or wake) its poll loop. Invalid ids are
/// dropped with a warning — callers pre-validate, this is the backstop.
/// An invalid `capsule_ref` degrades to the empty display hint. Returns
/// false when the registration was refused (invalid id, or the tracker is
/// at [`MAX_TRACKED_LAUNCHES`] live entries) so callers can surface an
/// honest rejection instead of a silent no-op.
pub fn register_launch(cx: &mut gpui::App, launch_id: String, capsule_ref: String) -> bool {
    if !is_valid_launch_id(&launch_id) {
        tracing::warn!("launch tracker: invalid launch_id shape — ignored");
        return false;
    }
    let capsule_ref = if is_valid_capsule_ref(&capsule_ref) {
        capsule_ref
    } else {
        String::new()
    };
    let outcome = {
        let snapshot = cx.global_mut::<LaunchTrackerSnapshot>();
        register_in(&mut snapshot.launches, &launch_id, capsule_ref)
    };
    match outcome {
        RegisterOutcome::NeedsPoll => {
            tracing::info!(launch_id = %launch_id, "launch tracker: tracking launch");
            spawn_poll_loop(cx, launch_id);
            true
        }
        RegisterOutcome::AlreadyPolling => true,
        RegisterOutcome::Rejected => {
            tracing::warn!(
                launch_id = %launch_id,
                max = MAX_TRACKED_LAUNCHES,
                "launch tracker: tracked-launch cap reached — registration refused"
            );
            false
        }
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

/// One poll attempt, classified so the loop can react honestly.
#[derive(Debug)]
enum FetchOutcome {
    Status(LaunchStatusResponse),
    /// The API refused this launch id for this account (403/404/…):
    /// the owner check the whole handoff design leans on (plan §4). A
    /// foreign or bogus id will never become visible — stop immediately
    /// instead of retrying for half an hour.
    Rejected { code: u16 },
    /// 401 — the cached session expired; drop creds and re-discover.
    AuthExpired,
    /// Network trouble / 5xx / 429 — retry with the same creds,
    /// bounded by [`MAX_CONSECUTIVE_FAILURES`].
    Transient(String),
}

/// True for HTTP statuses that mean "this launch will never be visible
/// to this account" — every 4xx except the retryable trio (401 auth
/// refresh, 408 timeout, 429 backoff).
fn rejection_is_terminal(code: u16) -> bool {
    (400..=499).contains(&code) && !matches!(code, 401 | 408 | 429)
}

fn fetch_launch_status(creds: &ApiCreds, launch_id: &str) -> FetchOutcome {
    let url = format!(
        "{}/v1/launches/{}",
        creds.api_base_url.trim_end_matches('/'),
        launch_id
    );
    let response = ureq::get(&url)
        .set(
            "Authorization",
            &format!("Bearer {}", creds.session_token),
        )
        .timeout(HTTP_TIMEOUT)
        .call();
    let body = match response {
        Ok(resp) => match resp.into_string() {
            Ok(body) => body,
            Err(error) => return FetchOutcome::Transient(error.to_string()),
        },
        Err(ureq::Error::Status(401, _)) => return FetchOutcome::AuthExpired,
        Err(ureq::Error::Status(code, _)) if rejection_is_terminal(code) => {
            return FetchOutcome::Rejected { code };
        }
        Err(error) => return FetchOutcome::Transient(error.to_string()),
    };
    match serde_json::from_str(&body) {
        Ok(status) => FetchOutcome::Status(status),
        Err(error) => FetchOutcome::Transient(format!("invalid status body: {error}")),
    }
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

/// Drop a tracked launch entirely (its bar tab disappears). Used when
/// the API owner-check rejects the id — a foreign/bogus launch must not
/// linger as a ghost "Starting" tab.
fn remove_launch(cx: &mut gpui::App, launch_id: &str) {
    let snapshot = cx.global_mut::<LaunchTrackerSnapshot>();
    snapshot.launches.retain(|launch| launch.launch_id != launch_id);
}

/// Give up on a launch we can no longer poll: surface it as failed
/// (honest error badge, not an eternal "Starting") and idle the entry.
fn mark_failed_and_idle(cx: &mut gpui::App, launch_id: &str) {
    let snapshot = cx.global_mut::<LaunchTrackerSnapshot>();
    if let Some(entry) = snapshot
        .launches
        .iter_mut()
        .find(|launch| launch.launch_id == launch_id)
    {
        if !is_terminal_state(&entry.state) {
            entry.state = "failed".to_string();
        }
        entry.polling = false;
    }
}

/// Per-tick result carried from the background executor to the loop.
enum TickResult {
    Status(LaunchStatusResponse),
    Rejected { code: u16 },
    /// Creds discovery failed / auth expired / network error — retryable
    /// but counted toward [`MAX_CONSECUTIVE_FAILURES`].
    Failure,
}

/// Background poll loop for one launch — same executor split as the
/// remote-runs poller: HTTP on the background executor, snapshot swap on
/// the GPUI main thread. Credentials are cached across ticks; they are
/// dropped only on a 401 (so an expired session re-discovers) — network
/// blips keep the cached creds instead of re-spawning the
/// `desktop-auth-handoff` discovery subprocess every 3s. The API's
/// owner-check rejections (403/404/…) are terminal: the entry is removed
/// and the loop ends, so a bogus or foreign launch_id can never pin a
/// poll loop or a ghost tab.
fn spawn_poll_loop(cx: &mut gpui::App, launch_id: String) {
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        let mut creds: Option<ApiCreds> = None;
        let mut polls: u32 = 0;
        let mut consecutive_failures: u32 = 0;
        loop {
            let attempt_creds = creds.clone();
            let id = launch_id.clone();
            let (next_creds, result) = be
                .spawn(async move {
                    let creds = match attempt_creds.or_else(crate::source_import_api::discover) {
                        Some(creds) => creds,
                        None => {
                            tracing::debug!(
                                launch_id = %id,
                                "launch poll: no API credentials; will retry"
                            );
                            return (None, TickResult::Failure);
                        }
                    };
                    match fetch_launch_status(&creds, &id) {
                        FetchOutcome::Status(status) => (Some(creds), TickResult::Status(status)),
                        FetchOutcome::Rejected { code } => {
                            (Some(creds), TickResult::Rejected { code })
                        }
                        FetchOutcome::AuthExpired => {
                            tracing::debug!(
                                launch_id = %id,
                                "launch poll got 401; will re-discover credentials"
                            );
                            (None, TickResult::Failure)
                        }
                        FetchOutcome::Transient(error) => {
                            tracing::debug!(
                                launch_id = %id,
                                error = %error,
                                "launch poll failed; will retry"
                            );
                            (Some(creds), TickResult::Failure)
                        }
                    }
                })
                .await;
            creds = next_creds;
            let mut done = false;
            match result {
                TickResult::Status(status) => {
                    consecutive_failures = 0;
                    aa.update(|cx| {
                        done = apply_status(cx, &launch_id, status);
                        if done {
                            mark_not_polling(cx, &launch_id);
                        }
                    });
                }
                TickResult::Rejected { code } => {
                    // Owner check said no (plan §4): this launch does not
                    // exist for this account. Drop the tab, end the loop.
                    tracing::warn!(
                        launch_id = %launch_id,
                        code,
                        "launch tracker: API rejected launch id — dropping"
                    );
                    aa.update(|cx| remove_launch(cx, &launch_id));
                    done = true;
                }
                TickResult::Failure => {
                    consecutive_failures += 1;
                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        tracing::warn!(
                            launch_id = %launch_id,
                            failures = consecutive_failures,
                            "launch tracker: repeated poll failures — giving up"
                        );
                        aa.update(|cx| mark_failed_and_idle(cx, &launch_id));
                        done = true;
                    }
                }
            }
            if done {
                break;
            }
            polls += 1;
            if polls >= MAX_POLLS {
                tracing::warn!(
                    launch_id = %launch_id,
                    "launch tracker: poll cap reached without a terminal state — giving up"
                );
                aa.update(|cx| mark_failed_and_idle(cx, &launch_id));
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
    fn owner_rejections_are_terminal_but_retryable_statuses_are_not() {
        for code in [400, 403, 404, 410, 422] {
            assert!(rejection_is_terminal(code), "{code} should be terminal");
        }
        for code in [401, 408, 429, 500, 502, 503] {
            assert!(!rejection_is_terminal(code), "{code} should be retryable");
        }
    }

    fn tracked(id: &str, state: &str, polling: bool) -> TrackedLaunch {
        TrackedLaunch {
            launch_id: id.to_string(),
            capsule_ref: String::new(),
            state: state.to_string(),
            app_url: None,
            opened: false,
            polling,
        }
    }

    #[test]
    fn register_in_wakes_idle_entries_and_dedupes_live_ones() {
        let mut launches = vec![tracked("lch_1", "failed", false)];
        assert_eq!(
            register_in(&mut launches, "lch_1", String::new()),
            RegisterOutcome::NeedsPoll
        );
        assert!(launches[0].polling);
        assert_eq!(
            register_in(&mut launches, "lch_1", String::new()),
            RegisterOutcome::AlreadyPolling
        );
        assert_eq!(launches.len(), 1);
    }

    #[test]
    fn register_in_caps_tracked_launches_and_evicts_idle_terminal_first() {
        // Fill to the cap with live "starting" entries — nothing evictable.
        let mut launches: Vec<TrackedLaunch> = (0..MAX_TRACKED_LAUNCHES)
            .map(|i| tracked(&format!("lch_{i}"), "starting", true))
            .collect();
        assert_eq!(
            register_in(&mut launches, "lch_new", String::new()),
            RegisterOutcome::Rejected
        );
        assert_eq!(launches.len(), MAX_TRACKED_LAUNCHES);

        // An idle terminal entry frees a slot for the newcomer.
        launches[0].state = "stopped".to_string();
        launches[0].polling = false;
        assert_eq!(
            register_in(&mut launches, "lch_new", String::new()),
            RegisterOutcome::NeedsPoll
        );
        assert_eq!(launches.len(), MAX_TRACKED_LAUNCHES);
        assert!(!launches.iter().any(|l| l.launch_id == "lch_0"));
        assert!(launches.iter().any(|l| l.launch_id == "lch_new"));
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

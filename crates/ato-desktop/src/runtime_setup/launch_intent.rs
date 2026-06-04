//! Desktop-side Runtime Setup launch-intent (#460 PR3b).
//!
//! When a capsule launch is interrupted because the host runtime needs setup
//! (or a reboot), the Desktop records *what the user was trying to open* and
//! sends them through Runtime Setup. Once the host runtime reaches `ready`, the
//! Desktop consumes that intent and returns the user to their original launch
//! instead of stranding them on the setup screen.
//!
//! This module is the Desktop counterpart of the CLI's
//! `ato-cli::application::runtime_setup_launch`: it reads/writes the **same**
//! marker file (`~/.ato/runtime-setup/launch-intent.json`) using the shared
//! [`capsule_core::runtime_setup::RuntimeSetupLaunchIntent`] model, so the two
//! sides cannot drift. PR3a wired the CLI read + `resume-after-reboot`
//! `launchContinuation`; this PR wires the Desktop write/consume + replay.
//!
//! Like the reboot-resume marker, the intent is advisory and self-healing: a
//! missing, corrupt, or stale intent is treated as "nothing to resume" and is
//! never surfaced as an error.

use std::path::{Path, PathBuf};

use capsule_core::runtime_setup::{
    LaunchIntentKind, LaunchIntentNextStep, RUNTIME_SETUP_LAUNCH_INTENT_SCHEMA_VERSION,
    RuntimeSetupLaunchIntent, RuntimeSetupStatus, ToolKind,
};
use gpui::App;

use crate::state::GuestRoute;

use super::push_runtime_setup;

/// Launch intents older than this are ignored. Mirrors the CLI's
/// `LAUNCH_INTENT_TTL_MS` so both sides discard the same stale markers.
const LAUNCH_INTENT_TTL_MS: u64 = 24 * 60 * 60 * 1000; // 24h

/// The bundled pgweb sample handle — a single-service, secret-free capsule used
/// as the Podman smoke (same handle PR3a's onboarding "Continue to sample app"
/// path launches).
const PGWEB_SAMPLE_HANDLE: &str = "capsule://github.com/sosedoff/pgweb";

/// Default on-disk path for the launch-intent marker. Never falls back to /tmp;
/// matches `ato-cli::application::runtime_setup_launch::launch_intent_path`.
pub(crate) fn launch_intent_path() -> PathBuf {
    capsule_core::common::paths::ato_path_or_workspace_tmp("runtime-setup/launch-intent.json")
}

/// Current wall-clock as unix milliseconds (0 on a pre-epoch clock).
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── marker IO (kept byte-compatible with the CLI side) ────────────────────────

/// Write a launch intent to `path`, creating parent directories as needed.
fn write_launch_intent_at(path: &Path, intent: &RuntimeSetupLaunchIntent) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(intent)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    std::fs::write(path, json)
}

/// Read a launch intent from `path`. Returns `None` when absent or corrupt.
fn read_launch_intent_at(path: &Path) -> Option<RuntimeSetupLaunchIntent> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remove the launch-intent marker at `path`. A missing file is success.
fn clear_launch_intent_at(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Whether `intent` is older than [`LAUNCH_INTENT_TTL_MS`] relative to
/// `now_unix_ms`. An intent stamped in the future is not stale.
fn is_intent_stale(intent: &RuntimeSetupLaunchIntent, now_unix_ms: u64) -> bool {
    now_unix_ms.saturating_sub(intent.created_at_unix_ms) > LAUNCH_INTENT_TTL_MS
}

/// Read and remove the intent at `path` in one step (consume). Returns the
/// intent only when present and not stale; clears the marker either way (a stale
/// intent is discarded). Idempotent: a second call returns `None`.
fn consume_launch_intent_at(path: &Path, now_unix_ms: u64) -> Option<RuntimeSetupLaunchIntent> {
    let intent = read_launch_intent_at(path)?;
    let _ = clear_launch_intent_at(path);
    if is_intent_stale(&intent, now_unix_ms) {
        None
    } else {
        Some(intent)
    }
}

// ── default-path convenience wrappers ─────────────────────────────────────────

/// Read the launch intent at the default path (`None` if absent/corrupt/stale).
/// Used to drive the "pending launch" banner; never clears the marker.
pub(crate) fn peek_pending_launch() -> Option<RuntimeSetupLaunchIntent> {
    let intent = read_launch_intent_at(&launch_intent_path())?;
    if is_intent_stale(&intent, now_unix_ms()) {
        None
    } else {
        Some(intent)
    }
}

/// Clear the launch intent at the default path (e.g. "Cancel pending launch").
pub(crate) fn clear_pending_launch() {
    let _ = clear_launch_intent_at(&launch_intent_path());
}

// ── route ⇄ intent mapping (pure) ─────────────────────────────────────────────

/// A launch intent that the Desktop cannot (yet) replay. Carried so the caller
/// can surface a clear, non-scary message instead of silently doing nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnsupportedLaunchIntent {
    pub message: String,
}

fn unsupported(message: impl Into<String>) -> UnsupportedLaunchIntent {
    UnsupportedLaunchIntent {
        message: message.into(),
    }
}

/// Whether a route is a capsule launch that needs the host OCI runtime (and so
/// is worth recording as a launch intent / gating on Runtime Setup). External
/// URLs, terminals, and already-resolved sessions are not.
pub(crate) fn route_needs_host_runtime(route: &GuestRoute) -> bool {
    matches!(
        route,
        GuestRoute::CapsuleHandle { .. }
            | GuestRoute::CapsuleUrl { .. }
            | GuestRoute::LocalManifest(_)
    )
}

/// Build a launch intent from an attempted launch route. Returns `None` for
/// routes that are not replayable host-runtime capsule launches, or when the
/// launch input is empty/untrusted — so we never record an intent we cannot or
/// should not resume.
pub(crate) fn intent_from_route(
    route: &GuestRoute,
    source_surface: &str,
    now_unix_ms: u64,
) -> Option<RuntimeSetupLaunchIntent> {
    let (intent_kind, launch_input, display_label) = match route {
        GuestRoute::CapsuleHandle {
            handle,
            label,
            community_toml_id,
        } => {
            let handle = handle.trim();
            if !is_trusted_launch_input(handle) {
                return None;
            }
            match community_toml_id.as_ref().map(|c| c.trim()) {
                Some(ctoml) if !ctoml.is_empty() => (
                    LaunchIntentKind::CommunityTomlId,
                    ctoml.to_string(),
                    Some(label.clone()),
                ),
                _ => (
                    LaunchIntentKind::CapsuleUrl,
                    handle.to_string(),
                    Some(label.clone()),
                ),
            }
        }
        GuestRoute::CapsuleUrl { handle, label, .. } => {
            let handle = handle.trim();
            if !is_trusted_launch_input(handle) {
                return None;
            }
            (
                LaunchIntentKind::CapsuleUrl,
                handle.to_string(),
                Some(label.clone()),
            )
        }
        // Other routes (external URLs, terminals, resolved sessions, local
        // manifests) are not replayable handles → don't record an intent.
        _ => return None,
    };

    Some(RuntimeSetupLaunchIntent {
        schema_version: RUNTIME_SETUP_LAUNCH_INTENT_SCHEMA_VERSION,
        created_at_unix_ms: now_unix_ms,
        source_surface: source_surface.to_string(),
        intent_kind,
        launch_input,
        expected_next_step: LaunchIntentNextStep::ContinueLaunch,
        request_id: None,
        display_label: display_label.filter(|l| !l.trim().is_empty()),
    })
}

/// A launch input is trusted enough to replay if it is non-empty and free of
/// whitespace / control characters (a capsule handle or `capsule://…` URL never
/// contains those). This rejects empty/garbled inputs before they reach disk.
fn is_trusted_launch_input(input: &str) -> bool {
    !input.is_empty() && !input.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Whether `handle` looks like a replayable capsule handle / `capsule://…` URL.
fn is_valid_capsule_handle(handle: &str) -> bool {
    is_trusted_launch_input(handle) && (handle.starts_with("capsule://") || handle.contains('/'))
}

fn capsule_handle_route(handle: String, label: Option<String>) -> GuestRoute {
    let label = label.filter(|l| !l.trim().is_empty()).unwrap_or_else(|| {
        // Derive a friendly label from the last path segment.
        handle
            .trim_start_matches("capsule://")
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(handle.as_str())
            .to_string()
    });
    GuestRoute::CapsuleHandle {
        handle,
        label,
        community_toml_id: None,
    }
}

/// Map a recorded launch intent back to a concrete [`GuestRoute`] to replay.
///
/// Supported today (PR3b): `CapsuleUrl` handles and the bundled `SampleCapsule`
/// (pgweb). `CommunityTomlId` and `SourceUrl` are recorded but not yet
/// replayable — they fail safely with a user-visible message rather than
/// launching the wrong thing.
pub(crate) fn launch_intent_to_guest_route(
    intent: &RuntimeSetupLaunchIntent,
) -> Result<GuestRoute, UnsupportedLaunchIntent> {
    match intent.intent_kind {
        LaunchIntentKind::SampleCapsule => {
            let input = intent.launch_input.trim();
            // Only pgweb is wired for launch continuity today. An empty slug
            // defaults to it (the onboarding sample); anything else is unknown.
            if input.is_empty() || sample_is_pgweb(input) {
                Ok(capsule_handle_route(
                    PGWEB_SAMPLE_HANDLE.to_string(),
                    intent.display_label.clone(),
                ))
            } else {
                Err(unsupported(format!("unknown sample capsule '{input}'")))
            }
        }
        LaunchIntentKind::CapsuleUrl => {
            let handle = intent.launch_input.trim();
            if is_valid_capsule_handle(handle) {
                Ok(capsule_handle_route(
                    handle.to_string(),
                    intent.display_label.clone(),
                ))
            } else {
                Err(unsupported(format!("invalid capsule handle '{handle}'")))
            }
        }
        LaunchIntentKind::CommunityTomlId => Err(unsupported(
            "resuming a community-recipe launch after Runtime Setup is not supported yet",
        )),
        LaunchIntentKind::SourceUrl => Err(unsupported(
            "resuming a source-URL launch after Runtime Setup is not supported yet",
        )),
    }
}

/// Whether a sample slug/handle refers to the bundled pgweb sample.
fn sample_is_pgweb(input: &str) -> bool {
    let input = input.trim().trim_start_matches("capsule://");
    input == "pgweb"
        || input == "github.com/sosedoff/pgweb"
        || input.ends_with("/sosedoff/pgweb")
}

// ── host-runtime readiness predicate ──────────────────────────────────────────

/// Whether the host OCI runtime is ready to launch a capsule, per a refreshed
/// [`RuntimeSetupStatus`]. Scoped to Podman (the host runtime Runtime Setup
/// prepares); Docker fallback is out of scope for #460 PR3b.
pub(crate) fn host_runtime_ready(status: &RuntimeSetupStatus) -> bool {
    status
        .get(ToolKind::Podman)
        .map(|tool| tool.ready)
        .unwrap_or(false)
}

// ── App-level orchestration ───────────────────────────────────────────────────

/// Record an interrupted capsule launch as a launch intent so Runtime Setup can
/// return the user to it once the host runtime is ready. Writing the marker is
/// best-effort: a failed write logs and is otherwise ignored (the user simply
/// won't be auto-resumed). Returns the recorded intent for banner hydration.
pub(crate) fn record_launch_intent(
    route: &GuestRoute,
    source_surface: &str,
) -> Option<RuntimeSetupLaunchIntent> {
    let intent = intent_from_route(route, source_surface, now_unix_ms())?;
    if let Err(err) = write_launch_intent_at(&launch_intent_path(), &intent) {
        tracing::warn!(error = %err, "failed to record runtime-setup launch intent");
        return None;
    }
    Some(intent)
}

/// Try to resume an interrupted capsule launch now that Runtime Setup reported a
/// refreshed status. No-op unless the host runtime is ready and a non-stale
/// launch intent is present. Consumes (clears) the marker **before** dispatching
/// so duplicate terminal/resume events cannot double-launch; an unsupported
/// intent or a failed dispatch surfaces a clear message on the Runtime Setup
/// surface rather than silently looping back into setup.
pub(crate) fn try_resume_launch_if_ready(cx: &mut App, status: &RuntimeSetupStatus) {
    if !host_runtime_ready(status) {
        return;
    }
    // Consume-before-dispatch (clears the on-disk marker) is the dedupe: resume
    // handling runs serially on the App foreground thread, so a second event
    // reads `None`. Stale/corrupt intents are discarded here harmlessly.
    let Some(intent) = consume_launch_intent_at(&launch_intent_path(), now_unix_ms()) else {
        return;
    };

    match launch_intent_to_guest_route(&intent) {
        Ok(route) => {
            tracing::info!(
                handle = %intent.launch_input,
                "Runtime ready — resuming interrupted capsule launch"
            );
            if let Err(err) = crate::window::launch_window::open_consent_window_for_route(cx, route)
            {
                push_launch_resume_error(
                    cx,
                    &format!("Couldn't resume the pending launch: {err:#}"),
                );
            }
        }
        Err(unsupported) => {
            push_launch_resume_error(
                cx,
                &format!(
                    "Runtime is ready, but Ato can't automatically reopen this launch \
                     ({}). Open it again from the app.",
                    unsupported.message
                ),
            );
        }
    }
}

/// What the Desktop should do with a pending launch when `resume-after-reboot`
/// reports back (#460 PR3b). Decided purely from the CLI payload so it is
/// unit-testable; the actual launch still re-gates on host readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RebootResumeLaunch {
    /// The CLI saw a live continuation; attempt resume against `status`. The
    /// Desktop still consumes its own marker and re-checks Podman readiness.
    Continue(Box<RuntimeSetupStatus>),
    /// The CLI considered the intent stale — clear the Desktop marker.
    Discard,
    /// Nothing pending, or still waiting — leave the marker and do nothing.
    None,
}

/// Interpret the `runtimeSetupResume` object emitted by
/// `ato internal runtime resume-after-reboot --json`. Reads `launchContinuation`
/// and the refreshed `runtimeSetupStatus`. Read-only / pure.
pub(crate) fn reboot_resume_launch_action(resume_inner: &serde_json::Value) -> RebootResumeLaunch {
    let status = resume_inner
        .get("launchContinuation")
        .and_then(|c| c.get("status"))
        .and_then(|s| s.as_str());
    match status {
        Some("continue") => {
            // Re-derive readiness from the refreshed status snapshot rather than
            // trusting the continuation alone.
            match resume_inner
                .get("runtimeSetupStatus")
                .and_then(|s| serde_json::from_value::<RuntimeSetupStatus>(s.clone()).ok())
            {
                Some(status) => RebootResumeLaunch::Continue(Box::new(status)),
                None => RebootResumeLaunch::None,
            }
        }
        Some("discard") => RebootResumeLaunch::Discard,
        // "pending", null, or absent → nothing to do yet.
        _ => RebootResumeLaunch::None,
    }
}

/// Apply a reboot-resume launch decision on the App thread: resume if ready,
/// clear a discarded marker, else no-op.
pub(crate) fn apply_reboot_resume_launch(cx: &mut App, resume_inner: &serde_json::Value) {
    match reboot_resume_launch_action(resume_inner) {
        RebootResumeLaunch::Continue(status) => try_resume_launch_if_ready(cx, &status),
        RebootResumeLaunch::Discard => clear_pending_launch(),
        RebootResumeLaunch::None => {}
    }
}

/// Surface a launch-resume failure on whichever Runtime Setup surface is open.
fn push_launch_resume_error(cx: &mut App, message: &str) {
    tracing::warn!(message, "runtime-setup launch resume failed");
    let payload = serde_json::json!({
        "ok": false,
        "launchResumeFailed": true,
        "error": { "message": message },
    });
    push_runtime_setup(cx, &payload.to_string());
}

/// Build the pending-launch banner hydrate payload (an explicit `null` clears
/// the banner).
fn pending_launch_payload(intent: Option<&RuntimeSetupLaunchIntent>) -> String {
    let pending = intent.map(|intent| {
        serde_json::json!({
            "label": intent.display_label.clone().unwrap_or_else(|| intent.launch_input.clone()),
            "launchInput": intent.launch_input,
            "kind": format!("{:?}", intent.intent_kind),
        })
    });
    serde_json::json!({ "ok": true, "pendingLaunch": pending }).to_string()
}

/// Hydrate the Runtime Setup surfaces with the current pending-launch banner
/// state (or an explicit clear). Pushed when an intent is recorded and when one
/// is consumed/cancelled.
pub(crate) fn push_pending_launch(cx: &mut App, intent: Option<&RuntimeSetupLaunchIntent>) {
    push_runtime_setup(cx, &pending_launch_payload(intent));
}

/// Launch a capsule, routing through Runtime Setup first when the host OCI
/// runtime is not ready (#460 PR3b, Case B).
///
/// Only capsule launches that need the host runtime are gated, and only when
/// Podman is the selected, enabled engine (Docker/other engines fall straight
/// through — fallback is out of scope). The readiness probe runs off the UI
/// thread; the launch decision is applied on the foreground:
/// - host runtime ready, or status indeterminate (fail open) → open the normal
///   consent/launch window;
/// - not ready → record the launch intent and open Runtime Setup with a pending
///   launch banner, so completion/reboot resume returns the user here.
pub(crate) fn open_capsule_launch_gated(
    cx: &mut App,
    route: GuestRoute,
    requested_client: crate::state::session::SessionClientKind,
    source_surface: &str,
) {
    let config = crate::config::load_config();
    let podman_engine = config.runtime.backend_engines.oci == crate::config::OciBackendEngine::Podman
        && config.runtime.podman_enabled;

    if !route_needs_host_runtime(&route) || !podman_engine {
        open_consent_or_log(cx, route, requested_client);
        return;
    }

    let surface = source_surface.to_string();
    let async_app = cx.to_async();
    let fe = cx.foreground_executor().clone();
    let be = cx.background_executor().clone();
    let be_work = be.clone();
    fe.spawn(async move {
        let status = be_work
            .spawn(async move {
                crate::orchestrator::resolve_ato_binary()
                    .ok()
                    .and_then(|ato| super::status::run_setup_status(&ato).ok())
                    .and_then(|value| serde_json::from_value::<RuntimeSetupStatus>(value).ok())
            })
            .await;
        let _ = async_app.update(move |cx| {
            match status.as_ref() {
                // Status read AND host runtime not ready → divert to Runtime Setup.
                Some(status) if !host_runtime_ready(status) => {
                    let intent = record_launch_intent(&route, &surface);
                    open_runtime_setup_for_pending_launch(cx, intent.as_ref());
                }
                // Ready, or status indeterminate (probe failed) → fail open and
                // launch normally; never block a launch on a failed probe.
                _ => open_consent_or_log(cx, route, requested_client),
            }
        });
    })
    .detach();
}

fn open_consent_or_log(
    cx: &mut App,
    route: GuestRoute,
    requested_client: crate::state::session::SessionClientKind,
) {
    if let Err(err) = crate::window::launch_window::open_consent_window_for_route_with_client(
        cx,
        route,
        requested_client,
    ) {
        tracing::error!(error = %err, "open_consent_window_for_route failed");
    }
}

/// Open the Runtime Setup surface (Settings → Runtime) for a pending launch and
/// hydrate it with the banner + a fresh status, once the WebView is idle.
fn open_runtime_setup_for_pending_launch(
    cx: &mut App,
    intent: Option<&RuntimeSetupLaunchIntent>,
) {
    if let Err(err) = crate::window::settings_window::open_settings_window(cx) {
        tracing::error!(error = %err, "failed to open Settings for pending launch");
        return;
    }
    // The settings WebView registers its shell asynchronously, so defer the
    // banner + status push until it is idle (mirrors the status-probe pattern).
    let payload = pending_launch_payload(intent);
    let async_app = cx.to_async();
    let fe = cx.foreground_executor().clone();
    let be = cx.background_executor().clone();
    fe.spawn(async move {
        crate::webview_init_guard::wait_until_idle(&be).await;
        let _ = async_app.update(move |cx| {
            push_runtime_setup(cx, &payload);
            super::status::spawn_runtime_setup_status(cx, None);
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capsule_intent(kind: LaunchIntentKind, input: &str) -> RuntimeSetupLaunchIntent {
        RuntimeSetupLaunchIntent {
            schema_version: RUNTIME_SETUP_LAUNCH_INTENT_SCHEMA_VERSION,
            created_at_unix_ms: 1_000,
            source_surface: "launch_flow".to_string(),
            intent_kind: kind,
            launch_input: input.to_string(),
            expected_next_step: LaunchIntentNextStep::ContinueLaunch,
            request_id: None,
            display_label: Some("pgweb".to_string()),
        }
    }

    fn ready_status(podman_ready: bool) -> RuntimeSetupStatus {
        use capsule_core::runtime_setup::{RecommendedAction, ToolSource, ToolStatus};
        let podman = if podman_ready {
            ToolStatus::ready(ToolKind::Podman, ToolSource::External, None, "ready")
        } else {
            ToolStatus::missing(
                ToolKind::Podman,
                RecommendedAction::PrepareHostRuntime,
                "not ready",
            )
        };
        RuntimeSetupStatus {
            tools: vec![podman],
            windows_substrate: None,
        }
    }

    // ── marker IO ──────────────────────────────────────────────────────────────

    #[test]
    fn write_read_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("runtime-setup/launch-intent.json");
        let intent = capsule_intent(LaunchIntentKind::CapsuleUrl, "capsule://github.com/x/y");
        write_launch_intent_at(&path, &intent).expect("write");
        assert_eq!(read_launch_intent_at(&path), Some(intent));
    }

    #[test]
    fn read_absent_and_corrupt_are_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_launch_intent_at(&dir.path().join("nope.json")), None);
        let corrupt = dir.path().join("c.json");
        std::fs::write(&corrupt, "{ not json").expect("write");
        assert_eq!(read_launch_intent_at(&corrupt), None);
    }

    #[test]
    fn consume_returns_then_clears_idempotently() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("launch-intent.json");
        write_launch_intent_at(
            &path,
            &capsule_intent(LaunchIntentKind::CapsuleUrl, "capsule://github.com/x/y"),
        )
        .expect("write");
        assert!(consume_launch_intent_at(&path, 2_000).is_some());
        assert!(!path.exists(), "consume clears the marker");
        // Second consume finds nothing — no double launch.
        assert_eq!(consume_launch_intent_at(&path, 2_000), None);
    }

    #[test]
    fn consume_discards_stale_intent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("launch-intent.json");
        write_launch_intent_at(
            &path,
            &capsule_intent(LaunchIntentKind::CapsuleUrl, "capsule://github.com/x/y"),
        )
        .expect("write");
        let got = consume_launch_intent_at(&path, 1_000 + LAUNCH_INTENT_TTL_MS + 1);
        assert_eq!(got, None, "stale intent is not resumed");
        assert!(!path.exists(), "stale intent is still cleared");
    }

    // ── intent_from_route ──────────────────────────────────────────────────────

    #[test]
    fn intent_from_capsule_handle_is_capsule_url() {
        let route = GuestRoute::CapsuleHandle {
            handle: "github.com/sosedoff/pgweb".to_string(),
            label: "pgweb".to_string(),
            community_toml_id: None,
        };
        let intent = intent_from_route(&route, "launch_flow", 5_000).expect("intent");
        assert_eq!(intent.intent_kind, LaunchIntentKind::CapsuleUrl);
        assert_eq!(intent.launch_input, "github.com/sosedoff/pgweb");
        assert_eq!(intent.display_label.as_deref(), Some("pgweb"));
        assert_eq!(intent.created_at_unix_ms, 5_000);
    }

    #[test]
    fn intent_from_capsule_handle_with_ctoml_is_community() {
        let route = GuestRoute::CapsuleHandle {
            handle: "github.com/acme/chat".to_string(),
            label: "chat".to_string(),
            community_toml_id: Some("ctoml_abc".to_string()),
        };
        let intent = intent_from_route(&route, "omnibar", 5_000).expect("intent");
        assert_eq!(intent.intent_kind, LaunchIntentKind::CommunityTomlId);
        assert_eq!(intent.launch_input, "ctoml_abc");
    }

    #[test]
    fn intent_from_empty_or_untrusted_handle_is_none() {
        // Empty handle.
        let empty = GuestRoute::CapsuleHandle {
            handle: "   ".to_string(),
            label: "x".to_string(),
            community_toml_id: None,
        };
        assert!(intent_from_route(&empty, "launch_flow", 1).is_none());
        // Whitespace / control chars in the handle → untrusted.
        let bad = GuestRoute::CapsuleHandle {
            handle: "capsule://a b/c".to_string(),
            label: "x".to_string(),
            community_toml_id: None,
        };
        assert!(intent_from_route(&bad, "launch_flow", 1).is_none());
    }

    #[test]
    fn intent_from_external_url_is_none() {
        let route = GuestRoute::ExternalUrl(url::Url::parse("https://example.com").unwrap());
        assert!(intent_from_route(&route, "launch_flow", 1).is_none());
    }

    // ── launch_intent_to_guest_route ───────────────────────────────────────────

    #[test]
    fn capsule_url_intent_maps_to_capsule_handle() {
        let intent = capsule_intent(LaunchIntentKind::CapsuleUrl, "capsule://github.com/x/y");
        match launch_intent_to_guest_route(&intent).expect("route") {
            GuestRoute::CapsuleHandle { handle, .. } => {
                assert_eq!(handle, "capsule://github.com/x/y");
            }
            other => panic!("expected CapsuleHandle, got {other:?}"),
        }
    }

    #[test]
    fn sample_pgweb_intent_maps_to_pgweb_handle() {
        for input in ["", "pgweb", "capsule://github.com/sosedoff/pgweb"] {
            let intent = capsule_intent(LaunchIntentKind::SampleCapsule, input);
            match launch_intent_to_guest_route(&intent).expect("route") {
                GuestRoute::CapsuleHandle { handle, .. } => {
                    assert_eq!(handle, PGWEB_SAMPLE_HANDLE, "input {input:?}");
                }
                other => panic!("expected CapsuleHandle, got {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_sample_fails_safely() {
        let intent = capsule_intent(LaunchIntentKind::SampleCapsule, "mystery-app");
        assert!(launch_intent_to_guest_route(&intent).is_err());
    }

    #[test]
    fn invalid_capsule_url_fails_safely() {
        // No slash, not a capsule:// URL → not a valid handle.
        let intent = capsule_intent(LaunchIntentKind::CapsuleUrl, "notahandle");
        assert!(launch_intent_to_guest_route(&intent).is_err());
    }

    #[test]
    fn community_and_source_kinds_are_unsupported_for_now() {
        assert!(
            launch_intent_to_guest_route(&capsule_intent(
                LaunchIntentKind::CommunityTomlId,
                "ctoml_abc"
            ))
            .is_err()
        );
        assert!(
            launch_intent_to_guest_route(&capsule_intent(
                LaunchIntentKind::SourceUrl,
                "https://github.com/x/y"
            ))
            .is_err()
        );
    }

    // ── host_runtime_ready ─────────────────────────────────────────────────────

    #[test]
    fn host_runtime_ready_tracks_podman() {
        assert!(host_runtime_ready(&ready_status(true)));
        assert!(!host_runtime_ready(&ready_status(false)));
        // No Podman tool reported → not ready.
        assert!(!host_runtime_ready(&RuntimeSetupStatus::default()));
    }

    // ── reboot_resume_launch_action ────────────────────────────────────────────

    #[test]
    fn reboot_resume_continue_with_status_is_continue() {
        let inner = serde_json::json!({
            "launchContinuation": { "status": "continue", "intent": {} },
            "runtimeSetupStatus": { "tools": [], "windows_substrate": null },
        });
        match reboot_resume_launch_action(&inner) {
            RebootResumeLaunch::Continue(_) => {}
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    #[test]
    fn reboot_resume_continue_without_status_is_none() {
        // Defensive: a "continue" with no status snapshot cannot be re-gated.
        let inner = serde_json::json!({
            "launchContinuation": { "status": "continue" },
        });
        assert_eq!(reboot_resume_launch_action(&inner), RebootResumeLaunch::None);
    }

    #[test]
    fn reboot_resume_discard_and_pending_and_null() {
        let discard = serde_json::json!({ "launchContinuation": { "status": "discard" } });
        assert_eq!(
            reboot_resume_launch_action(&discard),
            RebootResumeLaunch::Discard
        );
        let pending = serde_json::json!({ "launchContinuation": { "status": "pending" } });
        assert_eq!(
            reboot_resume_launch_action(&pending),
            RebootResumeLaunch::None
        );
        let null = serde_json::json!({ "launchContinuation": null });
        assert_eq!(reboot_resume_launch_action(&null), RebootResumeLaunch::None);
        let absent = serde_json::json!({});
        assert_eq!(
            reboot_resume_launch_action(&absent),
            RebootResumeLaunch::None
        );
    }

    // ── route_needs_host_runtime ───────────────────────────────────────────────

    #[test]
    fn route_needs_host_runtime_only_for_capsule_launches() {
        assert!(route_needs_host_runtime(&GuestRoute::CapsuleHandle {
            handle: "github.com/x/y".to_string(),
            label: "y".to_string(),
            community_toml_id: None,
        }));
        assert!(!route_needs_host_runtime(&GuestRoute::ExternalUrl(
            url::Url::parse("https://example.com").unwrap()
        )));
        assert!(!route_needs_host_runtime(&GuestRoute::Terminal {
            session_id: "s".to_string(),
        }));
    }
}

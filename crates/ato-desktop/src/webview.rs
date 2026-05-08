use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine as _;
use capsule_core::common::paths::ato_path_or_workspace_tmp;
use gpui::{AnyWindowHandle, AppContext, AsyncApp, Window};
use http::header::{CONTENT_TYPE, COOKIE};
use http::{HeaderMap, HeaderValue};
use include_dir::{include_dir, Dir};
#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::runtime::AnyObject;
#[cfg(target_os = "macos")]
use objc2::{msg_send, sel, ClassType};
#[cfg(target_os = "macos")]
use objc2_app_kit::NSView;
#[cfg(target_os = "macos")]
use objc2_foundation::MainThreadMarker;
use serde::Deserialize;
use serde_json::Value;
use wry::http::{Request, Response};
#[cfg(target_os = "macos")]
use wry::WebViewExtMacOS;
use wry::{
    NewWindowResponse, PageLoadEvent, Rect, RequestAsyncResponder, WebContext, WebView,
    WebViewBuilder,
};

use crate::automation::command::{AutomationCommand, PendingAutomationRequest};
use crate::automation::AutomationHost;
use crate::bridge::{BridgeProxy, GuestBridgeResponse, GuestSessionContext, ShellEvent};
use crate::config::SecretEntry;
use crate::logging::TARGET_FAVICON;
use crate::orchestrator::{
    resolve_and_start_guest, spawn_cli_session, spawn_log_tail_session, spawn_terminal,
    stop_guest_session, take_pending_cli_command, take_pending_share_terminal, GuestLaunchSession,
    LaunchError, SpawnKind, SpawnSpec,
};
use crate::state::{
    ActiveWebPane, ActivityTone, AppState, AuthMode, AuthPolicyRegistry, AuthSessionStatus,
    BrowserCommandKind, CapabilityGrant, GuestRoute, PaneBounds, PendingConfigRequest,
    PendingConsentRequest, ShellMode, WebSessionState,
};
use crate::terminal::{TerminalCore, TryRecvOutput};
use crate::ui::share::{resolve_share_icon, web_favicon_origin, ShareIconSource};
use capsule_wire::handle::CapsuleDisplayStrategy;
use tracing::{debug, error, info, warn};

const DEVTOOLS_DEBUG_ENV: &str = "ATO_DESKTOP_DEVTOOLS_DEBUG";
const HOST_PANEL_SCHEME: &str = "capsule-host";
const HOST_PANEL_PROFILE: &str = "host-panel";
const HOST_PANEL_OVERLAY_PANE_ID: usize = usize::MAX;
static HOST_PANEL_DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/frontend/dist");

/// Preload injected into `terminal://` WebViews so xterm.js can reach the host.
///
/// The xterm.js page (see `assets/terminal/index.html`) calls
/// `window.__ato_terminal_bridge(jsonString)` on every keystroke and resize.
/// Rust's [`bridge::GuestBridgeRequest`] uses `#[serde(tag = "kind", rename_all = "kebab-case")]`,
/// so we translate the JS-side `{ type: "TerminalInput" | "TerminalResize" | … }`
/// envelope into `{ kind: "terminal-input" | "terminal-resize" | … }` before
/// handing it to `window.ipc.postMessage`. Unknown types (e.g. `TerminalReady`)
/// are still forwarded so the host's activity log sees them; they are harmless
/// if serde rejects them.
const TERMINAL_BRIDGE_PRELOAD: &str = r#"(function () {
  function toKebab(name) {
    return String(name)
      .replace(/([a-z0-9])([A-Z])/g, "$1-$2")
      .replace(/([A-Z]+)([A-Z][a-z])/g, "$1-$2")
      .toLowerCase();
  }
  window.__ato_terminal_bridge = function (body) {
    try {
      var obj = typeof body === "string" ? JSON.parse(body) : body;
      if (!obj || typeof obj !== "object") return;
      var kind = toKebab(obj.type || "");
      var payload = {};
      Object.keys(obj).forEach(function (k) {
        if (k !== "type") payload[k] = obj[k];
      });
      payload.kind = kind;
      if (window.ipc && typeof window.ipc.postMessage === "function") {
        window.ipc.postMessage(JSON.stringify(payload));
      }
    } catch (e) {
      try { console.error("ato-terminal-bridge error", e); } catch (_) {}
    }
  };
})();
"#;

fn devtools_debug_enabled() -> bool {
    std::env::var_os(DEVTOOLS_DEBUG_ENV)
        .map(|value| {
            let value = value.to_string_lossy();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

fn log_devtools(message: impl AsRef<str>) {
    if devtools_debug_enabled() {
        eprintln!("[ato-desktop][devtools] {}", message.as_ref());
    }
}

fn format_bounds(bounds: PaneBounds) -> String {
    format!(
        "x={:.1} y={:.1} w={:.1} h={:.1}",
        bounds.x, bounds.y, bounds.width, bounds.height
    )
}

fn format_optional_bounds(bounds: Option<PaneBounds>) -> String {
    bounds
        .map(format_bounds)
        .unwrap_or_else(|| "<unavailable>".to_string())
}

struct AuthHandoffSignal {
    pane_id: usize,
    url: String,
}

pub struct WebViewManager {
    views: HashMap<usize, ManagedWebView>,
    overlay_host_panel: Option<ManagedWebView>,
    pending_launches: HashMap<String, PendingLaunch>,
    active_pane_id: Option<usize>,
    responder_target: Option<ResponderTarget>,
    async_app: AsyncApp,
    window_handle: AnyWindowHandle,
    preload_registry: PreloadRegistry,
    protocol_router: ProtocolRouter,
    bridge: BridgeProxy,
    visibility_cache: HashMap<usize, bool>,
    pending_auth_handoffs: Arc<Mutex<Vec<AuthHandoffSignal>>>,
    /// `ato://` deep links observed by a WebView navigation handler.
    /// These never load inside the WebView; they are forwarded to
    /// AppState::handle_host_route on the next sync_from_state pass
    /// so OAuth callbacks delivered via the in-app sign-in flow
    /// reach the same code path as macOS Launch Services callbacks.
    pending_callback_urls: Arc<Mutex<Vec<String>>>,
    /// Live PTY sessions keyed by session_id.
    terminal_sessions: HashMap<String, Box<dyn TerminalCore>>,
    /// Session IDs that have already exited — prevents re-spawning a shell after a share terminal ends.
    completed_terminal_sessions: HashSet<String>,
    /// Spawn errors queued until terminal page is loaded, then shown via xterm error banner.
    pending_terminal_errors: HashMap<String, String>,
    /// Stop-signal senders for background log followers keyed by session_id.
    log_followers: HashMap<String, Sender<()>>,
    /// Automation host — handles AI-agent socket requests.
    automation: AutomationHost,
    /// Whether `prewarm` has been invoked. WKWebView framework load
    /// only needs to happen once per process; subsequent real tabs
    /// reuse the warm XPC services.
    prewarmed: bool,
    /// Sender for the per-pane "is there a newer registry version?" check.
    /// Set once at startup by `DesktopShell::install_capsule_update_channel`;
    /// cloned per spawned worker so result delivery survives manager lifecycle.
    /// `None` until the channel is installed (e.g. in unit tests where the
    /// background check is irrelevant).
    capsule_update_tx: Option<std::sync::mpsc::Sender<(usize, crate::state::CapsuleUpdate)>>,
    /// Shared `WebContext` so every pane uses the same on-disk
    /// `WKWebsiteDataStore`. This makes cookies and localStorage
    /// persist across tab open/close and across restarts (data
    /// directory: `~/.ato/desktop/webcontext/`). Without it each
    /// `WebContext::new(None)` was ephemeral and ato.run sign-in
    /// state was lost the moment the pane was rebuilt.
    web_context: WebContext,
    /// Retained-session table — RFC: SURFACE_CLOSE_SEMANTICS. Pane
    /// close demotes the session to this table instead of stopping
    /// it; reopen within TTL hits the Phase 1 fast path naturally
    /// (the on-disk session record stays alive). TTL expiry / app
    /// quit / LRU eviction stop sessions in fire-and-forget
    /// background threads so the UI never blocks on
    /// `ato app session stop`.
    retention: crate::retention::RetentionTable,
}

struct ManagedWebView {
    pane_id: usize,
    route: GuestRoute,
    route_key: String,
    bounds: PaneBounds,
    host_panel_payload_json: Option<String>,
    launched_session: Option<GuestLaunchSession>,
    webview: WebView,
    #[cfg(target_os = "macos")]
    frame_host: Option<Retained<NSView>>,
    // _context removed: WebContext is now shared on WebViewManager
    // (persistent on-disk store) so it outlives every ManagedWebView
    // by definition.
}

#[derive(Debug, Deserialize)]
struct DesktopAuthHandoff {
    session_token: String,
    site_base_url: String,
    api_base_url: String,
}

#[derive(Debug, Deserialize)]
struct HostPanelIpcEnvelope {
    #[serde(rename = "__ato_host_panel__")]
    message: HostPanelIpcMessage,
    #[serde(rename = "paneId")]
    pane_id: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct HostPanelIpcMessage {
    kind: String,
    path: Option<String>,
    command: Option<String>,
    payload: Option<Value>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
}

impl ManagedWebView {
    fn actual_bounds(&self) -> Option<PaneBounds> {
        #[cfg(target_os = "macos")]
        if let Some(frame_host) = &self.frame_host {
            return Some(bounds_from_ns_view(frame_host));
        }

        self.webview.bounds().ok().map(rect_to_bounds)
    }

    fn apply_bounds(&mut self, bounds: PaneBounds) -> Result<()> {
        #[cfg(target_os = "macos")]
        if let Some(frame_host) = &self.frame_host {
            apply_bounds_to_macos_frame_host(frame_host, &self.webview, bounds)?;
            self.bounds = bounds;
            return Ok(());
        }

        self.webview.set_bounds(bounds_to_rect(bounds))?;
        self.bounds = bounds;
        Ok(())
    }

    fn set_visible(&self, visible: bool) -> Result<()> {
        #[cfg(target_os = "macos")]
        if let Some(frame_host) = &self.frame_host {
            frame_host.setHidden(!visible);
            return Ok(());
        }

        self.webview.set_visible(visible)?;
        Ok(())
    }
}

impl Drop for ManagedWebView {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(frame_host) = &self.frame_host {
            frame_host.removeFromSuperview();
        }
    }
}

struct PendingLaunch {
    pane_id: usize,
    route_key: String,
    /// Mirrors the `handle` argument of the originating
    /// `ensure_pending_local_launch` call. Carried alongside
    /// `route_key` so the disconnect-fallback path in the drain can
    /// still produce a `PendingLaunchResult` whose `handle` field is
    /// authoritative — the receiver only sees the worker's own
    /// `PendingLaunchResult`, never the queue entry.
    handle: String,
    receiver: Receiver<PendingLaunchResult>,
}

struct PendingLaunchResult {
    route_key: String,
    /// Original handle this launch was queued under (mirrors the
    /// `handle` arg of `ensure_pending_local_launch`). Used by the
    /// drain path to reset the per-handle consent retry budget on a
    /// successful launch — the previous payload model derived the
    /// handle from the resulting `session` only on the success path,
    /// which gave the consent retry-once gate no anchor in the
    /// `Err(MissingConsent { handle, .. })` branch.
    handle: String,
    /// Carries either the live session or a typed `LaunchError`. The
    /// `MissingConfig` variant must reach `drain_pending_launches`
    /// intact so the modal can be populated — collapsing to `String`
    /// here would erase the structured payload Day 4 retry depends on.
    session: Result<GuestLaunchSession, LaunchError>,
}

impl WebViewManager {
    pub fn new(window_handle: AnyWindowHandle, async_app: AsyncApp) -> Self {
        let automation = AutomationHost::new();
        automation.start();

        // Spawn a foreground polling task that wakes GPUI when automation requests arrive.
        // The socket thread sets `has_pending = true`; this loop detects it within 50ms.
        {
            use std::sync::atomic::Ordering;
            use std::time::Duration;
            let has_pending = Arc::clone(&automation.has_pending);
            let fe = async_app.foreground_executor().clone();
            let be = async_app.background_executor().clone();
            let async_app_poll = async_app.clone();
            fe.spawn(async move {
                loop {
                    be.timer(Duration::from_millis(50)).await;
                    if has_pending.swap(false, Ordering::Relaxed) {
                        notify_window(async_app_poll.clone(), window_handle);
                    }
                }
            })
            .detach();
        }

        let web_context_dir = ato_path_or_workspace_tmp("desktop/webcontext");
        let _ = std::fs::create_dir_all(&web_context_dir);
        let web_context = WebContext::new(Some(web_context_dir));

        Self {
            views: HashMap::new(),
            overlay_host_panel: None,
            pending_launches: HashMap::new(),
            active_pane_id: None,
            responder_target: None,
            async_app,
            window_handle,
            preload_registry: PreloadRegistry,
            protocol_router: ProtocolRouter,
            bridge: BridgeProxy::new(),
            visibility_cache: HashMap::new(),
            pending_auth_handoffs: Arc::new(Mutex::new(Vec::new())),
            pending_callback_urls: Arc::new(Mutex::new(Vec::new())),
            terminal_sessions: HashMap::new(),
            completed_terminal_sessions: HashSet::new(),
            pending_terminal_errors: HashMap::new(),
            log_followers: HashMap::new(),
            automation,
            prewarmed: false,
            web_context,
            capsule_update_tx: None,
            retention: crate::retention::RetentionTable::with_defaults(),
        }
    }

    /// Hand the manager a Sender it should clone whenever a capsule pane
    /// launches, so the worker thread can post its `CapsuleUpdate` result
    /// back to `DesktopShell::poll_capsule_updates`. Calling this with
    /// `None` (or never calling it) disables the background check — handy
    /// in unit tests that don't need the registry round-trip.
    pub fn install_capsule_update_channel(
        &mut self,
        tx: std::sync::mpsc::Sender<(usize, crate::state::CapsuleUpdate)>,
    ) {
        self.capsule_update_tx = Some(tx);
    }

    /// Build a 1×1 throwaway WebView pointed at about:blank so the
    /// macOS WebKit framework + WKWebView XPC services
    /// (com.apple.WebKit.WebContent / .Networking / .GPU) load early
    /// in the app lifecycle. Without this, the very first real tab
    /// pays the framework + 3-process spawn cost on the UI thread,
    /// which the user sees as a multi-second hang on app launch.
    /// Subsequent tabs are fast because the XPC services and dyld
    /// caches are already warm. Idempotent — runs once.
    pub fn prewarm(&mut self, window: &Window) {
        if self.prewarmed {
            return;
        }
        self.prewarmed = true;

        use wry::dpi::{LogicalPosition, LogicalSize};
        // Position off-screen and 1×1 so the prewarm view is invisible
        // even briefly. Errors here are silently ignored — prewarm is
        // best-effort optimisation. Use the shared web_context so the
        // prewarm and the real tabs share one on-disk data store.
        let result = WebViewBuilder::new_with_web_context(&mut self.web_context)
            .with_url("about:blank")
            .with_visible(false)
            .with_bounds(Rect {
                position: LogicalPosition::new(-100, -100).into(),
                size: LogicalSize::new(1u32, 1u32).into(),
            })
            .build_as_child(window);
        // Drop the WebView on this scope exit. The XPC services
        // remain alive in the OS, ready for the next real WebView.
        drop(result);
    }

    pub fn sync_from_state(&mut self, window: &Window, state: &mut AppState) {
        // Prewarm the WKWebView framework + XPC services before the
        // first real tab is built. After the first sync_from_state
        // call this is a no-op.
        self.prewarm(window);

        // RFC: SURFACE_CLOSE_SEMANTICS — opportunistic TTL sweep on
        // every render. Cheap (≤ cap entries to walk); fires only
        // graceful background stops so the UI thread is untouched.
        // Idle apps may keep a session past its TTL until the next
        // render — `Drop` covers any leftover at process exit.
        self.sweep_expired_retention(state);

        // Drain ato:// / capsule:// deep links seen by the WebView
        // navigation handler so OAuth callbacks delivered through
        // the in-app sign-in WebView reach handle_host_route. This
        // is the same code path the macOS Launch Services route
        // (open_url_bridge) uses for browser-delivered callbacks.
        let callback_urls: Vec<String> = {
            let mut q = self
                .pending_callback_urls
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            q.drain(..).collect()
        };
        for url in callback_urls {
            state.handle_host_route(&url);
        }

        // Drain auth handoff signals from navigation handlers before any other reconciliation.
        let auth_signals: Vec<AuthHandoffSignal> = {
            let mut q = self
                .pending_auth_handoffs
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            q.drain(..).collect()
        };
        for signal in auth_signals {
            let session_id = state.begin_auth_handoff(signal.pane_id, &signal.url);
            if let Some(s) = state
                .auth_sessions
                .iter_mut()
                .find(|s| s.session_id == session_id)
            {
                s.status = AuthSessionStatus::OpenedInBrowser;
            }
            let _ = Command::new("open").arg(&signal.url).status();
        }

        // Pull bridge activity into app state first so rebuilds always see the latest guest messages.
        state.extend_activity(self.bridge.drain_activity());

        // RFC: SURFACE_CLOSE_SEMANTICS §6.4 — mirror retention size
        // into AppState so omnibar suggestions / chrome can render
        // "Stop all retained sessions (N)" without holding a back-
        // reference to WebViewManager.
        state.retention_count = self.retention.len();
        let shell_events = self.bridge.drain_shell_events();
        self.apply_shell_events(&shell_events, state);
        state.apply_shell_events(shell_events);
        self.drain_pending_launches(window, state);

        // Dispatch automation requests early so OpenUrl (and ListPanes) work even
        // when there is no active WebView pane yet.
        self.dispatch_automation_requests(state);

        let Some(active) = state.active_web_pane() else {
            if let Some(previous_pane_id) = self.active_pane_id.take() {
                self.set_cached_visibility(previous_pane_id, false, state);
                self.bridge
                    .log(ActivityTone::Info, "Detached active child webview");
            }
            self.automation.set_active_pane(None);
            self.sync_responder_target(state);
            return;
        };

        if self.active_pane_id != Some(active.pane_id) {
            if let Some(previous_pane_id) = self.active_pane_id {
                self.set_cached_visibility(previous_pane_id, false, state);
            }
            self.active_pane_id = Some(active.pane_id);
            self.automation.set_active_pane(Some(active.pane_id));
        }

        let route_key = active.route.to_string();
        let reuse_action = self
            .views
            .get(&active.pane_id)
            .map(|existing| {
                reuse_action(
                    existing.pane_id,
                    &existing.route,
                    &existing.route_key,
                    &active,
                )
            })
            .unwrap_or(WebViewReuseAction::Rebuild);

        if matches!(reuse_action, WebViewReuseAction::Rebuild) {
            if let Some(previous) = self.views.remove(&active.pane_id) {
                self.automation.fail_requests_for_pane(active.pane_id);
                self.automation.mark_page_unloaded(active.pane_id);
                self.stop_launched_session(&previous, state);
                state.sync_web_session_state(previous.pane_id, WebSessionState::Closed);
            }

            match &active.route {
                GuestRoute::CapsuleHandle { handle, .. } => {
                    self.ensure_pending_local_launch(active.pane_id, &route_key, handle, state);
                }
                _ if active.profile == HOST_PANEL_PROFILE => {
                    let payload = crate::settings::host_panel_payload_for_url(state, &route_key);
                    match self.build_host_panel_child_webview(
                        window,
                        active.pane_id,
                        active.route.clone(),
                        active.bounds,
                        Some(payload),
                    ) {
                        Ok(webview) => {
                            state.sync_web_session_state(active.pane_id, WebSessionState::Mounted);
                            self.bridge.log(
                                ActivityTone::Info,
                                format!("Built host panel WebView for {}", active.route),
                            );
                            self.views.insert(active.pane_id, webview);
                        }
                        Err(error) => {
                            state.sync_web_session_state(active.pane_id, WebSessionState::Closed);
                            state.push_activity(
                                ActivityTone::Error,
                                format!("Failed to build host panel WebView: {error}"),
                            );
                            return;
                        }
                    }
                }
                _ => match self.build_webview(
                    window,
                    &active,
                    None,
                    state.auth_policy_registry.clone(),
                ) {
                    Ok(webview) => {
                        if !route_requires_ready_signal(&active.route) {
                            state.sync_web_session_state(active.pane_id, WebSessionState::Mounted);
                        }
                        self.bridge.log(
                            ActivityTone::Info,
                            format!("Built child webview for {}", active.route),
                        );
                        self.views.insert(active.pane_id, webview);
                    }
                    Err(error) => {
                        state.sync_web_session_state(active.pane_id, WebSessionState::Closed);
                        state.push_activity(
                            ActivityTone::Error,
                            format!("Failed to build child webview: {error}"),
                        );
                        return;
                    }
                },
            }
        } else if matches!(reuse_action, WebViewReuseAction::Navigate) {
            if let Some(existing) = self.views.get_mut(&active.pane_id) {
                if let Err(error) = existing.webview.load_url(&route_key) {
                    state.push_activity(
                        ActivityTone::Error,
                        format!("Failed to navigate child webview: {error}"),
                    );
                } else {
                    existing.route = active.route.clone();
                    existing.route_key = route_key.clone();
                }
            }
        }

        let webview_bounds = content_bounds(active.bounds);

        if let Some(existing) = self.views.get_mut(&active.pane_id) {
            let actual_bounds = existing.actual_bounds();
            let needs_resize = actual_bounds
                .map(|bounds| bounds_changed(bounds, webview_bounds))
                .unwrap_or_else(|| bounds_changed(existing.bounds, webview_bounds));

            if devtools_debug_enabled()
                && (needs_resize || bounds_changed(existing.bounds, webview_bounds))
            {
                log_devtools(format!(
                    "sync pane={} route={} desired={} cached={} actual={} shell_mode={:?} needs_resize={}",
                    active.pane_id,
                    active.route,
                    format_bounds(webview_bounds),
                    format_bounds(existing.bounds),
                    format_optional_bounds(actual_bounds),
                    state.shell_mode,
                    needs_resize
                ));
            }

            if needs_resize {
                if let Err(error) = existing.apply_bounds(webview_bounds) {
                    state.push_activity(
                        ActivityTone::Error,
                        format!("Failed to resize child webview: {error}"),
                    );
                    log_devtools(format!(
                        "sync resize failed pane={} desired={} error={error}",
                        active.pane_id,
                        format_bounds(webview_bounds)
                    ));
                } else {
                    log_devtools(format!(
                        "sync resize applied pane={} desired={}",
                        active.pane_id,
                        format_bounds(webview_bounds)
                    ));
                }
            }

            if active.profile == HOST_PANEL_PROFILE {
                let payload = crate::settings::host_panel_payload_for_url(state, &route_key);
                let payload_json =
                    serde_json::to_string(&payload).unwrap_or_else(|_| "null".to_string());
                sync_host_panel_payload(existing, &payload_json, state);
            }
        }

        self.set_cached_visibility(
            active.pane_id,
            should_show_webview(
                &active.route,
                &active_web_session(state, active.pane_id).unwrap_or(active.session.clone()),
                state.shell_mode.clone(),
                webview_bounds,
            ),
            state,
        );

        // Spawn a PTY terminal session if this is a Terminal pane and no session exists yet.
        if let GuestRoute::Terminal { session_id } = &active.route {
            let session_id = session_id.clone();
            if !self.terminal_sessions.contains_key(&session_id)
                && !self.completed_terminal_sessions.contains(&session_id)
            {
                // Priority 1: pending share terminal (spawned by capsule-core executor).
                if let Some(proc) = take_pending_share_terminal(&session_id) {
                    info!(session_id = %session_id, "Using share-spawned terminal session");
                    self.terminal_sessions
                        .insert(session_id.clone(), Box::new(proc));
                    state.sync_web_session_state(active.pane_id, WebSessionState::Mounted);
                } else if let Some(spec) = take_pending_cli_command(&session_id) {
                    // Priority 2: pending CLI launch spec from an `ato://cli` deep link.
                    match spawn_cli_session(session_id.clone(), 80, 24, spec.clone(), Vec::new()) {
                        Ok(proc) => {
                            info!(session_id = %session_id, ?spec, "Spawned CLI session from ato://cli");
                            self.terminal_sessions
                                .insert(session_id.clone(), Box::new(proc));
                            state.sync_web_session_state(active.pane_id, WebSessionState::Mounted);
                        }
                        Err(e) => {
                            error!(session_id = %session_id, error = %e, "Failed to spawn CLI session");
                            self.pending_terminal_errors.insert(
                                session_id.clone(),
                                format!("Failed to spawn CLI session: {e}"),
                            );
                            self.completed_terminal_sessions.insert(session_id.clone());
                            state.sync_web_session_state(active.pane_id, WebSessionState::Mounted);
                        }
                    }
                } else {
                    // Priority 3: default interactive shell via nacelle.
                    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
                    match spawn_terminal(SpawnSpec {
                        session_id: session_id.clone(),
                        cols: 80,
                        rows: 24,
                        kind: SpawnKind::NacelleShell { shell },
                        secrets: Vec::new(),
                    }) {
                        Ok(proc) => {
                            info!(session_id = %session_id, "Spawned terminal PTY session");
                            self.terminal_sessions
                                .insert(session_id.clone(), Box::new(proc));
                            state.sync_web_session_state(active.pane_id, WebSessionState::Mounted);
                        }
                        Err(e) => {
                            error!(session_id = %session_id, error = %e, "Failed to spawn terminal PTY");
                            self.pending_terminal_errors.insert(
                                session_id.clone(),
                                format!("Failed to spawn terminal PTY: {e}"),
                            );
                            self.completed_terminal_sessions.insert(session_id.clone());
                            state.sync_web_session_state(active.pane_id, WebSessionState::Mounted);
                        }
                    }
                }
            }

            // Drain PTY output and push to xterm.js via evaluate_script.
            // Guard on page_loaded so xterm.js is fully initialised before we write.
            if self.automation.is_page_loaded(active.pane_id) {
                if let Some(view) = self.views.get_mut(&active.pane_id) {
                    if let Some(proc) = self.terminal_sessions.get(&session_id) {
                        let mut disconnected = false;
                        loop {
                            match proc.try_recv_output() {
                                TryRecvOutput::Data(b64) => {
                                    let json = serde_json::to_string(&b64).unwrap_or_default();
                                    let script = format!("window.__ato_write_terminal({json});");
                                    if let Err(e) = view.webview.evaluate_script(&script) {
                                        warn!(error = %e, "evaluate_script for terminal output failed");
                                    }
                                }
                                TryRecvOutput::Empty => break,
                                TryRecvOutput::Disconnected => {
                                    let _ = view
                                        .webview
                                        .evaluate_script("window.__ato_terminal_exit(0);");
                                    disconnected = true;
                                    break;
                                }
                            }
                        }
                        if disconnected {
                            self.terminal_sessions.remove(&session_id);
                            self.completed_terminal_sessions.insert(session_id.clone());
                        }
                    }

                    if let Some(error_message) = self.pending_terminal_errors.remove(&session_id) {
                        let json = serde_json::to_string(&error_message).unwrap_or_default();
                        let script = format!("window.__ato_terminal_error({json});");
                        if let Err(e) = view.webview.evaluate_script(&script) {
                            warn!(session_id = %session_id, error = %e, "failed to report terminal startup error");
                        }
                    }
                }
            }
        }

        if let Some(existing) = self.views.get_mut(&active.pane_id) {
            for command in state.drain_browser_commands(active.pane_id) {
                let label = format!("{command:?}");
                let result = match command {
                    BrowserCommandKind::Back => existing.webview.evaluate_script("history.back();"),
                    BrowserCommandKind::Forward => {
                        existing.webview.evaluate_script("history.forward();")
                    }
                    BrowserCommandKind::Reload => existing.webview.reload(),
                };

                if let Err(error) = result {
                    state.push_activity(
                        ActivityTone::Error,
                        format!("Failed to run browser command {label}: {error}"),
                    );
                }
            }
        }

        self.sync_responder_target(state);
    }

    pub fn sync_responder_target(&mut self, state: &mut AppState) {
        let desired = self.desired_responder_target(state);
        if self.responder_target == Some(desired) {
            return;
        }

        let result = match desired {
            ResponderTarget::Host => self.focus_host_view(),
            ResponderTarget::WebView(pane_id) => self.focus_webview(pane_id),
        };

        match result {
            Ok(()) => {
                self.responder_target = Some(desired);
            }
            Err(error) => {
                state.push_activity(
                    ActivityTone::Error,
                    format!("Failed to update focus target: {error}"),
                );
            }
        }
    }

    pub fn wants_host_focus(&self, state: &AppState) -> bool {
        matches!(self.desired_responder_target(state), ResponderTarget::Host)
    }

    /// Process all pending automation requests from the AI-agent socket.
    ///
    /// Called at end of every `sync_from_state` cycle.
    fn dispatch_automation_requests(&mut self, state: &mut AppState) {
        use std::time::Instant;
        use AutomationCommand::*;

        let requests = self.automation.drain_requests();
        if requests.is_empty() {
            return;
        }

        let mut requeue: Vec<PendingAutomationRequest> = Vec::new();

        for req in requests {
            if req.is_expired() {
                req.send(Err("automation command timed out".into()));
                continue;
            }

            // Commands that don't require a live WebView.
            match &req.command {
                ListPanes => {
                    let panes: Vec<serde_json::Value> = self
                        .views
                        .keys()
                        .map(|id| serde_json::json!({ "pane_id": id }))
                        .collect();
                    req.send(Ok(serde_json::json!({ "panes": panes })));
                    continue;
                }
                FocusPane { .. } => {
                    req.send(Ok(serde_json::json!({ "ok": true })));
                    continue;
                }
                OpenUrl { url } => {
                    state.navigate_to_url(url);
                    req.send(Ok(serde_json::json!({ "ok": true })));
                    continue;
                }
                SetCapsuleSecrets {
                    handle,
                    secrets,
                    clear_pending_config,
                } => {
                    let outcome =
                        apply_capsule_secrets(state, handle, secrets, *clear_pending_config);
                    match outcome {
                        Ok(applied) => req.send(Ok(serde_json::json!({
                            "ok": true,
                            "applied": applied,
                        }))),
                        Err(message) => req.send(Err(message)),
                    };
                    continue;
                }
                ApproveExecutionPlanConsent { handle } => {
                    match apply_capsule_consent(state, handle) {
                        Ok(()) => req.send(Ok(serde_json::json!({
                            "ok": true,
                            "approved_handle": handle,
                        }))),
                        Err(message) => req.send(Err(message)),
                    };
                    continue;
                }
                StopActiveSession => {
                    // Snapshot active session metadata before invoking stop so
                    // the response can distinguish "no active session"
                    // (had_active_session=false, stopped=false) from "stop
                    // failed" (had_active_session=true, stopped=false).
                    // `WebViewManager::stop_active_session` returns `false`
                    // for both today (`webview.rs` stop_active_session).
                    let (had_active_session, session_id_before, handle_before) = self
                        .active_pane_id
                        .and_then(|pane_id| self.views.get(&pane_id))
                        .and_then(|v| v.launched_session.as_ref())
                        .map(|s| (true, Some(s.session_id.clone()), Some(s.handle.clone())))
                        .unwrap_or((false, None, None));

                    let stopped = self.stop_active_session(state);
                    req.send(Ok(serde_json::json!({
                        "ok": true,
                        "stopped": stopped,
                        "had_active_session": had_active_session,
                        "session_id": session_id_before,
                        "handle": handle_before,
                    })));
                    continue;
                }
                _ => {}
            }

            // Resolve pane_id=0 → active pane.
            let pane_id = if req.pane_id == 0 {
                match self.active_pane_id {
                    Some(id) => id,
                    None => {
                        req.send(Err("no active pane".into()));
                        continue;
                    }
                }
            } else {
                req.pane_id
            };

            // Navigation commands don't need a loaded page; all JS commands do.
            let needs_loaded = !matches!(
                &req.command,
                Navigate { .. } | NavigateBack | NavigateForward | Screenshot
            );

            if needs_loaded && !self.automation.is_page_loaded(pane_id) {
                if req.wait_deadline.map_or(false, |d| Instant::now() < d)
                    || matches!(req.command, WaitFor { .. })
                {
                    requeue.push(req);
                } else {
                    req.send(Err("page not yet loaded".into()));
                }
                continue;
            }

            let Some(view) = self.views.get(&pane_id) else {
                req.send(Err(format!("pane {pane_id} not found")));
                continue;
            };

            dispatch_automation_command(req, &view.webview, pane_id, &self.automation);
        }

        self.automation.requeue(requeue);
    }

    pub fn open_devtools_for_active_pane(&mut self, state: &mut AppState) {
        if let Some(pane_id) = self.active_pane_id {
            if let Some(view) = self.views.get_mut(&pane_id) {
                let expected_bounds = state
                    .active_web_pane()
                    .filter(|active| active.pane_id == pane_id)
                    .map(|active| content_bounds(active.bounds));
                let before_open = view.actual_bounds();
                log_devtools(format!(
                    "open_devtools start pane={} route={} cached={} actual_before={} expected={}",
                    pane_id,
                    view.route,
                    format_bounds(view.bounds),
                    format_optional_bounds(before_open),
                    format_optional_bounds(expected_bounds)
                ));

                view.webview.open_devtools();

                #[cfg(target_os = "macos")]
                detach_macos_devtools_if_supported(&view.webview);

                let after_open = view.actual_bounds();
                log_devtools(format!(
                    "open_devtools shown pane={} actual_after_open={} expected={}",
                    pane_id,
                    format_optional_bounds(after_open),
                    format_optional_bounds(expected_bounds)
                ));

                if let Some(expected_bounds) = expected_bounds {
                    if let Err(error) = view.apply_bounds(expected_bounds) {
                        state.push_activity(
                            ActivityTone::Error,
                            format!("Failed to restore child webview bounds after opening DevTools: {error}"),
                        );
                        log_devtools(format!(
                            "open_devtools restore failed pane={} expected={} error={error}",
                            pane_id,
                            format_bounds(expected_bounds)
                        ));
                    } else {
                        let after_restore = view.actual_bounds();
                        log_devtools(format!(
                            "open_devtools restore applied pane={} expected={} actual_after_restore={}",
                            pane_id,
                            format_bounds(expected_bounds),
                            format_optional_bounds(after_restore)
                        ));
                    }
                } else {
                    log_devtools(format!(
                        "open_devtools skipped restore pane={} reason=no-active-pane-bounds",
                        pane_id
                    ));
                }
            } else {
                log_devtools(format!(
                    "open_devtools skipped pane={} reason=missing-webview",
                    pane_id
                ));
            }
        } else {
            log_devtools("open_devtools skipped reason=no-active-pane");
        }
    }

    pub fn delegate_select_all(&mut self, state: &AppState) -> Result<bool> {
        let Some(pane_id) = self.active_webview_pane_id(state) else {
            return Ok(false);
        };
        self.focus_webview(pane_id)?;
        self.views
            .get(&pane_id)
            .context("active webview missing")?
            .webview
            .evaluate_script(select_all_script())?;
        Ok(true)
    }

    pub fn delegate_paste(&mut self, state: &AppState, text: &str) -> Result<bool> {
        let Some(pane_id) = self.active_webview_pane_id(state) else {
            return Ok(false);
        };
        self.focus_webview(pane_id)?;
        let script = paste_script(text);
        self.views
            .get(&pane_id)
            .context("active webview missing")?
            .webview
            .evaluate_script(&script)?;
        Ok(true)
    }

    pub fn delegate_copy(&mut self, state: &AppState, cut: bool) -> Result<bool> {
        let Some(pane_id) = self.active_webview_pane_id(state) else {
            return Ok(false);
        };
        self.focus_webview(pane_id)?;

        let Some(view) = self.views.get(&pane_id) else {
            return Ok(false);
        };
        let script = copy_script(cut);
        view.webview
            .evaluate_script_with_callback(&script, move |response| {
                let Ok(value) = serde_json::from_str::<Value>(&response) else {
                    return;
                };
                let text = value
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if text.is_empty() {
                    return;
                }
                let _ = write_text_to_system_clipboard(&text);
            })?;
        Ok(true)
    }

    fn active_webview_pane_id(&self, state: &AppState) -> Option<usize> {
        if self.wants_host_focus(state) {
            return None;
        }
        state.active_web_pane().map(|pane| pane.pane_id)
    }

    fn apply_shell_events(&mut self, events: &[ShellEvent], state: &AppState) {
        for event in events {
            match event {
                ShellEvent::HostPanelRouteChanged { pane_id, path } => {
                    let full_url = format!("{HOST_PANEL_SCHEME}://panel{path}");
                    if let Ok(parsed) = full_url.parse() {
                        if *pane_id == HOST_PANEL_OVERLAY_PANE_ID {
                            if let Some(view) = self.overlay_host_panel.as_mut() {
                                view.route = GuestRoute::ExternalUrl(parsed);
                                view.route_key = full_url;
                            }
                        } else if let Some(view) = self.views.get_mut(pane_id) {
                            view.route = GuestRoute::ExternalUrl(parsed);
                            view.route_key = full_url;
                        }
                    }
                }
                ShellEvent::HostPanelCommand { .. } => {}
                ShellEvent::UrlChanged { pane_id, url } => {
                    if let Some(view) = self.views.get_mut(pane_id) {
                        if let Ok(parsed) = url.parse() {
                            view.route = GuestRoute::ExternalUrl(parsed);
                            view.route_key = url.clone();
                        }
                    }
                }
                ShellEvent::TerminalInput {
                    session_id,
                    data_b64,
                } => {
                    if let Some(proc) = self.terminal_sessions.get(session_id) {
                        // Decode base64 and forward to PTY stdin.
                        match base64::engine::general_purpose::STANDARD.decode(data_b64) {
                            Ok(bytes) => {
                                if !proc.send_input(bytes) {
                                    warn!(session_id = %session_id, "PTY input channel closed");
                                }
                            }
                            Err(e) => {
                                warn!(session_id = %session_id, error = %e, "base64 decode failed for terminal input");
                            }
                        }
                    } else {
                        debug!(session_id = %session_id, "terminal input: no PTY session found");
                    }
                }
                ShellEvent::TerminalResize {
                    session_id,
                    cols,
                    rows,
                } => {
                    if let Some(proc) = self.terminal_sessions.get(session_id) {
                        if !proc.send_resize(*cols, *rows) {
                            warn!(session_id = %session_id, "PTY resize channel closed");
                        }
                    } else {
                        debug!(session_id = %session_id, cols, rows, "terminal resize: no PTY session found");
                    }
                }
                ShellEvent::GetSecrets {
                    request_id,
                    pane_id,
                } => {
                    if let Some(pid) = pane_id {
                        let handle = self
                            .views
                            .get(pid)
                            .and_then(|v| v.launched_session.as_ref())
                            .map(|s| s.handle.clone())
                            .unwrap_or_default();
                        let secrets = state.secret_store.secrets_for_capsule(&handle);
                        let payload: std::collections::HashMap<&str, &str> = secrets
                            .iter()
                            .map(|s| (s.key.as_str(), s.value.as_str()))
                            .collect();
                        let payload_json =
                            serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
                        let script = format!(
                            "window.__ATO_HOST__ && window.__ATO_HOST__.resolveSecrets({}, {});",
                            request_id, payload_json
                        );
                        if let Some(view) = self.views.get_mut(pid) {
                            if let Err(e) = view.webview.evaluate_script(&script) {
                                warn!(pane_id = pid, error = %e, "failed to deliver GetSecrets response");
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn drain_pending_launches(&mut self, window: &Window, state: &mut AppState) {
        let mut completed_keys = Vec::new();
        let mut completed = Vec::new();

        for (key, pending) in &self.pending_launches {
            match pending.receiver.try_recv() {
                Ok(result) => {
                    completed_keys.push(key.clone());
                    completed.push((pending.pane_id, result));
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    completed_keys.push(key.clone());
                    completed.push((
                        pending.pane_id,
                        PendingLaunchResult {
                            route_key: pending.route_key.clone(),
                            handle: pending.handle.clone(),
                            session: Err(LaunchError::Other(
                                "guest session worker disconnected before completion".to_string(),
                            )),
                        },
                    ));
                }
            }
        }

        for key in completed_keys {
            self.pending_launches.remove(&key);
        }

        for (pane_id, completed) in completed {
            let Some(active) = state.active_web_pane() else {
                if let Ok(session) = completed.session {
                    warn!(pane_id, session_id = %session.session_id, "no active pane; stopping orphaned session");
                    self.stop_guest_session_record(&session, state);
                }
                continue;
            };

            if active.pane_id != pane_id || active.route.to_string() != completed.route_key {
                if let Ok(session) = completed.session {
                    warn!(pane_id, "pane/route mismatch; stopping stale session");
                    self.stop_guest_session_record(&session, state);
                }
                continue;
            }

            match completed.session {
                Ok(session) => {
                    // A successful launch retires any retry-once budget
                    // recorded against this handle: a future E302 (e.g.
                    // after a policy-segment-hash change) should get a
                    // fresh modal, not a fatal toast.
                    state.reset_consent_retry_budget(&completed.handle);

                    let is_web_url = session.display_strategy == CapsuleDisplayStrategy::WebUrl;
                    let is_terminal_stream =
                        session.display_strategy == CapsuleDisplayStrategy::TerminalStream;

                    if is_terminal_stream {
                        // Switch the pane from Web(CapsuleHandle) → Terminal so the render
                        // loop drains output via window.__ato_write_terminal.
                        let terminal_session_id = session.session_id.clone();
                        let title = session.normalized_handle.clone();
                        let log_path = session.log_path.clone();
                        state.mount_terminal_stream_pane(
                            pane_id,
                            terminal_session_id.clone(),
                            title.clone(),
                        );

                        // Check for a pending share terminal (piped PTY from capsule-core executor)
                        // before falling back to log-tail.
                        let terminal_ok = if let Some(proc) =
                            take_pending_share_terminal(&terminal_session_id)
                        {
                            info!(pane_id, session_id = %terminal_session_id, "using share-spawned piped terminal session");
                            self.terminal_sessions
                                .insert(terminal_session_id.clone(), Box::new(proc));
                            true
                        } else {
                            // Fallback: log-tail for capsule sessions managed by ato-cli
                            match log_path {
                                Some(lp) => {
                                    match spawn_log_tail_session(terminal_session_id.clone(), lp) {
                                        Ok(proc) => {
                                            info!(pane_id, session_id = %terminal_session_id, "log-tail session spawned for terminal_stream");
                                            self.terminal_sessions.insert(
                                                terminal_session_id.clone(),
                                                Box::new(proc),
                                            );
                                            true
                                        }
                                        Err(e) => {
                                            error!(pane_id, error = %e, "failed to spawn log-tail session");
                                            false
                                        }
                                    }
                                }
                                None => {
                                    error!(pane_id, "terminal_stream session has no log_path and no pending share terminal");
                                    false
                                }
                            }
                        };

                        if terminal_ok {
                            // Build a terminal:// webview by creating a synthetic ActiveWebPane
                            // with GuestRoute::Terminal so build_webview uses the right protocol.
                            let terminal_pane = ActiveWebPane {
                                workspace_id: active.workspace_id,
                                task_id: active.task_id,
                                pane_id: active.pane_id,
                                title: title.clone(),
                                route: GuestRoute::Terminal {
                                    session_id: terminal_session_id.clone(),
                                },
                                partition_id: terminal_session_id.clone(),
                                profile: "terminal".to_string(),
                                capabilities: active.capabilities.clone(),
                                session: WebSessionState::Launching,
                                source_label: None,
                                trust_state: None,
                                restricted: false,
                                snapshot_label: None,
                                canonical_handle: None,
                                session_id: Some(terminal_session_id.clone()),
                                adapter: None,
                                manifest_path: None,
                                runtime_label: None,
                                display_strategy: None,
                                log_path: None,
                                local_url: None,
                                healthcheck_url: None,
                                invoke_url: None,
                                served_by: None,
                                auth_flow: false,
                                bounds: active.bounds.clone(),
                            };
                            match self.build_webview(
                                window,
                                &terminal_pane,
                                None,
                                state.auth_policy_registry.clone(),
                            ) {
                                Ok(webview) => {
                                    info!(pane_id, session_id = %terminal_session_id, "terminal webview built for terminal_stream");
                                    self.bridge.log(
                                        ActivityTone::Info,
                                        format!("Terminal stream started for {title}"),
                                    );
                                    self.start_log_follower(active.pane_id, &session);
                                    self.views.insert(active.pane_id, webview);
                                }
                                Err(error) => {
                                    error!(pane_id, %error, "failed to build terminal webview for terminal_stream");
                                    state.push_activity(
                                        ActivityTone::Error,
                                        format!("Failed to build terminal view: {error}"),
                                    );
                                }
                            }
                        } else {
                            state.push_activity(
                                ActivityTone::Error,
                                format!("Failed to start log-tail for {title}"),
                            );
                        }
                    } else {
                        match self.build_webview(
                            window,
                            &active,
                            Some(session),
                            state.auth_policy_registry.clone(),
                        ) {
                            Ok(webview) => {
                                info!(pane_id, route = %active.route, "child webview built");
                                self.bridge.log(
                                    ActivityTone::Info,
                                    format!("Built child webview for {}", active.route),
                                );
                                // WebUrl sessions stay in Launching until PageLoadEvent::Finished
                                // fires SessionReady → Mounted.  This keeps the GPUI loading
                                // screen visible while the web app (e.g. Next.js) compiles and
                                // renders its first frame, preventing a blank white flash.
                                if is_web_url {
                                    state.sync_web_session_state(
                                        active.pane_id,
                                        WebSessionState::Launching,
                                    );
                                }
                                if let Some(session) = webview.launched_session.as_ref() {
                                    match resolve_share_icon(session) {
                                        Some(ShareIconSource::Direct(icon)) => {
                                            info!(
                                                target: TARGET_FAVICON,
                                                pane_id = active.pane_id,
                                                session_id = %session.session_id,
                                                source = %icon,
                                                "applying direct share icon to pane"
                                            );
                                            state.pane_icons.insert(active.pane_id, icon);
                                        }
                                        Some(ShareIconSource::FaviconOrigin(origin)) => {
                                            info!(
                                                target: TARGET_FAVICON,
                                                pane_id = active.pane_id,
                                                session_id = %session.session_id,
                                                origin = %origin,
                                                "share icon will use favicon fallback via pane local_url"
                                            );
                                            state.pane_icons.remove(&active.pane_id);
                                        }
                                        None => {
                                            error!(
                                                target: TARGET_FAVICON,
                                                pane_id = active.pane_id,
                                                session_id = %session.session_id,
                                                "share icon resolution returned no source"
                                            );
                                            state.pane_icons.remove(&active.pane_id);
                                        }
                                    }
                                    // Mirror session metadata onto the WebPane so the
                                    // route-info popover (and inspector) can show the
                                    // dev-server URL, log path, runtime label, etc.
                                    // Without this the launched_session lives only on
                                    // ManagedWebView and the popover renders mostly empty.
                                    apply_launch_session_metadata(state, active.pane_id, session);
                                    // Kick off the registry update check on a worker
                                    // thread; the result lands on DesktopShell via the
                                    // mpsc channel installed by install_capsule_update_channel.
                                    self.spawn_capsule_update_check(active.pane_id, session, state);
                                    self.start_log_follower(active.pane_id, session);
                                }
                                self.views.insert(active.pane_id, webview);
                            }
                            Err(error) => {
                                error!(pane_id, %error, "failed to build child webview");
                                state.sync_web_session_state(
                                    active.pane_id,
                                    WebSessionState::Closed,
                                );
                                state.push_activity(
                                    ActivityTone::Error,
                                    format!("Failed to build child webview: {error}"),
                                );
                            }
                        }
                    }
                }
                Err(LaunchError::MissingConfig {
                    handle,
                    target,
                    fields,
                    original_secrets,
                }) => {
                    // Recoverable: the capsule is missing user-supplied
                    // config. Pin the request on AppState so the next
                    // render surfaces the modal; do NOT push an error
                    // toast (the modal IS the surface) and do NOT mark
                    // the pane as `LaunchFailed` — Day 4's Save handler
                    // will re-arm the launch by clearing
                    // `pending_config` and re-entering this same
                    // `ensure_pending_local_launch` path.
                    info!(
                        pane_id,
                        handle = %handle,
                        target = ?target,
                        field_count = fields.len(),
                        "guest session needs config; surfacing modal"
                    );
                    state.set_pending_config(PendingConfigRequest {
                        handle,
                        target,
                        fields,
                        original_secrets,
                    });
                }
                Err(LaunchError::MissingConsent {
                    handle,
                    scoped_id,
                    version,
                    target_label,
                    policy_segment_hash,
                    provisioning_policy_hash,
                    summary,
                    original_secrets,
                }) => {
                    // Retry-once policy: if the user already approved
                    // once for this (handle, target_label) this session
                    // and we still got E302 for the same target,
                    // something is structurally wrong (CLI didn't see
                    // the record we just appended). Fall through to a
                    // fatal toast rather than re-open the modal — that
                    // would loop the user.
                    //
                    // Different `target_label` under the same handle
                    // (multi-target orchestration capsule) does NOT
                    // trip the budget: each target's ExecutionPlan
                    // consents separately, with its own policy hashes.
                    if state.consent_retry_already_consumed(&handle, &target_label) {
                        error!(
                            pane_id,
                            handle = %handle,
                            target = %target_label,
                            "consent re-required after approve; surfacing fatal (no modal loop)"
                        );
                        state.sync_web_session_state(active.pane_id, WebSessionState::LaunchFailed);
                        state.push_activity(
                            ActivityTone::Error,
                            format!(
                                "Failed to start guest session: ExecutionPlan consent was re-requested for '{handle}' (target {target_label}) after approval. Re-launch from the omnibar to retry."
                            ),
                        );
                        // Reset the budget so a manual re-launch starts
                        // from a clean state.
                        state.reset_consent_retry_budget(&handle);
                    } else {
                        info!(
                            pane_id,
                            handle = %handle,
                            target = %target_label,
                            "guest session needs ExecutionPlan consent; surfacing modal"
                        );
                        state.set_pending_consent(PendingConsentRequest {
                            handle,
                            scoped_id,
                            version,
                            target_label,
                            policy_segment_hash,
                            provisioning_policy_hash,
                            summary,
                            original_secrets,
                        });
                    }
                }
                Err(LaunchError::Other(message)) => {
                    error!(pane_id, error = %message, "guest session failed");
                    // Use LaunchFailed (not Closed) to prevent ensure_pending_local_launch
                    // from re-queuing a new attempt on every render frame.
                    state.sync_web_session_state(active.pane_id, WebSessionState::LaunchFailed);
                    state.push_activity(
                        ActivityTone::Error,
                        format!("Failed to start guest session: {message}"),
                    );
                }
            }
        }
    }

    fn ensure_pending_local_launch(
        &mut self,
        pane_id: usize,
        route_key: &str,
        handle: &str,
        state: &mut AppState,
    ) {
        let key = pending_launch_key(pane_id, route_key);
        if self.pending_launches.contains_key(&key) {
            return;
        }

        // If a previous attempt for this exact route already failed permanently, do not
        // re-queue — this is the gate that breaks the infinite retry loop.
        // navigate_to_url() always sets Launching, so the user can explicitly retry by
        // re-entering the URL in the omnibar.
        if let Some(active) = state.active_web_pane() {
            if active.session == WebSessionState::LaunchFailed {
                return;
            }
        }

        // Second gate: if a config modal is open for THIS handle, the
        // user is mid-edit. Re-spawning would just re-trip the same
        // E103 in the background and rebuild the modal under their
        // cursor. The Save handler clears `pending_config`, which
        // collapses this guard on the next render and re-arms the
        // launch with the freshly stored secrets.
        if let Some(pending) = &state.pending_config {
            if pending.handle == handle {
                return;
            }
        }

        // Same gate, mirror for E302 consent: if the consent modal is
        // open for THIS handle, the user is mid-decision. The Approve
        // handler clears `pending_consent` and marks the retry budget;
        // both branches collapse this guard on the next render.
        if let Some(pending) = &state.pending_consent {
            if pending.handle == handle {
                return;
            }
        }

        info!(pane_id, handle, "queuing guest session launch");
        let (sender, receiver) = channel();
        let route_key = route_key.to_string();
        let handle = handle.to_string();
        let background_executor = self.async_app.background_executor().clone();
        let foreground_executor = self.async_app.foreground_executor().clone();
        let async_app = self.async_app.clone();
        let window_handle = self.window_handle;

        // Collect secrets granted for this capsule handle before moving into the async block.
        let secrets: Vec<SecretEntry> = state
            .secret_store
            .secrets_for_capsule(&handle)
            .into_iter()
            .cloned()
            .collect();
        // Same idea for plaintext config — capture a snapshot now so
        // the background thread doesn't reach back into AppState.
        let plain_configs: Vec<(String, String)> =
            state.capsule_config_store.configs_for_capsule(&handle);

        self.pending_launches.insert(
            key,
            PendingLaunch {
                pane_id,
                route_key: route_key.clone(),
                handle: handle.clone(),
                receiver,
            },
        );
        state.push_activity(
            ActivityTone::Info,
            format!("Launching guest session for {route_key}"),
        );

        let launch_task = background_executor.spawn(async move {
            // Propagate `LaunchError` end-to-end — `drain_pending_launches`
            // needs the typed enum to distinguish E103 (modal) from the
            // opaque toast path. Logging happens at the consumer so the
            // structured payload survives the channel.
            let result = PendingLaunchResult {
                route_key: route_key.clone(),
                handle: handle.clone(),
                session: resolve_and_start_guest(&handle, &secrets, &plain_configs).inspect_err(
                    |err| {
                        error!(handle = %handle, error = %err, "guest session launch failed");
                    },
                ),
            };
            if result.session.is_ok() {
                info!(handle = %handle, route_key = %result.route_key, "guest session launched");
            }

            if let Err(error) = sender.send(result) {
                if let Ok(session) = error.0.session {
                    let _ = stop_guest_session(&session.session_id);
                }
            }
        });

        foreground_executor
            .spawn(async move {
                launch_task.await;
                notify_window(async_app, window_handle);
            })
            .detach();
    }

    fn build_webview(
        &mut self,
        window: &Window,
        pane: &ActiveWebPane,
        local_session: Option<GuestLaunchSession>,
        auth_policy: AuthPolicyRegistry,
    ) -> Result<ManagedWebView> {
        if pane.profile == HOST_PANEL_PROFILE {
            return self.build_host_panel_webview(window, pane);
        }

        let scheme = if matches!(pane.route, GuestRoute::Terminal { .. }) {
            "terminal".to_string()
        } else {
            self.protocol_router.scheme_for(&pane.partition_id)
        };
        let mut launched_session = None;
        let mut session_context = None;
        // build_flags may be overridden below for WebUrl sessions (see CapsuleHandle branch).
        let mut build_flags = build_flags_for_route(&pane.route);
        // Signals that we should inject a minimal window.onload ready script + IPC handler
        // for raw web app (WebUrl) sessions rather than relying on PageLoadEvent::Finished.
        let mut inject_window_ready_signal = false;

        let (url, bridge_endpoint, allowlist, route_content, guest_payload) = match &pane.route {
            GuestRoute::Capsule {
                session,
                entry_path,
            } => {
                // Existing capsule sessions map directly to the custom protocol scheme.
                let allowlist = pane
                    .capabilities
                    .iter()
                    .map(|capability| capability.as_str().to_string())
                    .collect::<Vec<_>>();
                (
                    format!("{scheme}://{session}{entry_path}"),
                    Some(format!("{scheme}://{session}/__ato/bridge")),
                    allowlist,
                    RouteContent::EmbeddedWelcome,
                    None,
                )
            }
            GuestRoute::ExternalUrl(url) => (
                url.as_str().to_string(),
                None,
                Vec::new(),
                RouteContent::External,
                None,
            ),
            GuestRoute::CapsuleUrl { url, .. } => (
                url.as_str().to_string(),
                None,
                pane.capabilities
                    .iter()
                    .map(|capability| capability.as_str().to_string())
                    .collect::<Vec<_>>(),
                RouteContent::External,
                None,
            ),
            GuestRoute::CapsuleHandle { .. } => {
                let session = local_session.ok_or_else(|| {
                    anyhow::anyhow!("capsule webview build requires resolved guest session")
                })?;
                for note in &session.notes {
                    self.bridge.log(ActivityTone::Info, note.clone());
                }
                self.bridge.log(
                    ActivityTone::Info,
                    format!(
                        "Started ato-cli guest session {} for {}",
                        session.session_id, session.normalized_handle
                    ),
                );
                // RFC: SURFACE_CLOSE_SEMANTICS — if this session_id
                // was sitting in the retention table (i.e. the user
                // closed and reopened the same capsule within TTL),
                // remove it without stopping. The fast path on the
                // orchestrator side has already verified PID + start
                // time + healthcheck; the session is now "active"
                // again, not "retained", so eviction triggers must
                // not fire on it.
                if self
                    .retention
                    .take_by_session_id(&session.session_id)
                    .is_some()
                {
                    tracing::debug!(
                        session_id = %session.session_id,
                        handle = %session.handle,
                        "session reopened from retention table"
                    );
                }
                launched_session = Some(session.clone());

                // Web dev-server sessions navigate directly to the local URL without the
                // capsule:// custom protocol — the app is served by an external process.
                // Override build_flags to External-style: no bridge injection, no custom
                // protocol, and page-load updates the URL (not waits for a ready signal).
                // Without this override, the webview stays hidden because CapsuleHandle
                // route_requires_ready_signal=true and the bridge preload script is injected
                // into the raw web app, preventing it from ever becoming "Mounted".
                // We keep inject_bridge=false (no preload pollution). Instead of relying on
                // PageLoadEvent::Finished (which fires on initial HTML commit, before JS executes),
                // we inject a minimal window.onload script + dedicated IPC handler so SessionReady
                // only fires after all scripts have run and the page has actually rendered.
                if session.display_strategy == CapsuleDisplayStrategy::WebUrl {
                    build_flags = BuildFlags {
                        inject_bridge: false,
                        enable_ipc: false,
                        enable_custom_protocol: false,
                        page_load_behavior: PageLoadBehavior::None,
                        observe_title_changes: true,
                    };
                    inject_window_ready_signal = true;
                    let url = session.local_url.clone().ok_or_else(|| {
                        anyhow::anyhow!("WebUrl session has no local_url: {}", session.session_id)
                    })?;
                    (url, None, Vec::new(), RouteContent::External, None)
                } else {
                    let session_id = session.session_id.clone();
                    let frontend_path = session
                        .frontend_url_path()
                        .unwrap_or_else(|| "/index.html".to_string());
                    session_context = Some(GuestSessionContext {
                        pane_id: pane.pane_id,
                        session_id: session.session_id.clone(),
                        adapter: session.adapter.clone().unwrap_or_default(),
                        invoke_url: session.invoke_url.clone().unwrap_or_default(),
                        app_root: session.app_root.clone(),
                    });
                    (
                        format!("{scheme}://{session_id}{frontend_path}"),
                        Some(format!("{scheme}://{session_id}/__ato/bridge")),
                        session.capabilities.clone(),
                        RouteContent::GuestAssets(session.clone()),
                        Some(session.session_payload()),
                    )
                }
            }
            GuestRoute::Terminal { session_id } => (
                format!("terminal://{session_id}/"),
                None,
                vec!["terminal".to_string()],
                RouteContent::TerminalAssets,
                None,
            ),
        };

        let webview_bounds = content_bounds(pane.bounds);
        // Shared persistent WebContext — see WebViewManager.web_context
        // for rationale (cookie persistence + cross-pane sharing for
        // ato.run sign-in state).
        let mut builder = WebViewBuilder::new_with_web_context(&mut self.web_context)
            .with_bounds(bounds_to_rect(webview_bounds));

        // Layer 1: tag every Desktop WebView with a custom UA suffix
        // so ato.run server can render Desktop-specific UX (Launch
        // buttons, no "Download Desktop" promo, etc.) without
        // round-tripping through JS detection.
        builder = builder.with_user_agent(&format!(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) Version/17.0 Safari/605.1.15 AtoDesktop/{}",
            env!("CARGO_PKG_VERSION")
        ));

        // Layer 1 (client side): inject a JS marker before page
        // scripts load so ato.run client code can feature-gate on
        // window.__ATO_DESKTOP__ without parsing User-Agent.
        builder = builder.with_initialization_script_for_main_only(
            format!(
                "window.__ATO_DESKTOP__ = {{ version: \"{}\", platform: \"{}\" }};",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
            ),
            true,
        );

        if build_flags.inject_bridge {
            let preload_script = self.preload_registry.script_for(
                &pane.profile,
                self.bridge.preload_environment(&allowlist),
                bridge_endpoint,
                guest_payload,
            );
            builder = builder.with_initialization_script_for_main_only(preload_script, true);
        }

        // Terminal routes ship their own minimal bridge shim. The xterm.js page
        // calls `window.__ato_terminal_bridge(jsonString)` for every keystroke;
        // this shim forwards the message to `window.ipc.postMessage` after
        // translating the JS-side `type` field ("TerminalInput", …) to the
        // kebab-case `kind` tag that `GuestBridgeRequest` expects.
        if matches!(pane.route, GuestRoute::Terminal { .. }) {
            builder = builder.with_initialization_script_for_main_only(
                TERMINAL_BRIDGE_PRELOAD.to_string(),
                true,
            );
        }

        // Inject automation agent when the pane has the Automation capability.
        if pane.capabilities.contains(&CapabilityGrant::Automation) {
            builder = builder.with_initialization_script_for_main_only(
                include_str!("../assets/automation/agent.js").to_string(),
                true,
            );
        }

        if build_flags.enable_ipc {
            let route = pane.route.clone();
            let allowlist_for_ipc = allowlist.clone();
            let bridge = self.bridge.clone();
            let session_context_for_ipc = session_context.clone();
            builder = builder.with_ipc_handler(move |request| {
                let response = bridge.handle_message(
                    request.body(),
                    &allowlist_for_ipc,
                    session_context_for_ipc.as_ref(),
                );
                if matches!(response, GuestBridgeResponse::Denied { .. }) {
                    bridge.log(
                        ActivityTone::Warning,
                        format!("Guest request denied for route {}", route),
                    );
                }
            });
        }

        // For WebUrl sessions (share URL web dev servers): inject a minimal preload script that
        // fires window.ipc.postMessage on window.onload rather than relying on
        // PageLoadEvent::Finished. window.onload fires after ALL scripts have loaded and
        // executed, meaning React/Vue/Next.js has rendered its initial UI before we show the
        // webview — eliminating the blank white flash.
        if inject_window_ready_signal {
            let ready_script = "(function(){\
                function s(){try{window.ipc.postMessage('{\"__ato_ready__\":true}');}catch(e){}}\
                if(document.readyState==='complete'){s();}\
                else{window.addEventListener('load',s,{once:true});}\
            })();";
            builder =
                builder.with_initialization_script_for_main_only(ready_script.to_string(), true);
            let bridge = self.bridge.clone();
            let pane_id = pane.pane_id;
            let async_app = self.async_app.clone();
            let window_handle = self.window_handle;
            builder = builder.with_ipc_handler(move |request| {
                if request.body().contains("__ato_ready__") {
                    bridge.push_shell_event(ShellEvent::SessionReady { pane_id });
                    notify_window(async_app.clone(), window_handle);
                }
                // All other IPC messages from the raw web app are silently ignored.
            });
        }

        if build_flags.enable_custom_protocol {
            let protocol = self.protocol_router.clone();
            let scheme_name = scheme.clone();
            let bridge = self.bridge.clone();
            let allowlist = allowlist.clone();
            let session_context = session_context.clone();
            let route_content = route_content.clone();
            // Serve custom-scheme assets off the UI thread so filesystem and bridge work stay responsive.
            builder = builder.with_asynchronous_custom_protocol(
                scheme,
                move |_webview_id, request, responder| {
                    protocol.handle_async(
                        &scheme_name,
                        request,
                        responder,
                        bridge.clone(),
                        allowlist.clone(),
                        session_context.clone(),
                        route_content.clone(),
                    )
                },
            );
        }

        // Phase 0 (RFC: SURFACE_MATERIALIZATION §5.1) — base extras
        // shared by every SURFACE-TIMING line emitted from this build.
        // `since_click_ms` (added per emission below) is the actual
        // user-perceived metric — `elapsed_ms` is meaningless for
        // instant-marker stages like `navigation_start` and
        // `first_visible_signal`, so we anchor those against the click
        // origin captured by `resolve_and_start_capsule`.
        let surface_click_origin = launched_session.as_ref().and_then(|s| s.click_origin);
        let surface_base_extras = {
            let mut extras = crate::surface_timing::SurfaceExtras::default()
                .with_partition_id(pane.partition_id.clone())
                .with_route_key(pane.route.to_string());
            if let Some(session) = launched_session.as_ref() {
                extras = extras.with_session_id(session.session_id.clone());
            }
            extras
        };

        // Always install a page-load handler.
        // - PageLoadEvent::Started → mark pane as not-loaded (guard for evaluate_script)
        // - PageLoadEvent::Finished → mark loaded + push bridge shell events
        {
            let bridge = self.bridge.clone();
            let automation = self.automation.clone();
            let pane_id = pane.pane_id;
            let page_load_behavior = build_flags.page_load_behavior;
            let async_app = self.async_app.clone();
            let window_handle = self.window_handle;
            let click_origin = surface_click_origin;
            let base_extras = surface_base_extras.clone();
            builder = builder.with_on_page_load_handler(move |event, url| match event {
                PageLoadEvent::Started => {
                    // Phase 0 (RFC: SURFACE_MATERIALIZATION §5.1):
                    // navigation_start fires when Wry begins fetching
                    // the initial document. Wry calls this on its
                    // worker thread; emit_stage is thread-safe (just
                    // an eprintln behind an env check).
                    let extras = match click_origin {
                        Some(origin) => {
                            base_extras.clone().with_since_click_ms(origin.elapsed_ms())
                        }
                        None => base_extras.clone(),
                    };
                    crate::surface_timing::emit_stage("navigation_start", "ok", 0, None, &extras);
                    automation.mark_page_unloaded(pane_id);
                }
                PageLoadEvent::Finished => {
                    // navigation_finished. Note: PageLoadEvent::Finished
                    // is "DOM-loaded plus initial subresources," not
                    // first_paint. v0 uses this as a best-effort proxy
                    // for first_visible_signal as well — emitting both
                    // names so the log can be filtered either way.
                    // Phase 3a's native overlay will produce a more
                    // precise first_visible_signal once it lands.
                    let extras = match click_origin {
                        Some(origin) => {
                            base_extras.clone().with_since_click_ms(origin.elapsed_ms())
                        }
                        None => base_extras.clone(),
                    };
                    crate::surface_timing::emit_stage(
                        "navigation_finished",
                        "ok",
                        0,
                        None,
                        &extras,
                    );
                    crate::surface_timing::emit_stage(
                        "first_visible_signal",
                        "ok",
                        0,
                        None,
                        &extras,
                    );
                    if let Some(origin) = click_origin {
                        crate::surface_timing::emit_total(
                            origin.elapsed_ms(),
                            "first_visible_signal",
                            &base_extras,
                        );
                    }
                    automation.mark_page_loaded(pane_id);
                    match page_load_behavior {
                        PageLoadBehavior::UpdateExternalUrl => {
                            bridge.push_shell_event(ShellEvent::UrlChanged { pane_id, url });
                        }
                        PageLoadBehavior::MarkCapsuleReady => {
                            bridge.push_shell_event(ShellEvent::SessionReady { pane_id });
                        }
                        PageLoadBehavior::None => {}
                    }
                    notify_window(async_app.clone(), window_handle);
                }
            });
        }

        if build_flags.observe_title_changes {
            let bridge = self.bridge.clone();
            let pane_id = pane.pane_id;
            let async_app = self.async_app.clone();
            let window_handle = self.window_handle;
            builder = builder.with_document_title_changed_handler(move |title| {
                bridge.push_shell_event(ShellEvent::TitleChanged { pane_id, title });
                notify_window(async_app.clone(), window_handle);
            });
        }

        // For external URLs, intercept navigations that require browser-side auth.
        if let GuestRoute::ExternalUrl(_) = &pane.route {
            let pane_id = pane.pane_id;
            let signals = self.pending_auth_handoffs.clone();
            let callback_queue = self.pending_callback_urls.clone();
            let auth_flow = pane.auth_flow;
            builder = builder.with_navigation_handler(move |uri: String| {
                // ato:// deep links arrive here when ato.run finishes
                // an in-app OAuth flow and redirects to the desktop
                // callback. WKWebView cannot load custom schemes, so
                // we capture them and route via handle_host_route.
                if uri.starts_with("ato://") || uri.starts_with("capsule://") {
                    if let Ok(mut q) = callback_queue.lock() {
                        q.push(uri);
                    }
                    return false;
                }
                // Sign-in panes deliberately allow Google / GitHub /
                // Microsoft OAuth redirects to load in-WebView so
                // the resulting auth cookies persist in the shared
                // WebContext. Untrusted capsule WebViews still hand
                // those URLs off to the system browser.
                if auth_policy.classify(&uri) == AuthMode::BrowserRequired && !auth_flow {
                    if let Ok(mut q) = signals.lock() {
                        if !q.iter().any(|s: &AuthHandoffSignal| s.pane_id == pane_id) {
                            q.push(AuthHandoffSignal { pane_id, url: uri });
                        }
                    }
                    false // block navigation inside WebView
                } else {
                    true
                }
            });
        }

        builder = builder.with_new_window_req_handler(|_, _| NewWindowResponse::Allow);

        let desktop_auth_handoff = if should_install_ato_auth_cookies(&url) {
            Some(
                load_desktop_auth_handoff()
                    .with_context(|| format!("unable to prepare ato.run auth cookies for {url}"))?,
            )
        } else {
            None
        };

        let builder = if let Some(handoff) = &desktop_auth_handoff {
            builder.with_url_and_headers(&url, auth_initial_request_headers(handoff)?)
        } else {
            builder.with_url(&url)
        };

        // Phase 0 (RFC: SURFACE_MATERIALIZATION §3.1) — measure the
        // Wry / WKWebView creation cost. The pair `webview_create_start`
        // / `webview_create_end` brackets the actual `build_as_child`
        // call so callers can subtract preload-script setup, scheme
        // handler registration, etc., from the cost they're trying to
        // optimize in Phase 2B.
        //
        // We compute `since_click_ms` at emission time (not at timer
        // construction) so the value reflects the real wall-clock
        // distance from the click handler to that point — using
        // `SurfaceStageTimer` would freeze the extras at construction.
        let create_started = std::time::Instant::now();
        let extras_at_start = match surface_click_origin {
            Some(origin) => surface_base_extras
                .clone()
                .with_since_click_ms(origin.elapsed_ms()),
            None => surface_base_extras.clone(),
        };
        crate::surface_timing::emit_stage("webview_create_start", "ok", 0, None, &extras_at_start);
        let webview = builder
            .build_as_child(window)
            .with_context(|| format!("unable to create Wry child webview for {url}"))?;
        let create_elapsed_ms = create_started.elapsed().as_millis() as u64;
        let extras_at_end = match surface_click_origin {
            Some(origin) => surface_base_extras
                .clone()
                .with_since_click_ms(origin.elapsed_ms()),
            None => surface_base_extras.clone(),
        };
        crate::surface_timing::emit_stage(
            "webview_create_end",
            "ok",
            create_elapsed_ms,
            None,
            &extras_at_end,
        );

        if let Some(handoff) = &desktop_auth_handoff {
            install_ato_auth_cookies(&webview, handoff)
                .with_context(|| format!("unable to install ato.run auth cookies for {url}"))?;
        }

        #[cfg(target_os = "macos")]
        let frame_host = Some(install_macos_frame_host(&webview)?);

        Ok(ManagedWebView {
            pane_id: pane.pane_id,
            route: pane.route.clone(),
            route_key: pane.route.to_string(),
            bounds: webview_bounds,
            host_panel_payload_json: None,
            launched_session,
            webview,
            #[cfg(target_os = "macos")]
            frame_host,
        })
    }

    fn build_host_panel_webview(
        &mut self,
        window: &Window,
        pane: &ActiveWebPane,
    ) -> Result<ManagedWebView> {
        self.build_host_panel_child_webview(
            window,
            pane.pane_id,
            pane.route.clone(),
            pane.bounds,
            None,
        )
    }

    fn build_host_panel_overlay_webview(
        &mut self,
        window: &Window,
        route: url::Url,
        bounds: PaneBounds,
        payload: Option<Value>,
    ) -> Result<ManagedWebView> {
        self.build_host_panel_child_webview(
            window,
            HOST_PANEL_OVERLAY_PANE_ID,
            GuestRoute::ExternalUrl(route),
            bounds,
            payload,
        )
    }

    fn build_host_panel_child_webview(
        &mut self,
        window: &Window,
        pane_id: usize,
        route: GuestRoute,
        bounds: PaneBounds,
        payload: Option<Value>,
    ) -> Result<ManagedWebView> {
        let url = route.to_string();
        let webview_bounds = content_bounds(bounds);
        let payload_json = serde_json::to_string(&payload.unwrap_or(Value::Null))
            .unwrap_or_else(|_| "null".to_string());
        let mut builder = WebViewBuilder::new_with_web_context(&mut self.web_context)
            .with_bounds(bounds_to_rect(webview_bounds));

        builder = builder.with_user_agent(&format!(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) Version/17.0 Safari/605.1.15 AtoDesktop/{}",
            env!("CARGO_PKG_VERSION")
        ));
        builder = builder.with_initialization_script_for_main_only(
            format!(
                "window.__ATO_DESKTOP__ = {{ version: \"{}\", platform: \"{}\" }};",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
            ),
            true,
        );
        builder = builder.with_initialization_script_for_main_only(
            host_panel_bootstrap_script(pane_id, &payload_json),
            true,
        );

        let protocol = self.protocol_router.clone();
        let bridge = self.bridge.clone();
        builder = builder.with_asynchronous_custom_protocol(
            HOST_PANEL_SCHEME.to_string(),
            move |_webview_id, request, responder| {
                protocol.handle_async(
                    HOST_PANEL_SCHEME,
                    request,
                    responder,
                    bridge.clone(),
                    Vec::new(),
                    None,
                    RouteContent::External,
                )
            },
        );

        let dev_base = host_panel_dev_base_url();
        builder = builder.with_navigation_handler(move |uri: String| {
            url::Url::parse(&uri)
                .ok()
                .is_some_and(|url| allow_host_panel_navigation(&url, dev_base.as_ref()))
        });
        let bridge = self.bridge.clone();
        let async_app = self.async_app.clone();
        let window_handle = self.window_handle;
        builder = builder.with_ipc_handler(move |request| {
            let Ok(envelope) = serde_json::from_str::<HostPanelIpcEnvelope>(request.body()) else {
                return;
            };
            if envelope.message.kind == "route-change" {
                if let Some(path) = envelope.message.path {
                    bridge.push_shell_event(ShellEvent::HostPanelRouteChanged {
                        pane_id: envelope.pane_id.unwrap_or(pane_id),
                        path,
                    });
                    notify_window(async_app.clone(), window_handle);
                }
            } else if envelope.message.kind == "settings-command" {
                if let Some(command) = envelope.message.command {
                    bridge.push_shell_event(ShellEvent::HostPanelCommand {
                        pane_id: envelope.pane_id.unwrap_or(pane_id),
                        command,
                        payload: envelope.message.payload.unwrap_or(Value::Null),
                        request_id: envelope.message.request_id,
                    });
                    notify_window(async_app.clone(), window_handle);
                }
            }
        });
        builder = builder.with_new_window_req_handler(|_, _| NewWindowResponse::Allow);

        // Phase 0 (RFC: SURFACE_MATERIALIZATION §3.1) — host panel
        // WebView creation cost. Same bracketing as the capsule path
        // above; emitted with the same stage names so a SURFACE-TIMING
        // log can be filtered with one rule regardless of which
        // call site fired.
        crate::surface_timing::emit_stage(
            "webview_create_start",
            "ok",
            0,
            None,
            &crate::surface_timing::SurfaceExtras::default(),
        );
        let create_timer = crate::surface_timing::SurfaceStageTimer::start("webview_create_end");
        let webview = builder
            .with_url(&url)
            .build_as_child(window)
            .with_context(|| format!("unable to create Wry child host panel webview for {url}"))?;
        create_timer.finish_ok();

        #[cfg(target_os = "macos")]
        let frame_host = Some(install_macos_frame_host(&webview)?);

        Ok(ManagedWebView {
            pane_id,
            route,
            route_key: url,
            bounds: webview_bounds,
            host_panel_payload_json: Some(payload_json),
            launched_session: None,
            webview,
            #[cfg(target_os = "macos")]
            frame_host,
        })
    }

    /// Drop cached webviews / terminals for a list of pane ids.
    /// Called by DesktopShell after AppState::close_task so closing
    /// a tab actually tears down the underlying Wry views instead of
    /// leaking them on the heap.
    ///
    /// **RFC: SURFACE_CLOSE_SEMANTICS** — pane close no longer stops
    /// the underlying capsule session. The launched session is
    /// demoted to the retention table so a reopen within TTL hits
    /// the Phase 1 fast path. Other code paths that legitimately
    /// need an immediate stop (route-changed-to-different-capsule,
    /// orphaned session, explicit Stop UI in a follow-up PR) keep
    /// using `stop_launched_session` directly.
    pub fn prune_panes(&mut self, pane_ids: &[usize], state: &mut AppState) {
        for &pane_id in pane_ids {
            if Some(pane_id) == self.active_pane_id {
                self.active_pane_id = None;
                self.automation.set_active_pane(None);
            }
            self.automation.fail_requests_for_pane(pane_id);
            self.automation.mark_page_unloaded(pane_id);
            if let Some(view) = self.views.remove(&pane_id) {
                self.retain_launched_session(&view, state);
            }
            self.visibility_cache.remove(&pane_id);
        }
        // Opportunistic TTL sweep: any pane close is a natural place
        // to spot expired retentions (cheap O(n) over ≤ cap entries).
        self.sweep_expired_retention(state);
    }

    /// Demote `view`'s launched session into the retention table
    /// instead of stopping it immediately. The session record stays
    /// on disk and the process keeps running, so the next click on
    /// the same handle hits the Phase 1 fast path. LRU overflow
    /// returned by the table is graceful-stopped via fire-and-forget
    /// thread.
    ///
    /// Called by `prune_panes` only. Other call sites (route changed
    /// to a different capsule, orphaned session cleanup) still go
    /// through `stop_launched_session` for an immediate stop because
    /// retention semantics don't apply there (RFC §3 force-destroy
    /// cases).
    fn retain_launched_session(&mut self, view: &ManagedWebView, state: &mut AppState) {
        let Some(session) = view.launched_session.as_ref() else {
            return;
        };
        // Stop the log follower regardless — pane is gone, so there
        // is no UI consumer for the log stream.
        self.stop_log_follower(&session.session_id);

        let evicted = self.retention.retain(
            session.session_id.clone(),
            session.handle.clone(),
            std::time::Instant::now(),
        );

        // RFC: SURFACE_CLOSE_SEMANTICS §6.4 — discoverability hook.
        // tracing surfaces this in the developer log (`stderr`); the
        // user-facing surface is owed by the next PR (PR 4B.2: pane
        // context menu, command palette `Stop all retained sessions
        // (N)`). state.activity renders only error-toned entries
        // today, so a `push_activity(Info, …)` here would be a no-op
        // for end users — left out intentionally.
        let retained_count = self.retention.len();
        tracing::info!(
            session_id = %session.session_id,
            handle = %session.handle,
            retained_count,
            ttl_minutes = crate::retention::DEFAULT_TTL.as_secs() / 60,
            "session retained on pane close — reopen within TTL hits the fast path"
        );

        // Keep state.activity push in for tests + future error-overlay
        // diagnostics; if a launch fails right after retention the
        // overlay can include this trail. Do NOT rely on it for
        // user-visible discoverability.
        state.push_activity(
            crate::state::ActivityTone::Info,
            format!(
                "Session kept warm for {} minutes (capsule: {})",
                crate::retention::DEFAULT_TTL.as_secs() / 60,
                session.handle
            ),
        );

        for (entry, reason) in evicted {
            tracing::info!(
                session_id = %entry.session_id,
                handle = %entry.handle,
                reason = reason.as_str(),
                "retention table at capacity; evicting oldest"
            );
            crate::retention::spawn_graceful_stop(entry, reason);
        }
    }

    /// Sweep expired retention entries and graceful-stop them.
    /// Called from `sync_from_state` (every render) and from
    /// `prune_panes` so idle time alone keeps retention bounded.
    fn sweep_expired_retention(&mut self, _state: &mut AppState) {
        let evicted = self.retention.evict_expired(std::time::Instant::now());
        for (entry, reason) in evicted {
            tracing::info!(
                session_id = %entry.session_id,
                handle = %entry.handle,
                reason = reason.as_str(),
                "retention TTL expired; graceful stop"
            );
            crate::retention::spawn_graceful_stop(entry, reason);
        }
    }

    /// Number of capsule sessions currently sitting in the retention
    /// table. Surfaced by the chrome indicator + command palette
    /// (RFC: SURFACE_CLOSE_SEMANTICS §6.4) so users can tell when
    /// hidden processes are still running.
    pub fn retention_count(&self) -> usize {
        self.retention.len()
    }

    /// Explicit Stop for the active pane's underlying session
    /// (RFC: SURFACE_CLOSE_SEMANTICS §6.1 / §6.2). Stops the process,
    /// removes the session record, and drops any retention entry —
    /// reopen will go through the cold path.
    ///
    /// This is the user-initiated path, so the stop is **synchronous**
    /// and any error is surfaced as an activity entry (the user
    /// actively asked for this; failure should not be silent). For
    /// machine-driven stops (TTL / quit / LRU / pane-close demote)
    /// see `retention::spawn_graceful_stop`.
    pub fn stop_active_session(&mut self, state: &mut AppState) -> bool {
        let Some(active_pane_id) = self.active_pane_id else {
            return false;
        };
        let Some(view) = self.views.get(&active_pane_id) else {
            return false;
        };
        let Some(session) = view.launched_session.as_ref() else {
            return false;
        };
        let session_id = session.session_id.clone();
        let handle = session.handle.clone();

        // Drop from retention without stop — we're about to do an
        // immediate stop ourselves below, no need for the background
        // graceful-stop path.
        let _ = self.retention.take_by_session_id(&session_id);
        self.stop_log_follower(&session_id);

        match stop_guest_session(&session_id) {
            Ok(true) => {
                tracing::info!(
                    session_id = %session_id,
                    handle = %handle,
                    "stop_active_session: process terminated"
                );
                state.push_activity(
                    crate::state::ActivityTone::Info,
                    format!("Stopped session for {}", handle),
                );
                true
            }
            Ok(false) => {
                tracing::warn!(
                    session_id = %session_id,
                    handle = %handle,
                    "stop_active_session: session was already inactive"
                );
                state.push_activity(
                    crate::state::ActivityTone::Warning,
                    format!("Session for {} was already inactive", handle),
                );
                false
            }
            Err(err) => {
                tracing::error!(
                    session_id = %session_id,
                    handle = %handle,
                    error = %err,
                    "stop_active_session: graceful stop failed"
                );
                state.push_activity(
                    crate::state::ActivityTone::Error,
                    format!("Failed to stop session for {}: {err}", handle),
                );
                false
            }
        }
    }

    /// Drain every retained session and graceful-stop each in a
    /// background thread. Active panes (`self.views`) are
    /// **untouched** — the user has to close those panes first
    /// before the underlying session can be stopped via this path.
    /// Returns the number of sessions queued for stop.
    pub fn stop_all_retained_sessions(&mut self) -> usize {
        let drained = self.retention.drain();
        let count = drained.len();
        for (entry, _reason) in drained {
            // `_reason` is `AppQuit` because `drain()` reports it
            // that way; the caller-intent here is "user asked", so
            // tag the log accordingly. (Not worth a new
            // `EvictionReason::ExplicitStopAll` — only logs
            // distinguish.)
            tracing::info!(
                session_id = %entry.session_id,
                handle = %entry.handle,
                "stop_all_retained_sessions: graceful stop scheduled"
            );
            crate::retention::spawn_graceful_stop(entry, crate::retention::EvictionReason::AppQuit);
        }
        count
    }

    /// Mark `pane_id`'s update slot as `Checking` and dispatch a worker
    /// thread that calls `ato app latest <handle>` and posts the comparison
    /// result back via the installed channel. Skips silently when the
    /// session has no canonical handle / snapshot label (nothing to compare),
    /// or when no channel has been installed (tests).
    fn spawn_capsule_update_check(
        &self,
        pane_id: usize,
        session: &GuestLaunchSession,
        state: &mut AppState,
    ) {
        let Some(tx) = self.capsule_update_tx.clone() else {
            return;
        };
        let Some(canonical) = session.canonical_handle.clone() else {
            return;
        };
        let Some(current) = session.snapshot_label.clone() else {
            return;
        };

        state
            .capsule_updates
            .insert(pane_id, crate::state::CapsuleUpdate::Checking);

        std::thread::spawn(move || {
            let result = run_capsule_update_check(&canonical, &current);
            let _ = tx.send((pane_id, result));
        });
    }

    fn start_log_follower(&mut self, pane_id: usize, session: &GuestLaunchSession) {
        let Some(log_path) = session.log_path.clone() else {
            return;
        };
        if self.log_followers.contains_key(&session.session_id) {
            return;
        }

        let (stop_tx, stop_rx) = channel::<()>();
        let bridge = self.bridge.clone();
        let session_id = session.session_id.clone();

        thread::spawn(move || {
            follow_process_log(pane_id, &session_id, log_path, bridge, stop_rx);
        });

        self.log_followers
            .insert(session.session_id.clone(), stop_tx);
    }

    fn stop_log_follower(&mut self, session_id: &str) {
        if let Some(stop_tx) = self.log_followers.remove(session_id) {
            let _ = stop_tx.send(());
        }
    }

    fn stop_launched_session(&mut self, webview: &ManagedWebView, state: &mut AppState) {
        let Some(session) = &webview.launched_session else {
            return;
        };

        self.stop_guest_session_record(session, state);
    }

    fn stop_guest_session_record(&mut self, session: &GuestLaunchSession, state: &mut AppState) {
        self.stop_log_follower(&session.session_id);
        match stop_guest_session(&session.session_id) {
            Ok(true) => state.push_activity(
                ActivityTone::Info,
                format!("Stopped ato-cli guest session {}", session.session_id),
            ),
            Ok(false) => state.push_activity(
                ActivityTone::Warning,
                format!("Guest session {} was already inactive", session.session_id),
            ),
            Err(error) => state.push_activity(
                ActivityTone::Error,
                format!(
                    "Failed to stop guest session {}: {error}",
                    session.session_id
                ),
            ),
        }
    }

    /// Hide / unhide the active pane's WebView so an in-app GPUI
    /// overlay (omnibar autocomplete dropdown, etc.) can paint over
    /// it. The WKWebView is a native NSView and renders above every
    /// CALayer-backed GPUI element, so the only reliable way to make
    /// a GPUI overlay visible on top of it is to hide the WebView
    /// for the duration of the overlay.
    ///
    /// `hide=true` toggles the active pane invisible; `hide=false`
    /// restores it. No-op when there is no active pane.
    pub fn set_overlay_hides_webview(&mut self, hide: bool, state: &mut AppState) {
        let Some(active_pane_id) = self.active_pane_id else {
            return;
        };
        self.set_cached_visibility(active_pane_id, !hide, state);
    }

    pub fn sync_overlay_host_panel(
        &mut self,
        window: &Window,
        route: Option<url::Url>,
        bounds: Option<PaneBounds>,
        payload: Option<Value>,
        state: &mut AppState,
    ) {
        let Some(route) = route else {
            self.overlay_host_panel = None;
            return;
        };
        let Some(bounds) = bounds else {
            self.overlay_host_panel = None;
            return;
        };

        let route_key = route.to_string();
        let webview_bounds = content_bounds(bounds);
        let payload_json = serde_json::to_string(&payload.clone().unwrap_or(Value::Null))
            .unwrap_or_else(|_| "null".to_string());

        if let Some(existing) = self.overlay_host_panel.as_mut() {
            if existing.route_key == route_key {
                if bounds_changed(existing.bounds, webview_bounds) {
                    if let Err(error) = existing.apply_bounds(webview_bounds) {
                        state.push_activity(
                            ActivityTone::Error,
                            format!("Failed to resize overlay host panel: {error}"),
                        );
                    }
                }
                sync_host_panel_payload(existing, &payload_json, state);
                return;
            }
        }

        self.overlay_host_panel = None;

        match self.build_host_panel_overlay_webview(window, route, bounds, payload) {
            Ok(view) => {
                self.overlay_host_panel = Some(view);
            }
            Err(error) => state.push_activity(
                ActivityTone::Error,
                format!("Failed to create overlay host panel: {error}"),
            ),
        }
    }

    fn set_cached_visibility(&mut self, pane_id: usize, visible: bool, state: &mut AppState) {
        let cached = self
            .visibility_cache
            .get(&pane_id)
            .copied()
            .unwrap_or(!visible);
        if cached == visible {
            return;
        }

        log_devtools(format!(
            "visibility change pane={} from={} to={}",
            pane_id, cached, visible
        ));

        if let Some(view) = self.views.get_mut(&pane_id) {
            if let Err(error) = view.set_visible(visible) {
                state.push_activity(
                    ActivityTone::Error,
                    format!("Failed to update child webview visibility: {error}"),
                );
                log_devtools(format!(
                    "visibility change failed pane={} to={} error={error}",
                    pane_id, visible
                ));
                return;
            }
        }

        self.visibility_cache.insert(pane_id, visible);
    }

    fn desired_responder_target(&self, state: &AppState) -> ResponderTarget {
        if !matches!(state.shell_mode, ShellMode::Focus) {
            return ResponderTarget::Host;
        }

        let Some(active) = state.active_web_pane() else {
            return ResponderTarget::Host;
        };

        let is_visible = self
            .visibility_cache
            .get(&active.pane_id)
            .copied()
            .unwrap_or(false);

        if is_visible && self.views.contains_key(&active.pane_id) {
            ResponderTarget::WebView(active.pane_id)
        } else {
            ResponderTarget::Host
        }
    }

    fn focus_host_view(&self) -> Result<()> {
        let Some(ResponderTarget::WebView(pane_id)) = self.responder_target else {
            return Ok(());
        };

        let Some(view) = self.views.get(&pane_id) else {
            return Ok(());
        };

        view.webview
            .focus_parent()
            .with_context(|| format!("unable to focus host view from pane {pane_id}"))
    }

    fn focus_webview(&self, pane_id: usize) -> Result<()> {
        let Some(view) = self.views.get(&pane_id) else {
            return Ok(());
        };

        view.webview
            .focus()
            .with_context(|| format!("unable to focus child webview for pane {pane_id}"))
    }
}

impl Drop for WebViewManager {
    fn drop(&mut self) {
        for (_, stop_tx) in self.log_followers.drain() {
            let _ = stop_tx.send(());
        }

        // RFC: SURFACE_CLOSE_SEMANTICS §7.2 — app quit stops every
        // retained session in v0. Process exit is already
        // synchronous from the user's perspective, so blocking on
        // stop here is acceptable; no retention persists across
        // Desktop restarts in v0.
        for (entry, _reason) in self.retention.drain() {
            tracing::debug!(
                session_id = %entry.session_id,
                handle = %entry.handle,
                "stopping retained session on Desktop quit"
            );
            let _ = stop_guest_session(&entry.session_id);
        }

        // Best-effort shutdown so orphaned guest sessions do not survive process exit.
        for existing in self.views.drain().map(|(_, existing)| existing) {
            if let Some(session) = existing.launched_session.as_ref() {
                let _ = stop_guest_session(&session.session_id);
            }
        }

        for pending in self.pending_launches.drain().map(|(_, pending)| pending) {
            drop(pending.receiver);
        }
    }
}

/// Copy the bits of `CapsuleLaunchSession` that the route-info
/// popover reads (URLs, runtime + display strategy labels, paths)
/// onto the active WebPane. Keeping the launched_session as the
/// source of truth and just mirroring it avoids reshaping the
/// WebViewManager's lifecycle, while still letting the read-only
/// surfaces (popover, inspector) render the running dev-server URL.
fn apply_launch_session_metadata(
    state: &mut AppState,
    pane_id: usize,
    session: &GuestLaunchSession,
) {
    let runtime_label = if !session.runtime.target_label.is_empty() {
        Some(session.runtime.target_label.clone())
    } else {
        session.runtime.runtime.clone()
    };
    state.update_capsule_route_metadata(
        pane_id,
        session.canonical_handle.clone(),
        session.source.clone(),
        Some(session.trust_state.clone()),
        session.restricted,
        session.snapshot_label.clone(),
        Some(session.session_id.clone()),
        session.adapter.clone(),
        Some(session.manifest_path.display().to_string()),
        runtime_label,
        Some(session.display_strategy.as_str().to_string()),
        session.log_path.as_ref().map(|p| p.display().to_string()),
        session.local_url.clone(),
        session.healthcheck_url.clone(),
        session.invoke_url.clone(),
        session.served_by.clone(),
    );
}

/// Worker-thread body for the per-pane capsule update check.
///
/// Calls `orchestrator::fetch_latest_capsule_version` (which subprocess-runs
/// `ato app latest <handle> --json`) and compares the registry's reply to
/// the running snapshot label using semver. The result is funnelled back
/// to `DesktopShell::poll_capsule_updates` through the channel installed
/// by `install_capsule_update_channel`.
fn run_capsule_update_check(canonical_handle: &str, current: &str) -> crate::state::CapsuleUpdate {
    use crate::state::CapsuleUpdate;

    let latest = match crate::orchestrator::fetch_latest_capsule_version(canonical_handle) {
        Ok(Some(value)) => value,
        // Registry knows the capsule but has no published release yet —
        // nothing to upgrade to, so call it up-to-date rather than failed.
        Ok(None) => {
            return CapsuleUpdate::UpToDate {
                current: current.to_string(),
            };
        }
        Err(error) => {
            return CapsuleUpdate::Failed {
                message: format!("registry lookup failed: {error}"),
            };
        }
    };

    // Trim a leading 'v' on either side so capsule manifests using `v0.3.4`
    // and registries using `0.3.4` interoperate.
    let normalize = |s: &str| s.trim().trim_start_matches('v').to_string();
    let current_norm = normalize(current);
    let latest_norm = normalize(&latest);

    let parsed_current = semver::Version::parse(&current_norm);
    let parsed_latest = semver::Version::parse(&latest_norm);

    match (parsed_current, parsed_latest) {
        (Ok(current_v), Ok(latest_v)) if latest_v > current_v => CapsuleUpdate::Available {
            current: current_norm,
            latest: latest_norm.clone(),
            target_handle: target_handle_for_version(canonical_handle, &latest_norm),
        },
        (Ok(_), Ok(_)) => CapsuleUpdate::UpToDate {
            current: current_norm,
        },
        // Either side failed semver parsing — fall back to a plain string
        // inequality so non-standard version strings still surface a banner
        // when they differ. Better than silently swallowing the signal.
        _ => {
            if current_norm != latest_norm {
                CapsuleUpdate::Available {
                    current: current_norm,
                    latest: latest_norm.clone(),
                    target_handle: target_handle_for_version(canonical_handle, &latest_norm),
                }
            } else {
                CapsuleUpdate::UpToDate {
                    current: current_norm,
                }
            }
        }
    }
}

fn follow_process_log(
    pane_id: usize,
    session_id: &str,
    log_path: PathBuf,
    bridge: BridgeProxy,
    stop_rx: Receiver<()>,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !log_path.exists() {
        if stop_rx.try_recv().is_ok() {
            return;
        }
        if Instant::now() > deadline {
            bridge.log(
                ActivityTone::Warning,
                format!(
                    "Process log for session {} never appeared at {}",
                    session_id,
                    log_path.display()
                ),
            );
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }

    let file = match std::fs::File::open(&log_path) {
        Ok(file) => file,
        Err(error) => {
            bridge.log(
                ActivityTone::Warning,
                format!(
                    "Failed to open process log for session {}: {}",
                    session_id, error
                ),
            );
            return;
        }
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();

    loop {
        if stop_rx.try_recv().is_ok() {
            return;
        }

        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => thread::sleep(Duration::from_millis(50)),
            Ok(_) => {
                let message = line.trim_end_matches(['\r', '\n']).to_string();
                if message.is_empty() {
                    continue;
                }
                bridge.push_shell_event(ShellEvent::ProcessLog { pane_id, message });
            }
            Err(error) => {
                bridge.log(
                    ActivityTone::Warning,
                    format!(
                        "Process log follower for session {} stopped after read error: {}",
                        session_id, error
                    ),
                );
                return;
            }
        }
    }
}

/// Build the canonical handle pinned to a specific version. Strips an
/// existing `@<old>` suffix if present so the result is idempotent for the
/// "click Install update twice in a row" case.
///
/// Examples:
///   - `capsule://ato.run/koh0920/byok-ai-chat@0.3.3`, `0.3.4`
///       → `capsule://ato.run/koh0920/byok-ai-chat@0.3.4`
///   - `capsule://ato.run/koh0920/byok-ai-chat`,       `0.3.4`
///       → `capsule://ato.run/koh0920/byok-ai-chat@0.3.4`
fn target_handle_for_version(canonical_handle: &str, latest: &str) -> String {
    // Only strip the LAST `@` so publisher names containing `@` (unlikely but
    // possible) don't get truncated. The version suffix is whatever follows
    // the final `@` in the canonical handle.
    let base = match canonical_handle.rsplit_once('@') {
        Some((prefix, _existing_version)) => prefix,
        None => canonical_handle,
    };
    format!("{}@{}", base, latest)
}

fn should_install_ato_auth_cookies(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    parsed.host_str() == Some("ato.run") && parsed.path().starts_with("/dock")
}

fn load_desktop_auth_handoff() -> Result<DesktopAuthHandoff> {
    let ato_bin = crate::orchestrator::resolve_ato_binary()
        .context("failed to locate ato binary for desktop auth handoff")?;
    let output = Command::new(&ato_bin)
        .arg("desktop-auth-handoff")
        .output()
        .context("failed to run `ato desktop-auth-handoff`")?;

    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "`ato desktop-auth-handoff` exited non-zero: {}",
            detail.trim()
        );
    }

    serde_json::from_slice(&output.stdout)
        .context("failed to parse `ato desktop-auth-handoff` JSON")
}

fn auth_initial_request_headers(handoff: &DesktopAuthHandoff) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        COOKIE,
        HeaderValue::from_str(&store_session_cookie_header(&handoff.session_token))
            .context("failed to build ato.run Cookie header")?,
    );
    Ok(headers)
}

fn install_ato_auth_cookies(webview: &WebView, handoff: &DesktopAuthHandoff) -> Result<()> {
    for (domain, secure) in ato_auth_cookie_targets(handoff) {
        let session_cookie =
            cookie::Cookie::build(("better-auth.session_token", handoff.session_token.clone()))
                .domain(domain.clone())
                .path("/")
                .secure(secure)
                .http_only(true)
                .same_site(cookie::SameSite::Lax)
                .build();
        webview.set_cookie(&session_cookie)?;

        if secure {
            let secure_cookie = cookie::Cookie::build((
                "__Secure-better-auth.session_token",
                handoff.session_token.clone(),
            ))
            .domain(domain)
            .path("/")
            .secure(true)
            .http_only(true)
            .same_site(cookie::SameSite::Lax)
            .build();
            webview.set_cookie(&secure_cookie)?;
        }
    }
    Ok(())
}

fn ato_auth_cookie_targets(handoff: &DesktopAuthHandoff) -> Vec<(String, bool)> {
    let mut seen = HashSet::new();
    [&handoff.site_base_url, &handoff.api_base_url]
        .into_iter()
        .filter_map(|base| {
            let parsed = url::Url::parse(base).ok()?;
            let host = parsed.host_str()?.to_string();
            let secure = parsed.scheme() == "https";
            if seen.insert((host.clone(), secure)) {
                Some((host, secure))
            } else {
                None
            }
        })
        .collect()
}

fn store_session_cookie_header(session_token: &str) -> String {
    format!(
        "better-auth.session_token={}; __Secure-better-auth.session_token={}",
        session_token, session_token
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WebViewReuseAction {
    Rebuild,
    Navigate,
    Keep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponderTarget {
    Host,
    WebView(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BuildFlags {
    inject_bridge: bool,
    enable_ipc: bool,
    enable_custom_protocol: bool,
    page_load_behavior: PageLoadBehavior,
    observe_title_changes: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageLoadBehavior {
    None,
    UpdateExternalUrl,
    MarkCapsuleReady,
}

fn reuse_action(
    existing_pane_id: usize,
    existing_route: &GuestRoute,
    existing_route_key: &str,
    next: &ActiveWebPane,
) -> WebViewReuseAction {
    if existing_pane_id != next.pane_id {
        return WebViewReuseAction::Rebuild;
    }

    if existing_route_key == next.route.to_string() {
        return WebViewReuseAction::Keep;
    }

    if matches!(existing_route, GuestRoute::ExternalUrl(_))
        && matches!(next.route, GuestRoute::ExternalUrl(_))
    {
        return WebViewReuseAction::Navigate;
    }

    WebViewReuseAction::Rebuild
}

fn build_flags_for_route(route: &GuestRoute) -> BuildFlags {
    match route {
        GuestRoute::ExternalUrl(_) | GuestRoute::CapsuleUrl { .. } => BuildFlags {
            inject_bridge: false,
            enable_ipc: false,
            enable_custom_protocol: false,
            page_load_behavior: PageLoadBehavior::UpdateExternalUrl,
            observe_title_changes: true,
        },
        GuestRoute::Capsule { .. } | GuestRoute::CapsuleHandle { .. } => BuildFlags {
            inject_bridge: true,
            enable_ipc: true,
            enable_custom_protocol: true,
            page_load_behavior: PageLoadBehavior::MarkCapsuleReady,
            observe_title_changes: false,
        },
        GuestRoute::Terminal { .. } => BuildFlags {
            inject_bridge: false,
            enable_ipc: true,
            enable_custom_protocol: true,
            page_load_behavior: PageLoadBehavior::None,
            observe_title_changes: false,
        },
    }
}

fn select_all_script() -> &'static str {
    r#"(() => {
  const active = document.activeElement;
  const isTextInput = active && (
    active.tagName === 'TEXTAREA' ||
    (active.tagName === 'INPUT' && !['button','checkbox','color','file','hidden','image','radio','range','reset','submit'].includes((active.type || '').toLowerCase()))
  );
  if (isTextInput) {
    active.focus();
    active.select();
    return;
  }
  if (active && active.isContentEditable) {
    const selection = window.getSelection();
    if (!selection) return;
    const range = document.createRange();
    range.selectNodeContents(active);
    selection.removeAllRanges();
    selection.addRange(range);
    return;
  }
  document.execCommand('selectAll');
})();"#
}

fn paste_script(text: &str) -> String {
    let text = serde_json::to_string(text).expect("clipboard text should serialize");
    format!(
        r#"(() => {{
  const text = {text};
  const active = document.activeElement;
  const isTextInput = active && (
    active.tagName === 'TEXTAREA' ||
    (active.tagName === 'INPUT' && !['button','checkbox','color','file','hidden','image','radio','range','reset','submit'].includes((active.type || '').toLowerCase()))
  );
  if (isTextInput) {{
    active.focus();
    const start = active.selectionStart ?? active.value.length;
    const end = active.selectionEnd ?? start;
    active.setRangeText(text, start, end, 'end');
    active.dispatchEvent(new InputEvent('input', {{ bubbles: true, inputType: 'insertText', data: text }}));
    return;
  }}
  if (active && active.isContentEditable) {{
    active.focus();
    const selection = window.getSelection();
    if (!selection) return;
    if (!selection.rangeCount) {{
      const range = document.createRange();
      range.selectNodeContents(active);
      range.collapse(false);
      selection.addRange(range);
    }}
    selection.deleteFromDocument();
    selection.getRangeAt(0).insertNode(document.createTextNode(text));
    selection.collapseToEnd();
    return;
  }}
  document.execCommand('insertText', false, text);
}})();"#,
        text = text,
    )
}

fn copy_script(cut: bool) -> String {
    format!(
        r#"(() => {{
  const cut = {cut};
  const active = document.activeElement;
  const isTextInput = active && (
    active.tagName === 'TEXTAREA' ||
    (active.tagName === 'INPUT' && !['button','checkbox','color','file','hidden','image','radio','range','reset','submit'].includes((active.type || '').toLowerCase()))
  );
  if (isTextInput) {{
    active.focus();
    const start = active.selectionStart ?? 0;
    const end = active.selectionEnd ?? start;
    const text = active.value.slice(start, end);
    if (cut && text && !active.readOnly && !active.disabled) {{
      active.setRangeText('', start, end, 'start');
      active.dispatchEvent(new InputEvent('input', {{ bubbles: true, inputType: 'deleteByCut', data: null }}));
    }}
    return {{ text }};
  }}
  const selection = window.getSelection();
  const text = selection ? selection.toString() : '';
  if (cut && text) {{
    if (active && active.isContentEditable) {{
      selection.deleteFromDocument();
    }}
  }}
  return {{ text }};
}})();"#,
        cut = if cut { "true" } else { "false" },
    )
}

fn write_text_to_system_clipboard(text: &str) -> Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to spawn pbcopy")?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .context("failed to write clipboard contents to pbcopy")?;
    }
    let status = child.wait().context("failed to wait for pbcopy")?;
    if !status.success() {
        anyhow::bail!("pbcopy exited with status {status}");
    }
    Ok(())
}

fn pending_launch_key(pane_id: usize, route_key: &str) -> String {
    format!("{pane_id}:{route_key}")
}

fn route_requires_ready_signal(route: &GuestRoute) -> bool {
    matches!(
        route,
        GuestRoute::Capsule { .. } | GuestRoute::CapsuleHandle { .. }
    )
}

fn should_show_webview(
    route: &GuestRoute,
    session: &WebSessionState,
    shell_mode: ShellMode,
    bounds: PaneBounds,
) -> bool {
    matches!(shell_mode, ShellMode::Focus | ShellMode::CommandBar)
        && bounds.width > 8.0
        && bounds.height > 8.0
        && (!route_requires_ready_signal(route) || matches!(session, WebSessionState::Mounted))
}

fn active_web_session(state: &AppState, pane_id: usize) -> Option<WebSessionState> {
    state.active_panes().into_iter().find_map(|pane| {
        if pane.id != pane_id {
            return None;
        }

        match &pane.surface {
            crate::state::PaneSurface::Web(web) => Some(web.session.clone()),
            crate::state::PaneSurface::HostPanel(_) => None,
            crate::state::PaneSurface::Native { .. }
            | crate::state::PaneSurface::CapsuleStatus(_)
            | crate::state::PaneSurface::Inspector
            | crate::state::PaneSurface::DevConsole
            | crate::state::PaneSurface::Launcher
            | crate::state::PaneSurface::Terminal(_)
            | crate::state::PaneSurface::AuthHandoff { .. } => None,
        }
    })
}

fn notify_window(async_app: AsyncApp, window_handle: AnyWindowHandle) {
    // Defer the update_window borrow to a future tick. notify_window
    // is called from Wry callbacks (page-load, IPC, title-changed)
    // and async-task continuations. When several panes load near the
    // app launch — which happens whenever ~/.ato/desktop-tabs.json
    // restores more than one tab — the synchronous update_window can
    // re-enter the GPUI App RefCell while it is already mut-borrowed
    // by application.run() / an AppKit selector and panic with
    // "RefCell already borrowed" at gpui async_context.rs.
    //
    // 16 ms ≈ one frame is enough to release the original borrow.
    let bg = async_app.background_executor().clone();
    let fe = async_app.foreground_executor().clone();
    fe.spawn(async move {
        bg.timer(std::time::Duration::from_millis(16)).await;
        let mut async_app = async_app;
        let _ = async_app.update_window(window_handle, |_, window, _| {
            window.refresh();
        });
    })
    .detach();
}

#[derive(Clone)]
struct ProtocolRouter;

#[derive(Clone)]
enum RouteContent {
    EmbeddedWelcome,
    GuestAssets(GuestLaunchSession),
    External,
    TerminalAssets,
}

impl ProtocolRouter {
    fn handle_async(
        &self,
        scheme: &str,
        request: Request<Vec<u8>>,
        responder: RequestAsyncResponder,
        bridge: BridgeProxy,
        allowlist: Vec<String>,
        session: Option<GuestSessionContext>,
        content: RouteContent,
    ) {
        let host = request.uri().host().unwrap_or("welcome").to_string();
        let path = request.uri().path().to_string();

        // Bridge RPC is routed separately from asset serving because it carries structured host messages.
        if path == "/__ato/bridge" {
            // Respond on a worker thread so bridge processing never blocks the webview callback.
            thread::spawn(move || {
                let response = route_bridge_request(request, bridge, &allowlist, session.as_ref())
                    .unwrap_or_else(|error| {
                        Response::builder()
                            .status(500)
                            .header(CONTENT_TYPE, "application/json; charset=utf-8")
                            .body(Cow::Owned(
                                serde_json::json!({
                                    "status": "error",
                                    "request_id": serde_json::Value::Null,
                                    "message": error.to_string(),
                                })
                                .to_string()
                                .into_bytes(),
                            ))
                            .expect("bridge error response should build")
                    });
                responder.respond(response);
            });
            return;
        }

        let response = self
            .handle_with_parts(scheme, &host, &path, &content)
            .unwrap_or_else(|error| {
                Response::builder()
                    .status(500)
                    .header(CONTENT_TYPE, "text/plain; charset=utf-8")
                    .body(Cow::Owned(error.to_string().into_bytes()))
                    .expect("protocol error response should build")
            });
        responder.respond(response);
    }

    fn handle_with_parts(
        &self,
        scheme: &str,
        host: &str,
        path: &str,
        content: &RouteContent,
    ) -> Result<Response<Cow<'static, [u8]>>> {
        if scheme == HOST_PANEL_SCHEME {
            return serve_host_panel_asset(path);
        }

        match content {
            RouteContent::EmbeddedWelcome => handle_embedded_welcome(scheme, host, path),
            RouteContent::GuestAssets(session) => serve_guest_asset(session, host, path),
            RouteContent::External => build_plain_response(
                404,
                format!("custom protocol not available for external route {scheme}: {path}"),
                "text/plain; charset=utf-8",
            ),
            RouteContent::TerminalAssets => serve_terminal_asset(path),
        }
    }

    fn scheme_for(&self, partition_id: &str) -> String {
        format!("capsule{}", compact(partition_id))
    }
}

struct PreloadRegistry;

impl PreloadRegistry {
    fn script_for(
        &self,
        profile: &str,
        allowlist_json: String,
        bridge_endpoint: Option<String>,
        guest_session: Option<serde_json::Value>,
    ) -> String {
        let shim = match profile {
            "electron" => include_str!("../assets/preload/electron.js"),
            "wails" => include_str!("../assets/preload/wails.js"),
            _ => include_str!("../assets/preload/tauri.js"),
        };
        let endpoint_json = bridge_endpoint
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null)
            .to_string();
        let session_json = guest_session.unwrap_or(serde_json::Value::Null).to_string();
        format!(
            "window.__ATO_BRIDGE_ALLOWLIST__ = {allowlist_json};\nwindow.__ATO_BRIDGE_ENDPOINT__ = {endpoint_json};\nwindow.__ATO_GUEST_SESSION__ = {session_json};\n{}\n{}",
            include_str!("../assets/preload/host_bridge.js"),
            shim,
        )
    }
}

fn handle_embedded_welcome(
    scheme: &str,
    host: &str,
    path: &str,
) -> Result<Response<Cow<'static, [u8]>>> {
    if host != "welcome" {
        return build_plain_response(
            404,
            format!("unknown capsule session: {host}"),
            "text/plain; charset=utf-8",
        );
    }

    match path {
        "/" | "/index.html" => build_embedded_response(
            include_str!("../assets/capsule/welcome/index.html"),
            "text/html; charset=utf-8",
        ),
        "/app.js" => build_embedded_response(
            include_str!("../assets/capsule/welcome/app.js"),
            "text/javascript; charset=utf-8",
        ),
        "/style.css" => build_embedded_response(
            include_str!("../assets/capsule/welcome/style.css"),
            "text/css; charset=utf-8",
        ),
        _ => build_plain_response(
            404,
            format!("asset not found for {scheme}: {path}"),
            "text/plain; charset=utf-8",
        ),
    }
}

fn serve_host_panel_asset(path: &str) -> Result<Response<Cow<'static, [u8]>>> {
    if let Some(dev_base) = host_panel_dev_base_url() {
        return proxy_host_panel_asset(&dev_base, path);
    }

    let Some(asset_path) = resolve_host_panel_asset_path(path)? else {
        return build_plain_response(
            404,
            format!("host panel asset not found: {path}"),
            "text/plain; charset=utf-8",
        );
    };

    let file = HOST_PANEL_DIST
        .get_file(&asset_path)
        .with_context(|| format!("missing embedded host panel asset: {asset_path}"))?;

    Response::builder()
        .status(200)
        .header(CONTENT_TYPE, host_panel_content_type(&asset_path))
        .body(Cow::Borrowed(file.contents()))
        .context("failed to build host panel asset response")
}

fn proxy_host_panel_asset(dev_base: &url::Url, path: &str) -> Result<Response<Cow<'static, [u8]>>> {
    let request_url = host_panel_request_url(dev_base, path)?;
    let response = match ureq::get(request_url.as_str()).call() {
        Ok(response) => response,
        Err(error) => {
            return build_plain_response(
                502,
                format!("failed to proxy host panel asset from dev server: {error}"),
                "text/plain; charset=utf-8",
            );
        }
    };

    let status = response.status();
    let content_type = response
        .header("content-type")
        .unwrap_or_else(|| host_panel_content_type(request_url.path()))
        .to_string();
    let mut reader = response.into_reader();
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .context("failed to read proxied host panel asset body")?;

    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type)
        .body(Cow::Owned(body))
        .context("failed to build proxied host panel asset response")
}

fn host_panel_dev_base_url() -> Option<url::Url> {
    std::env::var("ATO_DESKTOP_FRONTEND_DEV_URL")
        .ok()
        .and_then(|value| url::Url::parse(value.trim()).ok())
}

#[cfg_attr(not(test), allow(dead_code))]
fn allow_host_panel_navigation(url: &url::Url, dev_base: Option<&url::Url>) -> bool {
    if url.scheme() == HOST_PANEL_SCHEME {
        return true;
    }

    let Some(dev_base) = dev_base else {
        return false;
    };

    url.scheme() == dev_base.scheme()
        && url.host_str() == dev_base.host_str()
        && url.port_or_known_default() == dev_base.port_or_known_default()
}

fn host_panel_request_url(dev_base: &url::Url, path: &str) -> Result<url::Url> {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Ok(dev_base.clone());
    }

    dev_base
        .join(trimmed)
        .with_context(|| format!("failed to resolve host panel dev asset path: {path}"))
}

fn resolve_host_panel_asset_path(path: &str) -> Result<Option<String>> {
    let trimmed = path.trim_start_matches('/');

    if trimmed
        .split('/')
        .any(|segment| segment == ".." || segment.contains('\\'))
    {
        anyhow::bail!("parent traversal is not allowed for host panel assets: {path}");
    }

    let candidate = if trimmed.is_empty() {
        "index.html".to_string()
    } else {
        trimmed.to_string()
    };

    if HOST_PANEL_DIST.get_file(&candidate).is_some() {
        return Ok(Some(candidate));
    }

    if Path::new(&candidate).extension().is_some() {
        return Ok(None);
    }

    Ok(Some("index.html".to_string()))
}

fn host_panel_content_type(path: &str) -> &'static str {
    match Path::new(path).extension().and_then(|value| value.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("ico") => "image/x-icon",
        Some("jpeg") | Some("jpg") => "image/jpeg",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("map") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ttf") => "font/ttf",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "text/plain; charset=utf-8",
    }
}

fn build_embedded_response(
    body: &'static str,
    content_type: &'static str,
) -> Result<Response<Cow<'static, [u8]>>> {
    Response::builder()
        .status(200)
        .header(CONTENT_TYPE, content_type)
        .header(
            http::header::HeaderName::from_static("content-security-policy"),
            "default-src 'self' data: https:; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; connect-src 'self' https:;",
        )
        .body(Cow::Borrowed(body.as_bytes()))
        .context("failed to build embedded protocol response")
}

fn serve_terminal_asset(path: &str) -> Result<Response<Cow<'static, [u8]>>> {
    // Terminal assets are embedded at compile time to avoid filesystem access.
    // CSP restricts script sources to self + inline so xterm.js can initialise.
    const CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; font-src 'self' data:; img-src 'self' data:;";
    let (body, content_type): (&'static [u8], &'static str) = match path {
        "/" | "/index.html" => (
            include_bytes!("../assets/terminal/index.html"),
            "text/html; charset=utf-8",
        ),
        "/xterm.js" => (
            include_bytes!("../assets/terminal/xterm.js"),
            "application/javascript; charset=utf-8",
        ),
        "/xterm.css" => (
            include_bytes!("../assets/terminal/xterm.css"),
            "text/css; charset=utf-8",
        ),
        "/addon-canvas.js" => (
            include_bytes!("../assets/terminal/addon-canvas.js"),
            "application/javascript; charset=utf-8",
        ),
        _ => {
            return build_plain_response(
                404,
                format!("terminal asset not found: {path}"),
                "text/plain; charset=utf-8",
            );
        }
    };
    Response::builder()
        .status(200)
        .header(CONTENT_TYPE, content_type)
        .header(
            http::header::HeaderName::from_static("content-security-policy"),
            CSP,
        )
        .body(Cow::Borrowed(body))
        .context("failed to build terminal asset response")
}

fn build_plain_response(
    status: u16,
    body: String,
    content_type: &'static str,
) -> Result<Response<Cow<'static, [u8]>>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type)
        .body(Cow::Owned(body.into_bytes()))
        .context("failed to build plain protocol response")
}

fn build_bytes_response(
    status: u16,
    body: Vec<u8>,
    content_type: &'static str,
) -> Result<Response<Cow<'static, [u8]>>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type)
        .header(
            http::header::HeaderName::from_static("content-security-policy"),
            "default-src 'self' data: https:; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; connect-src 'self' https:;",
        )
        .body(Cow::Owned(body))
        .context("failed to build bytes protocol response")
}

fn bounds_changed(current: PaneBounds, next: PaneBounds) -> bool {
    (current.x - next.x).abs() > 0.5
        || (current.y - next.y).abs() > 0.5
        || (current.width - next.width).abs() > 0.5
        || (current.height - next.height).abs() > 0.5
}

fn content_bounds(bounds: PaneBounds) -> PaneBounds {
    PaneBounds {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height.max(1.0),
    }
}

fn bounds_to_rect(bounds: PaneBounds) -> Rect {
    use wry::dpi::{LogicalPosition, LogicalSize};

    Rect {
        position: LogicalPosition::new(bounds.x.max(0.0) as i32, bounds.y.max(0.0) as i32).into(),
        size: LogicalSize::new(bounds.width.max(1.0) as u32, bounds.height.max(1.0) as u32).into(),
    }
}

fn rect_to_bounds(rect: Rect) -> PaneBounds {
    let (x, y): (f64, f64) = rect.position.to_logical::<f64>(1.0).into();
    let (width, height): (f64, f64) = rect.size.to_logical::<f64>(1.0).into();

    PaneBounds {
        x: x as f32,
        y: y as f32,
        width: width as f32,
        height: height as f32,
    }
}

fn host_panel_bootstrap_script(pane_id: usize, payload_json: &str) -> String {
    format!(
        "(function(){{\n  const initialPayload = {payload_json};\n  const paneId = {pane_id};\n  window.__ATO_HOST_PANEL_PAYLOAD__ = initialPayload;\n  window.__ATO_HOST_PANEL_HYDRATE__ = function(payload) {{\n    window.__ATO_HOST_PANEL_PAYLOAD__ = payload;\n    window.dispatchEvent(new CustomEvent('ato-host-panel-payload', {{ detail: payload }}));\n  }};\n  window.__ATO_HOST_PANEL_NOTIFY__ = function(message) {{\n    try {{\n      if (window.ipc && typeof window.ipc.postMessage === 'function') {{\n        window.ipc.postMessage(JSON.stringify({{ __ato_host_panel__: message, paneId }}));\n      }}\n    }} catch (_error) {{}}\n  }};\n}})();"
    )
}

fn sync_host_panel_payload(view: &mut ManagedWebView, payload_json: &str, state: &mut AppState) {
    if view.host_panel_payload_json.as_deref() == Some(payload_json) {
        return;
    }

    let script = format!(
        "(function(payload){{ if (window.__ATO_HOST_PANEL_HYDRATE__) {{ window.__ATO_HOST_PANEL_HYDRATE__(payload); }} else {{ window.__ATO_HOST_PANEL_PAYLOAD__ = payload; window.dispatchEvent(new CustomEvent('ato-host-panel-payload', {{ detail: payload }})); }} }} )({payload_json});"
    );

    if let Err(error) = view.webview.evaluate_script(&script) {
        state.push_activity(
            ActivityTone::Error,
            format!("Failed to update host panel payload: {error}"),
        );
        return;
    }

    view.host_panel_payload_json = Some(payload_json.to_string());
}

fn resolve_icon_source_for_payload(raw: &str) -> Option<String> {
    if raw.starts_with("http://")
        || raw.starts_with("https://")
        || raw.starts_with("data:")
        || raw.starts_with("file://")
    {
        return Some(raw.to_string());
    }
    let bytes = std::fs::read(raw).ok()?;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let ext = std::path::Path::new(raw)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png");
    let mime = match ext.to_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        _ => "image/png",
    };
    Some(format!("data:{mime};base64,{encoded}"))
}

pub(crate) fn overlay_host_panel_payload(state: &AppState) -> Option<Value> {
    let inspector = state.active_capsule_inspector()?;
    let capabilities = state
        .active_web_pane()
        .filter(|pane| pane.pane_id == inspector.pane_id)
        .map(|pane| {
            pane.capabilities
                .iter()
                .map(|capability| capability.as_str().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let logs = inspector
        .logs
        .iter()
        .map(|entry| {
            serde_json::json!({
                "stage": entry.stage.as_str(),
                "tone": activity_tone_label(entry.tone.clone()),
                "message": entry.message,
            })
        })
        .collect::<Vec<_>>();
    let network = state
        .network_logs
        .iter()
        .filter(|entry| entry.pane_id == inspector.pane_id)
        .rev()
        .take(12)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|entry| {
            serde_json::json!({
                "method": entry.method,
                "url": entry.url,
                "status": entry.status,
                "durationMs": entry.duration_ms,
            })
        })
        .collect::<Vec<_>>();
    let update = state
        .capsule_updates
        .get(&inspector.pane_id)
        .map(|update| match update {
            crate::state::CapsuleUpdate::Idle => serde_json::json!({ "kind": "idle" }),
            crate::state::CapsuleUpdate::Checking => serde_json::json!({ "kind": "checking" }),
            crate::state::CapsuleUpdate::UpToDate { current } => serde_json::json!({
                "kind": "up-to-date",
                "current": current,
            }),
            crate::state::CapsuleUpdate::Available {
                current,
                latest,
                target_handle,
            } => serde_json::json!({
                "kind": "available",
                "current": current,
                "latest": latest,
                "targetHandle": target_handle,
            }),
            crate::state::CapsuleUpdate::Failed { message } => serde_json::json!({
                "kind": "failed",
                "message": message,
            }),
        });
    let trust_label = inspector.trust_state.clone().unwrap_or_else(|| {
        if inspector.restricted {
            "untrusted".to_string()
        } else {
            "pending".to_string()
        }
    });
    let quick_open_url = inspector
        .local_url
        .clone()
        .or_else(|| inspector.invoke_url.clone())
        .or_else(|| inspector.healthcheck_url.clone());
    let icon_source = state
        .pane_icons
        .get(&inspector.pane_id)
        .and_then(|raw| resolve_icon_source_for_payload(raw))
        .or_else(|| {
            // No manifest icon — fall back to the capsule's web favicon.
            // The host panel WebView has no img-src CSP restriction, so an
            // http://127.0.0.1 URL is loadable as long as the capsule is running.
            inspector
                .local_url
                .as_deref()
                .and_then(|u| web_favicon_origin(u))
                .map(|origin| format!("{origin}/favicon.ico"))
        });

    Some(serde_json::json!({
        "capsuleDetail": {
            "paneId": inspector.pane_id,
            "title": inspector.title,
            "handle": inspector.handle,
            "canonicalHandle": inspector.canonical_handle,
            "sourceLabel": inspector.source_label,
            "trustLabel": trust_label,
            "restricted": inspector.restricted,
            "versionLabel": inspector.snapshot_label.unwrap_or_else(|| "unversioned".to_string()),
            "sessionLabel": web_session_state_label(inspector.session_state),
            "sessionId": inspector.session_id,
            "adapter": inspector.adapter,
            "runtimeLabel": inspector.runtime_label,
            "displayStrategy": inspector.display_strategy,
            "servedBy": inspector.served_by,
            "routeLabel": inspector.handle,
            "manifestPath": inspector.manifest_path,
            "logPath": inspector.log_path,
            "localUrl": inspector.local_url,
            "healthcheckUrl": inspector.healthcheck_url,
            "invokeUrl": inspector.invoke_url,
            "quickOpenUrl": quick_open_url,
            "capabilities": capabilities,
            "logs": logs,
            "network": network,
            "update": update,
            "iconSource": icon_source,
        }
    }))
}

fn web_session_state_label(state: crate::state::WebSessionState) -> &'static str {
    match state {
        crate::state::WebSessionState::Detached => "detached",
        crate::state::WebSessionState::Resolving => "resolving",
        crate::state::WebSessionState::Materializing => "materializing",
        crate::state::WebSessionState::Launching => "launching",
        crate::state::WebSessionState::Mounted => "mounted",
        crate::state::WebSessionState::Closed => "closed",
        crate::state::WebSessionState::LaunchFailed => "launch-failed",
    }
}

fn activity_tone_label(tone: crate::state::ActivityTone) -> &'static str {
    match tone {
        crate::state::ActivityTone::Info => "info",
        crate::state::ActivityTone::Warning => "warning",
        crate::state::ActivityTone::Error => "error",
    }
}

#[cfg(target_os = "macos")]
fn install_macos_frame_host(webview: &WebView) -> Result<Retained<NSView>> {
    let mtm =
        MainThreadMarker::new().context("macOS frame host must be created on the main thread")?;
    let native_webview = webview.webview();
    let native_view: &NSView = native_webview.as_super().as_super();
    let content_view = unsafe { native_view.superview() }
        .context("child WKWebView is missing its content view parent")?;

    let frame_host = NSView::new(mtm);
    frame_host.setFrame(native_view.frame());
    frame_host.setAutoresizesSubviews(false);
    frame_host.setClipsToBounds(true);
    frame_host.setWantsLayer(true);
    if let Some(layer) = frame_host.layer() {
        layer.setMasksToBounds(true);
    }

    native_view.removeFromSuperview();
    frame_host.addSubview(native_view);
    native_view.setFrame(frame_host.bounds());
    content_view.addSubview(&frame_host);

    log_devtools(format!(
        "installed frame host bounds={}",
        format_bounds(bounds_from_ns_view(&frame_host))
    ));

    Ok(frame_host)
}

#[cfg(target_os = "macos")]
fn apply_bounds_to_macos_frame_host(
    frame_host: &NSView,
    webview: &WebView,
    bounds: PaneBounds,
) -> Result<()> {
    let parent_view = unsafe { frame_host.superview() }
        .context("frame host is missing its parent content view")?;
    let parent_frame = parent_view.frame();
    let mut frame = frame_host.frame();

    frame.origin.x = bounds.x as f64;
    frame.origin.y = parent_frame.size.height - bounds.y as f64 - bounds.height as f64;
    frame.size.width = bounds.width as f64;
    frame.size.height = bounds.height as f64;
    frame_host.setFrame(frame);

    let native_webview = webview.webview();
    let native_view: &NSView = native_webview.as_super().as_super();
    native_view.setFrame(frame_host.bounds());

    Ok(())
}

#[cfg(target_os = "macos")]
fn bounds_from_ns_view(view: &NSView) -> PaneBounds {
    let frame = view.frame();
    let parent_height = unsafe { view.superview() }
        .map(|parent| parent.frame().size.height)
        .unwrap_or(frame.size.height);

    PaneBounds {
        x: frame.origin.x as f32,
        y: (parent_height - frame.origin.y - frame.size.height) as f32,
        width: frame.size.width as f32,
        height: frame.size.height as f32,
    }
}

#[cfg(target_os = "macos")]
fn detach_macos_devtools_if_supported(webview: &WebView) {
    unsafe {
        let native_webview = webview.webview();
        let inspector: Retained<AnyObject> = msg_send![&*native_webview, _inspector];
        let detach = sel!(detach);
        let supports_detach: bool = msg_send![&*inspector, respondsToSelector: detach];
        if !supports_detach {
            log_devtools("open_devtools detach unsupported by current WebKit inspector");
            return;
        }

        let is_attached = sel!(isAttached);
        let supports_is_attached: bool = msg_send![&*inspector, respondsToSelector: is_attached];
        let was_attached = if supports_is_attached {
            let attached: bool = msg_send![&*inspector, isAttached];
            attached
        } else {
            false
        };

        let (): () = msg_send![&*inspector, detach];

        let now_attached = if supports_is_attached {
            let attached: bool = msg_send![&*inspector, isAttached];
            Some(attached)
        } else {
            None
        };

        log_devtools(format!(
            "open_devtools detach requested was_attached={} now_attached={}",
            was_attached,
            now_attached
                .map(|attached| attached.to_string())
                .unwrap_or_else(|| "<unknown>".to_string())
        ));
    }
}

fn compact(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

fn route_bridge_request(
    request: Request<Vec<u8>>,
    bridge: BridgeProxy,
    allowlist: &[String],
    session: Option<&GuestSessionContext>,
) -> Result<Response<Cow<'static, [u8]>>> {
    // The bridge is POST-only; anything else is a protocol misuse, not an application error.
    if request.method() != http::Method::POST {
        return Response::builder()
            .status(405)
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .body(Cow::Owned(
                serde_json::json!({
                    "status": "error",
                    "request_id": serde_json::Value::Null,
                    "message": "bridge endpoint only accepts POST",
                })
                .to_string()
                .into_bytes(),
            ))
            .context("failed to build bridge method error response");
    }

    let response = bridge.handle_payload_bytes(request.body(), allowlist, session)?;
    let status = match response {
        GuestBridgeResponse::Ok { .. } => 200,
        GuestBridgeResponse::Denied { .. } => 403,
        GuestBridgeResponse::Error { .. } => 400,
    };
    let body = bridge.serialize_response(&response)?;

    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .header(
            http::header::HeaderName::from_static("cache-control"),
            "no-store",
        )
        .body(Cow::Owned(body))
        .context("failed to build bridge response")
}

fn serve_guest_asset(
    session: &GuestLaunchSession,
    host: &str,
    path: &str,
) -> Result<Response<Cow<'static, [u8]>>> {
    if host != session.session_id {
        return build_plain_response(
            404,
            format!("unknown guest session host: {host}"),
            "text/plain; charset=utf-8",
        );
    }

    let requested_path = if path == "/" {
        session
            .frontend_url_path()
            .unwrap_or_else(|| "/index.html".to_string())
    } else {
        path.to_string()
    };

    let root = session
        .app_root
        .canonicalize()
        .with_context(|| format!("failed to resolve app root {}", session.app_root.display()))?;
    let relative = requested_path.trim_start_matches('/');
    // Canonicalize before reading so guest assets cannot escape the capsule root.
    let raw_candidate = PathBuf::from(relative);
    let candidate = if raw_candidate.is_absolute() {
        raw_candidate
    } else {
        root.join(relative)
    };
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("failed to resolve guest asset {}", candidate.display()))?;

    if !canonical.starts_with(&root) {
        return build_plain_response(
            403,
            format!("guest asset path escapes root: {requested_path}"),
            "text/plain; charset=utf-8",
        );
    }

    let bytes = fs::read(&canonical)
        .with_context(|| format!("failed to read guest asset {}", canonical.display()))?;
    build_bytes_response(200, bytes, mime_for_path(&canonical))
}

fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Decode a standard base64 string into bytes.

// ── Apply helpers (shared between UI handlers and MCP tool dispatch) ─────────

/// Approve the pending ExecutionPlan consent for `handle`: invoke
/// `ato internal consent approve-execution-plan` (the CLI writer
/// owns the JSONL append), mark the per-handle retry-once budget as
/// consumed, and clear `AppState::pending_consent` so
/// `ensure_pending_local_launch` re-arms the launch on the next
/// render. Used by:
///
/// - the UI's `ApproveConsentForm` action handler, and
/// - the `approve_execution_plan_consent` MCP tool.
///
/// The two callers share this helper so the user-facing surface and
/// the automation surface go through the same code path. If the CLI
/// invocation fails, the modal stays open and the budget is NOT
/// consumed (the user can retry the same Approve).
pub(crate) fn apply_capsule_consent(state: &mut AppState, handle: &str) -> Result<(), String> {
    let request = state
        .pending_consent
        .as_ref()
        .filter(|r| r.handle == handle)
        .cloned()
        .ok_or_else(|| {
            format!(
                "no pending ExecutionPlan consent matches handle '{handle}' \
                 (the modal is either closed or pinned to a different handle)"
            )
        })?;

    crate::orchestrator::approve_execution_plan_consent(
        &request.scoped_id,
        &request.version,
        &request.target_label,
        &request.policy_segment_hash,
        &request.provisioning_policy_hash,
    )
    .map_err(|err| format!("failed to record consent: {err:#}"))?;

    state.mark_consent_retry_consumed(handle, &request.target_label);
    state.clear_pending_consent();
    Ok(())
}

// ── Automation command dispatch ───────────────────────────────────────────────

/// Apply a batch of secrets for `handle` and (optionally) clear an open
/// `pending_config` modal pointing at the same handle so the next render
/// re-arms the launch.
///
/// Returns the keys that were applied (in input order) on success. On the
/// first persist failure, returns `Err(message)` — earlier secrets that
/// already wrote successfully stay in `secrets.json`, which matches the
/// modal Save handler's behaviour (it also bails on first error after
/// surfacing it). The caller turns this into a JSON-RPC error so MCP
/// callers can distinguish a failed save from a successful one.
pub(crate) fn apply_capsule_secrets(
    state: &mut AppState,
    handle: &str,
    secrets: &[(String, String)],
    clear_pending_config: bool,
) -> Result<Vec<String>, String> {
    let mut applied = Vec::with_capacity(secrets.len());
    for (key, value) in secrets {
        if let Err(error) = state.add_secret(key.clone(), value.clone()) {
            return Err(format!("failed to save secret '{key}': {error}"));
        }
        if let Err(error) = state.grant_secret_to_capsule(handle, key) {
            return Err(format!(
                "failed to grant secret '{key}' to {handle}: {error}"
            ));
        }
        applied.push(key.clone());
    }

    if clear_pending_config {
        let matches = state
            .pending_config
            .as_ref()
            .map(|p| p.handle == handle)
            .unwrap_or(false);
        if matches {
            state.clear_pending_config();
        }
    }

    Ok(applied)
}

/// Execute a single automation command against a live WebView.
/// Called from `WebViewManager::dispatch_automation_requests` on the GPUI main thread.
fn dispatch_automation_command(
    req: PendingAutomationRequest,
    webview: &WebView,
    pane_id: usize,
    host: &AutomationHost,
) {
    use std::time::{Duration, Instant};
    use AutomationCommand::*;

    // Helper: call JS via evaluate_script_with_callback and route result to req.
    macro_rules! js_call {
        ($js:expr, $req:expr) => {{
            let tx = $req.clone_tx();
            let js_str: String = $js;
            if let Err(e) = webview.evaluate_script_with_callback(&js_str, move |result| {
                let v = serde_json::from_str::<Value>(&result)
                    .unwrap_or_else(|_| serde_json::json!({ "raw": result }));
                if let Ok(mut guard) = tx.lock() {
                    if let Some(sender) = guard.take() {
                        let _ = sender.send(Ok(v));
                    }
                }
            }) {
                $req.send(Err(e.to_string()));
            }
        }};
    }

    match req.command {
        Snapshot => js_call!("window.__atoAgent.snapshot()".into(), req),
        ConsoleMessages => js_call!("window.__atoAgent.getConsoleMessages()".into(), req),
        Click { ref ref_id } => {
            js_call!(
                format!(
                    "window.__atoAgent.click({})",
                    serde_json::to_string(ref_id).unwrap()
                ),
                req
            );
        }
        Fill {
            ref ref_id,
            ref value,
        } => {
            js_call!(
                format!(
                    "window.__atoAgent.fill({},{})",
                    serde_json::to_string(ref_id).unwrap(),
                    serde_json::to_string(value).unwrap()
                ),
                req
            );
        }
        Type {
            ref ref_id,
            ref text,
        } => {
            js_call!(
                format!(
                    "window.__atoAgent.type({},{})",
                    serde_json::to_string(ref_id).unwrap(),
                    serde_json::to_string(text).unwrap()
                ),
                req
            );
        }
        SelectOption {
            ref ref_id,
            ref value,
        } => {
            js_call!(
                format!(
                    "window.__atoAgent.selectOption({},{})",
                    serde_json::to_string(ref_id).unwrap(),
                    serde_json::to_string(value).unwrap()
                ),
                req
            );
        }
        Check {
            ref ref_id,
            checked,
        } => {
            js_call!(
                format!(
                    "window.__atoAgent.check({},{})",
                    serde_json::to_string(ref_id).unwrap(),
                    if checked { "true" } else { "false" }
                ),
                req
            );
        }
        PressKey { ref key } => {
            js_call!(
                format!(
                    "window.__atoAgent.pressKey({})",
                    serde_json::to_string(key).unwrap()
                ),
                req
            );
        }
        Evaluate { ref expression } => {
            // Run the expression directly (not via eval() inside agent.js) so that the
            // terminal page's CSP — which blocks 'unsafe-eval' — doesn't interfere.
            // evaluate_script_with_callback is a host-privileged API and bypasses CSP.
            let js = format!(
                "(function(){{try{{return JSON.stringify({{result:({})}}); }}catch(e){{return JSON.stringify({{error:String(e)}});}}}})()",
                expression
            );
            js_call!(js, req);
        }
        VerifyTextVisible { ref text } => {
            // Also check the xterm.js buffer for terminal panes (canvas-rendered text isn't
            // in document.body.textContent).
            let text_json = serde_json::to_string(text).unwrap();
            let js = format!(
                r#"(function(){{
  var needle = {text_json};
  if (document.body && document.body.textContent.includes(needle)) {{
    return JSON.stringify({{visible: true}});
  }}
  if (window.term) {{
    var buf = window.term.buffer.active;
    for (var i = 0; i < buf.length; i++) {{
      var line = buf.getLine(i);
      if (line && line.translateToString(true).includes(needle)) {{
        return JSON.stringify({{visible: true}});
      }}
    }}
  }}
  return JSON.stringify({{visible: false}});
}})()"#
            );
            js_call!(js, req);
        }
        VerifyElementVisible { ref ref_id } => {
            js_call!(
                format!(
                    "window.__atoAgent.verifyElementVisible({})",
                    serde_json::to_string(ref_id).unwrap()
                ),
                req
            );
        }
        WaitFor { ref selector, .. } => {
            let js = format!(
                "window.__atoAgent.waitFor({})",
                serde_json::to_string(selector).unwrap()
            );
            let tx = req.clone_tx();
            let deadline = req.wait_deadline;
            let host_clone = host.clone();
            let selector_clone = selector.clone();

            if let Err(e) = webview.evaluate_script_with_callback(&js, move |result| {
                let found = serde_json::from_str::<Value>(&result)
                    .ok()
                    .and_then(|v| v.get("found").and_then(|f| f.as_bool()))
                    .unwrap_or(false);

                if found {
                    if let Ok(mut guard) = tx.lock() {
                        if let Some(sender) = guard.take() {
                            let _ = sender.send(Ok(serde_json::json!({ "found": true })));
                        }
                    }
                } else if deadline.map_or(false, |d| Instant::now() < d) {
                    // Re-queue for retry; the foreground polling task retries within 50ms.
                    let remaining_ms = deadline
                        .map(|d| d.saturating_duration_since(Instant::now()).as_millis() as u64)
                        .unwrap_or(0);
                    if let Ok(mut guard) = tx.lock() {
                        if let Some(original_tx) = guard.take() {
                            let new_req = PendingAutomationRequest::new(
                                pane_id,
                                WaitFor {
                                    selector: selector_clone.clone(),
                                    timeout_ms: remaining_ms,
                                },
                                original_tx,
                            );
                            host_clone.requeue(vec![new_req]);
                            host_clone
                                .has_pending
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                } else {
                    if let Ok(mut guard) = tx.lock() {
                        if let Some(sender) = guard.take() {
                            let _ = sender.send(Err("wait_for timed out".into()));
                        }
                    }
                }
            }) {
                req.send(Err(e.to_string()));
            }
        }
        Screenshot => {
            let (inner_tx, inner_rx) = std::sync::mpsc::channel();
            crate::automation::screenshot::take_screenshot(webview, inner_tx);
            let req_tx = req.clone_tx();
            std::thread::spawn(
                move || match inner_rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(Ok(v)) => {
                        if let Ok(mut guard) = req_tx.lock() {
                            if let Some(sender) = guard.take() {
                                let _ = sender.send(Ok(v));
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        if let Ok(mut guard) = req_tx.lock() {
                            if let Some(sender) = guard.take() {
                                let _ = sender.send(Err(e));
                            }
                        }
                    }
                    Err(_) => {
                        if let Ok(mut guard) = req_tx.lock() {
                            if let Some(sender) = guard.take() {
                                let _ = sender.send(Err("screenshot timed out".into()));
                            }
                        }
                    }
                },
            );
        }
        Navigate { ref url } => {
            match webview.load_url(url) {
                Ok(()) => req.send(Ok(serde_json::json!({ "ok": true }))),
                Err(e) => req.send(Err(e.to_string())),
            };
        }
        NavigateBack => {
            let _ = webview.evaluate_script("history.back();");
            req.send(Ok(serde_json::json!({ "ok": true })));
        }
        NavigateForward => {
            let _ = webview.evaluate_script("history.forward();");
            req.send(Ok(serde_json::json!({ "ok": true })));
        }
        // Handled in dispatch_automation_requests before reaching here.
        ListPanes
        | FocusPane { .. }
        | OpenUrl { .. }
        | SetCapsuleSecrets { .. }
        | ApproveExecutionPlanConsent { .. }
        | StopActiveSession => {
            unreachable!()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{CapabilityGrant, WebSessionState};

    #[test]
    fn target_handle_replaces_existing_version_suffix() {
        assert_eq!(
            target_handle_for_version("capsule://ato.run/koh0920/byok-ai-chat@0.3.3", "0.3.4",),
            "capsule://ato.run/koh0920/byok-ai-chat@0.3.4",
        );
    }

    #[test]
    fn target_handle_appends_when_no_existing_version() {
        assert_eq!(
            target_handle_for_version("capsule://ato.run/koh0920/byok-ai-chat", "0.3.4",),
            "capsule://ato.run/koh0920/byok-ai-chat@0.3.4",
        );
    }

    #[test]
    fn target_handle_strips_only_last_at_suffix() {
        // Pathological case: an `@` somewhere earlier in the handle should
        // not get truncated. Only the trailing `@<version>` is replaced.
        assert_eq!(
            target_handle_for_version("capsule://ato.run/some@user/pkg@1.0.0", "1.1.0",),
            "capsule://ato.run/some@user/pkg@1.1.0",
        );
    }

    fn active_web_pane(route: GuestRoute, pane_id: usize) -> ActiveWebPane {
        ActiveWebPane {
            workspace_id: 1,
            task_id: 1,
            pane_id,
            title: route.to_string(),
            route: route.clone(),
            partition_id: "pane".to_string(),
            profile: "electron".to_string(),
            capabilities: vec![CapabilityGrant::OpenExternal],
            session: WebSessionState::Launching,
            source_label: None,
            trust_state: None,
            restricted: false,
            snapshot_label: None,
            canonical_handle: None,
            session_id: None,
            adapter: None,
            manifest_path: None,
            runtime_label: None,
            display_strategy: None,
            log_path: None,
            local_url: None,
            healthcheck_url: None,
            invoke_url: None,
            served_by: None,
            auth_flow: false,
            bounds: PaneBounds::empty(),
        }
    }

    #[test]
    fn dock_urls_install_ato_auth_cookies_only_for_ato_run_dock() {
        assert!(should_install_ato_auth_cookies("https://ato.run/dock"));
        assert!(should_install_ato_auth_cookies(
            "https://ato.run/dock/koh0920"
        ));
        assert!(!should_install_ato_auth_cookies("https://ato.run/auth"));
        assert!(!should_install_ato_auth_cookies("https://example.com/dock"));
    }

    #[test]
    fn ato_auth_cookie_targets_include_site_and_api_hosts() {
        let handoff = DesktopAuthHandoff {
            session_token: "secret".to_string(),
            site_base_url: "https://ato.run".to_string(),
            api_base_url: "https://api.ato.run".to_string(),
        };

        assert_eq!(
            ato_auth_cookie_targets(&handoff),
            vec![
                ("ato.run".to_string(), true),
                ("api.ato.run".to_string(), true),
            ]
        );
    }

    #[test]
    fn external_routes_disable_bridge_and_ipc() {
        let flags = build_flags_for_route(&GuestRoute::ExternalUrl(
            url::Url::parse("https://example.com").expect("url"),
        ));

        assert!(!flags.inject_bridge);
        assert!(!flags.enable_ipc);
        assert!(!flags.enable_custom_protocol);
        assert_eq!(
            flags.page_load_behavior,
            PageLoadBehavior::UpdateExternalUrl
        );
        assert!(flags.observe_title_changes);
    }

    #[test]
    fn capsule_routes_wait_for_ready_before_showing_webview() {
        let bounds = PaneBounds {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 480.0,
        };
        let route = GuestRoute::Capsule {
            session: "welcome".to_string(),
            entry_path: "/index.html".to_string(),
        };

        assert!(!should_show_webview(
            &route,
            &WebSessionState::Launching,
            ShellMode::Focus,
            bounds,
        ));
        assert!(should_show_webview(
            &route,
            &WebSessionState::Mounted,
            ShellMode::Focus,
            bounds,
        ));
    }

    #[test]
    fn external_routes_show_webview_without_ready_signal() {
        let bounds = PaneBounds {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 480.0,
        };
        let route = GuestRoute::ExternalUrl(url::Url::parse("https://example.com").expect("url"));

        assert!(should_show_webview(
            &route,
            &WebSessionState::Launching,
            ShellMode::Focus,
            bounds,
        ));
    }

    #[test]
    fn capsule_handle_web_url_build_flags_no_bridge_injection() {
        // WebUrl sessions must NOT inject the bridge — the preload script would be injected
        // into a raw web app that doesn't know about it, which would break the app.
        // They use a minimal window.onload IPC script (inject_window_ready_signal) so that
        // SessionReady only fires after all JS has executed and the app has rendered, rather
        // than on the premature PageLoadEvent::Finished (= didFinishNavigation = initial HTML commit).
        let external_flags = build_flags_for_route(&GuestRoute::ExternalUrl(
            url::Url::parse("http://localhost:3000").expect("url"),
        ));
        assert!(
            !external_flags.inject_bridge,
            "ExternalUrl must not inject bridge"
        );
        assert!(
            !external_flags.enable_ipc,
            "ExternalUrl must not enable IPC"
        );
        // ExternalUrl routes do NOT require ready signal → show on Launching
        let route = GuestRoute::ExternalUrl(url::Url::parse("http://localhost:3000").expect("url"));
        let bounds = PaneBounds {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 480.0,
        };
        assert!(
            should_show_webview(
                &route,
                &WebSessionState::Launching,
                ShellMode::Focus,
                bounds
            ),
            "ExternalUrl-style webview must be visible immediately on Launching state"
        );
        assert!(
            should_show_webview(&route, &WebSessionState::Mounted, ShellMode::Focus, bounds),
            "ExternalUrl-style webview must be visible when Mounted"
        );
    }

    #[test]
    fn command_bar_keeps_external_webviews_visible() {
        let bounds = PaneBounds {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 480.0,
        };
        let route = GuestRoute::ExternalUrl(url::Url::parse("https://example.com").expect("url"));

        assert!(should_show_webview(
            &route,
            &WebSessionState::Launching,
            ShellMode::CommandBar,
            bounds,
        ));
    }

    #[test]
    fn command_bar_keeps_ready_capsule_webviews_visible() {
        let bounds = PaneBounds {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 480.0,
        };
        let route = GuestRoute::Capsule {
            session: "welcome".to_string(),
            entry_path: "/index.html".to_string(),
        };

        assert!(should_show_webview(
            &route,
            &WebSessionState::Mounted,
            ShellMode::CommandBar,
            bounds,
        ));
    }

    #[test]
    fn reuse_action_navigates_between_external_urls_in_same_pane() {
        let existing =
            GuestRoute::ExternalUrl(url::Url::parse("https://example.com").expect("url"));
        let next = active_web_pane(
            GuestRoute::ExternalUrl(url::Url::parse("https://docs.rs").expect("url")),
            7,
        );

        assert_eq!(
            reuse_action(7, &existing, "https://example.com/", &next),
            WebViewReuseAction::Navigate
        );
    }

    #[test]
    fn reuse_action_rebuilds_on_route_kind_change() {
        let existing =
            GuestRoute::ExternalUrl(url::Url::parse("https://example.com").expect("url"));
        let next = active_web_pane(
            GuestRoute::Capsule {
                session: "welcome".to_string(),
                entry_path: "/index.html".to_string(),
            },
            7,
        );

        assert_eq!(
            reuse_action(7, &existing, "https://example.com/", &next),
            WebViewReuseAction::Rebuild
        );
    }

    #[test]
    fn terminal_bridge_preload_defines_ato_terminal_bridge() {
        // The preload must define window.__ato_terminal_bridge; without it the
        // xterm.js page has no channel to the host and keystrokes are dropped.
        assert!(
            super::TERMINAL_BRIDGE_PRELOAD.contains("window.__ato_terminal_bridge"),
            "preload must define the bridge entry point used by assets/terminal/index.html"
        );
        // The preload must route through window.ipc.postMessage — that is the
        // only channel the Wry WebView `with_ipc_handler` listens on.
        assert!(super::TERMINAL_BRIDGE_PRELOAD.contains("window.ipc"));
        assert!(super::TERMINAL_BRIDGE_PRELOAD.contains("postMessage"));
        // The preload must translate the JS `type` field to the kebab-case
        // `kind` tag that `GuestBridgeRequest` uses; otherwise serde refuses
        // to deserialize the message.
        assert!(super::TERMINAL_BRIDGE_PRELOAD.contains("kind"));
    }

    #[test]
    fn capsule_host_root_serves_frontend_index() {
        let response = serve_host_panel_asset("/").expect("host panel asset");

        assert_eq!(response.status(), 200);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        let body = std::str::from_utf8(response.body().as_ref()).expect("utf8");
        assert!(body.contains("<div id=\"root\"></div>"));
    }

    #[test]
    fn capsule_host_route_like_path_falls_back_to_index() {
        let root = serve_host_panel_asset("/").expect("host panel asset");
        let response = serve_host_panel_asset("/launcher").expect("host panel asset");

        assert_eq!(response.status(), 200);
        assert_eq!(response.body().as_ref(), root.body().as_ref());
    }

    #[test]
    fn capsule_host_missing_static_asset_returns_not_found() {
        let response = serve_host_panel_asset("/assets/missing.js").expect("host panel asset");

        assert_eq!(response.status(), 404);
    }

    #[test]
    fn capsule_host_rejects_parent_traversal() {
        let error = serve_host_panel_asset("/../secret.txt").expect_err("should reject traversal");

        assert!(error.to_string().contains("parent traversal"));
    }

    #[test]
    fn host_panel_request_url_uses_base_for_root() {
        let base = url::Url::parse("http://127.0.0.1:4174/").expect("url");

        let resolved = host_panel_request_url(&base, "/").expect("request url");

        assert_eq!(resolved.as_str(), "http://127.0.0.1:4174/");
    }

    #[test]
    fn host_panel_request_url_joins_nested_asset_paths() {
        let base = url::Url::parse("http://127.0.0.1:4174/").expect("url");

        let resolved = host_panel_request_url(&base, "/assets/main.js").expect("request url");

        assert_eq!(resolved.as_str(), "http://127.0.0.1:4174/assets/main.js");
    }

    #[test]
    fn host_panel_navigation_allows_capsule_host_scheme() {
        let target = url::Url::parse("capsule-host://host/launcher").expect("url");

        assert!(allow_host_panel_navigation(&target, None));
    }

    #[test]
    fn host_panel_navigation_rejects_external_origins_without_dev_url() {
        let target = url::Url::parse("https://example.com/settings").expect("url");

        assert!(!allow_host_panel_navigation(&target, None));
    }

    #[test]
    fn host_panel_navigation_allows_configured_dev_origin() {
        let target = url::Url::parse("http://127.0.0.1:4174/launcher").expect("url");
        let dev_base = url::Url::parse("http://127.0.0.1:4174/").expect("url");

        assert!(allow_host_panel_navigation(&target, Some(&dev_base)));
    }

    #[test]
    fn host_panel_navigation_rejects_other_dev_origins() {
        let target = url::Url::parse("http://127.0.0.1:4175/launcher").expect("url");
        let dev_base = url::Url::parse("http://127.0.0.1:4174/").expect("url");

        assert!(!allow_host_panel_navigation(&target, Some(&dev_base)));
    }

    // ── apply_capsule_secrets (used by automation MCP `set_capsule_secrets`) ──
    //
    // These tests pin the contract that the MCP path is wire-compatible with
    // the modal Save handler in `ui/mod.rs::save_pending_config`. They share
    // an env_lock because save_secrets reads ATO_HOME, and parallel tests
    // would otherwise see each other's tempdir.
    mod apply_capsule_secrets {
        use super::*;
        use crate::state::PendingConfigRequest;
        use std::ffi::OsString;
        use std::sync::{Mutex, MutexGuard, OnceLock};

        fn env_lock() -> MutexGuard<'static, ()> {
            static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            LOCK.get_or_init(|| Mutex::new(()))
                .lock()
                .expect("env lock")
        }

        struct EnvVarGuard {
            key: &'static str,
            previous: Option<OsString>,
        }

        impl EnvVarGuard {
            fn set_path(key: &'static str, value: &std::path::Path) -> Self {
                let previous = std::env::var_os(key);
                std::env::set_var(key, value);
                Self { key, previous }
            }
        }

        impl Drop for EnvVarGuard {
            fn drop(&mut self) {
                if let Some(value) = &self.previous {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }

        fn isolated_state() -> (tempfile::TempDir, EnvVarGuard, AppState) {
            let temp = tempfile::tempdir().expect("tempdir");
            let ato_home = temp.path().join("ato-home");
            std::fs::create_dir_all(ato_home.join("run")).expect("run dir");
            let guard = EnvVarGuard::set_path("ATO_HOME", &ato_home);
            // load_secrets / load_config / load_capsule_configs read under
            // ATO_HOME — initial() returns a state with no secrets pre-set.
            let state = AppState::initial();
            (temp, guard, state)
        }

        fn pending(handle: &str) -> PendingConfigRequest {
            PendingConfigRequest {
                handle: handle.to_string(),
                target: None,
                fields: Vec::new(),
                original_secrets: Vec::new(),
            }
        }

        #[test]
        fn persists_each_secret_grants_to_handle_and_returns_keys_in_order() {
            let _lock = env_lock();
            let (_tmp, _guard, mut state) = isolated_state();

            let secrets = vec![
                ("PG_PASSWORD".to_string(), "pgpw".to_string()),
                ("SECRET_KEY".to_string(), "sk".to_string()),
            ];

            let applied =
                apply_capsule_secrets(&mut state, "github.com/Koh0920/WasedaP2P", &secrets, true)
                    .expect("apply");

            assert_eq!(applied, vec!["PG_PASSWORD", "SECRET_KEY"]);

            let granted = state
                .secret_store
                .secrets_for_capsule("github.com/Koh0920/WasedaP2P");
            let mut keys: Vec<&str> = granted.iter().map(|e| e.key.as_str()).collect();
            keys.sort();
            assert_eq!(keys, vec!["PG_PASSWORD", "SECRET_KEY"]);
        }

        #[test]
        fn clears_pending_config_when_handle_matches_and_flag_is_true() {
            let _lock = env_lock();
            let (_tmp, _guard, mut state) = isolated_state();
            state.set_pending_config(pending("h"));

            apply_capsule_secrets(&mut state, "h", &[("K".into(), "v".into())], true)
                .expect("apply");

            assert!(state.pending_config.is_none(), "pending_config must clear");
        }

        #[test]
        fn leaves_pending_config_intact_when_flag_is_false() {
            let _lock = env_lock();
            let (_tmp, _guard, mut state) = isolated_state();
            state.set_pending_config(pending("h"));

            apply_capsule_secrets(&mut state, "h", &[("K".into(), "v".into())], false)
                .expect("apply");

            assert!(
                state.pending_config.is_some(),
                "pending_config must persist when flag=false"
            );
        }

        #[test]
        fn leaves_pending_config_intact_when_handle_mismatches() {
            let _lock = env_lock();
            let (_tmp, _guard, mut state) = isolated_state();
            state.set_pending_config(pending("other"));

            apply_capsule_secrets(&mut state, "h", &[("K".into(), "v".into())], true)
                .expect("apply");

            assert!(
                state.pending_config.is_some(),
                "modal for a different handle must not be dismissed"
            );
        }
    }

    // ── apply_capsule_consent (UI handler + MCP automation share path) ───
    //
    // These tests exercise the routing logic in `apply_capsule_consent`
    // — the handle-match check, the "no pending consent" error path,
    // and the success-path side effects on AppState. The actual CLI
    // invocation (`ato internal consent approve-execution-plan`) is
    // out of unit-test scope: it lives in `crate::orchestrator::
    // approve_execution_plan_consent`, gated behind `resolve_ato_binary`,
    // and is covered by an integration test (`tests/...`) that drives
    // the full plumbing surface.
    mod apply_capsule_consent {
        use super::*;
        use crate::state::PendingConsentRequest;

        fn pending(handle: &str) -> PendingConsentRequest {
            PendingConsentRequest {
                handle: handle.to_string(),
                scoped_id: "publisher/app".to_string(),
                version: "1.0.0".to_string(),
                target_label: "app".to_string(),
                policy_segment_hash: "blake3:aaa".to_string(),
                provisioning_policy_hash: "blake3:bbb".to_string(),
                summary: "Capsule: publisher/app@1.0.0".to_string(),
                original_secrets: Vec::new(),
            }
        }

        #[test]
        fn errors_when_no_pending_consent_matches_handle() {
            let mut state = AppState::initial();
            // No pending_consent at all.
            let err = apply_capsule_consent(&mut state, "any-handle").unwrap_err();
            assert!(
                err.contains("no pending ExecutionPlan consent"),
                "expected no-match error, got: {err}"
            );

            // Pending consent for a *different* handle must also reject —
            // approving by accident would leak consent to a capsule the
            // user never reviewed.
            state.set_pending_consent(pending("other-handle"));
            let err = apply_capsule_consent(&mut state, "wrong-handle").unwrap_err();
            assert!(
                err.contains("no pending ExecutionPlan consent"),
                "handle mismatch must error, got: {err}"
            );
        }

        /// Regression for the v0.5.0 per-target budget bug surfaced
        /// by #92 verification: a multi-target orchestration capsule
        /// (WasedaP2P → app + web) trips one E302 per target, each
        /// with its own policy hashes. Approving target=app must NOT
        /// poison the budget for target=web on the same handle.
        #[test]
        fn retry_budget_is_per_target_not_per_handle() {
            let mut state = AppState::initial();
            let handle = "capsule://github.com/Koh0920/WasedaP2P";

            // No budget consumed at the start.
            assert!(!state.consent_retry_already_consumed(handle, "app"));
            assert!(!state.consent_retry_already_consumed(handle, "web"));

            // Approving target=app marks ONLY (handle, "app") as
            // consumed. (handle, "web") is still untouched — its
            // E302 must still surface the modal next time.
            state.mark_consent_retry_consumed(handle, "app");
            assert!(state.consent_retry_already_consumed(handle, "app"));
            assert!(
                !state.consent_retry_already_consumed(handle, "web"),
                "web budget must NOT be poisoned by app's approve"
            );

            // Now approve target=web too.
            state.mark_consent_retry_consumed(handle, "web");
            assert!(state.consent_retry_already_consumed(handle, "app"));
            assert!(state.consent_retry_already_consumed(handle, "web"));

            // Reset (e.g. on Cancel or successful launch) clears
            // ALL targets under the handle.
            state.reset_consent_retry_budget(handle);
            assert!(!state.consent_retry_already_consumed(handle, "app"));
            assert!(!state.consent_retry_already_consumed(handle, "web"));
        }
    }
}

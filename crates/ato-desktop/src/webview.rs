use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use base64::Engine as _;
use gpui::{AnyWindowHandle, AppContext, AsyncApp, Window};
use http::header::CONTENT_TYPE;
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
use crate::orchestrator::{
    resolve_and_start_guest, spawn_cli_session, spawn_log_tail_session, spawn_terminal,
    stop_guest_session, take_pending_cli_command, take_pending_share_terminal, GuestLaunchSession,
    LaunchError, SpawnKind, SpawnSpec,
};
use crate::state::{
    ActiveWebPane, ActivityTone, AppState, AuthMode, AuthPolicyRegistry, AuthSessionStatus,
    BrowserCommandKind, CapabilityGrant, GuestRoute, PaneBounds, PendingConfigRequest, ShellMode,
    WebSessionState,
};
use crate::terminal::{TerminalCore, TryRecvOutput};
use capsule_wire::handle::CapsuleDisplayStrategy;
use tracing::{debug, error, info, warn};

const DEVTOOLS_DEBUG_ENV: &str = "ATO_DESKTOP_DEVTOOLS_DEBUG";

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
    /// Live PTY sessions keyed by session_id.
    terminal_sessions: HashMap<String, Box<dyn TerminalCore>>,
    /// Session IDs that have already exited — prevents re-spawning a shell after a share terminal ends.
    completed_terminal_sessions: HashSet<String>,
    /// Spawn errors queued until terminal page is loaded, then shown via xterm error banner.
    pending_terminal_errors: HashMap<String, String>,
    /// Automation host — handles AI-agent socket requests.
    automation: AutomationHost,
}

struct ManagedWebView {
    pane_id: usize,
    route: GuestRoute,
    route_key: String,
    bounds: PaneBounds,
    launched_session: Option<GuestLaunchSession>,
    webview: WebView,
    #[cfg(target_os = "macos")]
    frame_host: Option<Retained<NSView>>,
    _context: WebContext,
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
    receiver: Receiver<PendingLaunchResult>,
}

struct PendingLaunchResult {
    route_key: String,
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

        Self {
            views: HashMap::new(),
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
            terminal_sessions: HashMap::new(),
            completed_terminal_sessions: HashSet::new(),
            pending_terminal_errors: HashMap::new(),
            automation,
        }
    }

    pub fn sync_from_state(&mut self, window: &Window, state: &mut AppState) {
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
            self.sync_responder_target(state);
            return;
        };

        if self.active_pane_id != Some(active.pane_id) {
            if let Some(previous_pane_id) = self.active_pane_id {
                self.set_cached_visibility(previous_pane_id, false, state);
            }
            self.active_pane_id = Some(active.pane_id);
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
        let mut context = WebContext::new(None);
        let mut builder = WebViewBuilder::new_with_web_context(&mut context)
            .with_bounds(bounds_to_rect(webview_bounds));

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
            builder = builder.with_on_page_load_handler(move |event, url| match event {
                PageLoadEvent::Started => {
                    automation.mark_page_unloaded(pane_id);
                }
                PageLoadEvent::Finished => {
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
            builder = builder.with_navigation_handler(move |uri: String| {
                if auth_policy.classify(&uri) == AuthMode::BrowserRequired {
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

        let webview = builder
            .with_url(&url)
            .build_as_child(window)
            .with_context(|| format!("unable to create Wry child webview for {url}"))?;

        #[cfg(target_os = "macos")]
        let frame_host = Some(install_macos_frame_host(&webview)?);

        Ok(ManagedWebView {
            pane_id: pane.pane_id,
            route: pane.route.clone(),
            route_key: pane.route.to_string(),
            bounds: webview_bounds,
            launched_session,
            webview,
            #[cfg(target_os = "macos")]
            frame_host,
            _context: context,
        })
    }

    /// Drop cached webviews / terminals / launched sessions for a
    /// list of pane ids. Called by DesktopShell after AppState::close_task
    /// so closing a tab actually tears down the underlying Wry views
    /// instead of leaking them on the heap and leaving guest sessions
    /// running under ~/.ato/apps/.../sessions/.
    pub fn prune_panes(&mut self, pane_ids: &[usize], state: &mut AppState) {
        for &pane_id in pane_ids {
            if Some(pane_id) == self.active_pane_id {
                self.active_pane_id = None;
            }
            self.automation.fail_requests_for_pane(pane_id);
            self.automation.mark_page_unloaded(pane_id);
            if let Some(view) = self.views.remove(&pane_id) {
                self.stop_launched_session(&view, state);
            }
            self.visibility_cache.remove(&pane_id);
        }
    }

    fn stop_launched_session(&self, webview: &ManagedWebView, state: &mut AppState) {
        let Some(session) = &webview.launched_session else {
            return;
        };

        self.stop_guest_session_record(session, state);
    }

    fn stop_guest_session_record(&self, session: &GuestLaunchSession, state: &mut AppState) {
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

fn notify_window(mut async_app: AsyncApp, window_handle: AnyWindowHandle) {
    let _ = async_app.update_window(window_handle, |_, window, _| {
        window.refresh();
    });
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

// ── Automation command dispatch ───────────────────────────────────────────────

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
        ListPanes | FocusPane { .. } | OpenUrl { .. } => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{CapabilityGrant, WebSessionState};

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
            bounds: PaneBounds::empty(),
        }
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
}

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine as _;
use capsule::common::paths::ato_path_or_workspace_tmp;
use gpui::{AnyWindowHandle, AppContext, AsyncApp, Window};
use http::header::{CONTENT_TYPE, COOKIE};
use http::{HeaderMap, HeaderValue};
#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::runtime::AnyObject;
#[cfg(target_os = "macos")]
use objc2::{ClassType, msg_send, sel};
#[cfg(target_os = "macos")]
use objc2_app_kit::NSView;
#[cfg(target_os = "macos")]
use objc2_foundation::MainThreadMarker;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(target_os = "macos")]
use wry::WebViewBuilderExtDarwin;
#[cfg(target_os = "macos")]
use wry::WebViewExtMacOS;
use wry::http::{Request, Response};
use wry::{
    NewWindowResponse, PageLoadEvent, Rect, RequestAsyncResponder, WebContext, WebView,
    WebViewBuilder,
};

use crate::automation::AutomationHost;
use crate::automation::command::{AutomationCommand, PendingAutomationRequest};
use crate::bridge::{BridgeProxy, GuestBridgeResponse, GuestSessionContext, ShellEvent};
use crate::config::SecretEntry;
use crate::logging::TARGET_FAVICON;
use crate::orchestrator::{
    CommunityTomlInput, DesktopLaunchInput, GuestLaunchSession, LaunchError, SpawnKind, SpawnSpec,
    resolve_and_start_guest_with_input, spawn_cli_session, spawn_log_tail_session, spawn_terminal,
    stop_guest_session, take_pending_cli_command, take_pending_share_terminal,
};

/// Local helpers to construct `DesktopLaunchInput` from `ensure_pending_local_launch`
/// without needing a `mod` inside an `impl` block.
mod resolve_and_start_guest_with_input_fn {
    use super::{CommunityTomlInput, DesktopLaunchInput};

    pub(super) fn make_handle_input(handle: &str) -> DesktopLaunchInput {
        DesktopLaunchInput::from_handle(handle)
    }

    pub(super) fn make_community_input(handle: &str, ctoml_id: &str) -> DesktopLaunchInput {
        DesktopLaunchInput::CommunityToml(CommunityTomlInput {
            source_handle: handle.to_string(),
            ctoml_id: ctoml_id.to_string(),
        })
    }
}
use crate::proc_util::CommandNoWindowExt;
use crate::state::{
    ActiveWebPane, ActivityTone, AppState, AuthMode, AuthPolicyRegistry, AuthSessionStatus,
    BrowserCommandKind, CapabilityGrant, GuestRoute, PaneBounds, PaneId, PendingConfigRequest,
    PendingConsentRequest, ShellMode, WebSessionState, session::SessionRegistry,
};
use crate::terminal::{TerminalCore, TryRecvOutput};
use protocol::handle::CapsuleDisplayStrategy;
use share_icon::{ShareIconSource, resolve_share_icon};
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
    /// `ato://` deep links observed by a WebView navigation handler.
    /// These never load inside the WebView; they are forwarded to
    /// AppState::handle_host_route on the next sync_from_state pass
    /// so OAuth callbacks delivered via the in-app sign-in flow
    /// reach the same code path as macOS Launch Services callbacks.
    pending_callback_urls: Arc<Mutex<Vec<String>>>,
    /// Privileged `ato://` intents (run, runner control) classified + origin-
    /// accepted by `crate::intent` in a navigation handler. Drained on the next
    /// `sync_from_state` pass (which has `cx`) and dispatched after native
    /// confirmation. Kept separate from `pending_callback_urls` because these
    /// touch local execution / the runner agent and must never be forwarded to
    /// the origin-agnostic `handle_host_route` path.
    pending_privileged_intents: Arc<Mutex<Vec<crate::intent::PrivilegedIntent>>>,
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
    /// Shared `WebContext` for system routes (ExternalUrl, Terminal)
    /// so ato.run cookies and localStorage persist across tab open/close
    /// and across restarts (data directory: `~/.ato/desktop/webcontext/`).
    /// Capsule routes use isolated stores (incognito or profile-keyed)
    /// via `apply_webview_store_policy`; they do not share this context.
    web_context: WebContext,
    /// Retained-session table — RFC: SURFACE_CLOSE_SEMANTICS. Pane
    /// close demotes the session to this table instead of stopping
    /// it; reopen within TTL hits the Phase 1 fast path naturally
    /// (the on-disk session record stays alive). TTL expiry / app
    /// quit / LRU eviction stop sessions in fire-and-forget
    /// background threads so the UI never blocks on
    /// `ato app session stop`.
    retention: crate::retention::RetentionTable,
    webview_retention: WebViewRetentionTable,
    /// Last successful launch for each capsule handle. Persists across
    /// WebView rebuilds so the UI-stop path can find the live session
    /// even when the per-pane WebView's `launched_session` reference
    /// has been dropped — which we observe in the post-consent re-arm
    /// flow when a same-pane Rebuild evicts the previous WebView. See
    /// ato-run/ato#122 for the reproduction. Populated in
    /// `drain_pending_launches`'s success arm; consumed (cleared) in
    /// `stop_active_session` after a successful stop.
    handle_to_session: HashMap<String, GuestLaunchSession>,
    /// Whether the dock window is currently open. Used by `ListPanes`
    /// to expose the dock pane to automation callers.
    dock_is_open: bool,
}

/// Reserved pane ID for the dock window's WebView.
/// Never collides with real pane IDs (which start at 1 and are
/// assigned sequentially by `generate_pane_id`).
pub const DOCK_AUTOMATION_PANE_ID: usize = 999_000;

struct ManagedWebView {
    pane_id: usize,
    /// Mutable pane binding used by async handlers (IPC/page-load/title-change)
    /// so a retained WebView can be reattached to a new pane without keeping
    /// stale pane_id captures alive in closure environments.
    pane_binding: Arc<AtomicUsize>,
    route: GuestRoute,
    route_key: String,
    bounds: PaneBounds,
    launched_session: Option<GuestLaunchSession>,
    /// The WebView store class in use for this WebView, recorded at build time.
    /// Used by `sync_from_state` to detect store-class transitions (e.g.
    /// `CapsuleEphemeral` → `CapsuleProfile` when `install_profile_key` arrives)
    /// and force a `Rebuild` rather than keeping the mismatched store.
    store_class: WebViewStoreClass,
    /// The stable ingress key registered with `ato-netd` for this WebView.
    /// Tracks the ato-netd ingress registration for this WebView so the
    /// correct deregister call can be issued on session teardown.
    /// `None` when the route did not go through `ato-netd` (e.g. external
    /// URL, terminal, or fallback to direct `local_url`).
    ingress_registration: Option<crate::netd::IngressRegistration>,
    webview: WebView,
    #[cfg(target_os = "macos")]
    frame_host: Option<Retained<NSView>>,
}

#[derive(Debug, Deserialize)]
struct DesktopAuthHandoff {
    session_token: String,
    site_base_url: String,
    api_base_url: String,
    #[serde(default)]
    publisher_handle: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct AuthStatusResponse {
    signed_in: bool,
    api_base_url: String,
    account_hint: Option<String>,
}

impl ManagedWebView {
    fn rebind_pane_id(&mut self, pane_id: usize) {
        self.pane_binding.store(pane_id, Ordering::Relaxed);
        self.pane_id = pane_id;
    }

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

struct RetainedWebView {
    stable_origin_key: String,
    current_session_id: Option<String>,
    webview: ManagedWebView,
    retained_at: Instant,
}

struct WebViewRetentionTable {
    entries: Vec<RetainedWebView>,
    ttl: Duration,
    max_size: usize,
}

impl WebViewRetentionTable {
    fn with_defaults() -> Self {
        Self {
            entries: Vec::new(),
            ttl: crate::retention::DEFAULT_TTL,
            max_size: crate::retention::DEFAULT_MAX_RETAINED,
        }
    }

    fn retain(&mut self, mut entry: RetainedWebView, now: Instant) -> Vec<RetainedWebView> {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|item| item.stable_origin_key == entry.stable_origin_key)
        {
            existing.current_session_id = entry.current_session_id.take();
            existing.retained_at = now;
            existing.webview = entry.webview;
            return Vec::new();
        }

        entry.retained_at = now;
        self.entries.push(entry);

        let mut evicted = Vec::new();
        while self.entries.len() > self.max_size {
            evicted.push(self.entries.remove(0));
        }
        evicted
    }

    fn take_by_key(&mut self, stable_origin_key: &str) -> Option<RetainedWebView> {
        let idx = self
            .entries
            .iter()
            .position(|item| item.stable_origin_key == stable_origin_key)?;
        Some(self.entries.remove(idx))
    }

    fn take_by_session_id(&mut self, session_id: &str) -> Option<RetainedWebView> {
        let idx = self
            .entries
            .iter()
            .position(|item| item.current_session_id.as_deref() == Some(session_id))?;
        Some(self.entries.remove(idx))
    }

    fn evict_expired(&mut self, now: Instant) -> Vec<RetainedWebView> {
        let mut evicted = Vec::new();
        let mut i = 0;
        while i < self.entries.len() {
            if now.duration_since(self.entries[i].retained_at) >= self.ttl {
                evicted.push(self.entries.remove(i));
            } else {
                i += 1;
            }
        }
        evicted
    }

    fn drain(&mut self) -> Vec<RetainedWebView> {
        self.entries.drain(..).collect()
    }
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
            let pending = Arc::clone(&automation.pending);
            let fe = async_app.foreground_executor().clone();
            let be = async_app.background_executor().clone();
            let async_app_poll = async_app.clone();
            fe.spawn(async move {
                loop {
                    be.timer(Duration::from_millis(50)).await;
                    if crate::webview_init_guard::WebviewInitGuard::is_active() {
                        continue;
                    }
                    let queued = pending.lock().map(|q| !q.is_empty()).unwrap_or(false);
                    if has_pending.swap(false, Ordering::Relaxed) || queued {
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
            pending_privileged_intents: Arc::new(Mutex::new(Vec::new())),
            terminal_sessions: HashMap::new(),
            completed_terminal_sessions: HashSet::new(),
            pending_terminal_errors: HashMap::new(),
            log_followers: HashMap::new(),
            automation,
            prewarmed: false,
            web_context,
            capsule_update_tx: None,
            retention: crate::retention::RetentionTable::with_defaults(),
            webview_retention: WebViewRetentionTable::with_defaults(),
            handle_to_session: HashMap::new(),
            dock_is_open: false,
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

    /// Mark the dock window as open (`true`) or closed (`false`).
    /// Affects `ListPanes` output and dock automation dispatch.
    pub fn set_dock_open(&mut self, open: bool) {
        self.dock_is_open = open;
    }

    /// Clone the automation host (cheap — all state is behind `Arc`).
    pub fn automation_host(&self) -> crate::automation::AutomationHost {
        self.automation.clone()
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
        #[cfg(target_os = "windows")]
        crate::window::windows::prepare_window_for_webview(window);

        use wry::dpi::{LogicalPosition, LogicalSize};
        // Position off-screen and 1×1 so the prewarm view is invisible
        // even briefly. Errors here are silently ignored — prewarm is
        // best-effort optimisation. Use the shared web_context so the
        // prewarm and the real tabs share one on-disk data store.
        let _wv_guard = crate::webview_init_guard::WebviewInitGuard::new();
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
        self.sweep_expired_webview_retention();

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

        // Drain privileged intents (run, runner control) that `crate::intent`
        // classified + origin-accepted in a navigation handler. Dispatched here
        // (where `state` is available) — never through the origin-agnostic
        // host-route path. Runner control reuses `crate::runner_agent`: the
        // Desktop performs the privileged local CLI spawn; the runner backend,
        // CLI, and PWA management UI already exist.
        let privileged: Vec<crate::intent::PrivilegedIntent> = {
            let mut q = self
                .pending_privileged_intents
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            q.drain(..).collect()
        };
        for intent in privileged {
            dispatch_privileged_intent(state, intent);
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

        // Detect store-class transitions: if the route says Keep but the new
        // identity (e.g. install_profile_key just arrived) maps to a different
        // store class than the live WebView was built with, force a Rebuild so
        // the WebView gets the correct persistent store.
        let reuse_action = if matches!(reuse_action, WebViewReuseAction::Keep) {
            let new_identity = WebViewStoreIdentity {
                route: active.route.clone(),
                trust_state: active.trust_state.clone(),
                install_profile_key: active.install_profile_key.clone(),
                publisher_identity: None,
                source_identity: active.canonical_handle.clone(),
                snapshot_label: active.snapshot_label.clone(),
            };
            let new_class = store_class_for_identity(&new_identity);
            if self
                .views
                .get(&active.pane_id)
                .map(|v| v.store_class != new_class)
                .unwrap_or(false)
            {
                tracing::debug!(
                    pane_id = active.pane_id,
                    ?new_class,
                    "store class changed — forcing WebView rebuild"
                );
                WebViewReuseAction::Rebuild
            } else {
                reuse_action
            }
        } else {
            reuse_action
        };

        // Tracks whether the Navigate branch's `load_url` call below failed,
        // so the post-reuse `Mounted` cleanup can skip force-promoting a
        // pane whose new navigation never actually started (#143 review).
        let mut navigate_load_url_failed = false;

        if matches!(reuse_action, WebViewReuseAction::Rebuild) {
            if let Some(previous) = self.views.remove(&active.pane_id) {
                self.automation.fail_requests_for_pane(active.pane_id);
                self.automation.mark_page_unloaded(active.pane_id);
                self.stop_launched_session(&previous, state);
                state.sync_web_session_state(previous.pane_id, WebSessionState::Closed);
            }

            let active_identity = WebViewStoreIdentity {
                route: active.route.clone(),
                trust_state: active.trust_state.clone(),
                install_profile_key: active.install_profile_key.clone(),
                publisher_identity: None,
                source_identity: active.canonical_handle.clone(),
                snapshot_label: active.snapshot_label.clone(),
            };
            let retention_key = webview_retention_key_for_identity(&active_identity)
                .or_else(|| webview_retention_key_for_route(&active.route));
            if let Some(mut retained) = retention_key
                .as_deref()
                .and_then(|key| self.webview_retention.take_by_key(key))
            {
                if let Some(session_id) = retained.current_session_id.as_deref() {
                    let _ = self.retention.take_by_session_id(session_id);
                }
                let restored_session = retained.webview.launched_session.clone();
                retained.webview.rebind_pane_id(active.pane_id);
                retained.webview.route = active.route.clone();
                retained.webview.route_key = route_key.clone();
                if let Err(error) = retained.webview.apply_bounds(content_bounds(active.bounds)) {
                    state.push_activity(
                        ActivityTone::Error,
                        format!("Failed to resize retained webview: {error}"),
                    );
                }
                self.views.insert(active.pane_id, retained.webview);
                if let Some(session) = restored_session.as_ref() {
                    self.handle_to_session
                        .insert(session.handle.clone(), session.clone());
                    apply_launch_session_metadata(state, active.pane_id, session);
                    self.spawn_capsule_update_check(active.pane_id, session, state);
                    self.start_log_follower(active.pane_id, session);
                }
                state.sync_web_session_state(active.pane_id, WebSessionState::Mounted);
                self.automation.mark_page_loaded(active.pane_id);
            } else {
                match &active.route {
                    GuestRoute::CapsuleHandle {
                        handle,
                        community_toml_id,
                        ..
                    } => {
                        self.ensure_pending_local_launch(
                            active.pane_id,
                            &route_key,
                            handle,
                            community_toml_id.as_deref(),
                            state,
                        );
                    }
                    _ => match self.build_webview(
                        window,
                        &active,
                        None,
                        state.auth_policy_registry.clone(),
                    ) {
                        Ok(webview) => {
                            if !route_requires_ready_signal(&active.route) {
                                state.sync_web_session_state(
                                    active.pane_id,
                                    WebSessionState::Mounted,
                                );
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
            }
        } else if matches!(reuse_action, WebViewReuseAction::Navigate)
            && let Some(existing) = self.views.get_mut(&active.pane_id)
        {
            if let Err(error) = existing.webview.load_url(&route_key) {
                state.push_activity(
                    ActivityTone::Error,
                    format!("Failed to navigate child webview: {error}"),
                );
                // load_url failed: don't force-promote to Mounted below —
                // the new navigation never happened and the user should
                // see something other than a confidently-mounted stale
                // page.
                navigate_load_url_failed = true;
            } else {
                existing.route = active.route.clone();
                existing.route_key = route_key.clone();
            }
        }

        // navigate_to_url always resets the focused pane to
        // `WebSessionState::Launching`. The Rebuild branch above
        // immediately transitions back to `Mounted` for routes that
        // don't need a guest ready signal — but Navigate (URL change in
        // an already-built ExternalUrl WebView) and Keep (same URL
        // re-typed into the omnibar) skipped that transition, leaving
        // the launching overlay (`render_generic_loading_overlay` →
        // "Starting app…") permanently on top of the live WebView.
        // Mirror the Rebuild logic for the reuse paths so omnibar
        // navigation between web pages clears the overlay (#143).
        if !navigate_load_url_failed
            && should_force_mounted_after_reuse(
                reuse_action,
                self.views.contains_key(&active.pane_id),
                &active.route,
            )
        {
            state.sync_web_session_state(active.pane_id, WebSessionState::Mounted);
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
                // Priority 1: pending share terminal (spawned by capsule executor).
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
            if self.automation.is_page_loaded(active.pane_id)
                && let Some(view) = self.views.get_mut(&active.pane_id)
            {
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
        use AutomationCommand::*;
        use std::time::Instant;

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
                    let mut panes: Vec<serde_json::Value> = self
                        .views
                        .keys()
                        .map(|id| serde_json::json!({ "pane_id": id }))
                        .collect();
                    if self.dock_is_open {
                        panes.push(serde_json::json!({
                            "pane_id": DOCK_AUTOMATION_PANE_ID,
                            "kind": "dock",
                            "url": "ato://dock",
                        }));
                    }
                    req.send(Ok(serde_json::json!({ "panes": panes })));
                    continue;
                }
                FocusPane { .. } => {
                    req.send(Ok(serde_json::json!({ "ok": true })));
                    continue;
                }
                ClosePane { pane_id } => {
                    state.pending_close_panes.push_back(*pane_id);
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
                RestartActiveSession => {
                    // In standard (non-Focus) WebView mode, restart is not yet
                    // implemented. Return a typed error so callers can distinguish
                    // this from a generic failure and fall back to stop + reopen.
                    req.send(Err(
                        "restart_active_session is only supported in Focus mode".to_string(),
                    ));
                    continue;
                }
                HostDispatchAction { action, url } => {
                    if let Some(response) =
                        crate::app::navigate_to_url_mcp_preflight(action, url.as_deref())
                    {
                        req.send(Ok(response));
                        continue;
                    }
                    // Push onto the queue; `DesktopShell::render` drains
                    // it on the next paint and invokes the matching
                    // window::open_* helper. This bypasses macOS
                    // Accessibility permission entirely.
                    state.pending_host_actions.push_back(action.clone());
                    // The render loop is event-driven — without an
                    // explicit notify the shell might not repaint until
                    // some other event fires (user input, timer). Kick
                    // a refresh so the queued action gets drained
                    // promptly.
                    notify_window(self.async_app.clone(), self.window_handle);
                    req.send(Ok(serde_json::json!({
                        "ok": true,
                        "queued_action": action,
                    })));
                    continue;
                }
                ListSessions => {
                    let entries = self
                        .async_app
                        .update(|cx| cx.global::<SessionRegistry>().view_entries());
                    match serde_json::to_value(&entries) {
                        Ok(sessions) => {
                            req.send(Ok(serde_json::json!({ "sessions": sessions })));
                        }
                        Err(e) => {
                            req.send(Err(format!("serialize sessions failed: {e}")));
                        }
                    }
                    continue;
                }
                AuthStatus => {
                    let status = match crate::orchestrator::resolve_ato_binary() {
                        Ok(ato_bin) => {
                            match Command::new(&ato_bin)
                                .no_console_window()
                                .arg("desktop-auth-handoff")
                                .output()
                            {
                                Ok(output) if output.status.success() => {
                                    auth_status_from_handoff_stdout(&output.stdout)
                                }
                                _ => signed_out_auth_status(),
                            }
                        }
                        Err(_) => signed_out_auth_status(),
                    };
                    match serde_json::to_value(status) {
                        Ok(json) => req.send(Ok(json)),
                        Err(_) => req.send(Ok(
                            serde_json::to_value(signed_out_auth_status()).unwrap_or_default()
                        )),
                    };
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

            // Dock pane requests are routed to `dispatch_dock_automation_requests`
            // which is called from DesktopShell::render() after this method returns.
            if pane_id == DOCK_AUTOMATION_PANE_ID {
                requeue.push(req);
                continue;
            }

            // Navigation commands don't need a loaded page; all JS commands do.
            let needs_loaded = !matches!(
                &req.command,
                Navigate { .. } | NavigateBack | NavigateForward | Screenshot
            );

            if needs_loaded && !self.automation.is_page_loaded(pane_id) {
                if req.wait_deadline.is_some_and(|d| Instant::now() < d) {
                    requeue.push(req);
                } else {
                    req.send(Err(page_not_loaded_message(state, pane_id)));
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

    /// Dispatch automation requests targeting the dock pane (`DOCK_AUTOMATION_PANE_ID`).
    ///
    /// Called from `DesktopShell::render()` after `sync_from_state` so we have
    /// access to the dock `WebView` via the GPUI entity.  `dock_view` is `None`
    /// when the dock window is not currently open.
    pub fn dispatch_dock_automation_requests(
        &mut self,
        state: &mut AppState,
        dock_view: Option<&WebView>,
    ) {
        use AutomationCommand::*;

        // Separate dock-targeted requests from everything else.
        let all = self.automation.drain_requests();
        if all.is_empty() {
            return;
        }

        let mut non_dock: Vec<PendingAutomationRequest> = Vec::new();

        for req in all {
            if req.pane_id != DOCK_AUTOMATION_PANE_ID {
                non_dock.push(req);
                continue;
            }

            if req.is_expired() {
                req.send(Err("automation command timed out".into()));
                continue;
            }

            let needs_loaded = !matches!(
                &req.command,
                Navigate { .. } | NavigateBack | NavigateForward | Screenshot
            );

            if needs_loaded && !self.automation.is_page_loaded(DOCK_AUTOMATION_PANE_ID) {
                // Give the dock HTML page time to finish loading before failing.
                // Re-enqueue; the 50 ms polling loop will retry on the next frame.
                if !req.is_expired() {
                    non_dock.push(req); // will be requeued below
                } else {
                    req.send(Err("dock page not loaded".into()));
                }
                continue;
            }

            match dock_view {
                Some(webview) => {
                    dispatch_automation_command(
                        req,
                        webview,
                        DOCK_AUTOMATION_PANE_ID,
                        &self.automation,
                    );
                }
                None => {
                    req.send(Err("dock is not open".into()));
                }
            }
        }

        // Return non-dock requests (and any dock retries mixed in) to the queue.
        self.automation.requeue(non_dock);
        let _ = state; // may be used for diagnostics in future
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
                    if let Some(view) = self.views.get_mut(pane_id)
                        && let Ok(parsed) = url.parse()
                    {
                        view.route = GuestRoute::ExternalUrl(parsed);
                        view.route_key = url.clone();
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
                        if let Some(view) = self.views.get_mut(pid)
                            && let Err(e) = view.webview.evaluate_script(&script)
                        {
                            warn!(pane_id = pid, error = %e, "failed to deliver GetSecrets response");
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

                    // Index the live session by handle so `stop_active_session`
                    // can find it even if a subsequent same-pane Rebuild evicts
                    // the WebView whose `launched_session` field was the only
                    // anchor (ato-run/ato#122). The map is cleared on a
                    // successful stop; if a second successful launch fires
                    // for the same handle (e.g. capsule restart), the entry
                    // is overwritten with the newer session — old entries
                    // never accumulate.
                    self.handle_to_session
                        .insert(completed.handle.clone(), session.clone());

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

                        // Check for a pending share terminal (piped PTY from capsule executor)
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
                                    error!(
                                        pane_id,
                                        "terminal_stream session has no log_path and no pending share terminal"
                                    );
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
                                install_profile_key: None,
                                auth_flow: false,
                                bounds: active.bounds,
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
                                    // Anchor handle→session for the
                                    // stop_active_session fallback
                                    // (#122). Doing it here (in
                                    // addition to the
                                    // drain_pending_launches success
                                    // arm) covers every successful
                                    // WebView build — including paths
                                    // where the WebView is built but
                                    // the drain success arm took a
                                    // different branch.
                                    self.handle_to_session
                                        .insert(session.handle.clone(), session.clone());
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
                    community_toml_id,
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
                    // #117 — route into the unified resolution request.
                    // The legacy `pending_config` slot is left untouched
                    // (callers that still observe it stay correct); the
                    // unified resolution modal takes precedence in the
                    // ui/modals render gate when both are populated.
                    state.merge_config_into_resolution(PendingConfigRequest {
                        handle,
                        target,
                        fields,
                        original_secrets,
                        community_toml_id,
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
                    community_toml_id,
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
                        // #117 — route into the unified resolution
                        // request, same as the E103 arm above. The
                        // retry-budget gate stays per-target so a
                        // post-Approve loop still surfaces a fatal
                        // toast rather than re-opening the modal.
                        state.merge_consent_into_resolution(PendingConsentRequest {
                            handle,
                            scoped_id,
                            version,
                            target_label,
                            policy_segment_hash,
                            provisioning_policy_hash,
                            summary,
                            original_secrets,
                            community_toml_id,
                        });
                    }
                }
                Err(LaunchError::PreflightAggregate {
                    handle,
                    requirements,
                    original_secrets,
                    community_toml_id,
                }) => {
                    // #117 — eager preflight returned the full set of
                    // pending requirements before any provisioning ran.
                    // Convert each envelope into the existing per-error
                    // PendingConfig / PendingConsent shapes and route
                    // through `merge_*_into_resolution`, so the
                    // unified resolution modal sees one populated
                    // request with everything visible at once instead
                    // of accumulating across N launch retries.
                    use capsule::interactive_resolution::InteractiveResolutionKind;
                    info!(
                        pane_id,
                        handle = %handle,
                        requirement_count = requirements.len(),
                        "preflight surfaced aggregate requirements; populating unified modal"
                    );
                    for envelope in requirements {
                        match envelope.kind {
                            InteractiveResolutionKind::SecretsRequired { target, schema } => {
                                state.merge_config_into_resolution(PendingConfigRequest {
                                    handle: handle.clone(),
                                    target,
                                    fields: schema,
                                    original_secrets: original_secrets.clone(),
                                    community_toml_id: community_toml_id.clone(),
                                });
                            }
                            InteractiveResolutionKind::ConsentRequired {
                                scoped_id,
                                version,
                                target_label,
                                policy_segment_hash,
                                provisioning_policy_hash,
                                summary,
                            } => {
                                state.merge_consent_into_resolution(PendingConsentRequest {
                                    handle: handle.clone(),
                                    scoped_id,
                                    version,
                                    target_label,
                                    policy_segment_hash,
                                    provisioning_policy_hash,
                                    summary,
                                    original_secrets: original_secrets.clone(),
                                    community_toml_id: community_toml_id.clone(),
                                });
                            }
                            // #404: the unified modal does not yet render a
                            // folder picker for state-binding requirements. The
                            // backend resolve seam
                            // (`capsule::installed_state::resolve_state_binding_from_path`)
                            // exists; wiring the GPUI picker that calls it is a
                            // follow-up PR. Log so the requirement is observable.
                            InteractiveResolutionKind::StateBindingRequired {
                                state_key,
                                label,
                            } => {
                                info!(
                                    pane_id,
                                    handle = %handle,
                                    %state_key,
                                    %label,
                                    "preflight surfaced a state-binding requirement; folder picker is a follow-up"
                                );
                            }
                        }
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
        community_toml_id: Option<&str>,
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
        if let Some(active) = state.active_web_pane()
            && active.session == WebSessionState::LaunchFailed
        {
            return;
        }

        // Second gate: if a config modal is open for THIS handle, the
        // user is mid-edit. Re-spawning would just re-trip the same
        // E103 in the background and rebuild the modal under their
        // cursor. The Save handler clears `pending_config`, which
        // collapses this guard on the next render and re-arms the
        // launch with the freshly stored secrets.
        if let Some(pending) = &state.pending_config
            && pending.handle == handle
        {
            return;
        }

        // Same gate, mirror for E302 consent: if the consent modal is
        // open for THIS handle, the user is mid-decision. The Approve
        // handler clears `pending_consent` and marks the retry budget;
        // both branches collapse this guard on the next render.
        if let Some(pending) = &state.pending_consent
            && pending.handle == handle
        {
            return;
        }

        // #117 — same gate for the unified resolution modal. Without
        // this, every render frame after `LaunchError::PreflightAggregate`
        // would re-spawn the launch worker (the legacy single-slot
        // gates above don't trip because `pending_resolution` is the
        // populated field, not `pending_config` / `pending_consent`).
        // The result before this gate was an info!/error! pair every
        // few tens of ms while the user was filling in the modal —
        // log spam plus wasted preflight subprocess spawns. The
        // Submit/Cancel handlers clear `pending_resolution`, which
        // collapses this guard on the next render so the freshly-
        // resolved retry can fire exactly once.
        if let Some(pending) = &state.pending_resolution
            && pending.handle == handle
        {
            return;
        }

        info!(pane_id, handle, "queuing guest session launch");
        let (sender, receiver) = channel();
        let route_key = route_key.to_string();
        let handle = handle.to_string();
        // Build the typed launch input now so the background thread has all context.
        let launch_input = if let Some(cid) = community_toml_id {
            resolve_and_start_guest_with_input_fn::make_community_input(&handle, cid)
        } else {
            resolve_and_start_guest_with_input_fn::make_handle_input(&handle)
        };
        let background_executor = self.async_app.background_executor().clone();
        let foreground_executor = self.async_app.foreground_executor().clone();
        let async_app = self.async_app.clone();
        let window_handle = self.window_handle;

        // Collect secrets granted for this capsule handle before moving into the async block.
        let secrets: Vec<SecretEntry> = state.secret_store.secrets_for_capsule(&handle);
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
        state.sync_web_session_state(pane_id, WebSessionState::Resolving);
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
                session: resolve_and_start_guest_with_input(
                    &launch_input,
                    &secrets,
                    &plain_configs,
                    None,
                )
                .inspect_err(|err| {
                    // #117 — interactive-resolution errors
                    // (preflight aggregate, missing config,
                    // missing consent) are expected states, not
                    // failures. Log them at info so the user-side
                    // log stream stays readable while the modal
                    // is open; reserve `error!` for genuinely
                    // unexpected breakage. The orchestrator
                    // upstream already logs at warn when it
                    // recognises these cases, so info here keeps
                    // both sides at-or-below-warn.
                    match err {
                        LaunchError::MissingConfig { .. }
                        | LaunchError::MissingConsent { .. }
                        | LaunchError::PreflightAggregate { .. } => {
                            info!(
                                handle = %handle,
                                error = %err,
                                "guest session launch awaiting user input"
                            );
                        }
                        LaunchError::Other(_) => {
                            error!(
                                handle = %handle,
                                error = %err,
                                "guest session launch failed"
                            );
                        }
                    }
                }),
            };
            if result.session.is_ok() {
                info!(handle = %handle, route_key = %result.route_key, "guest session launched");
            }

            if let Err(error) = sender.send(result)
                && let Ok(session) = error.0.session
            {
                let _ = stop_guest_session(&session.session_id);
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

        let pane_binding = Arc::new(AtomicUsize::new(pane.pane_id));
        let mut ingress_registration: Option<crate::netd::IngressRegistration> = None;

        // Build the store identity for this pane.  Fields that are not yet
        // plumbed into ActiveWebPane (install_profile_key, publisher_identity)
        // are always None for now; the classifier will fall back to
        // CapsuleEphemeral until the #350 activation follow-up populates them.
        let pane_identity = WebViewStoreIdentity {
            route: pane.route.clone(),
            trust_state: pane.trust_state.clone(),
            install_profile_key: pane.install_profile_key.clone(),
            publisher_identity: None, // not yet plumbed; will be added when store record carries publisher
            source_identity: pane.canonical_handle.clone(),
            snapshot_label: pane.snapshot_label.clone(),
        };

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
            GuestRoute::CapsuleHandle { .. } | GuestRoute::LocalManifest(_) => {
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
                    let local_url = session.local_url.clone().ok_or_else(|| {
                        anyhow::anyhow!("WebUrl session has no local_url: {}", session.session_id)
                    })?;
                    let url = {
                        use crate::netd::{IngressRegistration, IngressRegistrationKind};

                        // CapsuleEphemeral routes use a session-unique ephemeral port
                        // (not persisted in stable_origin_ports.json). System routes
                        // use stable ingress for consistent origin across restarts.
                        let route_store_class = store_class_for_identity(&pane_identity);
                        if route_store_class == WebViewStoreClass::CapsuleEphemeral {
                            // Ephemeral key is scoped to this session so it never collides
                            // with stable keys or another capsule's ephemeral key.
                            let ephemeral_key = format!("ephemeral:{}", session.session_id);
                            match crate::netd::register_ephemeral_ingress(
                                &ephemeral_key,
                                &local_url,
                            ) {
                                Ok(port) => {
                                    let ingress_url = format!("http://127.0.0.1:{port}/");
                                    info!(
                                        key = %ephemeral_key,
                                        port = port,
                                        local_url = %local_url,
                                        "registered ato-netd ephemeral ingress route"
                                    );
                                    ingress_registration = Some(IngressRegistration {
                                        key: ephemeral_key,
                                        kind: IngressRegistrationKind::Ephemeral,
                                    });
                                    ingress_url
                                }
                                Err(err) => {
                                    warn!(
                                        route = %pane.route,
                                        session_id = %session.session_id,
                                        error = %err,
                                        "failed to register ephemeral ingress; \
                                         falling back to direct local_url"
                                    );
                                    local_url
                                }
                            }
                        } else if let WebViewStoreClass::CapsuleProfile { ref uuid } =
                            route_store_class
                        {
                            // Stable profile-aligned ingress: the key is derived from the
                            // profile UUID so that storage partition and origin are 1:1.
                            // Two installed profiles of the same handle get different ports.
                            let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
                            let profile_key = format!("profile:{hex}");
                            match crate::netd::register_stable_ingress(&profile_key, &local_url) {
                                Ok(port) => {
                                    let ingress_url = format!("http://127.0.0.1:{port}/");
                                    info!(
                                        key = %profile_key,
                                        port = port,
                                        local_url = %local_url,
                                        "registered ato-netd stable profile ingress route"
                                    );
                                    ingress_registration = Some(IngressRegistration {
                                        key: profile_key,
                                        kind: IngressRegistrationKind::Stable,
                                    });
                                    ingress_url
                                }
                                Err(err) => {
                                    warn!(
                                        route = %pane.route,
                                        session_id = %session.session_id,
                                        error = %err,
                                        "failed to register profile ingress; \
                                         falling back to direct local_url"
                                    );
                                    local_url
                                }
                            }
                        } else if let Some(key) = crate::netd::logical_key_for_route(&pane.route) {
                            match crate::netd::register_stable_ingress(&key, &local_url) {
                                Ok(port) => {
                                    let ingress_url = format!("http://127.0.0.1:{port}/");
                                    info!(
                                        key = %key,
                                        port = port,
                                        local_url = %local_url,
                                        "registered ato-netd stable ingress route"
                                    );
                                    ingress_registration = Some(IngressRegistration {
                                        key,
                                        kind: IngressRegistrationKind::Stable,
                                    });
                                    ingress_url
                                }
                                Err(err) => {
                                    warn!(
                                        route = %pane.route,
                                        session_id = %session.session_id,
                                        error = %err,
                                        "failed to register ato-netd ingress route; \
                                         falling back to direct local_url"
                                    );
                                    local_url
                                }
                            }
                        } else {
                            local_url
                        }
                    };
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

        // Rollback guard: if any step below returns Err after an ingress
        // route has been registered in ato-netd, call
        // `deregister_ingress_if_registered` before propagating the error so
        // the daemon does not hold a stale ephemeral (or stable) route
        // indefinitely.  The guard is disarmed on the success path by taking
        // the value out via `reg_guard.0.take()` just before `Ok(...)`.
        struct IngressRollback(Option<crate::netd::IngressRegistration>);
        impl Drop for IngressRollback {
            fn drop(&mut self) {
                deregister_ingress_if_registered(&self.0);
            }
        }
        let mut reg_guard = IngressRollback(ingress_registration);

        let webview_bounds = content_bounds(pane.bounds);

        // Determine the store class for this pane using the full identity
        // (trust_state + install_profile_key + source/publisher identity).
        // CapsuleProfile is assigned only when trust and install_profile_key
        // are both present; until #350 activation plumbs those fields into
        // ActiveWebPane, capsule routes always produce CapsuleEphemeral.
        let store_class = store_class_for_identity(&pane_identity);

        // System routes (ExternalUrl, Terminal) share the persistent
        // WebViewManager::web_context so ato.run sign-in cookies survive
        // tab close/reopen and cross-pane (dock ↔ store ↔ settings).
        //
        // CapsuleEphemeral routes (all capsule routes for now) get a
        // per-session non-persistent store via `with_incognito(true)` →
        // WKWebsiteDataStore::nonPersistentDataStore().  Each call creates
        // an independent in-memory store, so capsules are fully isolated
        // from each other and no state survives session end.
        //
        // CapsuleProfile (reserved — not yet assigned by store_class_for_route)
        // will use `with_data_store_identifier([u8; 16])` on macOS 14+ for
        // persistent profile-keyed storage once trust/profile identity is
        // available in GuestRoute (#350 follow-up).  On macOS <14 it falls
        // back to incognito rather than silently sharing the default store.
        let mut builder = WebViewBuilder::new_with_web_context(&mut self.web_context)
            .with_bounds(bounds_to_rect(webview_bounds));
        builder = apply_webview_store_policy(builder, &store_class);

        // Layer 1: tag every Desktop WebView with a custom UA suffix
        // so ato.run server can render Desktop-specific UX (Launch
        // buttons, no "Download Desktop" promo, etc.) without
        // round-tripping through JS detection.
        builder = builder.with_user_agent(format!(
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
            let pane_binding = pane_binding.clone();
            let async_app = self.async_app.clone();
            let window_handle = self.window_handle;
            builder = builder.with_ipc_handler(move |request| {
                if request.body().contains("__ato_ready__") {
                    bridge.push_shell_event(ShellEvent::SessionReady {
                        pane_id: pane_binding.load(Ordering::Relaxed),
                    });
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
            let pane_binding = pane_binding.clone();
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
                    automation.mark_page_unloaded(pane_binding.load(Ordering::Relaxed));
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
                    let pane_id = pane_binding.load(Ordering::Relaxed);
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
            let pane_binding = pane_binding.clone();
            let async_app = self.async_app.clone();
            let window_handle = self.window_handle;
            builder = builder.with_document_title_changed_handler(move |title| {
                bridge.push_shell_event(ShellEvent::TitleChanged {
                    pane_id: pane_binding.load(Ordering::Relaxed),
                    title,
                });
                notify_window(async_app.clone(), window_handle);
            });
        }

        // For external URLs, intercept navigations that require browser-side auth.
        if let GuestRoute::ExternalUrl(pane_url) = &pane.route {
            let pane_binding = pane_binding.clone();
            let signals = self.pending_auth_handoffs.clone();
            let callback_queue = self.pending_callback_urls.clone();
            let privileged_queue = self.pending_privileged_intents.clone();
            let auth_flow = pane.auth_flow;
            // Origin of the page emitting intents in this pane. The P0 trust
            // boundary keys off this: only trusted Ato origins may drive local
            // behaviour via `ato://` / `capsule://` navigations.
            let origin_host = pane_url.host_str().unwrap_or_default().to_string();
            // Tracks whether this pane has navigated to a different host than its
            // initial (trusted) origin. The initial pane URL is NOT a reliable
            // origin for later intents — a page loaded after a cross-origin
            // top-level navigation must not inherit the pane's trust. Sign-in
            // panes (`auth_flow`) are exempt: they legitimately round-trip
            // through external OAuth origins and back.
            let navigated_off_origin =
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            builder = builder.with_navigation_handler(move |uri: String| {
                // Record cross-origin top-level navigations (reset when the pane
                // returns to its trusted origin).
                if uri.starts_with("http://") || uri.starts_with("https://") {
                    navigated_off_origin.store(
                        crate::intent::is_cross_origin_navigation(&origin_host, &uri),
                        Ordering::Relaxed,
                    );
                }
                // ato:// / capsule:// deep links arrive here when an Ato web
                // surface (the embedded PWA Home, the dock, an in-app OAuth
                // redirect) navigates to a custom scheme. WKWebView cannot load
                // these, so we capture them — but first classify + origin-gate
                // every one (see `crate::intent`). Untrusted origins and
                // unknown/malformed verbs are rejected and logged, never acted
                // on; this is the hard boundary that stops an arbitrary site
                // loaded in a pane from driving local execution.
                if uri.starts_with("ato://") || uri.starts_with("capsule://") {
                    // Use an empty (untrusted) origin once a non-auth pane has
                    // navigated off its trusted origin, so an off-origin page
                    // cannot emit trusted intents even though the pane was
                    // opened at a trusted URL.
                    let effective_origin =
                        if !auth_flow && navigated_off_origin.load(Ordering::Relaxed) {
                            ""
                        } else {
                            origin_host.as_str()
                        };
                    match crate::intent::classify(effective_origin, &uri) {
                        crate::intent::IntentDecision::HostRoute(route) => {
                            if let Ok(mut q) = callback_queue.lock() {
                                q.push(route);
                            }
                        }
                        crate::intent::IntentDecision::Privileged(intent) => {
                            // Privileged intents (run, runner control) are
                            // queued for the next sync_from_state pass (which
                            // has `cx`) and dispatched there. They are NOT
                            // forwarded to the origin-agnostic host-route path.
                            // They are accepted only from a trusted, on-origin
                            // pane (the gate above); see `dispatch_privileged_intent`
                            // for the per-verb handling and its confirmation
                            // model.
                            tracing::info!(
                                origin = %origin_host,
                                ?intent,
                                "intent: privileged intent accepted"
                            );
                            if let Ok(mut q) = privileged_queue.lock() {
                                q.push(intent);
                            }
                        }
                        crate::intent::IntentDecision::Reject(reason) => {
                            tracing::warn!(
                                origin = %origin_host,
                                %uri,
                                %reason,
                                "intent: rejected"
                            );
                        }
                    }
                    return false;
                }
                // Sign-in panes deliberately allow Google / GitHub /
                // Microsoft OAuth redirects to load in-WebView so
                // the resulting auth cookies persist in the shared
                // WebContext. Untrusted capsule WebViews still hand
                // those URLs off to the system browser.
                if auth_policy.classify(&uri) == AuthMode::BrowserRequired && !auth_flow {
                    let pane_id = pane_binding.load(Ordering::Relaxed);
                    if let Ok(mut q) = signals.lock()
                        && !q.iter().any(|s: &AuthHandoffSignal| s.pane_id == pane_id)
                    {
                        q.push(AuthHandoffSignal { pane_id, url: uri });
                    }
                    false // block navigation inside WebView
                } else {
                    true
                }
            });
        }

        builder = builder.with_new_window_req_handler(|_, _| NewWindowResponse::Allow);

        // Auth cookie injection is gated on BOTH the route's store class
        // (must be System) and the URL pattern (ato.run/dock).  The store-
        // class gate is the hard structural guard: even if a CapsuleUrl
        // happens to point at https://ato.run/dock, `allows_ato_auth_cookies`
        // returns false for CapsuleEphemeral routes and cookie injection is
        // skipped.  The URL predicate is a secondary filter that limits
        // injection to the specific system pages that need it.
        let desktop_auth_handoff = if store_class.allows_ato_auth_cookies()
            && should_install_ato_auth_cookies(&url)
        {
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
        let _wv_guard = crate::webview_init_guard::WebviewInitGuard::new();
        #[cfg(target_os = "windows")]
        crate::window::windows::prepare_window_for_webview(window);
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
            pane_binding,
            route: pane.route.clone(),
            route_key: pane.route.to_string(),
            bounds: webview_bounds,
            launched_session,
            store_class: store_class_for_identity(&pane_identity),
            // Disarm the rollback guard: the registration now lives in
            // ManagedWebView and will be deregistered by the explicit stop /
            // eviction paths instead.
            ingress_registration: reg_guard.0.take(),
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
                self.retain_webview(view);
            }
            self.visibility_cache.remove(&pane_id);
        }
        // Opportunistic TTL sweep: any pane close is a natural place
        // to spot expired retentions (cheap O(n) over ≤ cap entries).
        self.sweep_expired_retention(state);
        self.sweep_expired_webview_retention();
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

    fn retain_webview(&mut self, view: ManagedWebView) {
        if !is_webview_retention_eligible_route(&view.route) {
            return;
        }
        let Some(stable_origin_key) = stable_origin_key_for_webview(&view) else {
            return;
        };
        let _ = view.set_visible(false);
        let current_session_id = view.launched_session.as_ref().map(|s| s.session_id.clone());
        let evicted = self.webview_retention.retain(
            RetainedWebView {
                stable_origin_key,
                current_session_id,
                webview: view,
                retained_at: Instant::now(),
            },
            Instant::now(),
        );
        for entry in &evicted {
            deregister_ingress_if_registered(&entry.webview.ingress_registration);
        }
        drop(evicted);
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

    fn sweep_expired_webview_retention(&mut self) {
        let evicted = self.webview_retention.evict_expired(Instant::now());
        for entry in &evicted {
            deregister_ingress_if_registered(&entry.webview.ingress_registration);
        }
        drop(evicted);
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
    /// removes the session record, drops any retention entry, and
    /// **evicts the pane's cached WebView** so the next navigate to
    /// the same capsule URL goes through the Rebuild branch in
    /// `sync_from_state` (which calls `ensure_pending_local_launch`).
    /// Without that eviction, `reuse_action` returns `Keep` for the
    /// unchanged route_key and the launch never re-arms (#112).
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

        // Primary path: the active pane's WebView still holds a
        // `launched_session` reference (the simple case — no consent
        // re-arm in between, no Rebuild that evicted the launch
        // info). Fallback path: walk `handle_to_session` for the
        // active pane's handle. The fallback exists because the
        // post-consent re-arm flow in our Track C receipt
        // (claudedocs/aodd-receipts/track-c-desktop-mcp-...) had
        // `view.launched_session = None` while uvicorn + provider
        // were still running and an `ato app session start` had
        // succeeded; without this fallback the UI stop becomes a
        // silent no-op. See ato-run/ato#122.
        let primary = self
            .views
            .get(&active_pane_id)
            .and_then(|v| v.launched_session.as_ref())
            .cloned();

        let session: GuestLaunchSession = if let Some(s) = primary {
            s
        } else {
            let Some(handle) = self
                .views
                .get(&active_pane_id)
                .and_then(|v| route_handle(&v.route))
                .or_else(|| {
                    state
                        .active_web_pane()
                        .and_then(|active| route_handle(&active.route))
                })
            else {
                return false;
            };
            let Some(s) = self.handle_to_session.get(&handle).cloned() else {
                return false;
            };
            tracing::info!(
                pane_id = active_pane_id,
                handle = %handle,
                session_id = %s.session_id,
                "stop_active_session: using handle_to_session fallback (launched_session was None on the active WebView)"
            );
            s
        };
        let session_id = session.session_id.clone();
        let handle = session.handle.clone();

        // Drop from retention without stop — we're about to do an
        // immediate stop ourselves below, no need for the background
        // graceful-stop path.
        let _ = self.retention.take_by_session_id(&session_id);
        self.stop_log_follower(&session_id);

        let stopped = match stop_guest_session(&session_id) {
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
                // Stop FAILED — leave the cached view in place so the
                // caller can retry stop on the same session. Evicting
                // here would orphan a (still-running) ato-cli session
                // we'd lose track of.
                return false;
            }
        };

        // Stop SUCCEEDED (Ok(true) = process terminated) or the session
        // was already inactive (Ok(false) = workload was gone before
        // the call). Mirror the cleanup that the Rebuild branch in
        // `sync_from_state` runs (`webview.rs:504-509`): drop the
        // cached WebView, fail any pending automation requests for the
        // pane, mark the page unloaded.
        //
        // Sync `WebSessionState::LaunchFailed` (NOT `Closed`) on the
        // pane: with the cached view evicted, the next render would
        // otherwise enter `sync_from_state`'s Rebuild branch and call
        // `ensure_pending_local_launch` immediately, auto-relaunching
        // the capsule the user just stopped. The same gate at
        // `webview.rs:1521-1525` already uses LaunchFailed as the
        // sentinel that blocks re-fire (the comment at the launch-fail
        // site reads "Use LaunchFailed (not Closed) to prevent
        // ensure_pending_local_launch from re-firing"). An explicit
        // `state.navigate_to_url(<url>)` resets the surface to
        // `WebSessionState::Resolving`, which clears LaunchFailed and
        // lets the launch re-arm — that is exactly what the user's
        // omnibar entry / `browser_navigate` MCP call does, and it is
        // what #112 needs from this fix.
        if let Some(previous) = self.views.remove(&active_pane_id) {
            deregister_ingress_if_registered(&previous.ingress_registration);
            self.automation.fail_requests_for_pane(active_pane_id);
            self.automation.mark_page_unloaded(active_pane_id);
            tracing::debug!(
                pane_id = active_pane_id,
                session_id = %session_id,
                "stop_active_session: evicted cached WebView so same-URL re-navigate retriggers Rebuild"
            );
        }
        // Drop the handle→session anchor on a successful stop so the
        // next launch for this handle starts from a clean state.
        // Errored stops keep the entry: the user can retry the same
        // stop without losing the reference.
        if stopped {
            self.handle_to_session.remove(&handle);
            if let Some(retained) = self.webview_retention.take_by_session_id(&session_id) {
                deregister_ingress_if_registered(&retained.webview.ingress_registration);
                drop(retained);
            } else if let Some(retained) = self
                .webview_retention
                .take_by_key(&format!("handle:{handle}"))
            {
                deregister_ingress_if_registered(&retained.webview.ingress_registration);
                drop(retained);
            }
        }
        state.sync_web_session_state(active_pane_id, WebSessionState::LaunchFailed);

        stopped
    }

    /// Drain every retained session and graceful-stop each in a
    /// background thread. Active panes (`self.views`) are
    /// **untouched** — the user has to close those panes first
    /// before the underlying session can be stopped via this path.
    /// Returns the number of sessions queued for stop.
    pub fn stop_all_retained_sessions(&mut self) -> usize {
        for entry in self.webview_retention.drain() {
            deregister_ingress_if_registered(&entry.webview.ingress_registration);
        }
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
        // Deregister the ato-netd ingress route before stopping the session.
        deregister_ingress_if_registered(&webview.ingress_registration);

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

        if let Some(view) = self.views.get_mut(&pane_id)
            && let Err(error) = view.set_visible(visible)
        {
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
        for entry in self.webview_retention.drain() {
            deregister_ingress_if_registered(&entry.webview.ingress_registration);
        }

        // Best-effort shutdown so orphaned guest sessions do not survive process exit.
        for existing in self.views.drain().map(|(_, existing)| existing) {
            deregister_ingress_if_registered(&existing.ingress_registration);
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
        session.install_profile_key.clone(),
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

/// Dispatch a privileged `ato://` intent. The intent has already been
/// origin-validated by `crate::intent` AND confirmed to come from a trusted,
/// on-origin pane (the navigation-handler gate) — an arbitrary or off-origin web
/// page cannot reach here.
///
/// Confirmation model per verb (no separate modal yet):
///   - `runner/register` → `ato runner login` opens a browser device-flow; the
///     operator's sign-in there IS the explicit authorization.
///   - `runner/start` / `runner/stop` → toggle the local `ato runner serve`
///     agent. These are first-party-origin-gated (only the trusted Ato Home can
///     reach them) and recorded in the activity log so they are never silent; an
///     additional explicit confirm dialog is a follow-up.
///   - `run` → acknowledged in the activity log only. The full
///     run-capsule-on-this-device path (store-ref resolution + a native pre-run
///     confirmation) is a follow-up.
fn dispatch_privileged_intent(state: &mut AppState, intent: crate::intent::PrivilegedIntent) {
    use crate::intent::PrivilegedIntent;
    use crate::runner_agent::RunnerStatus;
    use crate::state::ActivityTone;

    match intent {
        PrivilegedIntent::RunnerRegister => {
            let status = crate::runner_agent::register();
            state.push_activity(
                ActivityTone::Info,
                format!(
                    "Connected Runner: registration started — authorize in your browser ({})",
                    status.label()
                ),
            );
        }
        PrivilegedIntent::RunnerStart => {
            let status = crate::runner_agent::start();
            let tone = if matches!(status, RunnerStatus::Error(_)) {
                ActivityTone::Warning
            } else {
                ActivityTone::Info
            };
            state.push_activity(tone, format!("Connected Runner: {}", status.label()));
        }
        PrivilegedIntent::RunnerStop => {
            let status = crate::runner_agent::stop();
            state.push_activity(
                ActivityTone::Info,
                format!("Connected Runner: {}", status.label()),
            );
        }
        PrivilegedIntent::Run { source, run_id } => {
            let detail = run_id.map(|id| format!(" (run {id})")).unwrap_or_default();
            state.push_activity(
                ActivityTone::Info,
                format!(
                    "Run requested for {source}{detail}. Open it from Discover to run on this device."
                ),
            );
        }
    }
}

/// Hosts that serve the embedded `ato-pwa` Home and therefore should receive
/// the desktop's signed-in account cookies (so Home renders signed-in without a
/// second login).
///
/// This is a **fixed allowlist** — the production + staging Home hosts only.
/// Debug builds additionally allow loopback for a local `vite` dev server. An
/// arbitrary `ATO_APP_BASE_URL` host is deliberately NOT injected into in
/// release builds: the Home may be pointed at a custom deploy, but the desktop's
/// account session cookie is only ever handed to known Ato origins.
fn is_pwa_home_host(host: &str) -> bool {
    if host == "app.ato.run" || host == "stg-app.ato.run" {
        return true;
    }
    cfg!(debug_assertions) && crate::intent::is_loopback_host(host)
}

fn should_install_ato_auth_cookies(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let host = parsed.host_str().unwrap_or_default();
    // Legacy: the in-product dock page served from the marketing site.
    if host == "ato.run" && parsed.path().starts_with("/dock") {
        return true;
    }
    // The embedded PWA Home is a SPA served from the root, so any path on a
    // recognized app host qualifies (the store-class gate at the call site
    // still restricts injection to System routes only).
    is_pwa_home_host(host)
}

pub(crate) fn default_api_base_url() -> String {
    std::env::var("ATO_API_BASE_URL")
        .or_else(|_| std::env::var("ATO_STORE_API_URL"))
        .unwrap_or_else(|_| "https://api.ato.run".to_string())
}

pub(crate) fn signed_out_auth_status() -> AuthStatusResponse {
    AuthStatusResponse {
        signed_in: false,
        api_base_url: default_api_base_url(),
        account_hint: None,
    }
}

pub(crate) fn auth_status_from_handoff_stdout(stdout: &[u8]) -> AuthStatusResponse {
    match serde_json::from_slice::<DesktopAuthHandoff>(stdout) {
        Ok(handoff) => AuthStatusResponse {
            signed_in: true,
            api_base_url: handoff.api_base_url,
            account_hint: handoff.publisher_handle,
        },
        Err(_) => signed_out_auth_status(),
    }
}

fn load_desktop_auth_handoff() -> Result<DesktopAuthHandoff> {
    let ato_bin = crate::orchestrator::resolve_ato_binary()
        .context("failed to locate ato binary for desktop auth handoff")?;
    let output = Command::new(&ato_bin)
        .no_console_window()
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
        GuestRoute::Capsule { .. }
        | GuestRoute::CapsuleHandle { .. }
        | GuestRoute::LocalManifest(_) => BuildFlags {
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

/// True when `sync_from_state` reused (Navigate or Keep) an existing
/// child WebView whose route does not need a guest "ready" signal — the
/// caller must then force-transition the pane to `WebSessionState::Mounted`
/// so the launching overlay set by `AppState::navigate_to_url` clears
/// (#143). The Rebuild branch already handles its own transition.
fn should_force_mounted_after_reuse(
    reuse_action: WebViewReuseAction,
    has_existing_view: bool,
    route: &GuestRoute,
) -> bool {
    !matches!(reuse_action, WebViewReuseAction::Rebuild)
        && has_existing_view
        && !route_requires_ready_signal(route)
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
            crate::state::PaneSurface::HostPanel(_)
            | crate::state::PaneSurface::CapsuleStatus(_)
            | crate::state::PaneSurface::Inspector
            | crate::state::PaneSurface::DevConsole
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
        crate::webview_init_guard::wait_until_idle(&bg).await;
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

fn page_not_loaded_message(state: &AppState, pane_id: PaneId) -> String {
    let Some(inspector) = state.capsule_inspector_by_pane_id(pane_id) else {
        return "page not yet loaded".to_string();
    };

    let session_state = inspector.session_state.clone();
    let session_label = web_session_state_label(session_state.clone());
    let pending_suffix = pending_prelaunch_requirement_message(state, &inspector.handle)
        .map(|message| format!("; {message}"))
        .unwrap_or_default();
    match session_state {
        crate::state::WebSessionState::Resolving
        | crate::state::WebSessionState::Materializing
        | crate::state::WebSessionState::Launching => {
            format!(
                "page not yet loaded (session: {session_label}; launch still in progress{pending_suffix})"
            )
        }
        _ => format!("page not yet loaded (session: {session_label}{pending_suffix})"),
    }
}

fn pending_prelaunch_requirement_message(state: &AppState, handle: &str) -> Option<String> {
    let mut labels = Vec::new();

    if let Some(request) = state
        .pending_resolution
        .as_ref()
        .filter(|request| request.handle == handle)
    {
        labels.extend(
            request
                .secrets
                .iter()
                .map(|item| match item.target.as_deref() {
                    Some(target) if !target.is_empty() => format!("config:{target}"),
                    _ => "config".to_string(),
                }),
        );
        labels.extend(request.consents.iter().map(|item| {
            if item.target_label.is_empty() {
                "consent".to_string()
            } else {
                format!("consent:{}", item.target_label)
            }
        }));
    } else {
        if let Some(request) = state
            .pending_config
            .as_ref()
            .filter(|request| request.handle == handle)
        {
            labels.push(match request.target.as_deref() {
                Some(target) if !target.is_empty() => format!("config:{target}"),
                _ => "config".to_string(),
            });
        }
        if let Some(request) = state
            .pending_consent
            .as_ref()
            .filter(|request| request.handle == handle)
        {
            labels.push(if request.target_label.is_empty() {
                "consent".to_string()
            } else {
                format!("consent:{}", request.target_label)
            });
        }
    }

    if labels.is_empty() {
        None
    } else {
        Some(format!(
            "awaiting pre-launch requirements: {}",
            labels.join(", ")
        ))
    }
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

/// Extract the capsule handle from a [`GuestRoute`] for the
/// `stop_active_session` handle-fallback lookup (ato-run/ato#122).
/// Returns `None` for routes that aren't backed by a capsule
/// launch (`Capsule` is a sub-session shape, `ExternalUrl` and
/// `Terminal` are not capsule-launched).
fn route_handle(route: &GuestRoute) -> Option<String> {
    match route {
        GuestRoute::CapsuleHandle { handle, .. } => Some(handle.clone()),
        GuestRoute::CapsuleUrl { handle, .. } => Some(handle.clone()),
        GuestRoute::LocalManifest(local) => Some(local.source_handle.clone()),
        GuestRoute::Capsule { .. } | GuestRoute::ExternalUrl(_) | GuestRoute::Terminal { .. } => {
            None
        }
    }
}

fn stable_origin_key_for_route(route: &GuestRoute) -> Option<String> {
    match route {
        GuestRoute::CapsuleHandle { handle, .. } => Some(format!("handle:{handle}")),
        GuestRoute::CapsuleUrl { handle, .. } => Some(format!("url:{handle}")),
        GuestRoute::LocalManifest(local) => Some(format!("handle:{}", local.source_handle)),
        GuestRoute::Capsule { session, .. } => Some(format!("session:{session}")),
        GuestRoute::ExternalUrl(_) | GuestRoute::Terminal { .. } => None,
    }
}

/// Which WebKit storage store a WebView may use, and the host-level
/// policies that govern it.
///
/// `System` — the shared persistent `WKWebsiteDataStore`.  Used by
/// `ExternalUrl` (ato.run dock, Store, sign-in panes) and `Terminal`.
/// ato.run auth cookies may be injected here.
///
/// `CapsuleEphemeral` — a per-session non-persistent store via
/// `WKWebsiteDataStore.nonPersistentDataStore()` (set through
/// `WebViewBuilder::with_incognito(true)`).  Each WebView receives an
/// independent in-memory store; no state survives session end.
/// Used by transient / preview / local-manifest routes.  ato.run auth
/// cookies must **never** enter this store.
///
/// `CapsuleProfile` — a profile-keyed persistent store derived from the
/// capsule's stable identity (`uuid`).  On macOS 14+ this maps to
/// `WKWebsiteDataStore::dataStoreForIdentifier`, giving the capsule its
/// own persistent cookie jar and localStorage that survive Desktop
/// restarts.  On macOS <14 and on Linux the `uuid` field is unused and
/// the implementation falls back to incognito (non-persistent, isolated)
/// rather than silently sharing the default store.
/// **Reserved for future use**: `store_class_for_route` does not
/// currently assign any route to `CapsuleProfile`.  It will be used
/// once trust/profile identity is plumbed into `GuestRoute` (#350
/// follow-up).  ato.run auth cookies must **never** enter this store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebViewStoreClass {
    System,
    CapsuleEphemeral,
    /// Profile-keyed persistent store.  `uuid` is a 16-byte identifier
    /// derived deterministically from the capsule's stable namespaced
    /// key via BLAKE3 so that the same capsule always maps to the same
    /// WKWebsiteDataStore identifier across Desktop restarts.
    CapsuleProfile {
        uuid: [u8; 16],
    },
}

impl WebViewStoreClass {
    /// Returns `true` only when it is safe to inject ato.run auth cookies
    /// into this store.  Capsule stores must never receive first-party
    /// ato.run session cookies.
    fn allows_ato_auth_cookies(self) -> bool {
        matches!(self, WebViewStoreClass::System)
    }

    /// Returns `true` when the route uses an incognito (non-persistent,
    /// per-session) store rather than a persistent profile store.
    fn uses_incognito_store(self) -> bool {
        matches!(self, WebViewStoreClass::CapsuleEphemeral)
    }
}

/// Full identity context passed to `store_class_for_identity`.
///
/// Extends a bare `GuestRoute` with the trust/profile metadata available
/// in `ActiveWebPane` at WebView build time.  Keeping identity separate
/// from `GuestRoute` avoids adding optional classifier fields to a type
/// that is also used for routing and navigation.
///
/// Fields that are not yet carried by `ActiveWebPane` (e.g.
/// `install_profile_key`, `publisher_identity`) will be `None` in
/// production until the #350 activation follow-up plumbs them.  When all
/// required fields are present, `store_class_for_identity` assigns
/// `CapsuleProfile`; until then every capsule route falls back to
/// `CapsuleEphemeral`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebViewStoreIdentity {
    /// Route variant — determines the System / Capsule base class.
    pub route: GuestRoute,
    /// Trust classification for the capsule.
    ///
    /// Known values: `"local"` (local or trusted build),
    /// `"untrusted"` (remote, not yet verified), `None` (unknown /
    /// still resolving).  Only `"local"` (and a future `"trusted"`)
    /// is eligible for `CapsuleProfile`.
    pub trust_state: Option<String>,
    /// Install profile key — set for capsules that the user has
    /// explicitly installed via `ato install`.  `None` for transient
    /// preview / single-run launches.
    ///
    /// Not yet plumbed into `ActiveWebPane`; always `None` in
    /// production.  Once populated, an installed + trusted capsule
    /// reaches `CapsuleProfile`; everything else stays
    /// `CapsuleEphemeral`.
    pub install_profile_key: Option<String>,
    /// Publisher identity from the capsule manifest or install record
    /// (e.g. `"github.com/someorg"`).  Included in the profile UUID
    /// so that a handle transferred to a different publisher cannot
    /// silently inherit the original store.
    pub publisher_identity: Option<String>,
    /// Source identity — typically the canonical capsule handle (e.g.
    /// `"capsule://github.com/org/app"`).  Primary namespace component
    /// for the persistent store UUID.
    pub source_identity: Option<String>,
    /// Snapshot/revision label.  Intentionally **excluded** from the
    /// profile UUID: app updates should preserve user localStorage /
    /// cookies across revisions.  If publisher or source changes, the
    /// corresponding fields already create a distinct UUID.
    pub snapshot_label: Option<String>,
}

impl WebViewStoreIdentity {
    /// Construct a minimal identity from a route alone (no trust /
    /// install metadata).  Equivalent to a transient / unresolved
    /// launch: capsule routes get `CapsuleEphemeral`, system routes
    /// get `System`.
    pub(crate) fn from_route(route: GuestRoute) -> Self {
        Self {
            route,
            trust_state: None,
            install_profile_key: None,
            publisher_identity: None,
            source_identity: None,
            snapshot_label: None,
        }
    }
}

/// Returns `true` when `trust_state` represents a trusted capsule
/// that is eligible for persistent profile storage.
///
/// Current trusted values: `"local"`.
/// Future: a `"trusted"` value for registry-verified installed capsules.
fn is_trusted_trust_state(trust_state: Option<&str>) -> bool {
    matches!(trust_state, Some("local"))
}

/// Maps a `WebViewStoreIdentity` to a `WebViewStoreClass`.
///
/// This is the single authoritative classifier.  `store_class_for_route`
/// delegates here via a minimal identity (no trust/install metadata), so
/// all existing call sites continue to work without change.
///
/// Assignment rules:
/// - `ExternalUrl` / `Terminal` → `System` regardless of identity.
/// - Capsule route with trusted `trust_state` **and** a non-`None`
///   `install_profile_key` → `CapsuleProfile { uuid }`.
/// - Everything else (untrusted, unknown, no `install_profile_key`,
///   `LocalManifest`, transient `Capsule`) → `CapsuleEphemeral`.
///
/// `CapsuleProfile` is safe to activate only when both conditions hold
/// because `trust_state` without an install key means the capsule is a
/// transient run (not an owned install), and an install key without a
/// trusted state means it arrived from an unknown / untrusted source.
fn store_class_for_identity(identity: &WebViewStoreIdentity) -> WebViewStoreClass {
    // System routes always use the shared persistent context.
    match &identity.route {
        GuestRoute::ExternalUrl(_) | GuestRoute::Terminal { .. } => {
            return WebViewStoreClass::System;
        }
        _ => {}
    }

    // Grant CapsuleProfile only when the capsule is both trusted AND
    // has an explicit install profile key.  Either condition missing
    // means this is a transient, preview, or untrusted run.
    if let (true, Some(install_profile_key)) = (
        is_trusted_trust_state(identity.trust_state.as_deref()),
        identity.install_profile_key.as_deref(),
    ) {
        let uuid = profile_store_uuid_from_identity(
            install_profile_key,
            identity.publisher_identity.as_deref(),
            identity.source_identity.as_deref(),
        );
        return WebViewStoreClass::CapsuleProfile { uuid };
    }

    WebViewStoreClass::CapsuleEphemeral
}

/// Derive a stable 16-byte store UUID from the full install identity.
///
/// Key components (revision/snapshot intentionally excluded — see
/// `WebViewStoreIdentity::snapshot_label` doc):
/// - `install_profile_key` — stable user-install identity
/// - `source_identity` — canonical capsule handle
/// - `publisher_identity` — publisher handle (or empty string)
///
/// Different `install_profile_key` values → different UUIDs (user
/// install isolation).  Different publisher → different UUID (prevents
/// an org-transfer from inheriting the old store).
fn profile_store_uuid_from_identity(
    install_profile_key: &str,
    publisher_identity: Option<&str>,
    source_identity: Option<&str>,
) -> [u8; 16] {
    let key = format!(
        "profile:ipk={}:src={}:pub={}",
        install_profile_key,
        source_identity.unwrap_or(""),
        publisher_identity.unwrap_or(""),
    );
    profile_store_uuid(&key)
}

/// Derive a 16-byte profile identifier from a namespaced capsule key.
///
/// The key must already be namespaced (e.g. `"handle:{handle}"` or
/// `"url:{handle}"` or `"profile:ipk=..."`) so that different route
/// types for the same handle string cannot collide.  BLAKE3 provides a
/// stable, collision-resistant mapping that is consistent across Rust
/// version upgrades (unlike `DefaultHasher`).
fn profile_store_uuid(namespaced_key: &str) -> [u8; 16] {
    let hash = blake3::hash(namespaced_key.as_bytes());
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&hash.as_bytes()[..16]);
    uuid
}

/// Detect the macOS major version at runtime.
///
/// Used to guard `with_data_store_identifier` which requires macOS 14+.
/// Returns 0 on non-macOS targets (the cfg guard prevents this being
/// called there, but the signature must compile on all platforms).
#[cfg(target_os = "macos")]
fn macos_major_version() -> i64 {
    use objc2_foundation::NSProcessInfo;
    // SAFETY: processInfo() is safe to call from any thread on macOS.
    // The return type is Retained<NSProcessInfo>, which is Send.
    let info = NSProcessInfo::processInfo();
    info.operatingSystemVersion().majorVersion as i64
}

/// Apply the WebKit data-store policy for a given store class to a
/// `WebViewBuilder`.
///
/// Centralises all data-store selection logic so `build_webview` stays
/// readable and the policy is testable in isolation.
///
/// Policy:
/// - `System`: no override — WebView uses `self.web_context` (shared
///   persistent `WKWebsiteDataStore.defaultDataStore()`).
/// - `CapsuleEphemeral`: `with_incognito(true)` →
///   `WKWebsiteDataStore.nonPersistentDataStore()`.  Each WebView
///   receives an independent in-memory store.
/// - `CapsuleProfile { uuid }`:
///   - macOS 14+: `with_data_store_identifier(uuid)` →
///     `WKWebsiteDataStore.dataStoreForIdentifier(NSUUID)` (persistent,
///     profile-keyed).
///   - macOS <14 or non-macOS: `with_incognito(true)` (non-persistent,
///     isolated — prevents sharing `defaultDataStore` even though
///     storage cannot be persisted on these platforms).
fn apply_webview_store_policy<'a>(
    builder: WebViewBuilder<'a>,
    store_class: &WebViewStoreClass,
) -> WebViewBuilder<'a> {
    match store_class {
        WebViewStoreClass::System => builder,
        WebViewStoreClass::CapsuleEphemeral => builder.with_incognito(true),
        WebViewStoreClass::CapsuleProfile { uuid } => {
            #[cfg(target_os = "macos")]
            {
                // dataStoreForIdentifier is macOS 14+ / iOS 17+.
                // On macOS <14, passing a data_store_identifier silently falls
                // back to defaultDataStore — which would break isolation by
                // sharing the default persistent store.  Guard with a runtime
                // version check and fall back to incognito instead.
                if macos_major_version() >= 14 {
                    return builder.with_data_store_identifier(*uuid);
                }
            }
            let _ = uuid;
            builder.with_incognito(true)
        }
    }
}

/// Maps a `GuestRoute` to its `WebViewStoreClass`.
///
/// Thin wrapper around `store_class_for_identity` that creates a minimal
/// identity (no trust/profile metadata).  With no `install_profile_key`
/// or `trust_state`, capsule routes always fall back to
/// `CapsuleEphemeral`, which is the correct safe default.
///
/// Call sites that have additional metadata available from `ActiveWebPane`
/// should use `store_class_for_identity` directly to enable
/// `CapsuleProfile` assignment once the #350 activation follow-up plumbs
/// `install_profile_key` into `ActiveWebPane`.
fn store_class_for_route(route: &GuestRoute) -> WebViewStoreClass {
    store_class_for_identity(&WebViewStoreIdentity::from_route(route.clone()))
}

fn is_webview_retention_eligible_route(route: &GuestRoute) -> bool {
    // System routes (ExternalUrl, Terminal) are not retained because they
    // represent external sites whose state lives in the shared persistent
    // WebContext — closing and reopening them is cheap.  All capsule
    // routes (both CapsuleEphemeral and CapsuleProfile) are eligible for
    // retention so a warm WebView can be reused on reopen.
    !matches!(store_class_for_route(route), WebViewStoreClass::System)
}

/// Deregister the ato-netd ingress route recorded on a `ManagedWebView`,
/// dispatching to the correct stable or ephemeral deregister function.
/// No-op when `registration` is `None`.
fn deregister_ingress_if_registered(registration: &Option<crate::netd::IngressRegistration>) {
    let Some(reg) = registration else { return };
    match reg.kind {
        crate::netd::IngressRegistrationKind::Stable => {
            crate::netd::deregister_stable_ingress(&reg.key);
        }
        crate::netd::IngressRegistrationKind::Ephemeral => {
            crate::netd::deregister_ephemeral_ingress(&reg.key);
        }
    }
}

/// Compute the retention-table lookup key for a route.
///
/// The key must align with the storage partition so a retained WebView is
/// only reused when its store class and identity match the new request.
///
/// - `System` → `None` (system routes are not retained).
/// - `CapsuleEphemeral` → handle-based key (warm reuse for the same
///   capsule handle within the retention TTL is correct; the incognito
///   store is per-WebView instance, so reusing the object also reuses
///   its isolated in-memory store).
/// - `CapsuleProfile { uuid }` → `"profile:{uuid_hex}"` so that two
///   routes with different profile identities never share a retained
///   WebView.  Currently unreachable from the production classifier
///   (reserved for #350 follow-up).
fn webview_retention_key_for_route(route: &GuestRoute) -> Option<String> {
    match store_class_for_route(route) {
        WebViewStoreClass::System => None,
        WebViewStoreClass::CapsuleEphemeral => stable_origin_key_for_route(route),
        WebViewStoreClass::CapsuleProfile { uuid } => {
            let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
            Some(format!("profile:{hex}"))
        }
    }
}

/// Compute the retention key for a route with full identity context.
///
/// For `CapsuleProfile` routes, the key is `"profile:{uuid_hex}"` derived from
/// the install_profile_key/source/publisher — NOT the handle string — so two
/// installed profiles of the same handle get distinct retention entries.
/// For `CapsuleEphemeral`, falls back to route-only key (handle-based).
fn webview_retention_key_for_identity(identity: &WebViewStoreIdentity) -> Option<String> {
    match store_class_for_identity(identity) {
        WebViewStoreClass::System => None,
        WebViewStoreClass::CapsuleEphemeral => stable_origin_key_for_route(&identity.route),
        WebViewStoreClass::CapsuleProfile { uuid } => {
            let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
            Some(format!("profile:{hex}"))
        }
    }
}

fn stable_origin_key_for_webview(view: &ManagedWebView) -> Option<String> {
    match &view.store_class {
        WebViewStoreClass::System => None,
        WebViewStoreClass::CapsuleEphemeral => {
            // Prefer session.handle so the retained WebView can be found
            // on pane re-open within the TTL.
            if let Some(session) = view.launched_session.as_ref() {
                Some(format!("handle:{}", session.handle))
            } else {
                stable_origin_key_for_route(&view.route)
            }
        }
        WebViewStoreClass::CapsuleProfile { uuid } => {
            // Retention key must align with the storage partition UUID so
            // that routes with different profile identities never reuse
            // each other's WebView objects.
            let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
            Some(format!("profile:{hex}"))
        }
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

// ── Apply helpers (shared between UI handlers and MCP tool dispatch) ─────────

/// Approve the pending ExecutionPlan consent for `handle`: invoke
/// `ato internal consent approve-execution-plan` (the CLI writer
/// owns the JSONL append), mark the per-handle retry-once budget as
/// consumed, and clear the matching legacy `pending_consent` or
/// unified `pending_resolution.consents` entry so
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
    if let Some(request) = state
        .pending_consent
        .as_ref()
        .filter(|r| r.handle == handle)
        .cloned()
    {
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
        return Ok(());
    }

    if apply_pending_resolution_consents(state, handle, |consent| {
        crate::orchestrator::approve_execution_plan_consent(
            &consent.scoped_id,
            &consent.version,
            &consent.target_label,
            &consent.policy_segment_hash,
            &consent.provisioning_policy_hash,
        )
        .map_err(|err| format!("failed to record consent: {err:#}"))
    })? {
        return Ok(());
    }

    Err(format!(
        "no pending ExecutionPlan consent matches handle '{handle}' \
         (the modal is either closed or pinned to a different handle)"
    ))
}

fn apply_pending_resolution_consents<F>(
    state: &mut AppState,
    handle: &str,
    mut approve: F,
) -> Result<bool, String>
where
    F: FnMut(&crate::state::PendingConsentItem) -> Result<(), String>,
{
    let consents = state
        .pending_resolution
        .as_ref()
        .filter(|request| request.handle == handle)
        .map(|request| request.consents.clone())
        .unwrap_or_default();

    if consents.is_empty() {
        return Ok(false);
    }

    for consent in &consents {
        approve(consent)?;
        state.mark_consent_retry_consumed(handle, &consent.target_label);
    }

    if let Some(request) = state
        .pending_resolution
        .as_mut()
        .filter(|request| request.handle == handle)
    {
        request.consents.clear();
        if request.is_empty() {
            state.clear_pending_resolution();
        }
    }

    Ok(true)
}

fn clear_matching_pending_resolution_secrets(
    state: &mut AppState,
    handle: &str,
    resolved_secret_keys: &std::collections::BTreeSet<String>,
) {
    let Some(request) = state
        .pending_resolution
        .as_mut()
        .filter(|request| request.handle == handle)
    else {
        return;
    };

    request.secrets.retain(|item| {
        item.fields.iter().any(|field| match &field.kind {
            protocol::config::ConfigKind::Secret => !resolved_secret_keys.contains(&field.name),
            _ => true,
        })
    });

    if request.is_empty() {
        state.clear_pending_resolution();
    }
}

// ── Automation command dispatch ───────────────────────────────────────────────

/// Apply a batch of secrets for `handle` and (optionally) clear an open
/// legacy `pending_config` modal or any fully-resolved secret sections in
/// the unified `pending_resolution` request for the same handle so the next
/// render re-arms the launch.
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
        let resolved_secret_keys = applied.iter().cloned().collect();
        let matches = state
            .pending_config
            .as_ref()
            .map(|p| p.handle == handle)
            .unwrap_or(false);
        if matches {
            state.clear_pending_config();
        }
        clear_matching_pending_resolution_secrets(state, handle, &resolved_secret_keys);
    }

    Ok(applied)
}

/// Execute a single automation command against a live WebView.
/// Called from `WebViewManager::dispatch_automation_requests` on the GPUI main thread.
pub(crate) fn dispatch_automation_command(
    req: PendingAutomationRequest,
    webview: &WebView,
    pane_id: usize,
    host: &AutomationHost,
) {
    use AutomationCommand::*;
    use std::time::{Duration, Instant};

    // Helper: call JS via evaluate_script_with_callback and route result to req.
    macro_rules! js_call {
        ($js:expr, $req:expr) => {{
            let tx = $req.clone_tx();
            let js_str: String = $js;
            if let Err(e) = webview.evaluate_script_with_callback(&js_str, move |result| {
                let v = decode_js_callback_value(&result);
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
        ClickAt { x, y } => {
            js_call!(
                format!(
                    "(function(){{var el=document.elementFromPoint({x},{y});if(!el)return JSON.stringify({{ok:false,error:'no element at ({x},{y})'}});el.dispatchEvent(new MouseEvent('click',{{bubbles:true,cancelable:true,clientX:{x},clientY:{y}}}));return JSON.stringify({{ok:true}});}})()"
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
                let found = decode_js_callback_value(&result)
                    .get("found")
                    .and_then(|f| f.as_bool())
                    .unwrap_or(false);

                if found {
                    if let Ok(mut guard) = tx.lock()
                        && let Some(sender) = guard.take()
                    {
                        let _ = sender.send(Ok(serde_json::json!({ "found": true })));
                    }
                } else if deadline.is_some_and(|d| Instant::now() < d) {
                    // Re-queue for retry; the foreground polling task retries within 50ms.
                    let remaining_ms = deadline
                        .map(|d| d.saturating_duration_since(Instant::now()).as_millis() as u64)
                        .unwrap_or(0);
                    if let Ok(mut guard) = tx.lock()
                        && let Some(original_tx) = guard.take()
                    {
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
                } else {
                    if let Ok(mut guard) = tx.lock()
                        && let Some(sender) = guard.take()
                    {
                        let _ = sender.send(Err("wait_for timed out".into()));
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
                        if let Ok(mut guard) = req_tx.lock()
                            && let Some(sender) = guard.take()
                        {
                            let _ = sender.send(Ok(v));
                        }
                    }
                    Ok(Err(e)) => {
                        if let Ok(mut guard) = req_tx.lock()
                            && let Some(sender) = guard.take()
                        {
                            let _ = sender.send(Err(e));
                        }
                    }
                    Err(_) => {
                        if let Ok(mut guard) = req_tx.lock()
                            && let Some(sender) = guard.take()
                        {
                            let _ = sender.send(Err("screenshot timed out".into()));
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
        | ClosePane { .. }
        | OpenUrl { .. }
        | SetCapsuleSecrets { .. }
        | ApproveExecutionPlanConsent { .. }
        | StopActiveSession
        | RestartActiveSession
        | HostDispatchAction { .. }
        | ListSessions
        | AuthStatus => {
            unreachable!()
        }
    }
}

fn decode_js_callback_value(result: &str) -> Value {
    match serde_json::from_str::<Value>(result) {
        Ok(Value::String(inner)) => {
            serde_json::from_str::<Value>(&inner).unwrap_or_else(|_| Value::String(inner))
        }
        Ok(value) => value,
        Err(_) => Value::String(result.to_string()),
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
            install_profile_key: None,
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
    fn pwa_home_hosts_install_ato_auth_cookies() {
        // The embedded PWA Home is a root-served SPA → any path qualifies.
        assert!(should_install_ato_auth_cookies("https://app.ato.run/"));
        assert!(should_install_ato_auth_cookies(
            "https://app.ato.run/#route=/runners"
        ));
        assert!(should_install_ato_auth_cookies("https://stg-app.ato.run/"));
        // A look-alike / unrelated host must not receive account cookies — even
        // an arbitrary host that might be configured as the Home base URL.
        assert!(!should_install_ato_auth_cookies(
            "https://app.evil.example/"
        ));
        assert!(!should_install_ato_auth_cookies("https://notapp.ato.run/"));
        assert!(!should_install_ato_auth_cookies("https://custom.example/"));
        // Loopback is only injected into in debug builds (local dev server).
        assert_eq!(
            should_install_ato_auth_cookies("http://localhost:5173/"),
            cfg!(debug_assertions)
        );
    }

    /// Transient/preview capsule routes (LocalManifest, Capsule) must use
    /// the ephemeral (non-persistent) store and must never receive ato.run
    /// auth cookies.  This is the regression guard for #352.
    #[test]
    fn capsule_ephemeral_routes_deny_auth_cookies() {
        let ephemeral_routes: &[GuestRoute] = &[
            GuestRoute::Capsule {
                session: "s1".into(),
                entry_path: "/index.html".into(),
            },
            GuestRoute::LocalManifest(crate::state::LocalManifestRoute {
                manifest_path: "/tmp/capsule.toml".into(),
                source_handle: "capsule://ato.run/koh0920/local".into(),
                label: "local".into(),
                requested_ref: "main".into(),
                resolved_commit: "abc".into(),
                manifest_source: crate::state::ManifestSource::Repo,
                manifest_hash: "hash".into(),
                draft_id: "d1".into(),
            }),
        ];

        for route in ephemeral_routes {
            let cls = store_class_for_route(route);
            assert_eq!(
                cls,
                WebViewStoreClass::CapsuleEphemeral,
                "route {route} should be CapsuleEphemeral"
            );
            assert!(
                cls.uses_incognito_store(),
                "route {route} should use an incognito (non-persistent) store"
            );
            assert!(
                !cls.allows_ato_auth_cookies(),
                "route {route} must not allow ato.run auth cookie injection"
            );
        }
    }

    /// All capsule routes (CapsuleHandle, CapsuleUrl, LocalManifest, Capsule)
    /// must use `CapsuleEphemeral` — isolated non-persistent store.
    /// `CapsuleProfile` is reserved for future trusted-install routes (see
    /// #350 follow-up) and is not currently reachable from the production
    /// classifier.  Regression guard for #350 and #352.
    #[test]
    fn capsule_handle_and_url_routes_are_ephemeral_not_profile() {
        let ato_dock_url = url::Url::parse("https://ato.run/dock").expect("url");
        let handle = "capsule://ato.run/koh0920/blinko";

        let handle_route = GuestRoute::CapsuleHandle {
            handle: handle.into(),
            label: "Blinko".into(),
            community_toml_id: None,
        };
        let url_route = GuestRoute::CapsuleUrl {
            handle: handle.into(),
            label: "hello".into(),
            url: ato_dock_url.clone(), // deliberately points at ato.run/dock
        };

        let handle_cls = store_class_for_route(&handle_route);
        let url_cls = store_class_for_route(&url_route);

        // Both must be CapsuleEphemeral (not CapsuleProfile or System).
        assert_eq!(
            handle_cls,
            WebViewStoreClass::CapsuleEphemeral,
            "CapsuleHandle should be CapsuleEphemeral, got {handle_cls:?}"
        );
        assert_eq!(
            url_cls,
            WebViewStoreClass::CapsuleEphemeral,
            "CapsuleUrl should be CapsuleEphemeral, got {url_cls:?}"
        );

        // Auth cookies must be denied regardless of URL.
        assert!(
            !handle_cls.allows_ato_auth_cookies(),
            "CapsuleHandle must not allow ato.run auth cookie injection"
        );
        assert!(
            !url_cls.allows_ato_auth_cookies(),
            "CapsuleUrl pointing at ato.run/dock must not allow auth cookie injection"
        );

        // CapsuleEphemeral must use an incognito (non-persistent) store.
        assert!(
            handle_cls.uses_incognito_store(),
            "CapsuleHandle CapsuleEphemeral should use incognito store"
        );
    }

    /// CapsuleProfile (the future persistent-profile type) always denies
    /// auth cookies and does NOT report uses_incognito_store (it aims for
    /// persistent storage on supported platforms).  Tests the type directly
    /// since it is not currently reachable from the production classifier.
    #[test]
    fn capsule_profile_store_class_denies_auth_cookies_and_is_not_incognito() {
        let uuid = profile_store_uuid("handle:capsule://ato.run/user/app");
        let profile_cls = WebViewStoreClass::CapsuleProfile { uuid };
        assert!(
            !profile_cls.allows_ato_auth_cookies(),
            "CapsuleProfile must never allow ato.run auth cookie injection"
        );
        assert!(
            !profile_cls.uses_incognito_store(),
            "CapsuleProfile should not report uses_incognito_store (aims for persistent store)"
        );
    }

    /// Retention key for a CapsuleProfile must use the profile UUID hex
    /// string, not the handle string, so routes with different profile
    /// identities cannot accidentally share a retained WebView.
    /// (CapsuleProfile is currently unreachable from the production
    /// classifier; this tests the helper in isolation.)
    #[test]
    fn capsule_profile_retention_key_uses_uuid_not_handle() {
        let handle = "capsule://ato.run/user/app";
        let uuid = profile_store_uuid(&format!("handle:{handle}"));
        let uuid_hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
        let expected_key = format!("profile:{uuid_hex}");

        // The ephemeral key for the same handle uses a handle: prefix.
        let handle_route = GuestRoute::CapsuleHandle {
            handle: handle.into(),
            label: "app".into(),
            community_toml_id: None,
        };
        let ephemeral_key = webview_retention_key_for_route(&handle_route);
        assert_ne!(
            ephemeral_key.as_deref(),
            Some(expected_key.as_str()),
            "CapsuleEphemeral retention key must differ from CapsuleProfile key"
        );
        assert!(
            ephemeral_key
                .as_deref()
                .map_or(false, |k| k.starts_with("handle:")),
            "CapsuleEphemeral retention key must use handle: prefix"
        );
    }

    /// profile_store_uuid must be deterministic and stable across calls
    /// (same input → same UUID, different inputs → different UUIDs).
    #[test]
    fn profile_store_uuid_is_deterministic_and_unique() {
        let a = profile_store_uuid("handle:capsule://ato.run/user/app");
        let a2 = profile_store_uuid("handle:capsule://ato.run/user/app");
        let b = profile_store_uuid("url:capsule://ato.run/user/app");
        let c = profile_store_uuid("handle:capsule://ato.run/user/other");

        assert_eq!(a, a2, "same input must produce the same UUID");
        assert_ne!(
            a, b,
            "different namespace prefixes must produce different UUIDs"
        );
        assert_ne!(a, c, "different handles must produce different UUIDs");
        assert_ne!(b, c, "url-namespace vs different handle must differ");
    }

    /// ExternalUrl pointing at ato.run/dock is a system route and is the
    /// only kind of route that may receive ato.run auth cookies.
    #[test]
    fn external_url_ato_run_dock_is_system_and_allows_auth_cookies() {
        let route = GuestRoute::ExternalUrl(url::Url::parse("https://ato.run/dock").expect("url"));
        let cls = store_class_for_route(&route);
        assert_eq!(cls, WebViewStoreClass::System);
        assert!(!cls.uses_incognito_store());
        assert!(cls.allows_ato_auth_cookies());
        // Combined predicate matches what build_webview checks:
        assert!(
            cls.allows_ato_auth_cookies()
                && should_install_ato_auth_cookies("https://ato.run/dock")
        );
    }

    /// ExternalUrl pointing at a non-ato.run host is system-class (shared
    /// context) but should_install_ato_auth_cookies returns false.
    #[test]
    fn external_url_non_dock_is_system_but_no_auth_cookie_injection() {
        let route = GuestRoute::ExternalUrl(url::Url::parse("https://example.com").expect("url"));
        let cls = store_class_for_route(&route);
        assert_eq!(cls, WebViewStoreClass::System);
        assert!(cls.allows_ato_auth_cookies());
        // URL predicate blocks injection even though store class permits it.
        assert!(!should_install_ato_auth_cookies("https://example.com"));
    }

    /// Terminal is a system route and must not inject ato.run auth cookies
    /// (the URL predicate already returns false for terminal:// but the
    /// store-class check is the structural guard).
    #[test]
    fn terminal_route_is_system_class() {
        let route = GuestRoute::Terminal {
            session_id: "sess-1".into(),
        };
        let cls = store_class_for_route(&route);
        assert_eq!(cls, WebViewStoreClass::System);
        assert!(!cls.uses_incognito_store());
        // terminal:// does not match ato.run/dock so no injection.
        assert!(!should_install_ato_auth_cookies("terminal://sess-1/"));
    }

    #[test]
    fn ato_auth_cookie_targets_include_site_and_api_hosts() {
        let handoff = DesktopAuthHandoff {
            session_token: "secret".to_string(),
            site_base_url: "https://ato.run".to_string(),
            api_base_url: "https://api.ato.run".to_string(),
            publisher_handle: Some("koh".to_string()),
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
    fn decode_js_callback_value_unwraps_double_encoded_object() {
        let value = decode_js_callback_value("\"{\\\"found\\\":true}\"");

        assert_eq!(value.get("found").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn decode_js_callback_value_preserves_plain_strings() {
        let value = decode_js_callback_value("\"not-json\"");

        assert_eq!(value.as_str(), Some("not-json"));
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
    fn reuse_action_keeps_same_route_in_same_pane() {
        // Documents the gate that `stop_active_session` works around by
        // evicting the cached view (#112): when the user re-navigates to
        // the exact same capsule URL on the same pane, `reuse_action`
        // returns `Keep`, not `Rebuild`. Without the post-stop eviction,
        // that means `ensure_pending_local_launch` is never called and
        // the relaunch silently no-ops. The fix in `stop_active_session`
        // removes the entry from `WebViewManager::views` so this test's
        // gate is bypassed at the call site (`sync_from_state` falls
        // through to `unwrap_or(WebViewReuseAction::Rebuild)` when the
        // view is absent).
        let route = GuestRoute::CapsuleHandle {
            handle: "capsule://github.com/Koh0920/WasedaP2P".to_string(),
            label: "capsule://github.com/Koh0920/WasedaP2P".to_string(),
            community_toml_id: None,
        };
        let route_key = route.to_string();
        let next = active_web_pane(route.clone(), 7);

        assert_eq!(
            reuse_action(7, &route, &route_key, &next),
            WebViewReuseAction::Keep,
            "same-pane same-route navigate must produce Keep — \
             stop_active_session relies on evicting views[pane_id] \
             to bypass this branch (#112)"
        );
    }

    #[test]
    fn stable_origin_key_uses_handle_for_capsule_routes() {
        let handle_route = GuestRoute::CapsuleHandle {
            handle: "capsule://org/demo@1.0.0".to_string(),
            label: "demo".to_string(),
            community_toml_id: None,
        };
        let url_route = GuestRoute::CapsuleUrl {
            handle: "capsule://org/demo@1.0.0".to_string(),
            label: "demo".to_string(),
            url: url::Url::parse("http://127.0.0.1:3000").expect("url"),
        };

        assert_eq!(
            stable_origin_key_for_route(&handle_route),
            Some("handle:capsule://org/demo@1.0.0".to_string())
        );
        // CapsuleUrl uses "url:" prefix to avoid collision with CapsuleHandle
        assert_eq!(
            stable_origin_key_for_route(&url_route),
            Some("url:capsule://org/demo@1.0.0".to_string())
        );
    }

    #[test]
    fn stable_origin_key_is_not_created_for_external_or_terminal_routes() {
        let external =
            GuestRoute::ExternalUrl(url::Url::parse("https://example.com").expect("url"));
        let terminal = GuestRoute::Terminal {
            session_id: "term-1".to_string(),
        };

        assert_eq!(stable_origin_key_for_route(&external), None);
        assert_eq!(stable_origin_key_for_route(&terminal), None);
    }

    #[test]
    fn webview_retention_scope_keeps_capsule_backed_routes_only() {
        let capsule_handle_route = GuestRoute::CapsuleHandle {
            handle: "capsule://org/demo@1.0.0".to_string(),
            label: "demo".to_string(),
            community_toml_id: None,
        };
        let capsule_session_route = GuestRoute::Capsule {
            session: "session-1".to_string(),
            entry_path: "/index.html".to_string(),
        };
        let capsule_url_route = GuestRoute::CapsuleUrl {
            handle: "capsule://org/demo@1.0.0".to_string(),
            label: "demo".to_string(),
            url: url::Url::parse("http://127.0.0.1:4173/app").expect("url"),
        };
        let external_route =
            GuestRoute::ExternalUrl(url::Url::parse("https://example.com").expect("url"));
        let terminal_route = GuestRoute::Terminal {
            session_id: "term-1".to_string(),
        };

        assert!(is_webview_retention_eligible_route(&capsule_handle_route));
        assert!(is_webview_retention_eligible_route(&capsule_session_route));
        assert!(is_webview_retention_eligible_route(&capsule_url_route));
        assert!(!is_webview_retention_eligible_route(&external_route));
        assert!(!is_webview_retention_eligible_route(&terminal_route));
    }

    #[test]
    fn force_mounted_after_reuse_clears_overlay_for_external_url_navigate() {
        // Regression for #143. navigate_to_url resets the focused pane to
        // `WebSessionState::Launching`; in the Navigate path the WebView
        // is reused (load_url) so the build_webview branch's mounted
        // transition never runs, leaving the launching overlay stuck
        // ("Starting app…") on top of a live external page.
        let route = GuestRoute::ExternalUrl(url::Url::parse("https://docs.rs").expect("url"));
        assert!(should_force_mounted_after_reuse(
            WebViewReuseAction::Navigate,
            true,
            &route,
        ));
        assert!(should_force_mounted_after_reuse(
            WebViewReuseAction::Keep,
            true,
            &route,
        ));
    }

    #[test]
    fn force_mounted_after_reuse_skips_when_no_existing_view() {
        // Without a cached entry in WebViewManager.views the Rebuild
        // branch will run and own its own mounted transition, so the
        // post-reuse cleanup must not run.
        let route = GuestRoute::ExternalUrl(url::Url::parse("https://docs.rs").expect("url"));
        assert!(!should_force_mounted_after_reuse(
            WebViewReuseAction::Navigate,
            false,
            &route,
        ));
    }

    #[test]
    fn force_mounted_after_reuse_skips_capsule_routes() {
        // CapsuleHandle / Capsule routes need an explicit guest "ready"
        // signal before the overlay clears. Forcing Mounted here would
        // unhide the WebView before the guest has wired its bridge.
        let route = GuestRoute::CapsuleHandle {
            handle: "capsule://github.com/Koh0920/WasedaP2P".to_string(),
            label: "capsule://github.com/Koh0920/WasedaP2P".to_string(),
            community_toml_id: None,
        };
        assert!(!should_force_mounted_after_reuse(
            WebViewReuseAction::Navigate,
            true,
            &route,
        ));
        assert!(!should_force_mounted_after_reuse(
            WebViewReuseAction::Keep,
            true,
            &route,
        ));
    }

    #[test]
    fn force_mounted_after_reuse_skips_rebuild_branch() {
        // The Rebuild branch already owns its own state transition; doing
        // it twice is harmless but signals confused ownership.
        let route = GuestRoute::ExternalUrl(url::Url::parse("https://docs.rs").expect("url"));
        assert!(!should_force_mounted_after_reuse(
            WebViewReuseAction::Rebuild,
            true,
            &route,
        ));
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

    // ── apply_capsule_secrets (used by automation MCP `set_capsule_secrets`) ──
    //
    // These tests pin the contract that the MCP path is wire-compatible with
    // the modal Save handler in `ui/mod.rs::save_pending_config`. They share
    // an env_lock because save_secrets reads ATO_HOME, and parallel tests
    // would otherwise see each other's tempdir.
    mod apply_capsule_secrets {
        use super::*;
        use crate::state::{
            PendingConfigRequest, PendingConsentItem, PendingResolutionRequest, PendingSecretsItem,
        };
        use protocol::config::{ConfigField, ConfigKind};
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
                unsafe {
                    std::env::set_var(key, value);
                }
                Self { key, previous }
            }
        }

        impl Drop for EnvVarGuard {
            fn drop(&mut self) {
                if let Some(value) = &self.previous {
                    unsafe {
                        std::env::set_var(self.key, value);
                    }
                } else {
                    unsafe {
                        std::env::remove_var(self.key);
                    }
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
                community_toml_id: None,
            }
        }

        fn secret_field(name: &str) -> ConfigField {
            ConfigField {
                name: name.to_string(),
                label: None,
                description: None,
                kind: ConfigKind::Secret,
                default: None,
                placeholder: None,
            }
        }

        fn string_field(name: &str) -> ConfigField {
            ConfigField {
                name: name.to_string(),
                label: None,
                description: None,
                kind: ConfigKind::String,
                default: None,
                placeholder: None,
            }
        }

        fn consent_item(target_label: &str) -> PendingConsentItem {
            PendingConsentItem {
                scoped_id: format!("publisher/{target_label}"),
                version: "1.0.0".to_string(),
                target_label: target_label.to_string(),
                policy_segment_hash: format!("blake3:{target_label}-policy"),
                provisioning_policy_hash: format!("blake3:{target_label}-prov"),
                summary: format!("Consent for {target_label}"),
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

        #[test]
        fn clears_matching_pending_resolution_secret_sections() {
            let _lock = env_lock();
            let (_tmp, _guard, mut state) = isolated_state();
            state.pending_resolution = Some(PendingResolutionRequest {
                handle: "capsule://github.com/Koh0920/WasedaP2P".to_string(),
                original_secrets: Vec::new(),
                secrets: vec![PendingSecretsItem {
                    target: Some("app".to_string()),
                    fields: vec![secret_field("SECRET_KEY")],
                }],
                consents: Vec::new(),
                community_toml_id: None,
            });

            apply_capsule_secrets(
                &mut state,
                "capsule://github.com/Koh0920/WasedaP2P",
                &[("SECRET_KEY".into(), "v".into())],
                true,
            )
            .expect("apply");

            assert!(
                state.pending_resolution.is_none(),
                "resolved secrets-only pending_resolution must clear"
            );
        }

        #[test]
        fn keeps_remaining_resolution_items_after_secret_apply() {
            let _lock = env_lock();
            let (_tmp, _guard, mut state) = isolated_state();
            state.pending_resolution = Some(PendingResolutionRequest {
                handle: "capsule://github.com/Koh0920/WasedaP2P".to_string(),
                original_secrets: Vec::new(),
                secrets: vec![
                    PendingSecretsItem {
                        target: Some("app".to_string()),
                        fields: vec![secret_field("SECRET_KEY")],
                    },
                    PendingSecretsItem {
                        target: Some("web".to_string()),
                        fields: vec![string_field("APP_MODE")],
                    },
                ],
                consents: vec![consent_item("web")],
                community_toml_id: None,
            });

            apply_capsule_secrets(
                &mut state,
                "capsule://github.com/Koh0920/WasedaP2P",
                &[("SECRET_KEY".into(), "v".into())],
                true,
            )
            .expect("apply");

            let pending = state
                .pending_resolution
                .as_ref()
                .expect("consent + non-secret config must remain");
            assert_eq!(pending.secrets.len(), 1);
            assert_eq!(pending.secrets[0].target.as_deref(), Some("web"));
            assert_eq!(pending.consents.len(), 1);
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
        use crate::state::{PendingConsentItem, PendingConsentRequest, PendingResolutionRequest};

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
                community_toml_id: None,
            }
        }

        fn consent_item(target_label: &str) -> PendingConsentItem {
            PendingConsentItem {
                scoped_id: format!("publisher/{target_label}"),
                version: "1.0.0".to_string(),
                target_label: target_label.to_string(),
                policy_segment_hash: format!("blake3:{target_label}-policy"),
                provisioning_policy_hash: format!("blake3:{target_label}-prov"),
                summary: format!("Consent for {target_label}"),
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

        #[test]
        fn resolves_matching_pending_resolution_consents() {
            let mut state = AppState::initial();
            let handle = "capsule://github.com/Koh0920/WasedaP2P";
            state.pending_resolution = Some(PendingResolutionRequest {
                handle: handle.to_string(),
                original_secrets: Vec::new(),
                secrets: Vec::new(),
                consents: vec![consent_item("app"), consent_item("web")],
                community_toml_id: None,
            });

            let handled =
                apply_pending_resolution_consents(&mut state, handle, |_| Ok(())).expect("approve");

            assert!(
                handled,
                "matching pending_resolution consents must be handled"
            );
            assert!(
                state.pending_resolution.is_none(),
                "all consents resolved -> clear"
            );
            assert!(state.consent_retry_already_consumed(handle, "app"));
            assert!(state.consent_retry_already_consumed(handle, "web"));
        }

        #[test]
        fn preserves_other_resolution_requirements_when_approving_consents() {
            let mut state = AppState::initial();
            let handle = "capsule://github.com/Koh0920/WasedaP2P";
            state.pending_resolution = Some(PendingResolutionRequest {
                handle: handle.to_string(),
                original_secrets: Vec::new(),
                secrets: vec![crate::state::PendingSecretsItem {
                    target: Some("app".to_string()),
                    fields: Vec::new(),
                }],
                consents: vec![consent_item("app")],
                community_toml_id: None,
            });

            apply_pending_resolution_consents(&mut state, handle, |_| Ok(())).expect("approve");

            let pending = state
                .pending_resolution
                .as_ref()
                .expect("remaining secrets must keep pending_resolution open");
            assert_eq!(pending.secrets.len(), 1);
            assert!(pending.consents.is_empty());
        }
    }

    mod pending_prelaunch_requirement_message {
        use super::*;
        use crate::state::{
            PendingConfigRequest, PendingConsentItem, PendingConsentRequest,
            PendingResolutionRequest, PendingSecretsItem,
        };

        fn pending_config(handle: &str, target: Option<&str>) -> PendingConfigRequest {
            PendingConfigRequest {
                handle: handle.to_string(),
                target: target.map(str::to_string),
                fields: Vec::new(),
                original_secrets: Vec::new(),
                community_toml_id: None,
            }
        }

        fn pending_consent(handle: &str, target_label: &str) -> PendingConsentRequest {
            PendingConsentRequest {
                handle: handle.to_string(),
                scoped_id: "publisher/app".to_string(),
                version: "1.0.0".to_string(),
                target_label: target_label.to_string(),
                policy_segment_hash: "blake3:policy".to_string(),
                provisioning_policy_hash: "blake3:prov".to_string(),
                summary: "Consent summary".to_string(),
                original_secrets: Vec::new(),
                community_toml_id: None,
            }
        }

        #[test]
        fn summarizes_legacy_pending_config_and_consent() {
            let mut state = AppState::initial();
            let handle = "capsule://github.com/Koh0920/WasedaP2P";
            state.set_pending_config(pending_config(handle, Some("app")));
            state.set_pending_consent(pending_consent(handle, "web"));

            let message = pending_prelaunch_requirement_message(&state, handle)
                .expect("legacy requirements message");

            assert_eq!(
                message,
                "awaiting pre-launch requirements: config:app, consent:web"
            );
        }

        #[test]
        fn summarizes_unified_pending_resolution_items() {
            let mut state = AppState::initial();
            let handle = "capsule://github.com/Koh0920/WasedaP2P";
            state.pending_resolution = Some(PendingResolutionRequest {
                handle: handle.to_string(),
                original_secrets: Vec::new(),
                secrets: vec![PendingSecretsItem {
                    target: Some("app".to_string()),
                    fields: Vec::new(),
                }],
                consents: vec![
                    PendingConsentItem {
                        scoped_id: "publisher/app".to_string(),
                        version: "1.0.0".to_string(),
                        target_label: "app".to_string(),
                        policy_segment_hash: "blake3:app-policy".to_string(),
                        provisioning_policy_hash: "blake3:app-prov".to_string(),
                        summary: "Consent app".to_string(),
                    },
                    PendingConsentItem {
                        scoped_id: "publisher/web".to_string(),
                        version: "1.0.0".to_string(),
                        target_label: "web".to_string(),
                        policy_segment_hash: "blake3:web-policy".to_string(),
                        provisioning_policy_hash: "blake3:web-prov".to_string(),
                        summary: "Consent web".to_string(),
                    },
                ],
                community_toml_id: None,
            });

            let message = pending_prelaunch_requirement_message(&state, handle)
                .expect("unified requirements message");

            assert_eq!(
                message,
                "awaiting pre-launch requirements: config:app, consent:app, consent:web"
            );
        }
    }

    #[test]
    fn auth_status_sanitizes_handoff_without_exposing_session_token() {
        let stdout = br#"{
            "session_token": "secret-token",
            "site_base_url": "https://ato.run",
            "api_base_url": "https://api.ato.run",
            "publisher_handle": "koh"
        }"#;

        let status = auth_status_from_handoff_stdout(stdout);
        let json = serde_json::to_value(&status).unwrap();

        assert_eq!(json["signed_in"], true);
        assert_eq!(json["api_base_url"], "https://api.ato.run");
        assert_eq!(json["account_hint"], "koh");
        assert!(json.get("session_token").is_none());
        assert!(!json.to_string().contains("secret-token"));
    }

    #[test]
    fn auth_status_returns_signed_out_on_handoff_failure() {
        let status = auth_status_from_handoff_stdout(b"not json");
        assert!(!status.signed_in);
        assert_eq!(status.api_base_url, default_api_base_url());
        assert!(status.account_hint.is_none());
    }

    #[test]
    fn auth_status_returns_signed_out_on_missing_publisher_handle() {
        let stdout = br#"{
            "session_token": "secret-token",
            "site_base_url": "https://ato.run",
            "api_base_url": "https://api.ato.run"
        }"#;

        let status = auth_status_from_handoff_stdout(stdout);
        assert!(status.signed_in);
        assert_eq!(status.api_base_url, "https://api.ato.run");
        assert!(status.account_hint.is_none());
    }

    #[test]
    fn auth_status_response_does_not_contain_token_field() {
        let status = AuthStatusResponse {
            signed_in: true,
            api_base_url: "https://api.ato.run".to_string(),
            account_hint: Some("koh".to_string()),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert!(json.get("session_token").is_none());
        assert!(json.get("token").is_none());
    }

    /// `deregister_ingress_if_registered` must be a no-op for `None` and not
    /// panic. Regression guard: any refactor that breaks this check will cause
    /// the guard drop in `build_webview` to panic on every WebView creation
    /// for routes without ingress registration.
    #[test]
    fn deregister_ingress_if_registered_noop_for_none() {
        deregister_ingress_if_registered(&None);
    }

    /// The ingress selection in `build_webview` is driven by `store_class_for_route`.
    /// `build_webview` dispatches ingress registration based on store class,
    /// not on `logical_key_for_route` alone.  For `CapsuleEphemeral` routes,
    /// ephemeral ingress is always chosen — even when a logical key exists
    /// (e.g. `CapsuleHandle` does derive a stable key from its handle).
    ///
    /// This test verifies that the store-class → ingress dispatch invariant
    /// holds for the routes that matter most, and that `System` routes
    /// (`ExternalUrl`, `Terminal`) are never classified as `CapsuleEphemeral`
    /// or `CapsuleProfile`.
    #[test]
    fn store_class_drives_ingress_dispatch() {
        use crate::netd::logical_key_for_route;

        // CapsuleHandle and CapsuleUrl → CapsuleEphemeral (isolated non-persistent
        // store; CapsuleProfile is reserved for future trusted-install routes).
        let ephemeral_routes = [
            GuestRoute::CapsuleHandle {
                handle: "capsule://org/demo@1.0.0".into(),
                label: "demo".into(),
                community_toml_id: None,
            },
            GuestRoute::CapsuleUrl {
                handle: "capsule://org/demo@1.0.0".into(),
                label: "demo".into(),
                url: url::Url::parse("http://127.0.0.1:3000").expect("url"),
            },
        ];
        for route in &ephemeral_routes {
            assert_eq!(
                store_class_for_route(route),
                WebViewStoreClass::CapsuleEphemeral,
                "route {route} must be CapsuleEphemeral"
            );
        }

        // System routes → System store (not capsule-isolated).
        let system_routes = [
            GuestRoute::ExternalUrl(url::Url::parse("https://example.com").expect("url")),
            GuestRoute::ExternalUrl(url::Url::parse("https://ato.run/dock").expect("url")),
        ];
        for route in &system_routes {
            assert_eq!(
                store_class_for_route(route),
                WebViewStoreClass::System,
                "route {route} must be System"
            );
            // System routes with ato.run/dock URL have no logical key and thus
            // no ingress registration.
            assert!(
                logical_key_for_route(route).is_none(),
                "ExternalUrl routes must not have a stable ingress key"
            );
        }
    }

    // ── WebViewStoreIdentity / store_class_for_identity tests ──────────

    fn capsule_handle_route() -> GuestRoute {
        GuestRoute::CapsuleHandle {
            handle: "capsule://ato.run/org/app".to_string(),
            label: "app".to_string(),
            community_toml_id: None,
        }
    }

    fn capsule_url_route() -> GuestRoute {
        GuestRoute::CapsuleUrl {
            handle: "capsule://ato.run/org/app".to_string(),
            label: "app".to_string(),
            url: url::Url::parse("http://127.0.0.1:9999/").expect("url"),
        }
    }

    fn local_manifest_route() -> GuestRoute {
        GuestRoute::LocalManifest(crate::state::LocalManifestRoute {
            manifest_path: "/tmp/capsule.toml".into(),
            source_handle: "capsule://ato.run/org/app".to_string(),
            label: "dev".to_string(),
            requested_ref: "main".to_string(),
            resolved_commit: "abc123".to_string(),
            manifest_source: crate::state::ManifestSource::Repo,
            manifest_hash: "deadbeef".to_string(),
            draft_id: "draft-0".to_string(),
        })
    }

    fn capsule_route() -> GuestRoute {
        GuestRoute::Capsule {
            session: "sess-abc".to_string(),
            entry_path: "/".to_string(),
        }
    }

    /// Minimal identity with no trust/profile metadata — simulates a
    /// transient route where `ActiveWebPane` fields are not yet populated.
    fn minimal_identity(route: GuestRoute) -> WebViewStoreIdentity {
        WebViewStoreIdentity::from_route(route)
    }

    /// Full trusted-installed identity for testing CapsuleProfile assignment.
    fn installed_trusted_identity(route: GuestRoute) -> WebViewStoreIdentity {
        WebViewStoreIdentity {
            route,
            trust_state: Some("local".to_string()),
            install_profile_key: Some("ipk_aabbcc".to_string()),
            publisher_identity: Some("github.com/org".to_string()),
            source_identity: Some("capsule://ato.run/org/app".to_string()),
            snapshot_label: Some("v1.2.3".to_string()),
        }
    }

    #[test]
    fn metadata_missing_capsule_handle_is_ephemeral() {
        // No trust_state and no install_profile_key → CapsuleEphemeral
        let cls = store_class_for_identity(&minimal_identity(capsule_handle_route()));
        assert_eq!(cls, WebViewStoreClass::CapsuleEphemeral);
    }

    #[test]
    fn metadata_missing_capsule_url_is_ephemeral() {
        let cls = store_class_for_identity(&minimal_identity(capsule_url_route()));
        assert_eq!(cls, WebViewStoreClass::CapsuleEphemeral);
    }

    #[test]
    fn metadata_missing_local_manifest_is_ephemeral() {
        let cls = store_class_for_identity(&minimal_identity(local_manifest_route()));
        assert_eq!(cls, WebViewStoreClass::CapsuleEphemeral);
    }

    #[test]
    fn metadata_missing_capsule_session_is_ephemeral() {
        let cls = store_class_for_identity(&minimal_identity(capsule_route()));
        assert_eq!(cls, WebViewStoreClass::CapsuleEphemeral);
    }

    #[test]
    fn untrusted_trust_state_is_ephemeral_even_with_ipk() {
        // untrusted + install_profile_key → still CapsuleEphemeral
        let id = WebViewStoreIdentity {
            route: capsule_handle_route(),
            trust_state: Some("untrusted".to_string()),
            install_profile_key: Some("ipk_aabbcc".to_string()),
            publisher_identity: None,
            source_identity: None,
            snapshot_label: None,
        };
        assert_eq!(
            store_class_for_identity(&id),
            WebViewStoreClass::CapsuleEphemeral
        );
    }

    #[test]
    fn trusted_without_install_profile_key_is_ephemeral() {
        // local trust but no install_profile_key → still CapsuleEphemeral
        let id = WebViewStoreIdentity {
            route: capsule_handle_route(),
            trust_state: Some("local".to_string()),
            install_profile_key: None,
            publisher_identity: Some("github.com/org".to_string()),
            source_identity: Some("capsule://ato.run/org/app".to_string()),
            snapshot_label: None,
        };
        assert_eq!(
            store_class_for_identity(&id),
            WebViewStoreClass::CapsuleEphemeral
        );
    }

    #[test]
    fn unknown_trust_state_with_ipk_is_ephemeral() {
        // trust_state=None + install_profile_key → still CapsuleEphemeral
        let id = WebViewStoreIdentity {
            route: capsule_handle_route(),
            trust_state: None,
            install_profile_key: Some("ipk_aabbcc".to_string()),
            publisher_identity: None,
            source_identity: None,
            snapshot_label: None,
        };
        assert_eq!(
            store_class_for_identity(&id),
            WebViewStoreClass::CapsuleEphemeral
        );
    }

    #[test]
    fn trusted_with_install_profile_key_is_capsule_profile() {
        // trusted + install_profile_key → CapsuleProfile
        let id = installed_trusted_identity(capsule_handle_route());
        assert!(
            matches!(
                store_class_for_identity(&id),
                WebViewStoreClass::CapsuleProfile { .. }
            ),
            "expected CapsuleProfile, got {:?}",
            store_class_for_identity(&id)
        );
    }

    #[test]
    fn system_route_is_system_regardless_of_trust_state() {
        let external =
            GuestRoute::ExternalUrl(url::Url::parse("https://ato.run/store").expect("url"));
        for trust in [
            None,
            Some("local".to_string()),
            Some("untrusted".to_string()),
        ] {
            let id = WebViewStoreIdentity {
                route: external.clone(),
                trust_state: trust.clone(),
                install_profile_key: Some("ipk_aabbcc".to_string()),
                publisher_identity: Some("pub".to_string()),
                source_identity: Some("capsule://ato.run/org/app".to_string()),
                snapshot_label: None,
            };
            assert_eq!(
                store_class_for_identity(&id),
                WebViewStoreClass::System,
                "ExternalUrl must be System regardless of trust_state={trust:?}"
            );
        }
    }

    #[test]
    fn profile_uuid_varies_by_install_profile_key() {
        let make_id = |ipk: &str| WebViewStoreIdentity {
            route: capsule_handle_route(),
            trust_state: Some("local".to_string()),
            install_profile_key: Some(ipk.to_string()),
            publisher_identity: Some("github.com/org".to_string()),
            source_identity: Some("capsule://ato.run/org/app".to_string()),
            snapshot_label: None,
        };
        let uuid_a = match store_class_for_identity(&make_id("ipk_aaaa")) {
            WebViewStoreClass::CapsuleProfile { uuid } => uuid,
            other => panic!("expected CapsuleProfile, got {other:?}"),
        };
        let uuid_b = match store_class_for_identity(&make_id("ipk_bbbb")) {
            WebViewStoreClass::CapsuleProfile { uuid } => uuid,
            other => panic!("expected CapsuleProfile, got {other:?}"),
        };
        assert_ne!(
            uuid_a, uuid_b,
            "different install_profile_key must produce different UUID"
        );
    }

    #[test]
    fn profile_uuid_varies_by_source_identity() {
        let make_id = |src: &str| WebViewStoreIdentity {
            route: capsule_handle_route(),
            trust_state: Some("local".to_string()),
            install_profile_key: Some("ipk_same".to_string()),
            publisher_identity: Some("github.com/org".to_string()),
            source_identity: Some(src.to_string()),
            snapshot_label: None,
        };
        let uuid_a = match store_class_for_identity(&make_id("capsule://ato.run/org/app-a")) {
            WebViewStoreClass::CapsuleProfile { uuid } => uuid,
            other => panic!("expected CapsuleProfile, got {other:?}"),
        };
        let uuid_b = match store_class_for_identity(&make_id("capsule://ato.run/org/app-b")) {
            WebViewStoreClass::CapsuleProfile { uuid } => uuid,
            other => panic!("expected CapsuleProfile, got {other:?}"),
        };
        assert_ne!(
            uuid_a, uuid_b,
            "different source_identity must produce different UUID"
        );
    }

    #[test]
    fn profile_uuid_varies_by_publisher_identity() {
        let make_id = |pub_id: &str| WebViewStoreIdentity {
            route: capsule_handle_route(),
            trust_state: Some("local".to_string()),
            install_profile_key: Some("ipk_same".to_string()),
            publisher_identity: Some(pub_id.to_string()),
            source_identity: Some("capsule://ato.run/org/app".to_string()),
            snapshot_label: None,
        };
        let uuid_a = match store_class_for_identity(&make_id("github.com/org-a")) {
            WebViewStoreClass::CapsuleProfile { uuid } => uuid,
            other => panic!("expected CapsuleProfile, got {other:?}"),
        };
        let uuid_b = match store_class_for_identity(&make_id("github.com/org-b")) {
            WebViewStoreClass::CapsuleProfile { uuid } => uuid,
            other => panic!("expected CapsuleProfile, got {other:?}"),
        };
        assert_ne!(
            uuid_a, uuid_b,
            "different publisher_identity must produce different UUID"
        );
    }

    #[test]
    fn profile_uuid_stable_across_snapshot_changes() {
        // snapshot_label is intentionally excluded from the profile UUID —
        // app updates should preserve user localStorage/cookies.
        let make_id = |snap: Option<&str>| WebViewStoreIdentity {
            route: capsule_handle_route(),
            trust_state: Some("local".to_string()),
            install_profile_key: Some("ipk_same".to_string()),
            publisher_identity: Some("github.com/org".to_string()),
            source_identity: Some("capsule://ato.run/org/app".to_string()),
            snapshot_label: snap.map(str::to_string),
        };
        let uuid_v1 = match store_class_for_identity(&make_id(Some("v1.0.0"))) {
            WebViewStoreClass::CapsuleProfile { uuid } => uuid,
            other => panic!("expected CapsuleProfile, got {other:?}"),
        };
        let uuid_v2 = match store_class_for_identity(&make_id(Some("v2.0.0"))) {
            WebViewStoreClass::CapsuleProfile { uuid } => uuid,
            other => panic!("expected CapsuleProfile, got {other:?}"),
        };
        let uuid_none = match store_class_for_identity(&make_id(None)) {
            WebViewStoreClass::CapsuleProfile { uuid } => uuid,
            other => panic!("expected CapsuleProfile, got {other:?}"),
        };
        assert_eq!(
            uuid_v1, uuid_v2,
            "UUID must be stable across snapshot changes"
        );
        assert_eq!(
            uuid_v1, uuid_none,
            "UUID must be stable when snapshot_label is absent"
        );
    }

    #[test]
    fn capsule_profile_does_not_allow_ato_auth_cookies() {
        let id = installed_trusted_identity(capsule_handle_route());
        let cls = store_class_for_identity(&id);
        assert!(
            matches!(cls, WebViewStoreClass::CapsuleProfile { .. }),
            "fixture must reach CapsuleProfile"
        );
        assert!(
            !cls.allows_ato_auth_cookies(),
            "CapsuleProfile must not allow ato auth cookies"
        );
    }

    #[test]
    fn capsule_profile_retention_key_differs_from_ephemeral_key() {
        // CapsuleProfile retention key uses the profile UUID hex, not the
        // handle string, so two routes with different profile identities
        // never share a retained WebView.
        let route = capsule_handle_route();
        let ephemeral_key = webview_retention_key_for_route(&route);
        let profile_id = installed_trusted_identity(route.clone());
        let profile_key = match store_class_for_identity(&profile_id) {
            WebViewStoreClass::CapsuleProfile { uuid } => {
                let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
                Some(format!("profile:{hex}"))
            }
            _ => panic!("expected CapsuleProfile"),
        };
        // Ephemeral retention key is handle-based; profile key is UUID-based.
        assert_ne!(
            ephemeral_key, profile_key,
            "ephemeral and profile retention keys must not collide"
        );
    }

    #[test]
    fn store_class_for_route_delegates_to_identity_classifier() {
        // store_class_for_route is the minimal-identity wrapper.  With no
        // install_profile_key it always returns CapsuleEphemeral for capsule
        // routes, and System for external/terminal routes.
        assert_eq!(
            store_class_for_route(&capsule_handle_route()),
            WebViewStoreClass::CapsuleEphemeral
        );
        assert_eq!(
            store_class_for_route(&GuestRoute::ExternalUrl(
                url::Url::parse("https://ato.run/").expect("url")
            )),
            WebViewStoreClass::System
        );
    }

    #[test]
    fn identity_retention_key_for_capsule_profile_uses_profile_prefix() {
        let id = installed_trusted_identity(capsule_handle_route());
        let key = webview_retention_key_for_identity(&id);
        assert!(
            key.as_deref()
                .map(|k| k.starts_with("profile:"))
                .unwrap_or(false),
            "CapsuleProfile retention key must start with 'profile:', got {key:?}"
        );
    }

    #[test]
    fn identity_retention_key_for_ephemeral_does_not_use_profile_prefix() {
        let id = minimal_identity(capsule_handle_route());
        let key = webview_retention_key_for_identity(&id);
        assert!(
            key.as_deref()
                .map(|k| !k.starts_with("profile:"))
                .unwrap_or(true),
            "CapsuleEphemeral retention key must not start with 'profile:', got {key:?}"
        );
    }

    #[test]
    fn identity_retention_key_is_stable_across_snapshot_changes() {
        let make_id = |snap: Option<&str>| installed_trusted_identity_with_snap(snap);
        let key_v1 = webview_retention_key_for_identity(&make_id(Some("v1.0")));
        let key_v2 = webview_retention_key_for_identity(&make_id(Some("v2.0")));
        assert_eq!(
            key_v1, key_v2,
            "retention key must be stable across snapshot changes (UUID excludes snapshot)"
        );
    }

    fn installed_trusted_identity_with_snap(snap: Option<&str>) -> WebViewStoreIdentity {
        WebViewStoreIdentity {
            route: capsule_handle_route(),
            trust_state: Some("local".to_string()),
            install_profile_key: Some("ipk_aabbcc".to_string()),
            publisher_identity: Some("github.com/org".to_string()),
            source_identity: Some("capsule://ato.run/org/app".to_string()),
            snapshot_label: snap.map(str::to_string),
        }
    }
}

/// Capsule icon and web-favicon resolution helpers (previously `ui::share::icon`).
mod share_icon {
    use std::path::Path;

    use protocol::handle::CapsuleDisplayStrategy;

    use crate::logging::TARGET_FAVICON;
    use crate::orchestrator::CapsuleLaunchSession;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum ShareIconSource {
        Direct(String),
        FaviconOrigin(String),
    }

    pub(crate) fn resolve_share_icon(session: &CapsuleLaunchSession) -> Option<ShareIconSource> {
        tracing::info!(
            target: TARGET_FAVICON,
            session_id = %session.session_id,
            handle = %session.handle,
            manifest_path = %session.manifest_path.display(),
            app_root = %session.app_root.display(),
            display_strategy = %session.display_strategy.as_str(),
            local_url = ?session.local_url,
            "resolving share icon"
        );

        if let Some(source) = resolve_capsule_icon_source(&session.manifest_path, &session.app_root)
        {
            tracing::info!(
                target: TARGET_FAVICON,
                session_id = %session.session_id,
                source = %source,
                "resolved share icon from capsule metadata"
            );
            return Some(ShareIconSource::Direct(source));
        }

        if session.display_strategy == CapsuleDisplayStrategy::WebUrl || session.local_url.is_some()
        {
            if let Some(local_url) = session.local_url.as_deref() {
                match web_favicon_origin(local_url) {
                    Some(origin) => {
                        tracing::info!(
                            target: TARGET_FAVICON,
                            session_id = %session.session_id,
                            local_url,
                            origin,
                            "resolved share icon to web favicon origin"
                        );
                        return Some(ShareIconSource::FaviconOrigin(origin));
                    }
                    None => {
                        tracing::error!(
                            target: TARGET_FAVICON,
                            session_id = %session.session_id,
                            local_url,
                            "failed to resolve share icon favicon origin from local_url"
                        );
                    }
                }
            } else {
                tracing::error!(
                    target: TARGET_FAVICON,
                    session_id = %session.session_id,
                    "web share icon fallback requested but session has no local_url"
                );
            }
        }

        tracing::error!(
            target: TARGET_FAVICON,
            session_id = %session.session_id,
            manifest_path = %session.manifest_path.display(),
            "failed to resolve share icon source"
        );
        None
    }

    /// Read `[metadata].icon` from a capsule manifest and resolve it to a value
    /// the sidebar can fetch: an absolute filesystem path for relative entries,
    /// or the raw string for `http(s)://`, `file://`, and `data:` image references.
    pub(crate) fn resolve_capsule_icon_source(
        manifest_path: &Path,
        app_root: &Path,
    ) -> Option<String> {
        let raw = match std::fs::read_to_string(manifest_path) {
            Ok(raw) => raw,
            Err(error) => {
                tracing::error!(
                    target: TARGET_FAVICON,
                    manifest_path = %manifest_path.display(),
                    error = %error,
                    "failed to read capsule manifest while resolving icon"
                );
                return None;
            }
        };
        let manifest: capsule::types::CapsuleManifest = match toml::from_str(&raw) {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::error!(
                    target: TARGET_FAVICON,
                    manifest_path = %manifest_path.display(),
                    error = %error,
                    "failed to parse capsule manifest while resolving icon"
                );
                return None;
            }
        };
        let Some(icon) = manifest.metadata.icon.filter(|s| !s.is_empty()) else {
            tracing::info!(
                target: TARGET_FAVICON,
                manifest_path = %manifest_path.display(),
                "capsule manifest has no metadata.icon"
            );
            return None;
        };
        tracing::info!(
            target: TARGET_FAVICON,
            manifest_path = %manifest_path.display(),
            icon,
            "found capsule metadata.icon"
        );
        if is_direct_image_reference(&icon) {
            tracing::info!(
                target: TARGET_FAVICON,
                manifest_path = %manifest_path.display(),
                source = %icon,
                "using direct capsule metadata icon reference"
            );
            return Some(icon);
        }

        // Published registry installs materialize source files under `source/`;
        // local dev manifests keep assets next to `capsule.toml`.
        let with_source = app_root.join("source").join(&icon);
        if with_source.exists() {
            let absolute = with_source.canonicalize().unwrap_or(with_source);
            tracing::info!(
                target: TARGET_FAVICON,
                manifest_path = %manifest_path.display(),
                source = %absolute.display(),
                "resolved capsule metadata icon from materialized source path"
            );
            return Some(absolute.to_string_lossy().to_string());
        }
        let bare = app_root.join(&icon);
        if bare.exists() {
            let absolute = bare.canonicalize().unwrap_or(bare);
            tracing::info!(
                target: TARGET_FAVICON,
                manifest_path = %manifest_path.display(),
                source = %absolute.display(),
                "resolved capsule metadata icon from app root path"
            );
            return Some(absolute.to_string_lossy().to_string());
        }
        tracing::error!(
            target: TARGET_FAVICON,
            manifest_path = %manifest_path.display(),
            app_root = %app_root.display(),
            icon,
            source_candidate = %with_source.display(),
            bare_candidate = %bare.display(),
            "capsule metadata icon relative path did not exist"
        );
        None
    }

    pub(crate) fn web_favicon_origin(local_url: &str) -> Option<String> {
        let parsed = match url::Url::parse(local_url) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::error!(
                    target: TARGET_FAVICON,
                    local_url,
                    error = %error,
                    "failed to parse local_url for favicon origin"
                );
                return None;
            }
        };
        if !matches!(parsed.scheme(), "http" | "https") {
            tracing::error!(
                target: TARGET_FAVICON,
                local_url,
                scheme = parsed.scheme(),
                "local_url scheme cannot provide a web favicon"
            );
            return None;
        }
        let origin = parsed.origin().ascii_serialization();
        tracing::info!(
            target: TARGET_FAVICON,
            local_url,
            origin,
            "normalized web favicon origin"
        );
        Some(origin)
    }

    fn is_direct_image_reference(value: &str) -> bool {
        value.starts_with("http://")
            || value.starts_with("https://")
            || value.starts_with("file://")
            || value.starts_with("data:")
    }

    #[cfg(test)]
    mod tests {
        use super::{resolve_capsule_icon_source, web_favicon_origin};

        fn write_manifest(root: &std::path::Path, icon: &str) -> std::path::PathBuf {
            let manifest_path = root.join("capsule.toml");
            std::fs::write(
                &manifest_path,
                format!(
                    r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "web/static"
run = "dist"

[metadata]
icon = "{icon}"
"#
                ),
            )
            .expect("write manifest");
            manifest_path
        }

        #[test]
        fn metadata_icon_direct_references_pass_through() {
            let tmp = tempfile::tempdir().expect("tempdir");

            for icon in [
                "https://example.com/icon.png",
                "http://example.com/icon.svg",
                "file:///Users/example/icon.png",
                "data:image/png;base64,AAAA",
            ] {
                let manifest_path = write_manifest(tmp.path(), icon);
                assert_eq!(
                    resolve_capsule_icon_source(&manifest_path, tmp.path()).as_deref(),
                    Some(icon)
                );
            }
        }

        #[test]
        fn metadata_icon_relative_path_resolves_against_app_root() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let manifest_path = write_manifest(tmp.path(), "assets/icon.png");
            let icon_path = tmp.path().join("assets/icon.png");
            std::fs::create_dir_all(icon_path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&icon_path, b"png").expect("write icon");

            assert_eq!(
                resolve_capsule_icon_source(&manifest_path, tmp.path()),
                Some(
                    icon_path
                        .canonicalize()
                        .expect("canonical")
                        .to_string_lossy()
                        .to_string()
                )
            );
        }

        #[test]
        fn metadata_icon_prefers_materialized_source_layout() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let manifest_path = write_manifest(tmp.path(), "assets/icon.png");
            let bare = tmp.path().join("assets/icon.png");
            std::fs::create_dir_all(bare.parent().expect("bare parent")).expect("mkdir bare");
            std::fs::write(&bare, b"bare").expect("write bare");

            let source = tmp.path().join("source/assets/icon.png");
            std::fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir source");
            std::fs::write(&source, b"source").expect("write source");

            assert_eq!(
                resolve_capsule_icon_source(&manifest_path, tmp.path()),
                Some(
                    source
                        .canonicalize()
                        .expect("canonical")
                        .to_string_lossy()
                        .to_string()
                )
            );
        }

        #[test]
        fn web_favicon_origin_normalizes_http_local_url() {
            assert_eq!(
                web_favicon_origin("http://127.0.0.1:5173/foo?bar=baz").as_deref(),
                Some("http://127.0.0.1:5173")
            );
            assert_eq!(
                web_favicon_origin("https://example.com/path").as_deref(),
                Some("https://example.com")
            );
        }

        #[test]
        fn web_favicon_origin_ignores_non_http_urls() {
            assert!(web_favicon_origin("file:///tmp/app/index.html").is_none());
            assert!(web_favicon_origin("capsule://ato.run/koh0920/app").is_none());
            assert!(web_favicon_origin("not a url").is_none());
        }
    }
}

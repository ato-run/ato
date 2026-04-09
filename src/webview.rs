use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
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
use wry::{
    NewWindowResponse, PageLoadEvent, Rect, RequestAsyncResponder, WebContext, WebView,
    WebViewBuilder,
};
#[cfg(target_os = "macos")]
use wry::WebViewExtMacOS;

use crate::bridge::{BridgeProxy, GuestBridgeResponse, GuestSessionContext, ShellEvent};
use crate::orchestrator::{resolve_and_start_guest, stop_guest_session, GuestLaunchSession};
use crate::state::{
    ActiveWebPane, ActivityTone, AppState, AuthMode, AuthPolicyRegistry, AuthSessionStatus,
    BrowserCommandKind, GuestRoute, PaneBounds, ShellMode, WebSessionState,
};

const DEVTOOLS_DEBUG_ENV: &str = "ATO_DESKTOP_DEVTOOLS_DEBUG";

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
    session: Result<GuestLaunchSession, String>,
}

impl WebViewManager {
    pub fn new(window_handle: AnyWindowHandle, async_app: AsyncApp) -> Self {
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
        }
    }

    pub fn sync_from_state(&mut self, window: &Window, state: &mut AppState) {
        // Drain auth handoff signals from navigation handlers before any other reconciliation.
        let auth_signals: Vec<AuthHandoffSignal> = {
            let mut q = self.pending_auth_handoffs.lock().unwrap_or_else(|e| e.into_inner());
            q.drain(..).collect()
        };
        for signal in auth_signals {
            let session_id = state.begin_auth_handoff(signal.pane_id, &signal.url);
            if let Some(s) = state.auth_sessions.iter_mut().find(|s| s.session_id == session_id) {
                s.status = AuthSessionStatus::OpenedInBrowser;
            }
            let _ = Command::new("open").arg(&signal.url).status();
        }

        // Pull bridge activity into app state first so rebuilds always see the latest guest messages.
        state.extend_activity(self.bridge.drain_activity());
        let shell_events = self.bridge.drain_shell_events();
        self.apply_shell_events(&shell_events);
        state.apply_shell_events(shell_events);
        self.drain_pending_launches(window, state);

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
                self.stop_launched_session(&previous, state);
                state.sync_web_session_state(previous.pane_id, WebSessionState::Closed);
            }

            match &active.route {
                GuestRoute::CapsuleHandle { handle, .. } => {
                    self.ensure_pending_local_launch(active.pane_id, &route_key, handle, state);
                }
                _ => match self.build_webview(window, &active, None, state.auth_policy_registry.clone()) {
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

            if devtools_debug_enabled() && (needs_resize || bounds_changed(existing.bounds, webview_bounds)) {
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

    fn apply_shell_events(&mut self, events: &[ShellEvent]) {
        for event in events {
            if let ShellEvent::UrlChanged { pane_id, url } = event {
                if let Some(view) = self.views.get_mut(pane_id) {
                    if let Ok(parsed) = url.parse() {
                        view.route = GuestRoute::ExternalUrl(parsed);
                        view.route_key = url.clone();
                    }
                }
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
                            session: Err(
                                "guest session worker disconnected before completion".to_string()
                            ),
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
                    self.stop_guest_session_record(&session, state);
                }
                continue;
            };

            if active.pane_id != pane_id || active.route.to_string() != completed.route_key {
                if let Ok(session) = completed.session {
                    self.stop_guest_session_record(&session, state);
                }
                continue;
            }

            match completed.session {
                Ok(session) => match self.build_webview(window, &active, Some(session), state.auth_policy_registry.clone()) {
                    Ok(webview) => {
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
                    }
                },
                Err(error) => {
                    state.sync_web_session_state(active.pane_id, WebSessionState::Closed);
                    state.push_activity(
                        ActivityTone::Error,
                        format!("Failed to start guest session: {error}"),
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

        let (sender, receiver) = channel();
        let route_key = route_key.to_string();
        let handle = handle.to_string();
        let background_executor = self.async_app.background_executor().clone();
        let foreground_executor = self.async_app.foreground_executor().clone();
        let async_app = self.async_app.clone();
        let window_handle = self.window_handle;

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
            let result = PendingLaunchResult {
                route_key,
                session: resolve_and_start_guest(&handle).map_err(|error| error.to_string()),
            };

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
        let scheme = self.protocol_router.scheme_for(&pane.partition_id);
        let mut launched_session = None;
        let mut session_context = None;
        let build_flags = build_flags_for_route(&pane.route);

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
                launched_session = Some(session.clone());
                (
                    format!("{scheme}://{session_id}{frontend_path}"),
                    Some(format!("{scheme}://{session_id}/__ato/bridge")),
                    session.capabilities.clone(),
                    RouteContent::GuestAssets(session.clone()),
                    Some(session.session_payload()),
                )
            }
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

        if !matches!(build_flags.page_load_behavior, PageLoadBehavior::None) {
            let bridge = self.bridge.clone();
            let pane_id = pane.pane_id;
            let page_load_behavior = build_flags.page_load_behavior;
            let async_app = self.async_app.clone();
            let window_handle = self.window_handle;
            builder = builder.with_on_page_load_handler(move |event, url| {
                if !matches!(event, PageLoadEvent::Finished) {
                    return;
                }

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
    matches!(route, GuestRoute::Capsule { .. } | GuestRoute::CapsuleHandle { .. })
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
    let mtm = MainThreadMarker::new().context("macOS frame host must be created on the main thread")?;
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
}

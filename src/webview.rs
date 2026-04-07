use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::thread;

use anyhow::{Context, Result};
use gpui::{AnyWindowHandle, AppContext, AsyncApp, Window};
use http::header::CONTENT_TYPE;
use wry::http::{Request, Response};
use wry::{PageLoadEvent, Rect, RequestAsyncResponder, WebContext, WebView, WebViewBuilder};

use crate::bridge::{BridgeProxy, GuestBridgeResponse, GuestSessionContext, ShellEvent};
use crate::orchestrator::{resolve_and_start_guest, stop_guest_session, GuestLaunchSession};
use crate::state::{
    ActiveWebPane, ActivityTone, AppState, BrowserCommandKind, GuestRoute, PaneBounds, ShellMode,
    WebSessionState,
};

pub struct WebViewManager {
    views: HashMap<usize, ManagedWebView>,
    pending_launches: HashMap<String, PendingLaunch>,
    active_pane_id: Option<usize>,
    async_app: AsyncApp,
    window_handle: AnyWindowHandle,
    preload_registry: PreloadRegistry,
    protocol_router: ProtocolRouter,
    bridge: BridgeProxy,
    visibility_cache: HashMap<usize, bool>,
}

struct ManagedWebView {
    pane_id: usize,
    route: GuestRoute,
    route_key: String,
    bounds: PaneBounds,
    launched_session: Option<GuestLaunchSession>,
    webview: WebView,
    _context: WebContext,
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
            async_app,
            window_handle,
            preload_registry: PreloadRegistry,
            protocol_router: ProtocolRouter,
            bridge: BridgeProxy::new(),
            visibility_cache: HashMap::new(),
        }
    }

    pub fn sync_from_state(&mut self, window: &Window, state: &mut AppState) {
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
                GuestRoute::LocalCapsule { handle, .. } => {
                    if matches!(active.session, WebSessionState::Launching) {
                        self.ensure_pending_local_launch(active.pane_id, &route_key, handle, state);
                    }
                }
                _ => match self.build_webview(window, &active, None) {
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
            if bounds_changed(existing.bounds, webview_bounds) {
                if let Err(error) = existing.webview.set_bounds(bounds_to_rect(webview_bounds)) {
                    state.push_activity(
                        ActivityTone::Error,
                        format!("Failed to resize child webview: {error}"),
                    );
                } else {
                    existing.bounds = webview_bounds;
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
                Ok(session) => match self.build_webview(window, &active, Some(session)) {
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
            GuestRoute::LocalCapsule { .. } => {
                let session = local_session.ok_or_else(|| {
                    anyhow::anyhow!("local capsule webview build requires resolved guest session")
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
                session_context = Some(GuestSessionContext {
                    pane_id: pane.pane_id,
                    session_id: session.session_id.clone(),
                    adapter: session.adapter.clone(),
                    invoke_url: session.invoke_url.clone(),
                    app_root: session.app_root.clone(),
                });
                launched_session = Some(session.clone());
                (
                    format!("{scheme}://{session_id}{}", session.frontend_url_path()),
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

        let webview = builder
            .with_url(&url)
            .build_as_child(window)
            .with_context(|| format!("unable to create Wry child webview for {url}"))?;

        Ok(ManagedWebView {
            pane_id: pane.pane_id,
            route: pane.route.clone(),
            route_key: pane.route.to_string(),
            bounds: webview_bounds,
            launched_session,
            webview,
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

        if let Some(view) = self.views.get_mut(&pane_id) {
            if let Err(error) = view.webview.set_visible(visible) {
                state.push_activity(
                    ActivityTone::Error,
                    format!("Failed to update child webview visibility: {error}"),
                );
                return;
            }
        }

        self.visibility_cache.insert(pane_id, visible);
    }
}

impl Drop for WebViewManager {
    fn drop(&mut self) {
        // Best-effort shutdown so orphaned guest sessions do not survive process exit.
        for existing in self.views.drain().map(|(_, existing)| existing) {
            if let Some(session) = existing.launched_session {
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
        GuestRoute::ExternalUrl(_) => BuildFlags {
            inject_bridge: false,
            enable_ipc: false,
            enable_custom_protocol: false,
            page_load_behavior: PageLoadBehavior::UpdateExternalUrl,
            observe_title_changes: true,
        },
        GuestRoute::Capsule { .. } | GuestRoute::LocalCapsule { .. } => BuildFlags {
            inject_bridge: true,
            enable_ipc: true,
            enable_custom_protocol: true,
            page_load_behavior: PageLoadBehavior::MarkCapsuleReady,
            observe_title_changes: false,
        },
    }
}

fn pending_launch_key(pane_id: usize, route_key: &str) -> String {
    format!("{pane_id}:{route_key}")
}

fn route_requires_ready_signal(route: &GuestRoute) -> bool {
    matches!(
        route,
        GuestRoute::Capsule { .. } | GuestRoute::LocalCapsule { .. }
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
            crate::state::PaneSurface::Native { .. } | crate::state::PaneSurface::Launcher => None,
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
        session.frontend_url_path()
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

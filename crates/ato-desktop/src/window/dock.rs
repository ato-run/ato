//! Dock window — mounts a Wry WebView loading the local
//! `ato-dock` system capsule HTML from
//! `assets/system/ato-dock/index.html`.
//!
//! The HTML is served via a `capsule-dock://` custom protocol
//! handler so that WKWebView receives it with a proper origin.
//!
//! The Dock is the developer hub: it starts with ato login if the
//! user is not authenticated, then flows to onboarding or the main
//! capsule management console. URL: `capsule://run.ato.desktop/dock`

use std::borrow::Cow;
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context, IntoElement, Render, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use serde_json::{json, Value};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::Response;
use wry::{Rect, WebView, WebViewBuilder};

use crate::localization::{compose_init_script, resolve_locale, tr};
use crate::orchestrator::resolve_ato_binary;
use crate::system_capsule::ipc as system_ipc;

const DOCK_SCHEME: &str = "capsule-dock";

/// Slot tracking the single open Dock window.
#[derive(Default)]
pub struct DockWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for DockWindowSlot {}

/// Slot tracking the live `DockWebView` entity so we can inject
/// identity updates into the existing WebView without closing the window.
#[derive(Default)]
pub struct DockEntitySlot(pub Option<gpui::Entity<DockWebView>>);
impl gpui::Global for DockEntitySlot {}

/// Lightweight GPUI entity whose only job is to keep the Wry
/// `WebView` alive for the lifetime of its window.
pub struct DockWebView {
    _webview: WebView,
    /// Shared identity state — updated by `notify_login_success` before reload.
    identity_state: Arc<Mutex<Value>>,
}

impl Render for DockWebView {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div().size_full().bg(rgb(0xffffff))
    }
}

const DOCK_HTML: &str =
    include_str!("../../assets/system/ato-dock/index.html");

/// Shell out to `ato whoami` to fetch authentication state.
/// Returns JSON matching the identity_window pattern.
fn fetch_identity() -> Value {
    let bin = match resolve_ato_binary() {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("fetch_identity: binary not found: {:?}", e);
            return json!({ "authenticated": false, "reason": "binary_not_found" });
        }
    };
    let output = match Command::new(&bin)
        .arg("whoami")
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("fetch_identity: whoami failed: {:?}", e);
            return json!({ "authenticated": false, "reason": "whoami_failed" });
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    tracing::info!("fetch_identity: whoami stdout = {:?}", stdout.trim());
    if !stdout.contains("✅ Authenticated") {
        tracing::info!("fetch_identity: not authenticated");
        return json!({ "authenticated": false, "reason": "not_authenticated" });
    }
    let mut user_id = None::<String>;
    let mut name = None::<String>;
    let mut email = None::<String>;
    let mut github = None::<String>;
    let mut publisher_handle = None::<String>;
    for line in stdout.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("User ID: ") { user_id = Some(rest.trim().to_string()); }
        else if let Some(rest) = line.strip_prefix("Name: ") { name = Some(rest.trim().to_string()); }
        else if let Some(rest) = line.strip_prefix("Email: ") { email = Some(rest.trim().to_string()); }
        else if let Some(rest) = line.strip_prefix("GitHub: @") { github = Some(rest.trim().to_string()); }
        else if let Some(rest) = line.strip_prefix("Publisher Handle: ") { publisher_handle = Some(rest.trim().to_string()); }
    }
    json!({ "authenticated": true, "user_id": user_id, "name": name, "email": email, "github": github, "publisher_handle": publisher_handle })
}

/// Open the Dock window. On a 2nd+ click the existing
/// window gets focused / brought to front rather than spawning a
/// duplicate. Returns the GPUI `WindowHandle`.
pub fn open_dock_window(cx: &mut App) -> Result<AnyWindowHandle> {
    // Focus-on-existing
    let existing = cx.global::<DockWindowSlot>().0;
    if let Some(handle) = existing {
        let result = handle.update(cx, |_, window, _| window.activate_window());
        match result {
            Ok(()) => return Ok(handle),
            Err(_) => {
                cx.set_global(DockWindowSlot(None));
            }
        }
    }

    let config = crate::config::load_config();
    let locale = resolve_locale(config.general.language);

    let identity = fetch_identity();
    // Share identity state so notify_login_success can update it before reload.
    let identity_state: Arc<Mutex<Value>> = Arc::new(Mutex::new(identity.clone()));
    let identity_state_for_protocol = identity_state.clone();

    // i18n-only init script (identity is now embedded directly in HTML)
    let init_script = compose_init_script(locale, None);

    let win_size = size(px(1100.0), px(760.0));
    let bounds = match cx.primary_display() {
        Some(d) => {
            let db = d.bounds();
            let left = db.origin.x + (db.size.width - win_size.width) / 2.0;
            let top = db.origin.y + px(108.0);
            Bounds { origin: gpui::point(left, top), size: win_size }
        }
        None => Bounds::centered(None, win_size, cx),
    };
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    let queue = system_ipc::new_queue();
    let drain_queue = queue.clone();

    let entity_capture: std::rc::Rc<std::cell::RefCell<Option<gpui::Entity<DockWebView>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let entity_capture2 = entity_capture.clone();

    let handle = cx.open_window(options, move |window, cx| {
        let win_size = window.bounds().size;
        let webview_rect = Rect {
            position: LogicalPosition::new(0i32, 0i32).into(),
            size: LogicalSize::new(
                f32::from(win_size.width) as u32,
                f32::from(win_size.height) as u32,
            )
            .into(),
        };
        let url = format!("{}://localhost/", DOCK_SCHEME);
        let webview = WebViewBuilder::new()
            .with_asynchronous_custom_protocol(
                DOCK_SCHEME.to_string(),
                move |_id, _req, responder| {
                    let current_identity = identity_state_for_protocol
                        .lock()
                        .map(|g| g.clone())
                        .unwrap_or_else(|_| json!({ "authenticated": false }));
                    let authenticated = current_identity
                        .get("authenticated")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let identity_json = serde_json::to_string(&current_identity)
                        .unwrap_or_else(|_| "null".to_string());
                    // Embed identity directly in the HTML so it is available
                    // before any scripts run — more reliable than WKUserScript.
                    let inject = format!(
                        "<head><script>window.__ATO_IDENTITY={};</script>",
                        identity_json
                    );
                    let html = DOCK_HTML.replacen("<head>", &inject, 1);
                    let injected = html.contains("window.__ATO_IDENTITY");
                    let html_prefix = &html[..html.len().min(180)];
                    tracing::info!(
                        authenticated,
                        injected,
                        html_prefix,
                        "dock protocol handler: serving HTML with identity"
                    );
                    let body: Cow<'static, [u8]> = Cow::Owned(html.into_bytes());
                    let response = Response::builder()
                        .header("Content-Type", "text/html; charset=utf-8")
                        .header("Cache-Control", "no-store, no-cache")
                        .body(body)
                        .expect("dev console HTML response must build");
                    responder.respond(response);
                },
            )
            .with_url(&url)
            .with_initialization_script(&init_script)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue.clone()))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Dev Console WebView");
        let view = cx.new(|_cx| DockWebView { _webview: webview, identity_state: identity_state.clone() });
        *entity_capture2.borrow_mut() = Some(view.clone());
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    })?;
    cx.set_global(DockWindowSlot(Some(*handle)));
    cx.set_global(DockEntitySlot(entity_capture.borrow_mut().take()));

    use crate::window::content_windows::{
        ContentWindowEntry, ContentWindowKind, OpenContentWindows,
    };
    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::Dock,
            title: gpui::SharedString::from(tr(locale, "dock.title")),
            subtitle: gpui::SharedString::from(tr(locale, "dock.subtitle")),
            url: gpui::SharedString::from("capsule://run.ato.desktop/dock"),
            last_focused_at: std::time::Instant::now(),
        },
    );
    system_ipc::spawn_drain_loop(cx, drain_queue, *handle);
    Ok(*handle)
}

/// Update the existing Dock WebView's identity after a successful login and reload the page.
///
/// Strategy:
/// 1. Update the shared `identity_state` Arc so the protocol handler embeds the new identity
///    in the HTML on the next request.
/// 2. Call `load_url` with a timestamp-suffixed URL to force WKWebView to navigate (different
///    URL avoids the same-URL no-op) and serve a fresh HTML with authenticated identity.
///
/// NOTE: We do NOT use `evaluate_script` here. It is unreliable for driving page transitions
/// because its completion callback fires asynchronously and mixing it with `load_url` cancels
/// the pending JS evaluation. Pure `load_url` with a different URL is the guaranteed approach.
///
/// Falls back to opening a fresh Dock if no live entity is tracked.
pub fn notify_login_success(cx: &mut App) {
    let entity = cx.try_global::<DockEntitySlot>().and_then(|s| s.0.clone());
    if let Some(entity) = entity {
        let identity = fetch_identity();
        let authenticated = identity.get("authenticated").and_then(|v| v.as_bool()).unwrap_or(false);
        tracing::info!(authenticated, "notify_login_success: updating identity and reloading dock WebView");

        entity.update(cx, |view, _cx| {
            // 1. Update Arc so the protocol handler serves authenticated HTML on next load.
            if let Ok(mut guard) = view.identity_state.lock() {
                *guard = identity;
            }
            // 2. Navigate to a timestamp-suffixed URL.
            //    The URL differs from the current one, so WKWebView performs a real navigation.
            //    The protocol handler ignores query params and serves the same HTML
            //    but now with authenticated identity embedded.
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let reload_url = format!("{}://localhost/?t={}", DOCK_SCHEME, ts);
            tracing::info!(reload_url, "notify_login_success: navigating to cache-busted URL");
            match view._webview.load_url(&reload_url) {
                Ok(()) => tracing::info!("notify_login_success: load_url dispatched"),
                Err(e) => tracing::warn!("notify_login_success: load_url failed: {:?}", e),
            }
        });

        // Bring dock window to front.
        if let Some(handle) = cx.try_global::<DockWindowSlot>().and_then(|s| s.0) {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        }
    } else {
        tracing::info!("notify_login_success: no live dock entity; opening fresh dock window");
        let _ = open_dock_window(cx);
    }
}

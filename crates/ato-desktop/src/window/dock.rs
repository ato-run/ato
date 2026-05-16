//! Dock window — mounts a Wry WebView loading the local
//! `ato-dock` system capsule HTML from
//! `assets/system/ato-dock/index.html`.
//!
//! The HTML is served via a `capsule-dock://` custom protocol
//! handler so WKWebView receives it with a proper origin.
//!
//! The Dock hosts the real publisher flow: source preparation,
//! manifest editing, verification, preview, and submit. All long-
//! running work stays off the GPUI thread and reports structured
//! events back into the WebView via `window.__ATO_DOCK_EVENT__(...)`.

use std::borrow::Cow;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context, IntoElement, Render, Window,
    WindowBounds, WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::Response;
use wry::{Rect, WebView, WebViewBuilder};

use crate::localization::{compose_init_script, resolve_locale, tr};
use crate::orchestrator::resolve_ato_binary;
use crate::state::GuestRoute;
use crate::system_capsule::ato_dock::DockSourceKind;
use crate::system_capsule::ipc as system_ipc;
use crate::system_capsule::manifest::system_capsule_url;
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

const DOCK_SCHEME: &str = "capsule-dock";
const DOCK_HTML: &str = include_str!("../../assets/system/ato-dock/index.html");

/// Slot tracking the single open Dock window.
#[derive(Default)]
pub struct DockWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for DockWindowSlot {}

/// Slot tracking the live `DockWebView` entity so background tasks can
/// stream results into the existing WebView.
#[derive(Default)]
pub struct DockEntitySlot(pub Option<gpui::Entity<DockWebView>>);
impl gpui::Global for DockEntitySlot {}

type DockEventQueue = Arc<Mutex<Vec<Value>>>;

#[derive(Clone)]
struct PreviewProcess {
    control_tx: Sender<PreviewControl>,
}

#[derive(Clone, Copy)]
enum PreviewControl {
    Stop,
}

struct DockRuntimeState {
    session_id: String,
    source_kind: Option<DockSourceKind>,
    source_value: Option<String>,
    working_directory: Option<PathBuf>,
    manifest_toml: Option<String>,
    latest_publish_json: Option<Value>,
    preview: Option<PreviewProcess>,
    preview_url: Option<String>,
    event_queue: DockEventQueue,
}

impl DockRuntimeState {
    fn new() -> Self {
        Self {
            session_id: new_dock_session_id(),
            source_kind: None,
            source_value: None,
            working_directory: None,
            manifest_toml: None,
            latest_publish_json: None,
            preview: None,
            preview_url: None,
            event_queue: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// Lightweight GPUI entity whose only job is to keep the Wry `WebView`
/// alive for the lifetime of its window and evaluate host events into
/// the page.
pub struct DockWebView {
    pub(crate) webview: WebView,
    identity_state: Arc<Mutex<Value>>,
    runtime_state: Arc<Mutex<DockRuntimeState>>,
    paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(DockWebView, paste);

impl WebViewPasteShell for DockWebView {
    fn active_paste_target(&self) -> Option<&WebView> {
        Some(&self.webview)
    }
}

impl DockWebView {
    fn emit_event(&mut self, event: &Value) {
        let payload = serde_json::to_string(event).unwrap_or_else(|_| "null".to_string());
        let script = format!("window.__ATO_DOCK_EVENT__ && window.__ATO_DOCK_EVENT__({payload});");
        if let Err(error) = self.webview.evaluate_script(&script) {
            tracing::warn!(?error, "dock: evaluate_script event dispatch failed");
        }
    }
}

impl Render for DockWebView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        paste_render_wrap!(
            div().size_full().bg(rgb(0xffffff)),
            cx,
            &self.paste.focus_handle
        )
    }
}

pub fn open_external_url(cx: &mut App, url: &str) -> Result<()> {
    let parsed = Url::parse(url).with_context(|| format!("Invalid URL: {url}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!("Dock can open only http(s) URLs");
    }
    crate::window::open_app_window(cx, GuestRoute::ExternalUrl(parsed)).map(|_| ())
}

pub fn open_settings(cx: &mut App) -> Result<()> {
    crate::window::settings_window::open_settings_window(cx).map(|_| ())
}

pub fn prepare_source(
    cx: &mut App,
    request_id: String,
    source_kind: DockSourceKind,
    value: String,
) -> Result<()> {
    let runtime = dock_runtime(cx)?;
    queue_runtime_event(
        &runtime,
        json!({
            "kind": "dock:phase_started",
            "request_id": request_id,
            "phase": "source_input",
            "message": "Preparing source",
        }),
    );

    thread::spawn(move || {
        let result = prepare_source_blocking(&runtime, source_kind, &value);
        match result {
            Ok(prepared) => {
                if let Ok(mut guard) = runtime.lock() {
                    stop_preview_via_runtime(&mut guard);
                    guard.source_kind = Some(source_kind);
                    guard.source_value = Some(value.clone());
                    guard.working_directory = Some(prepared.working_directory.clone());
                    guard.manifest_toml = Some(prepared.manifest_toml.clone());
                    guard.preview_url = None;
                }
                queue_runtime_event(
                    &runtime,
                    json!({
                        "kind": "dock:phase_completed",
                        "request_id": request_id,
                        "phase": "source_input",
                        "message": "Source is ready",
                        "source_kind": source_kind_label(source_kind),
                        "session_id": prepared.session_id,
                        "working_directory": prepared.working_directory.display().to_string(),
                        "manifest_path": prepared.manifest_path.display().to_string(),
                        "manifest_toml": prepared.manifest_toml,
                        "manifest_inference": prepared.manifest_inference,
                    }),
                );
            }
            Err(error) => {
                queue_runtime_event(
                    &runtime,
                    json!({
                        "kind": "dock:phase_failed",
                        "request_id": request_id,
                        "phase": "source_input",
                        "message": error.to_string(),
                    }),
                );
            }
        }
    });

    Ok(())
}

pub fn save_manifest(cx: &mut App, request_id: String, manifest_toml: String) -> Result<()> {
    let runtime = dock_runtime(cx)?;
    queue_runtime_event(
        &runtime,
        json!({
            "kind": "dock:phase_started",
            "request_id": request_id,
            "phase": "manifest",
            "message": "Saving manifest and syncing lockfile…",
        }),
    );

    thread::spawn(move || {
        let result = save_manifest_blocking(&runtime, &manifest_toml);
        match result {
            Ok(path) => {
                if let Ok(mut guard) = runtime.lock() {
                    guard.manifest_toml = Some(manifest_toml.clone());
                }
                queue_runtime_event(
                    &runtime,
                    json!({
                        "kind": "dock:phase_completed",
                        "request_id": request_id,
                        "phase": "manifest",
                        "message": "Draft saved and lockfile synced",
                        "manifest_path": path.display().to_string(),
                        "manifest_toml": manifest_toml,
                    }),
                );
            }
            Err(error) => {
                queue_runtime_event(
                    &runtime,
                    json!({
                        "kind": "dock:phase_failed",
                        "request_id": request_id,
                        "phase": "manifest",
                        "message": error.to_string(),
                    }),
                );
            }
        }
    });

    Ok(())
}

pub fn run_publish_phase(cx: &mut App, request_id: String) -> Result<()> {
    let runtime = dock_runtime(cx)?;
    queue_runtime_event(
        &runtime,
        json!({
            "kind": "dock:phase_started",
            "request_id": request_id,
            "phase": "verification",
            "message": "Running ato publish --build --json",
        }),
    );

    thread::spawn(
        move || match run_publish_command(&runtime, &["publish", "--build", "--json"]) {
            Ok(payload) => {
                if let Ok(mut guard) = runtime.lock() {
                    guard.latest_publish_json = Some(payload.clone());
                }
                enqueue_publish_phase_events(&runtime, &request_id, "verification", &payload);
                queue_runtime_event(
                    &runtime,
                    json!({
                        "kind": "dock:phase_completed",
                        "request_id": request_id,
                        "phase": "verification",
                        "message": payload.get("message").and_then(Value::as_str).unwrap_or("Verification completed"),
                        "payload": payload,
                    }),
                );
            }
            Err(error) => {
                queue_runtime_event(
                    &runtime,
                    json!({
                        "kind": "dock:phase_failed",
                        "request_id": request_id,
                        "phase": "verification",
                        "message": error.to_string(),
                    }),
                );
            }
        },
    );

    Ok(())
}

pub fn start_preview(cx: &mut App, request_id: String) -> Result<()> {
    let runtime = dock_runtime(cx)?;
    queue_runtime_event(
        &runtime,
        json!({
            "kind": "dock:phase_started",
            "request_id": request_id,
            "phase": "preview",
            "message": "Starting local preview",
        }),
    );

    thread::spawn(move || {
        if let Ok(mut guard) = runtime.lock() {
            stop_preview_via_runtime(&mut guard);
            guard.preview_url = None;
        }

        match start_preview_blocking(&runtime, &request_id) {
            Ok(()) => {
                queue_runtime_event(
                    &runtime,
                    json!({
                        "kind": "dock:phase_completed",
                        "request_id": request_id,
                        "phase": "preview",
                        "message": "Preview process started",
                    }),
                );
            }
            Err(error) => {
                queue_runtime_event(
                    &runtime,
                    json!({
                        "kind": "dock:phase_failed",
                        "request_id": request_id,
                        "phase": "preview",
                        "message": error.to_string(),
                    }),
                );
            }
        }
    });

    Ok(())
}

pub fn stop_preview(cx: &mut App, request_id: Option<String>) -> Result<()> {
    let runtime = dock_runtime(cx)?;
    let stopped = {
        let mut guard = runtime
            .lock()
            .map_err(|_| anyhow::anyhow!("Dock runtime lock poisoned"))?;
        stop_preview_via_runtime(&mut guard)
    };

    queue_runtime_event(
        &runtime,
        json!({
            "kind": if stopped { "dock:phase_completed" } else { "dock:phase_failed" },
            "request_id": request_id,
            "phase": "preview",
            "message": if stopped { "Stopping preview" } else { "No preview is running" },
        }),
    );
    Ok(())
}

pub fn submit_publish(cx: &mut App, request_id: String, visibility: Option<String>) -> Result<()> {
    let runtime = dock_runtime(cx)?;
    queue_runtime_event(
        &runtime,
        json!({
            "kind": "dock:phase_started",
            "request_id": request_id,
            "phase": "submit",
            "message": "Running ato publish --deploy --json",
            "visibility": visibility,
        }),
    );

    thread::spawn(move || {
        match run_publish_command(&runtime, &["publish", "--deploy", "--json"]) {
            Ok(payload) => {
                if let Ok(mut guard) = runtime.lock() {
                    guard.latest_publish_json = Some(payload.clone());
                }
                enqueue_publish_phase_events(&runtime, &request_id, "submit", &payload);

                // Apply visibility via the store API when the user chose public or limited.
                let vis_str = visibility.as_deref().unwrap_or("public");
                let visibility_update = if vis_str != "unlisted" {
                    let scoped_id = payload
                        .get("install")
                        .and_then(|i| i.get("scoped_id"))
                        .and_then(Value::as_str)
                        .or_else(|| payload.get("scoped_id").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_string();
                    let api_base = std::env::var("ATO_STORE_API_URL")
                        .unwrap_or_else(|_| "https://api.ato.run".to_string());
                    let token = read_session_token_from_credentials();
                    if !scoped_id.is_empty() {
                        if let Some(tok) = token {
                            match apply_visibility_after_publish(&scoped_id, vis_str, &api_base, &tok)
                            {
                                Ok(()) => {
                                    tracing::info!(
                                        scoped_id,
                                        vis_str,
                                        "dock: visibility updated"
                                    );
                                    json!({ "ok": true, "visibility": vis_str })
                                }
                                Err(err) => {
                                    tracing::warn!(?err, "dock: visibility update failed");
                                    json!({ "ok": false, "error": err.to_string() })
                                }
                            }
                        } else {
                            tracing::warn!("dock: no session token; skipping visibility update");
                            json!({ "ok": false, "error": "no session token" })
                        }
                    } else {
                        tracing::warn!("dock: no scoped_id in publish payload; skipping visibility update");
                        json!({ "ok": false, "error": "no scoped_id in publish payload" })
                    }
                } else {
                    json!({ "ok": true, "visibility": "unlisted" })
                };

                queue_runtime_event(
                    &runtime,
                    json!({
                        "kind": "dock:submit_completed",
                        "request_id": request_id,
                        "phase": "submit",
                        "message": payload.get("message").and_then(Value::as_str).unwrap_or("Publish completed"),
                        "payload": payload,
                        "visibility": visibility,
                        "visibility_update": visibility_update,
                    }),
                );
            }
            Err(error) => {
                queue_runtime_event(
                    &runtime,
                    json!({
                        "kind": "dock:phase_failed",
                        "request_id": request_id,
                        "phase": "submit",
                        "message": error.to_string(),
                        "visibility": visibility,
                    }),
                );
            }
        }
    });

    Ok(())
}

pub fn cleanup_dock_window(cx: &mut App) {
    if let Ok(runtime) = dock_runtime(cx) {
        if let Ok(mut guard) = runtime.lock() {
            stop_preview_via_runtime(&mut guard);
        }
    }
    cx.set_global(DockWindowSlot(None));
    cx.set_global(DockEntitySlot(None));
}

/// Shell out to `ato whoami` to fetch authentication state.
/// Returns JSON matching the identity window pattern.
fn fetch_identity() -> Value {
    let bin = match resolve_ato_binary() {
        Ok(b) => b,
        Err(error) => {
            tracing::warn!(?error, "dock: ato binary not found");
            return json!({ "authenticated": false, "reason": "binary_not_found" });
        }
    };
    let output = match Command::new(&bin)
        .arg("whoami")
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(?error, "dock: `ato whoami` failed");
            return json!({ "authenticated": false, "reason": "whoami_failed" });
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("✅ Authenticated") {
        return json!({ "authenticated": false, "reason": "not_authenticated" });
    }

    let mut user_id = None::<String>;
    let mut name = None::<String>;
    let mut email = None::<String>;
    let mut github = None::<String>;
    let mut publisher_handle = None::<String>;
    for line in stdout.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("User ID: ") {
            user_id = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Name: ") {
            name = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Email: ") {
            email = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("GitHub: @") {
            github = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Publisher Handle: ") {
            publisher_handle = Some(rest.trim().to_string());
        }
    }

    json!({
        "authenticated": true,
        "user_id": user_id,
        "name": name,
        "email": email,
        "github": github,
        "publisher_handle": publisher_handle,
    })
}

/// Open the Dock window. On a 2nd+ click the existing
/// window gets focused / brought to front rather than spawning a
/// duplicate. Returns the GPUI `WindowHandle`.
pub fn open_dock_window(cx: &mut App) -> Result<AnyWindowHandle> {
    let existing = cx.global::<DockWindowSlot>().0;
    if let Some(handle) = existing {
        let result = handle.update(cx, |_, window, _| window.activate_window());
        match result {
            Ok(()) => return Ok(handle),
            Err(_) => {
                cx.set_global(DockWindowSlot(None));
                cx.set_global(DockEntitySlot(None));
            }
        }
    }

    let config = crate::config::load_config();
    let locale = resolve_locale(config.general.language);
    let identity = fetch_identity();
    let identity_state: Arc<Mutex<Value>> = Arc::new(Mutex::new(identity.clone()));
    let identity_state_for_protocol = identity_state.clone();
    let runtime_state = Arc::new(Mutex::new(DockRuntimeState::new()));
    let runtime_state_for_protocol = runtime_state.clone();
    let queue = runtime_state
        .lock()
        .map(|state| state.event_queue.clone())
        .map_err(|_| anyhow::anyhow!("Dock runtime lock poisoned"))?;

    // Compose the init script: i18n strings first, then the automation
    // agent so `window.__atoAgent` is available for MCP automation.
    let init_script = format!(
        "{}\n{}",
        compose_init_script(locale, None),
        include_str!("../../assets/automation/agent.js"),
    );
    let win_size = size(px(1100.0), px(760.0));
    let bounds = match cx.primary_display() {
        Some(display) => {
            let db = display.bounds();
            let left = db.origin.x + (db.size.width - win_size.width) / 2.0;
            let top = db.origin.y + px(108.0);
            Bounds {
                origin: gpui::point(left, top),
                size: win_size,
            }
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
    let bridge_queue = system_ipc::new_queue();
    let drain_queue = bridge_queue.clone();

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
        let url = format!("{DOCK_SCHEME}://localhost/");

        // Clone the automation host so the page-load closure can call
        // mark_page_loaded without capturing a non-Send type.
        let automation_for_load = cx
            .try_global::<crate::automation::AutomationHost>()
            .cloned();

        let webview = WebViewBuilder::new()
            .with_asynchronous_custom_protocol(
                DOCK_SCHEME.to_string(),
                move |_id, _req, responder| {
                    let current_identity = identity_state_for_protocol
                        .lock()
                        .map(|guard| guard.clone())
                        .unwrap_or_else(|_| json!({ "authenticated": false }));
                    let runtime_snapshot = runtime_state_for_protocol
                        .lock()
                        .map(|guard| {
                            json!({
                                "session_id": guard.session_id,
                                "source_kind": guard.source_kind.map(source_kind_label),
                                "working_directory": guard
                                    .working_directory
                                    .as_ref()
                                    .map(|path| path.display().to_string()),
                                "manifest_toml": guard.manifest_toml,
                                "latest_publish_json": guard.latest_publish_json,
                                "preview_url": guard.preview_url,
                            })
                        })
                        .unwrap_or_else(|_| json!({}));
                    let inject = format!(
                        "<head><script>window.__ATO_IDENTITY={};window.__ATO_DOCK_BOOTSTRAP={};</script>",
                        serde_json::to_string(&current_identity)
                            .unwrap_or_else(|_| "null".to_string()),
                        serde_json::to_string(&runtime_snapshot)
                            .unwrap_or_else(|_| "null".to_string()),
                    );
                    let html = DOCK_HTML.replacen("<head>", &inject, 1);
                    let body: Cow<'static, [u8]> = Cow::Owned(html.into_bytes());
                    let response = Response::builder()
                        .header("Content-Type", "text/html; charset=utf-8")
                        .header("Cache-Control", "no-store, no-cache")
                        .body(body)
                        .expect("dock HTML response must build");
                    responder.respond(response);
                },
            )
            .with_url(&url)
            .with_initialization_script(&init_script)
            .with_on_page_load_handler(move |event, _url| {
                use wry::PageLoadEvent;
                if matches!(event, PageLoadEvent::Finished) {
                    if let Some(automation) = &automation_for_load {
                        automation.mark_page_loaded(
                            crate::webview::DOCK_AUTOMATION_PANE_ID,
                        );
                    }
                } else if matches!(event, PageLoadEvent::Started) {
                    if let Some(automation) = &automation_for_load {
                        automation.mark_page_unloaded(
                            crate::webview::DOCK_AUTOMATION_PANE_ID,
                        );
                    }
                }
            })
            .with_ipc_handler(system_ipc::make_ipc_handler(bridge_queue.clone()))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Dock WebView");
        let view = cx.new(|cx| DockWebView {
            webview,
            identity_state: identity_state.clone(),
            runtime_state: runtime_state.clone(),
            paste: WebViewPasteSupport::new(cx),
        });
        *entity_capture2.borrow_mut() = Some(view.clone());
        // Give GPUI focus to DockWebView so NativePaste/NativeCopy
        // key bindings dispatch here even when WKWebView has OS first-responder.
        window.focus(&view.read(cx).paste.focus_handle.clone(), cx);
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
            url: gpui::SharedString::from(system_capsule_url("dock")),
            capsule: None,
            last_focused_at: std::time::Instant::now(),
        },
    );
    system_ipc::spawn_drain_loop(cx, drain_queue, *handle);
    spawn_dock_event_loop(cx, queue, *handle);
    Ok(*handle)
}

/// Update the existing Dock WebView's identity after a successful login and reload the page.
pub fn notify_login_success(cx: &mut App) {
    let entity = cx
        .try_global::<DockEntitySlot>()
        .and_then(|slot| slot.0.clone());
    if let Some(entity) = entity {
        let identity = fetch_identity();
        entity.update(cx, |view, _cx| {
            if let Ok(mut guard) = view.identity_state.lock() {
                *guard = identity;
            }
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            let reload_url = format!("{DOCK_SCHEME}://localhost/?t={ts}");
            if let Err(error) = view.webview.load_url(&reload_url) {
                tracing::warn!(?error, "dock: load_url after login failed");
            }
        });

        if let Some(handle) = cx.try_global::<DockWindowSlot>().and_then(|slot| slot.0) {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        }
    } else {
        let _ = open_dock_window(cx);
    }
}

fn dock_runtime(cx: &mut App) -> Result<Arc<Mutex<DockRuntimeState>>> {
    let entity = cx
        .try_global::<DockEntitySlot>()
        .and_then(|slot| slot.0.clone())
        .context("Dock window is not open")?;
    Ok(entity.update(cx, |view, _cx| view.runtime_state.clone()))
}

fn queue_runtime_event(runtime: &Arc<Mutex<DockRuntimeState>>, event: Value) {
    let queue = runtime.lock().ok().map(|guard| guard.event_queue.clone());
    if let Some(queue) = queue {
        if let Ok(mut events) = queue.lock() {
            events.push(event);
        }
    }
}

fn spawn_dock_event_loop(cx: &mut App, queue: DockEventQueue, host: AnyWindowHandle) {
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        loop {
            be.timer(Duration::from_millis(50)).await;
            let drained = match queue.lock() {
                Ok(mut events) => std::mem::take(&mut *events),
                Err(_) => continue,
            };
            if drained.is_empty() {
                let host_alive = aa.update(|cx| host.update(cx, |_, _, _| ()).is_ok());
                if !host_alive {
                    return;
                }
                continue;
            }
            for event in drained {
                aa.update(|cx| {
                    if let Some(entity) = cx
                        .try_global::<DockEntitySlot>()
                        .and_then(|slot| slot.0.clone())
                    {
                        let _ = entity.update(cx, |view, _cx| view.emit_event(&event));
                    }
                });
            }
        }
    })
    .detach();
}

fn prepare_source_blocking(
    runtime: &Arc<Mutex<DockRuntimeState>>,
    source_kind: DockSourceKind,
    value: &str,
) -> Result<PreparedDockSource> {
    let session_id = runtime
        .lock()
        .map_err(|_| anyhow::anyhow!("Dock runtime lock poisoned"))?
        .session_id
        .clone();

    let working_directory = match source_kind {
        DockSourceKind::GithubRepo => clone_public_github_repo(&session_id, value)?,
        DockSourceKind::LocalPath => validate_local_source_path(value)?,
    };
    let manifest = load_manifest_or_template(&working_directory, source_kind, value)?;
    let manifest_path = working_directory.join("capsule.toml");
    Ok(PreparedDockSource {
        session_id,
        working_directory,
        manifest_path,
        manifest_toml: manifest.toml,
        manifest_inference: manifest.inference,
    })
}

fn save_manifest_blocking(
    runtime: &Arc<Mutex<DockRuntimeState>>,
    manifest_toml: &str,
) -> Result<PathBuf> {
    let working_directory = runtime
        .lock()
        .map_err(|_| anyhow::anyhow!("Dock runtime lock poisoned"))?
        .working_directory
        .clone()
        .context("Prepare a source before saving the manifest")?;

    let _: toml::Value = toml::from_str(manifest_toml)
        .with_context(|| "capsule.toml draft is not valid TOML".to_string())?;
    let manifest_path = working_directory.join("capsule.toml");
    let temp_path = working_directory.join(format!(
        ".capsule.toml.{}.tmp",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::write(&temp_path, manifest_toml)
        .with_context(|| format!("Failed to write {}", temp_path.display()))?;
    fs::rename(&temp_path, &manifest_path)
        .with_context(|| format!("Failed to move draft into {}", manifest_path.display()))?;

    // Regenerate lockfile so a subsequent preview does not hit E207.
    let ato_bin = resolve_ato_binary()?;
    let lock_output = Command::new(&ato_bin)
        .arg("lock")
        .current_dir(&working_directory)
        .stdin(Stdio::null())
        .output()
        .with_context(|| "Failed to run `ato lock`")?;
    if !lock_output.status.success() {
        let stderr = String::from_utf8_lossy(&lock_output.stderr);
        anyhow::bail!("Lock sync failed after saving manifest: {}", stderr.trim());
    }

    Ok(manifest_path)
}

fn run_publish_command(runtime: &Arc<Mutex<DockRuntimeState>>, args: &[&str]) -> Result<Value> {
    let working_directory = runtime
        .lock()
        .map_err(|_| anyhow::anyhow!("Dock runtime lock poisoned"))?
        .working_directory
        .clone()
        .context("Prepare a source before running publish")?;
    let ato_bin = resolve_ato_binary()?;
    let output = Command::new(&ato_bin)
        .args(args)
        .current_dir(&working_directory)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("Failed to run `{}`", args.join(" ")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        return parse_publish_json_output(&stdout);
    }

    if let Ok(payload) = parse_publish_json_output(&stdout) {
        return Ok(payload);
    }

    let detail = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else if !stdout.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        format!("`{}` exited with status {}", args.join(" "), output.status)
    };
    anyhow::bail!(detail)
}

fn enqueue_publish_phase_events(
    runtime: &Arc<Mutex<DockRuntimeState>>,
    request_id: &str,
    fallback_phase: &str,
    payload: &Value,
) {
    let Some(phases) = payload.get("phases").and_then(Value::as_array) else {
        return;
    };
    for phase in phases {
        if !phase
            .get("selected")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let phase_name = phase
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(fallback_phase);
        let ok = phase.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let message = phase
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(phase_name);
        queue_runtime_event(
            runtime,
            json!({
                "kind": if ok { "dock:phase_completed" } else { "dock:phase_failed" },
                "request_id": request_id,
                "phase": phase_name,
                "message": message,
                "payload": phase,
            }),
        );
    }
}

fn start_preview_blocking(runtime: &Arc<Mutex<DockRuntimeState>>, request_id: &str) -> Result<()> {
    let working_directory = runtime
        .lock()
        .map_err(|_| anyhow::anyhow!("Dock runtime lock poisoned"))?
        .working_directory
        .clone()
        .context("Prepare a source before starting preview")?;
    let ato_bin = resolve_ato_binary()?;
    let mut child = Command::new(&ato_bin)
        .arg("run")
        .arg(&working_directory)
        .current_dir(&working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to start `ato run {}`", working_directory.display()))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (control_tx, control_rx) = mpsc::channel();
    if let Ok(mut guard) = runtime.lock() {
        guard.preview = Some(PreviewProcess {
            control_tx: control_tx.clone(),
        });
        guard.preview_url = None;
    }

    if let Some(stdout) = stdout {
        let runtime_clone = runtime.clone();
        let request_id = request_id.to_string();
        thread::spawn(move || stream_preview_output(stdout, "stdout", &request_id, &runtime_clone));
    }
    if let Some(stderr) = stderr {
        let runtime_clone = runtime.clone();
        let request_id = request_id.to_string();
        thread::spawn(move || stream_preview_output(stderr, "stderr", &request_id, &runtime_clone));
    }

    let runtime_clone = runtime.clone();
    let request_id = request_id.to_string();
    thread::spawn(move || monitor_preview_process(child, control_rx, &request_id, &runtime_clone));
    Ok(())
}

fn stream_preview_output<R: std::io::Read>(
    reader: R,
    stream: &str,
    request_id: &str,
    runtime: &Arc<Mutex<DockRuntimeState>>,
) {
    let reader = BufReader::new(reader);
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        queue_runtime_event(
            runtime,
            json!({
                "kind": "dock:preview_log",
                "request_id": request_id,
                "stream": stream,
                "line": line,
            }),
        );
        if let Some(url) = detect_preview_url(&line) {
            let should_emit = if let Ok(mut guard) = runtime.lock() {
                if guard.preview_url.as_deref() == Some(url.as_str()) {
                    false
                } else {
                    guard.preview_url = Some(url.clone());
                    true
                }
            } else {
                false
            };
            if should_emit {
                queue_runtime_event(
                    runtime,
                    json!({
                        "kind": "dock:preview_url",
                        "request_id": request_id,
                        "url": url,
                    }),
                );
            }
        }
    }
}

fn monitor_preview_process(
    mut child: std::process::Child,
    control_rx: mpsc::Receiver<PreviewControl>,
    request_id: &str,
    runtime: &Arc<Mutex<DockRuntimeState>>,
) {
    loop {
        match control_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(PreviewControl::Stop) => {
                let _ = child.kill();
                let _ = child.wait();
                clear_preview_runtime(runtime);
                queue_runtime_event(
                    runtime,
                    json!({
                        "kind": "dock:phase_completed",
                        "request_id": request_id,
                        "phase": "preview",
                        "message": "Preview stopped",
                    }),
                );
                return;
            }
            Err(RecvTimeoutError::Timeout) => match child.try_wait() {
                Ok(Some(status)) => {
                    clear_preview_runtime(runtime);
                    let ok = status.success();
                    queue_runtime_event(
                        runtime,
                        json!({
                            "kind": if ok { "dock:phase_completed" } else { "dock:phase_failed" },
                            "request_id": request_id,
                            "phase": "preview",
                            "message": format!("Preview exited with status {}", status),
                        }),
                    );
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    clear_preview_runtime(runtime);
                    queue_runtime_event(
                        runtime,
                        json!({
                            "kind": "dock:phase_failed",
                            "request_id": request_id,
                            "phase": "preview",
                            "message": format!("Preview monitor failed: {error}"),
                        }),
                    );
                    return;
                }
            },
            Err(RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                clear_preview_runtime(runtime);
                return;
            }
        }
    }
}

fn clear_preview_runtime(runtime: &Arc<Mutex<DockRuntimeState>>) {
    if let Ok(mut guard) = runtime.lock() {
        guard.preview = None;
        guard.preview_url = None;
    }
}

fn stop_preview_via_runtime(runtime: &mut DockRuntimeState) -> bool {
    if let Some(preview) = runtime.preview.take() {
        let _ = preview.control_tx.send(PreviewControl::Stop);
        runtime.preview_url = None;
        true
    } else {
        false
    }
}

fn clone_public_github_repo(session_id: &str, raw_url: &str) -> Result<PathBuf> {
    let clone_url = normalize_public_github_url(raw_url)?;
    // Use the repo name as the working directory so that manifest inference
    // produces a meaningful capsule name (e.g. "hello-capsule") instead of
    // the opaque session ID (e.g. "dock-17a2b3c4d5e6f").
    let repo_name = repo_name_from_clone_url(&clone_url);
    let sources_root = dock_sources_root()?;
    let session_dir = sources_root.join(session_id);
    let target_dir = session_dir.join(&repo_name);
    if target_dir.exists() {
        fs::remove_dir_all(&target_dir)
            .with_context(|| format!("Failed to clear {}", target_dir.display()))?;
    }
    fs::create_dir_all(&session_dir)?;
    // Pre-create the target directory so git does not have to create it while
    // inheriting a CWD that is itself inside a git repository (which can confuse
    // git's internal bare-repo detection in some versions).
    fs::create_dir_all(&target_dir)?;

    let git_bin = resolve_git_binary();
    let output = Command::new(&git_bin)
        // Bypass any credential helper — we only clone public repos.
        .arg("-c")
        .arg("credential.helper=")
        // Suppress interactive prompts that would block the background thread.
        .env("GIT_TERMINAL_PROMPT", "0")
        // Clear env vars that cargo sets and that git inherits; these can
        // cause safe.bareRepository or other config injection to interfere
        // with the fresh git-init that `git clone` performs.
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_KEY_0")
        .env_remove("GIT_CONFIG_VALUE_0")
        // Ensure git does not walk up into a parent git repo when its CWD
        // happens to be inside the ato-desktop source tree.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        // Run from a neutral CWD outside any git working tree.
        .current_dir(&session_dir)
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg(&clone_url)
        .arg(&target_dir)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("Failed to run `{git_bin} clone` for {clone_url}"))?;
    if output.status.success() {
        return Ok(target_dir);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!("Failed to clone {}. {}", clone_url, stderr.trim())
}

/// Extract the repository name from a normalized GitHub clone URL.
/// `https://github.com/owner/hello-capsule.git` → `"hello-capsule"`
fn repo_name_from_clone_url(clone_url: &str) -> String {
    clone_url
        .rsplit('/')
        .next()
        .map(|s| s.trim_end_matches(".git"))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "source".to_string())
}

/// Find the git binary. Prefers the Homebrew git if present, then falls back
/// to whatever is on PATH (which is /usr/bin/git on macOS app bundles).
fn resolve_git_binary() -> String {
    for candidate in &["/opt/homebrew/bin/git", "/usr/local/bin/git", "git"] {
        if std::path::Path::new(candidate).is_absolute() {
            if std::path::Path::new(candidate).exists() {
                return candidate.to_string();
            }
        } else {
            // Non-absolute path — rely on PATH resolution; always allow "git".
            return candidate.to_string();
        }
    }
    "git".to_string()
}

fn validate_local_source_path(raw_path: &str) -> Result<PathBuf> {
    let path = PathBuf::from(raw_path.trim());
    if path.as_os_str().is_empty() {
        anyhow::bail!("Enter a local directory path");
    }
    let canonical = fs::canonicalize(&path)
        .with_context(|| format!("Local path does not exist: {}", path.display()))?;
    if !canonical.is_dir() {
        anyhow::bail!("Local path must be a directory");
    }
    Ok(canonical)
}

fn load_manifest_or_template(
    working_directory: &Path,
    source_kind: DockSourceKind,
    source_value: &str,
) -> Result<DockManifestDraft> {
    let manifest_path = working_directory.join("capsule.toml");
    if manifest_path.is_file() {
        let toml = fs::read_to_string(&manifest_path)
            .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
        return Ok(DockManifestDraft {
            toml,
            inference: json!({
                "mode": "existing_manifest",
                "warnings": [],
            }),
        });
    }
    infer_manifest_or_template(working_directory, source_kind, source_value)
}

fn infer_manifest_or_template(
    working_directory: &Path,
    source_kind: DockSourceKind,
    source_value: &str,
) -> Result<DockManifestDraft> {
    match infer_manifest_toml(working_directory) {
        Ok(inferred) => Ok(DockManifestDraft {
            toml: inferred.manifest_toml,
            inference: json!({
                "mode": inferred.inference_mode.unwrap_or_else(|| "static_inference".to_string()),
                "ok": inferred.ok.unwrap_or(true),
                "diagnostics": inferred.diagnostics.unwrap_or(Value::Array(Vec::new())),
                "unresolved": inferred.unresolved.unwrap_or(Value::Array(Vec::new())),
                "selection_gate": inferred.selection_gate.unwrap_or(Value::Null),
                "approval_gate": inferred.approval_gate.unwrap_or(Value::Null),
                "warnings": [],
            }),
        }),
        Err(error) => {
            let fallback = default_manifest_toml(working_directory, source_kind, source_value);
            Ok(DockManifestDraft {
                toml: fallback,
                inference: json!({
                    "mode": "placeholder_fallback",
                    "ok": false,
                    "diagnostics": [],
                    "unresolved": [],
                    "warnings": [format!("Static manifest inference failed: {error}")],
                }),
            })
        }
    }
}

fn infer_manifest_toml(working_directory: &Path) -> Result<InferredManifestResponse> {
    let ato_bin = resolve_ato_binary()?;
    let output = Command::new(&ato_bin)
        .arg("project")
        .arg("infer-manifest")
        .arg(working_directory)
        .arg("--json")
        .current_dir(working_directory)
        .stdin(Stdio::null())
        .output()
        .with_context(|| {
            format!(
                "Failed to run `{} project infer-manifest {}`",
                ato_bin.display(),
                working_directory.display()
            )
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("infer-manifest exited with status {}", output.status)
        };
        anyhow::bail!(detail);
    }

    let inferred: InferredManifestResponse = serde_json::from_str(&stdout)
        .with_context(|| "infer-manifest returned invalid JSON".to_string())?;
    if inferred.manifest_toml.trim().is_empty() {
        anyhow::bail!("infer-manifest returned an empty manifest");
    }
    Ok(inferred)
}

fn default_manifest_toml(
    working_directory: &Path,
    source_kind: DockSourceKind,
    source_value: &str,
) -> String {
    let slug_seed = match source_kind {
        DockSourceKind::GithubRepo => source_value
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("new-capsule"),
        DockSourceKind::LocalPath => working_directory
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("new-capsule"),
    };
    let slug = slugify(slug_seed);
    format!(
        "schema_version = \"0.3\"\nname = \"{slug}\"\nversion = \"0.1.0\"\ntype = \"app\"\nruntime = \"source\"\nworking_dir = \".\"\n"
    )
}

fn normalize_public_github_url(raw_url: &str) -> Result<String> {
    let url = Url::parse(raw_url.trim()).with_context(|| {
        "Enter a public GitHub repository URL like https://github.com/owner/repo".to_string()
    })?;
    let host = url
        .host_str()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if url.scheme() != "https" || host != "github.com" {
        anyhow::bail!("Only public https://github.com/<owner>/<repo> URLs are supported");
    }
    let segments: Vec<_> = url
        .path_segments()
        .map(|segments| segments.filter(|segment| !segment.is_empty()).collect())
        .unwrap_or_else(Vec::new);
    if segments.len() != 2 {
        anyhow::bail!("Use a repository root URL like https://github.com/<owner>/<repo>");
    }
    let owner = segments[0];
    let repo = segments[1].trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        anyhow::bail!("GitHub repository URL is missing owner or repo");
    }
    Ok(format!("https://github.com/{owner}/{repo}.git"))
}

fn parse_publish_json_output(stdout: &str) -> Result<Value> {
    serde_json::from_str(stdout.trim())
        .with_context(|| "Failed to parse `ato publish --json` output".to_string())
}

/// Read the session token stored in `~/.config/ato/credentials.toml`
/// (or `$XDG_CONFIG_HOME/ato/credentials.toml`).
/// Falls back to `~/.ato/credentials.toml` for legacy installs.
fn read_session_token_from_credentials() -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Creds {
        session_token: Option<String>,
    }

    let xdg_config = std::env::var("XDG_CONFIG_HOME").ok();
    let home = dirs::home_dir()?;

    let canonical = xdg_config
        .map(|d| PathBuf::from(d).join("ato").join("credentials.toml"))
        .unwrap_or_else(|| home.join(".config").join("ato").join("credentials.toml"));
    let legacy = home.join(".ato").join("credentials.toml");

    for path in &[canonical, legacy] {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(creds) = toml::from_str::<Creds>(&text) {
                if let Some(tok) = creds.session_token.filter(|s| !s.is_empty()) {
                    return Some(tok);
                }
            }
        }
    }
    None
}

/// After a successful `ato publish --deploy`, update the capsule's visibility
/// via `PATCH /v1/docks/:handle/capsules/:id`.
///
/// Returns `Ok(())` on success or a descriptive error on failure. A failure
/// here is non-fatal for the publish itself; callers log the error and include
/// it in the submit_completed event.
fn apply_visibility_after_publish(
    scoped_id: &str,
    visibility: &str,
    api_base: &str,
    session_token: &str,
) -> Result<()> {
    let parts: Vec<&str> = scoped_id.splitn(2, '/').collect();
    if parts.len() != 2 {
        anyhow::bail!("invalid scoped_id format: {scoped_id}");
    }
    let (handle, slug) = (parts[0], parts[1]);

    // 1. Find the internal capsule ID by slug.
    let list_url = format!("{api_base}/v1/docks/{handle}/capsules");
    let resp = ureq::get(&list_url)
        .set("Authorization", &format!("Bearer {session_token}"))
        .call()
        .with_context(|| format!("GET {list_url} failed"))?;

    let body: Value = resp
        .into_json()
        .with_context(|| "Failed to parse capsule list response")?;

    let capsule_id = body
        .get("capsules")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter().find(|c| {
                c.get("slug").and_then(Value::as_str) == Some(slug)
            })
        })
        .and_then(|c| c.get("id"))
        .and_then(Value::as_str)
        .with_context(|| format!("Capsule '{slug}' not found in dock for '{handle}'"))?
        .to_string();

    // 2. Apply visibility.
    let patch_url = format!("{api_base}/v1/docks/{handle}/capsules/{capsule_id}");
    ureq::patch(&patch_url)
        .set("Authorization", &format!("Bearer {session_token}"))
        .set("Content-Type", "application/json")
        .send_json(json!({ "visibility": visibility }))
        .with_context(|| format!("PATCH {patch_url} failed"))?;

    Ok(())
}

fn detect_preview_url(line: &str) -> Option<String> {
    for token in line
        .split(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '(' | ')' | '[' | ']'))
    {
        let trimmed =
            token.trim_matches(|ch: char| matches!(ch, ',' | ';' | '.' | '"' | '\'' | '<' | '>'));
        if !(trimmed.starts_with("http://127.0.0.1:") || trimmed.starts_with("http://localhost:")) {
            continue;
        }
        let Ok(url) = Url::parse(trimmed) else {
            continue;
        };
        return Some(url.to_string());
    }
    None
}

fn new_dock_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("dock-{nanos:x}")
}

fn dock_root_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not resolve the home directory")?;
    Ok(home.join(".ato").join("dock"))
}

fn dock_sources_root() -> Result<PathBuf> {
    Ok(dock_root_dir()?.join("sources"))
}

fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in input.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    if slug.is_empty() {
        "new-capsule".to_string()
    } else {
        slug
    }
}

fn source_kind_label(kind: DockSourceKind) -> &'static str {
    match kind {
        DockSourceKind::GithubRepo => "github_repo",
        DockSourceKind::LocalPath => "local_path",
    }
}

struct PreparedDockSource {
    session_id: String,
    working_directory: PathBuf,
    manifest_path: PathBuf,
    manifest_toml: String,
    manifest_inference: Value,
}

struct DockManifestDraft {
    toml: String,
    inference: Value,
}

#[derive(Debug, Deserialize)]
struct InferredManifestResponse {
    manifest_toml: String,
    ok: Option<bool>,
    inference_mode: Option<String>,
    diagnostics: Option<Value>,
    unresolved: Option<Value>,
    selection_gate: Option<Value>,
    approval_gate: Option<Value>,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        detect_preview_url, load_manifest_or_template, normalize_public_github_url,
        parse_publish_json_output, repo_name_from_clone_url,
    };
    use crate::system_capsule::ato_dock::DockSourceKind;

    #[test]
    fn parse_publish_json_output_reads_phase_payload() {
        let payload = parse_publish_json_output(
            r#"{
                "ok": true,
                "message": "Selected publish phases completed.",
                "registry": "https://api.ato.run",
                "route": "personal_dock_direct",
                "phases": [
                    { "name": "prepare", "selected": true, "ok": true, "status": "ok", "message": "prepare ok" },
                    { "name": "build", "selected": true, "ok": true, "status": "ok", "message": "build ok" },
                    { "name": "verify", "selected": true, "ok": true, "status": "ok", "message": "verify ok" }
                ]
            }"#,
        )
        .expect("publish json");

        assert_eq!(payload["route"], "personal_dock_direct");
        assert_eq!(payload["phases"].as_array().expect("phases").len(), 3);
    }

    #[test]
    fn detect_preview_url_picks_localhost_tokens() {
        assert_eq!(
            detect_preview_url("ready on http://127.0.0.1:43124/"),
            Some("http://127.0.0.1:43124/".to_string())
        );
        assert_eq!(
            detect_preview_url("Preview URL => http://localhost:3000"),
            Some("http://localhost:3000/".to_string())
        );
        assert_eq!(detect_preview_url("no preview URL here"), None);
    }

    #[test]
    fn normalize_public_github_url_accepts_repo_root_only() {
        assert_eq!(
            normalize_public_github_url("https://github.com/ato-run/ato").expect("repo url"),
            "https://github.com/ato-run/ato.git"
        );
        assert!(normalize_public_github_url("https://github.com/ato-run/ato/tree/main").is_err());
        assert!(normalize_public_github_url("http://github.com/ato-run/ato").is_err());
    }

    #[test]
    fn load_manifest_or_template_prefers_existing_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = "schema_version = \"0.3\"\nname = \"existing\"\n";
        fs::write(dir.path().join("capsule.toml"), manifest).expect("manifest");

        let loaded = load_manifest_or_template(
            dir.path(),
            DockSourceKind::LocalPath,
            dir.path().to_string_lossy().as_ref(),
        )
        .expect("load manifest");

        assert_eq!(loaded.toml, manifest);
        assert_eq!(loaded.inference["mode"], "existing_manifest");
    }

    #[test]
    fn repo_name_from_clone_url_extracts_repo_slug() {
        assert_eq!(
            repo_name_from_clone_url("https://github.com/Koh0920/hello-capsule.git"),
            "hello-capsule"
        );
        assert_eq!(
            repo_name_from_clone_url("https://github.com/ato-run/ato.git"),
            "ato"
        );
    }
}

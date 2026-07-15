//! Dock window — mounts a Wry WebView loading the built
//! `ato-dock` system capsule from `assets/system/ato-dock/dist`.
//!
//! The built assets are served via a `capsule-dock://` custom
//! protocol handler so WKWebView receives them with a proper origin.
//!
//! The Dock hosts the real publisher flow: source preparation,
//! manifest editing, verification, preview, and submit. All long-
//! running work stays off the GPUI thread and reports structured
//! events back into the WebView via `window.__ATO_DOCK_EVENT__(...)`.

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, Context, IntoElement, Pixels, Render, Size, Window, WindowBounds,
    WindowDecorations, WindowOptions, div, px, rgb, size,
};
use gpui_component::TitleBar;
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::Response;
use wry::{Rect, WebView, WebViewBuilder};

use crate::localization::{compose_init_script, resolve_locale, tr};
use crate::orchestrator::resolve_ato_binary;
use crate::proc_util::CommandNoWindowExt;
use crate::source_import_session::normalize_github_import_input;
use crate::state::GuestRoute;
use crate::system_capsule::ato_dock::DockSourceKind;
use crate::system_capsule::broker::SystemCapsuleId;
use crate::system_capsule::ipc as system_ipc;
use crate::system_capsule::manifest::system_capsule_url;
use crate::system_capsule::static_resolver::resolve_system_capsule_asset;
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

const DOCK_SCHEME: &str = "capsule-dock";
const DOCK_SLUG: &str = "ato-dock";

fn dock_protocol_response(
    request_path: &str,
    identity: &Value,
    runtime_snapshot: &Value,
) -> Response<Cow<'static, [u8]>> {
    let asset = resolve_system_capsule_asset(DOCK_SLUG, request_path);
    let body = if asset.status_code == 200
        && asset.content_type.starts_with("text/html")
        && matches!(request_path, "" | "/" | "/index.html")
    {
        let inject = format!(
            "<head><script>window.__ATO_IDENTITY={};window.__ATO_DOCK_BOOTSTRAP={};</script>",
            serde_json::to_string(identity).unwrap_or_else(|_| "null".to_string()),
            serde_json::to_string(runtime_snapshot).unwrap_or_else(|_| "null".to_string()),
        );
        let html = String::from_utf8_lossy(&asset.body).replacen("<head>", &inject, 1);
        Cow::Owned(html.into_bytes())
    } else {
        Cow::Owned(asset.body)
    };

    Response::builder()
        .status(asset.status_code)
        .header("Content-Type", asset.content_type)
        .header("Cache-Control", "no-store, no-cache")
        .body(body)
        .expect("dock asset response must build")
}

/// Slot tracking the single open Dock window.
#[derive(Default)]
pub struct DockWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for DockWindowSlot {}

/// Slot tracking the live `DockWebView` entity so background tasks can
/// stream results into the existing WebView.
#[derive(Default)]
pub struct DockEntitySlot(pub Option<gpui::Entity<DockWebView>>);
impl gpui::Global for DockEntitySlot {}

/// Cached result of `fetch_identity()` to avoid the blocking
/// `ato whoami` subprocess on every dock reopen. Cleared on
/// login / logout so the next open fetches fresh state.
#[derive(Default)]
pub struct DockIdentityCache(pub Option<Value>);
impl gpui::Global for DockIdentityCache {}

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
    pub(crate) webview: Option<WebView>,
    window_size: Size<Pixels>,
    identity_state: Arc<Mutex<Value>>,
    runtime_state: Arc<Mutex<DockRuntimeState>>,
    paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(DockWebView, paste);

impl WebViewPasteShell for DockWebView {
    fn active_paste_target(&self) -> Option<&WebView> {
        self.webview.as_ref()
    }
}

impl DockWebView {
    fn emit_event(&mut self, event: &Value) {
        let payload = serde_json::to_string(event).unwrap_or_else(|_| "null".to_string());
        let script = format!("window.__ATO_DOCK_EVENT__ && window.__ATO_DOCK_EVENT__({payload});");
        if let Some(webview) = self.webview.as_ref()
            && let Err(error) = webview.evaluate_script(&script)
        {
            tracing::warn!(?error, "dock: evaluate_script event dispatch failed");
        }
    }

    fn sync_webview_bounds(&mut self, window: &mut Window) {
        let current = window.bounds().size;
        if current == self.window_size {
            return;
        }
        if let Some(webview) = self.webview.as_ref() {
            let _ = webview.set_bounds(Rect {
                position: LogicalPosition::new(0i32, 0i32).into(),
                size: LogicalSize::new(
                    f32::from(current.width) as u32,
                    f32::from(current.height) as u32,
                )
                .into(),
            });
        }
        self.window_size = current;
    }
}

impl Render for DockWebView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_webview_bounds(window);
        paste_render_wrap!(
            div().size_full().bg(rgb(0xffffff)),
            cx,
            &self.paste.focus_handle
        )
    }
}

pub fn open_external_url(cx: &mut App, url: &str) -> Result<gpui::AnyWindowHandle> {
    let parsed = Url::parse(url).with_context(|| format!("Invalid URL: {url}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!("Dock can open only http(s) URLs");
    }
    crate::window::open_app_window(cx, GuestRoute::ExternalUrl(parsed))
}

pub fn cleanup_dock_window(cx: &mut App) {
    if let Ok(runtime) = dock_runtime(cx)
        && let Ok(mut guard) = runtime.lock()
    {
        stop_preview_via_runtime(&mut guard);
    }
    cx.set_global(DockWindowSlot(None));
    cx.set_global(DockEntitySlot(None));
    cx.set_global(DockIdentityCache(None));
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
        .no_console_window()
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
        #[cfg(target_os = "macos")]
        if let Some(nswindow) = crate::window::macos::ns_window_for(cx, handle) {
            nswindow.makeKeyAndOrderFront(None);
        }
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
    let identity = match cx.global::<DockIdentityCache>().0.as_ref() {
        Some(cached) => {
            tracing::debug!("dock: reusing cached identity");
            cached.clone()
        }
        None => {
            let fresh = fetch_identity();
            cx.set_global(DockIdentityCache(Some(fresh.clone())));
            fresh
        }
    };
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
        window.set_window_title(crate::window::WINDOW_TITLE);
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

        let _wv_guard = crate::webview_init_guard::WebviewInitGuard::new();
        let webview = WebViewBuilder::new()
            .with_asynchronous_custom_protocol(
                DOCK_SCHEME.to_string(),
                move |_id, req, responder| {
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
                    let response = dock_protocol_response(
                        req.uri().path(),
                        &current_identity,
                        &runtime_snapshot,
                    );
                    responder.respond(response);
                },
            )
            .with_url(&url)
            .with_initialization_script(&init_script)
            .with_on_page_load_handler(move |event, _url| {
                use wry::PageLoadEvent;
                if matches!(event, PageLoadEvent::Finished) {
                    if let Some(automation) = &automation_for_load {
                        automation.mark_page_loaded(crate::webview::DOCK_AUTOMATION_PANE_ID);
                    }
                } else if matches!(event, PageLoadEvent::Started)
                    && let Some(automation) = &automation_for_load
                {
                    automation.mark_page_unloaded(crate::webview::DOCK_AUTOMATION_PANE_ID);
                }
            })
            .with_ipc_handler(system_ipc::make_ipc_handler_for_capsule(
                SystemCapsuleId::AtoDock,
                bridge_queue.clone(),
            ))
            .with_bounds(webview_rect);
        let webview = crate::window::build_child_webview("Dock window", webview, window);
        let view = cx.new(|cx| DockWebView {
            webview,
            window_size: win_size,
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

    // Intercept the OS close button (red traffic light) to hide the
    // dock instead of destroying it. The GPUI window + Wry WebView
    // stay alive, so the next dock click only needs to call
    // makeKeyAndOrderFront + activate_window without running
    // `fetch_identity`, creating a new WebView, or loading the page.
    let _ = handle.update(cx, |_, window, app_cx| {
        window.on_window_should_close(app_cx, move |window, _app| {
            #[cfg(target_os = "macos")]
            {
                tracing::info!("dock: close intercepted, hiding instead of destroying");
                crate::window::macos::hide_window_in_handler(window);
                false
            }
            #[cfg(target_os = "windows")]
            {
                tracing::info!("dock: close intercepted, hiding instead of destroying");
                crate::window::windows::hide_window_in_handler(window);
                false
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                let _ = window;
                true
            }
        });
    });

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
    cx.global_mut::<crate::system_capsule::window_registry::SystemCapsuleWindowRegistry>()
        .register(SystemCapsuleId::AtoDock, *handle);
    system_ipc::spawn_drain_loop(cx, drain_queue, *handle);
    spawn_dock_event_loop(cx, queue, *handle);
    Ok(*handle)
}

/// Update the existing Dock WebView's identity after a successful login and reload the page.
pub fn notify_login_success(cx: &mut App) {
    cx.set_global(DockIdentityCache(None));
    let identity = fetch_identity();
    cx.set_global(DockIdentityCache(Some(identity.clone())));

    let entity = cx
        .try_global::<DockEntitySlot>()
        .and_then(|slot| slot.0.clone());
    if let Some(entity) = entity {
        entity.update(cx, |view, _cx| {
            if let Ok(mut guard) = view.identity_state.lock() {
                *guard = identity;
            }
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            let reload_url = format!("{DOCK_SCHEME}://localhost/?t={ts}");
            if let Some(webview) = view.webview.as_ref()
                && let Err(error) = webview.load_url(&reload_url)
            {
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

/// Returns a handle to the Dock's live event queue, if the Dock window is
/// currently open. Events pushed onto it reach the Dock WebView's
/// `window.__ATO_DOCK_EVENT__` callback via the existing ~20/sec drain loop
/// (`spawn_dock_event_loop`).
///
/// The returned `Arc<Mutex<..>>` needs no further GPUI access, so callers may
/// clone it and push events from a background thread (e.g. while watching a
/// spawned child process's stdout) as well as from the GPUI foreground
/// thread.
pub fn dock_event_queue(cx: &mut App) -> Result<Arc<Mutex<Vec<Value>>>> {
    let runtime = dock_runtime(cx)?;
    let guard = runtime
        .lock()
        .map_err(|_| anyhow::anyhow!("Dock runtime lock poisoned"))?;
    Ok(guard.event_queue.clone())
}

fn spawn_dock_event_loop(cx: &mut App, queue: DockEventQueue, host: AnyWindowHandle) {
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        loop {
            be.timer(Duration::from_millis(50)).await;
            if crate::webview_init_guard::WebviewInitGuard::is_active() {
                continue;
            }
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
                        entity.update(cx, |view, _cx| view.emit_event(&event));
                    }
                });
            }
        }
    })
    .detach();
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
        .no_console_window()
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
    normalize_github_import_input(raw_url).map(|repo| repo.clone_url)
}

fn parse_publish_json_output(stdout: &str) -> Result<Value> {
    serde_json::from_str(stdout.trim())
        .with_context(|| "Failed to parse `ato publish --json` output".to_string())
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
        for input in [
            "ato-run/ato",
            "github.com/ato-run/ato",
            "https://github.com/ato-run/ato",
        ] {
            assert_eq!(
                normalize_public_github_url(input).expect("repo url"),
                "https://github.com/ato-run/ato.git"
            );
        }
        assert!(normalize_public_github_url("https://github.com/ato-run/ato/tree/main").is_err());
        assert!(normalize_public_github_url("http://github.com/ato-run/ato").is_err());
        assert!(normalize_public_github_url("capsule://github.com/ato-run/ato").is_err());
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

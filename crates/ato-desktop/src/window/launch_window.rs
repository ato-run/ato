//! `ato-launch` system-capsule host windows.
//!
//! Two transient wizard windows ride the capsule-launch flow:
//!
//!   - `open_consent_window` — pre-flight consent wizard. Renders
//!     `assets/system/ato-launch/consent.html`. User confirms identity,
//!     reviews requested permissions, and fills env-var inputs before
//!     clicking 承認して起動 (Approve) or キャンセル (Cancel).
//!   - `open_boot_window` — in-flight boot progress wizard. Renders
//!     `assets/system/ato-launch/boot.html`. Shows the launch steps
//!     (Capsule取得 → 依存解決 → 起動環境 → セキュリティ → データ保護
//!     → プライバシー設定). User can 中断 (AbortBoot).
//!
//! Real launch flow (capsule:// URL through the Control Bar URL pill
//! or the NavigateToUrl action): `open_consent_window_for_route` sets
//! `PendingLaunchTarget` to the target `GuestRoute` and opens the
//! consent wizard. On Approve, `ato_launch::dispatch` consumes the
//! pending target, calls `open_app_window` to spawn the real AppWindow,
//! and opens the boot wizard as a transient launch-ceremony overlay.
//! Phase 1 boot animation is still decorative; Phase 2 will drive it
//! from real orchestrator events emitted by
//! `orchestrator::resolve_and_start_guest`.
//!
//! Wizards are intentionally NOT registered in `OpenContentWindows`.
//! They are launch chrome, not destination content — the Card Switcher
//! should not list a half-formed AppWindow's wizard. The user-facing
//! AppWindow that follows a successful approve flow registers itself
//! the normal way via `open_app_window`.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use capsule_wire::config::ConfigKind;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context, Entity, IntoElement, Render,
    WeakEntity, Window, WindowBounds, WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use serde::Serialize;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};
#[cfg(target_os = "macos")]
use wry::WebViewExtMacOS;

use crate::app::{NativeCopy, NativeCut, NativePaste, NativeSelectAll};
use crate::localization::{compose_init_script, resolve_locale};
use crate::state::GuestRoute;
use crate::system_capsule::ipc as system_ipc;

const CONSENT_HTML: &str = include_str!("../../assets/system/ato-launch/consent.html");
const BOOT_HTML: &str = include_str!("../../assets/system/ato-launch/boot.html");

/// Pending capsule-launch target — set when `open_consent_window_for_route`
/// opens the consent wizard, consumed by `ato_launch::dispatch` on
/// Approve (spawns the real AppWindow) or cleared on Cancel.
///
/// Single-slot is sufficient for Phase 1 — the consent wizard is
/// modal-ish in practice; opening a second one before approving the
/// first replaces the pending target, which matches user intent
/// ("the most recent launch attempt is the one I'm about to confirm").
#[derive(Default, Debug, Clone)]
pub struct PendingLaunchTarget(pub Option<GuestRoute>);

impl gpui::Global for PendingLaunchTarget {}

/// Config key/value pairs collected from the consent form and passed
/// to `open_app_window` → `AppCapsuleShell::new` → `resolve_and_start_guest`.
/// Set by `ato_launch::dispatch(Approve)` before `open_app_window`.
/// Read and cleared inside `open_app_window` so other code paths
/// (focus_dispatcher direct opens, WebLinkView nav) get empty configs.
#[derive(Default, Debug, Clone)]
pub struct PendingLaunchConfigs(pub Vec<(String, String)>);

impl gpui::Global for PendingLaunchConfigs {}

/// Tracks the two transient wizard windows opened during a capsule boot flow:
///
/// - `boot_window`: the in-flight boot progress wizard
///   (`open_boot_window`).
/// - `app_window`: the destination AppWindow that owns `AppCapsuleShell`.
///
/// Set by `ato_launch::dispatch(Approve)` after both windows are open.
/// Consumed by `ato_launch::dispatch(AbortBoot)` to close both windows, and
/// by `AppCapsuleShell`'s polling task to close the boot wizard on launch
/// completion or failure.
#[derive(Default, Debug, Clone)]
pub struct BootWindowSlot {
    pub boot_window: Option<AnyWindowHandle>,
    pub app_window: Option<AnyWindowHandle>,
}

impl gpui::Global for BootWindowSlot {}

/// Weak handle to the `LaunchWindowShell` entity that owns the boot progress
/// WebView. Set by `open_boot_window` as a side effect so `AppCapsuleShell::new`
/// can drain orchestrator step events to the wizard without changing any
/// caller signatures. Cleared after `AppCapsuleShell::new` consumes it to
/// prevent cross-launch leakage.
#[derive(Default, Clone)]
pub struct PendingBootShell(pub Option<WeakEntity<LaunchWindowShell>>);

impl gpui::Global for PendingBootShell {}

// ── Consent preview types ──────────────────────────────────────────────────

/// A single config field shown in the consent form.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ConsentFieldItem {
    pub name: String,
    pub label: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    pub already_configured: bool,
}

/// A single requirements block rendered in the consent wizard.
/// Tag names match `InteractiveResolutionKind` wire names for consistency.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ConsentRequirementItem {
    #[serde(rename = "secrets_required")]
    Secret {
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        display_message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        display_hint: Option<String>,
        fields: Vec<ConsentFieldItem>,
    },
    #[serde(rename = "consent_required")]
    Consent {
        scoped_id: String,
        version: String,
        target_label: String,
        policy_segment_hash: String,
        provisioning_policy_hash: String,
        summary: String,
    },
}

/// Full preview data hydrated into the consent wizard WebView after preflight.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LaunchConsentPreview {
    pub preview_id: String,
    pub loading: bool,
    /// Set to `true` when `ato internal preflight` failed and the wizard could
    /// not collect requirements. The JS side disables Approve and shows an
    /// error/retry prompt when this flag is set.
    #[serde(default)]
    pub preflight_failed: bool,
    pub name: String,
    pub handle: String,
    pub capsule_id: String,
    pub capsule_version: String,
    pub visited_targets: Vec<String>,
    pub requirements: Vec<ConsentRequirementItem>,
}

/// In-flight consent preview global.
/// Set to a loading-state preview when the wizard opens; replaced with the
/// full preview when background preflight completes.
/// Consumed and cleared by `ato_launch::dispatch(Approve)` and `dispatch(Cancel)`.
#[derive(Default, Clone)]
pub struct PendingConsentPreview(pub Option<LaunchConsentPreview>);

impl gpui::Global for PendingConsentPreview {}

pub struct LaunchWindowShell {
    _webview: WebView,
}

impl Render for LaunchWindowShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // White backdrop in case the HTML is still painting.
        // key_context required so the LaunchWindowShell key bindings fire
        // (the WKWebView child is not first responder, so native Cmd+V/C
        // would never reach the HTML inputs without this).
        div()
            .size_full()
            .bg(rgb(0xffffff))
            .key_context("LaunchWindowShell")
            .on_action(cx.listener(Self::on_native_copy))
            .on_action(cx.listener(Self::on_native_cut))
            .on_action(cx.listener(Self::on_native_paste))
            .on_action(cx.listener(Self::on_native_select_all))
    }
}

impl LaunchWindowShell {
    fn on_native_copy(&mut self, _: &NativeCopy, _window: &mut Window, _cx: &mut Context<Self>) {
        let _ = self
            ._webview
            .evaluate_script("document.execCommand('copy')");
    }

    fn on_native_cut(&mut self, _: &NativeCut, _window: &mut Window, _cx: &mut Context<Self>) {
        let _ = self._webview.evaluate_script("document.execCommand('cut')");
    }

    fn on_native_paste(&mut self, _: &NativePaste, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = cx.read_from_clipboard() {
            if let Some(text) = item.text() {
                // Ensure WKWebView has OS first-responder before injecting the
                // paste script. Without this, document.activeElement may be
                // reset to <body> by the time the GPUI deferred action fires,
                // causing the isTextInput check in launch_paste_script to fail.
                #[cfg(target_os = "macos")]
                let _ = self._webview.focus();

                let script = launch_paste_script(&text);
                let _ = self._webview.evaluate_script(&script);
            }
        }
    }

    fn on_native_select_all(
        &mut self,
        _: &NativeSelectAll,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let _ = self
            ._webview
            .evaluate_script("document.execCommand('selectAll')");
    }

    /// Advance the boot wizard UI to step `n`. Called from the foreground
    /// polling task inside `AppCapsuleShell` as the orchestrator emits
    /// progress. The JS guards with `typeof window.__atoStep === 'function'`
    /// so a missed early-step call is silent (the HTML buffers pending steps
    /// via DOMContentLoaded replay).
    pub fn push_step(&self, step: u8) {
        let script = format!(
            "typeof window.__atoStep==='function'&&window.__atoStep({})",
            step
        );
        let _ = self._webview.evaluate_script(&script);
    }

    /// Inject the full consent preview into the wizard WebView.
    /// `preview_json` is a JSON-serialized `LaunchConsentPreview` value
    /// (already valid JS object literal syntax). Called from the GPUI main
    /// thread via `AsyncApp::update` after background preflight completes.
    pub fn hydrate_preview(&self, preview_json: &str) {
        let script = format!(
            "typeof window.__ato_hydrate_preview==='function'&&window.__ato_hydrate_preview({})",
            preview_json
        );
        let _ = self._webview.evaluate_script(&script);
    }
}

fn launch_paste_script(text: &str) -> String {
    let text = serde_json::to_string(text).expect("clipboard text should serialize");
    format!(
        r#"(() => {{
  const text = {text};
  // Prefer document.activeElement; fall back to the last element that
  // received a focusin event (stored by the __ato_last_focused tracker).
  // This is necessary because document.activeElement may be reset to
  // <body> by the time the GPUI deferred action fires on macOS.
  const active = (document.activeElement && document.activeElement !== document.body)
    ? document.activeElement
    : (window.__ato_last_focused || null);
  const isTextInput = active && (
    active.tagName === 'TEXTAREA' ||
    (active.tagName === 'INPUT' && !['button','checkbox','color','file','hidden','image','radio','range','reset','submit'].includes((active.type || '').toLowerCase()))
  );
  if (!isTextInput || active.readOnly || active.disabled) return;
  active.focus();
  const start = active.selectionStart ?? active.value.length;
  const end = active.selectionEnd ?? start;
  active.setRangeText(text, start, end, 'end');
  active.dispatchEvent(new InputEvent('input', {{ bubbles: true, inputType: 'insertText', data: text }}));
}})();"#,
        text = text,
    )
}

fn open_wizard(
    cx: &mut App,
    html: &'static str,
    w: f32,
    h: f32,
    init_script: Option<String>,
) -> Result<AnyWindowHandle> {
    let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let locale = resolve_locale(crate::config::load_config().general.language);
    let composed = compose_init_script(locale, init_script.as_deref());
    let queue = system_ipc::new_queue();
    let handle = cx.open_window(options, |window, cx| {
        let win_size = window.bounds().size;
        let webview_rect = Rect {
            position: LogicalPosition::new(0i32, 0i32).into(),
            size: LogicalSize::new(
                f32::from(win_size.width) as u32,
                f32::from(win_size.height) as u32,
            )
            .into(),
        };
        let queue_for_ipc = queue.clone();
        let webview = WebViewBuilder::new()
            .with_html(html)
            .with_initialization_script(&composed)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_ipc))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Launch wizard WebView");
        let shell = cx.new(|_cx| LaunchWindowShell { _webview: webview });
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    system_ipc::spawn_drain_loop(cx, queue, *handle);
    Ok(*handle)
}

// Window dimensions are tuned to the card content so the window IS
// the card — no surrounding chrome padding. Update these together
// when the HTML content grows or shrinks.
const CONSENT_W: f32 = 560.0;
const CONSENT_H: f32 = 560.0;
const BOOT_W: f32 = 440.0;
const BOOT_H: f32 = 520.0;

/// Spawn the consent wizard with no specific target — used for AODD
/// screenshot generation and standalone MCP testing.
/// Injects a minimal demo preview so the UI renders rather than showing
/// the loading spinner indefinitely.
pub fn open_consent_window(cx: &mut App) -> Result<()> {
    let demo_preview = serde_json::json!({
        "preview_id": "demo",
        "loading": false,
        "name": "サンプルカプセル",
        "handle": "github.com/example/sample",
        "capsule_id": "",
        "capsule_version": "",
        "visited_targets": [],
        "requirements": [],
    });
    let init_script = format!(
        "window.__ATO_LAUNCH_PREVIEW={};",
        serde_json::to_string(&demo_preview).unwrap_or_else(|_| "null".to_string())
    );
    open_wizard(cx, CONSENT_HTML, CONSENT_W, CONSENT_H, Some(init_script)).map(|_| ())
}

/// Real launch entrypoint: open the consent wizard for a concrete
/// `GuestRoute`. Stashes the route under `PendingLaunchTarget` so the
/// broker's Approve handler can spawn the real AppWindow on user
/// confirmation.
///
/// Opens the wizard immediately with a loading state, then spawns a
/// background task to run `ato internal preflight` and hydrate the
/// WebView with real capsule identity + requirements.
pub fn open_consent_window_for_route(cx: &mut App, route: GuestRoute) -> Result<()> {
    let (display_name, display_handle) = match &route {
        GuestRoute::CapsuleHandle { handle, label } => {
            let pretty_name = label
                .split(['/', '@', '-', '_'])
                .filter(|s| !s.is_empty())
                .next_back()
                .unwrap_or(label.as_str())
                .to_string();
            (pretty_name, handle.clone())
        }
        GuestRoute::ExternalUrl(url) => (
            url.host_str().unwrap_or("external").to_string(),
            url.as_str().to_string(),
        ),
        other => (format!("{:?}", other), "unknown".to_string()),
    };

    // Generate a stable preview_id for this launch attempt.
    let preview_id = {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let pid = std::process::id();
        format!("{ms}-{pid}")
    };

    cx.set_global(PendingLaunchTarget(Some(route)));

    // Inject loading-state preview so the wizard renders immediately.
    let loading_preview = serde_json::json!({
        "preview_id": preview_id,
        "loading": true,
        "name": display_name,
        "handle": display_handle,
        "capsule_id": "",
        "capsule_version": "",
        "visited_targets": [],
        "requirements": [],
    });
    let init_script = format!(
        "window.__ATO_LAUNCH_PREVIEW={};",
        serde_json::to_string(&loading_preview).unwrap_or_else(|_| "null".to_string())
    );

    let (_, shell) = open_consent_wizard_inner(cx, Some(init_script))?;
    let shell_weak = shell.downgrade();

    // Store a loading-state preview globally so dispatch(Approve) can
    // match preview_id even if hydration arrives after the user clicks.
    cx.set_global(PendingConsentPreview(Some(LaunchConsentPreview {
        preview_id: preview_id.clone(),
        loading: true,
        preflight_failed: false,
        name: display_name.clone(),
        handle: display_handle.clone(),
        capsule_id: String::new(),
        capsule_version: String::new(),
        visited_targets: vec![],
        requirements: vec![],
    })));

    // Spawn background preflight; hydrate on completion.
    let handle_clone = display_handle.clone();
    let name_clone = display_name.clone();
    let id_clone = preview_id.clone();
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        let preflight_handle = handle_clone.clone();
        let (preflight_result, secrets_store) = be
            .spawn(async move {
                let data = crate::orchestrator::collect_preflight_for_consent(&preflight_handle);
                let store = crate::config::load_secrets();
                (data, store)
            })
            .await;

        let _ = aa.update(|cx| {
            // Guard: only hydrate if this is still the active consent wizard.
            let current_id = cx
                .try_global::<PendingConsentPreview>()
                .and_then(|g| g.0.as_ref().map(|p| p.preview_id.clone()))
                .unwrap_or_default();
            if current_id != id_clone {
                tracing::debug!(
                    expected = %id_clone,
                    current = %current_id,
                    "consent preflight arrived for stale wizard — discarding"
                );
                return;
            }

            let preview = build_consent_preview(
                &name_clone,
                &handle_clone,
                &id_clone,
                preflight_result,
                &secrets_store,
            );
            let json = serde_json::to_string(&preview)
                .unwrap_or_else(|_| r#"{"loading":false,"preview_id":"","name":"","handle":"","capsule_id":"","capsule_version":"","visited_targets":[],"requirements":[]}"#.to_string());
            cx.set_global(PendingConsentPreview(Some(preview)));
            if let Some(shell) = shell_weak.upgrade() {
                shell.read(cx).hydrate_preview(&json);
            }
        });
    })
    .detach();

    Ok(())
}

/// Build a `LaunchConsentPreview` from preflight results and the current
/// secret store. Falls back to an identity-only preview when preflight is
/// unavailable for first-run remote handles; local/cached manifest failures
/// stay blocking.
fn build_consent_preview(
    name: &str,
    handle: &str,
    preview_id: &str,
    preflight: anyhow::Result<crate::orchestrator::ConsentPreflightData>,
    secrets_store: &crate::config::SecretStore,
) -> LaunchConsentPreview {
    // Keys already granted to this capsule — used for `already_configured`.
    let configured_keys: std::collections::HashSet<String> = secrets_store
        .secrets_for_capsule(handle)
        .into_iter()
        .map(|s| s.key.clone())
        .collect();

    match preflight {
        Ok(data) => {
            use capsule_core::interactive_resolution::InteractiveResolutionKind;

            let requirements = data
                .requirements
                .into_iter()
                .filter_map(|env| match env.kind {
                    InteractiveResolutionKind::SecretsRequired { target, schema } => {
                        let fields = schema
                            .into_iter()
                            .map(|f| {
                                let already_configured = configured_keys.contains(&f.name);
                                let kind_str = match &f.kind {
                                    ConfigKind::Secret => "secret",
                                    ConfigKind::String => "string",
                                    ConfigKind::Number => "number",
                                    ConfigKind::Enum { .. } => "enum",
                                }
                                .to_string();
                                let choices = match f.kind {
                                    ConfigKind::Enum { choices } => choices,
                                    _ => vec![],
                                };
                                ConsentFieldItem {
                                    name: f.name,
                                    label: f.label.unwrap_or_default(),
                                    kind: kind_str,
                                    description: f.description,
                                    placeholder: f.placeholder,
                                    choices,
                                    already_configured,
                                }
                            })
                            .collect();
                        Some(ConsentRequirementItem::Secret {
                            target,
                            display_message: env.display.message,
                            display_hint: env.display.hint,
                            fields,
                        })
                    }
                    InteractiveResolutionKind::ConsentRequired {
                        scoped_id,
                        version,
                        target_label,
                        policy_segment_hash,
                        provisioning_policy_hash,
                        summary,
                    } => Some(ConsentRequirementItem::Consent {
                        scoped_id,
                        version,
                        target_label,
                        policy_segment_hash,
                        provisioning_policy_hash,
                        summary,
                    }),
                })
                .collect();

            LaunchConsentPreview {
                preview_id: preview_id.to_string(),
                loading: false,
                preflight_failed: false,
                name: name.to_string(),
                handle: handle.to_string(),
                capsule_id: data.capsule_id,
                capsule_version: data.capsule_version,
                visited_targets: data.visited_targets,
                requirements,
            }
        }
        Err(err) => {
            let preflight_failed = !is_non_blocking_remote_preflight_error(handle, &err);
            if preflight_failed {
                tracing::warn!(
                    error = %err,
                    "consent preflight failed — wizard shows error state"
                );
            } else {
                tracing::warn!(
                    handle,
                    error = %err,
                    "consent preflight unavailable for remote handle — continuing with launch fallback"
                );
            }
            LaunchConsentPreview {
                preview_id: preview_id.to_string(),
                loading: false,
                preflight_failed,
                name: name.to_string(),
                handle: handle.to_string(),
                capsule_id: String::new(),
                capsule_version: String::new(),
                visited_targets: vec![],
                requirements: vec![],
            }
        }
    }
}

fn is_non_blocking_remote_preflight_error(handle: &str, err: &anyhow::Error) -> bool {
    if !looks_like_remote_launch_handle(handle) {
        return false;
    }

    let message = format!("{err:#}");
    message.contains("manifest path does not exist")
}

fn looks_like_remote_launch_handle(handle: &str) -> bool {
    let trimmed = handle.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with("~/")
        || trimmed.starts_with("./")
        || trimmed.starts_with("../")
    {
        return false;
    }

    trimmed.starts_with("github.com/")
        || trimmed.starts_with("capsule://github.com/")
        || trimmed.starts_with("capsule://ato.run/")
        || trimmed.starts_with("capsule://localhost:")
        || trimmed.starts_with("capsule://127.0.0.1:")
        || trimmed.starts_with("capsule://[::1]:")
        || (trimmed.split('/').filter(|part| !part.is_empty()).count() >= 2
            && !trimmed.contains("://"))
}

/// Internal helper: opens the consent wizard window and returns both the
/// GPUI window handle and the `Entity<LaunchWindowShell>` for later
/// hydration via `hydrate_preview`.
fn open_consent_wizard_inner(
    cx: &mut App,
    init_script: Option<String>,
) -> Result<(AnyWindowHandle, Entity<LaunchWindowShell>)> {
    let bounds = Bounds::centered(None, size(px(CONSENT_W), px(CONSENT_H)), cx);
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let locale = resolve_locale(crate::config::load_config().general.language);
    let composed = compose_init_script(locale, init_script.as_deref());
    let queue = system_ipc::new_queue();
    let shell_slot: Arc<Mutex<Option<Entity<LaunchWindowShell>>>> = Arc::new(Mutex::new(None));
    let shell_slot_inner = Arc::clone(&shell_slot);
    let queue_for_closure = queue.clone();

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
        let webview = WebViewBuilder::new()
            .with_html(CONSENT_HTML)
            .with_initialization_script(&composed)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_closure))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the consent WebView");
        let shell = cx.new(|_cx| LaunchWindowShell { _webview: webview });
        if let Ok(mut slot) = shell_slot_inner.lock() {
            *slot = Some(shell.clone());
        }
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    system_ipc::spawn_drain_loop(cx, queue, *handle);
    let shell = shell_slot
        .lock()
        .unwrap()
        .take()
        .expect("LaunchWindowShell entity must be populated by open_window closure");
    Ok((*handle, shell))
}

/// Spawn the boot progress wizard. Returns the `AnyWindowHandle` so the
/// caller can store it in `BootWindowSlot` for later programmatic close.
/// Also sets `PendingBootShell` so `AppCapsuleShell::new` can drain
/// orchestrator progress events to the wizard's WebView.
///
/// `route` is optional — when `Some`, injects `window.__ATO_BOOT = { name, handle }`
/// so the boot HTML can show the real capsule identity instead of the
/// generic placeholder. Pass `None` for standalone AODD/MCP test opens.
pub fn open_boot_window(cx: &mut App, route: Option<&GuestRoute>) -> Result<AnyWindowHandle> {
    let init_script = route.map(|r| {
        let (name, handle) = match r {
            GuestRoute::CapsuleHandle { handle, label } => {
                let pretty = label
                    .split(['/', '@', '-', '_'])
                    .filter(|s| !s.is_empty())
                    .next_back()
                    .unwrap_or(label.as_str())
                    .to_string();
                (pretty, handle.clone())
            }
            GuestRoute::ExternalUrl(url) => (
                url.host_str().unwrap_or("external").to_string(),
                url.as_str().to_string(),
            ),
            other => (format!("{:?}", other), "unknown".to_string()),
        };
        let payload = serde_json::json!({ "name": name, "handle": handle });
        format!(
            "window.__ATO_BOOT = {};",
            serde_json::to_string(&payload).unwrap_or_else(|_| "null".to_string())
        )
    });
    let (handle, shell) = open_boot_wizard_inner(cx, init_script)?;
    cx.set_global(PendingBootShell(Some(shell.downgrade())));
    Ok(handle)
}

/// Internal helper: opens the boot wizard window and returns both the GPUI
/// window handle and the `Entity<LaunchWindowShell>` so the caller can
/// store a `WeakEntity` in `PendingBootShell` for progress injection.
fn open_boot_wizard_inner(
    cx: &mut App,
    init_script: Option<String>,
) -> Result<(AnyWindowHandle, Entity<LaunchWindowShell>)> {
    let bounds = Bounds::centered(None, size(px(BOOT_W), px(BOOT_H)), cx);
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let locale = resolve_locale(crate::config::load_config().general.language);
    let composed = compose_init_script(locale, init_script.as_deref());
    let queue = system_ipc::new_queue();
    // Arc<Mutex<...>> so the entity can be captured across the Send closure.
    let shell_slot: Arc<Mutex<Option<Entity<LaunchWindowShell>>>> = Arc::new(Mutex::new(None));
    let shell_slot_inner = Arc::clone(&shell_slot);
    let queue_for_closure = queue.clone();

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
        let webview = WebViewBuilder::new()
            .with_html(BOOT_HTML)
            .with_initialization_script(&composed)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_closure))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the boot WebView");
        let shell = cx.new(|_cx| LaunchWindowShell { _webview: webview });
        if let Ok(mut slot) = shell_slot_inner.lock() {
            *slot = Some(shell.clone());
        }
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    system_ipc::spawn_drain_loop(cx, queue, *handle);
    let shell = shell_slot
        .lock()
        .unwrap()
        .take()
        .expect("LaunchWindowShell entity must be populated by open_window closure");
    Ok((*handle, shell))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_manifest_missing_preflight_is_non_blocking() {
        let err = anyhow::anyhow!(
            "ato internal preflight failed: preflight collection failed: manifest path does not exist: koh0920/flatnotes"
        );

        assert!(is_non_blocking_remote_preflight_error(
            "koh0920/flatnotes",
            &err
        ));
        assert!(is_non_blocking_remote_preflight_error(
            "capsule://github.com/Koh0920/cupbear",
            &err
        ));
    }

    #[test]
    fn local_manifest_missing_preflight_stays_blocking() {
        let err = anyhow::anyhow!(
            "ato internal preflight failed: preflight collection failed: manifest path does not exist: /missing/capsule.toml"
        );

        assert!(!is_non_blocking_remote_preflight_error(
            "/missing/capsule.toml",
            &err
        ));
        assert!(!is_non_blocking_remote_preflight_error(
            "./samples/demo",
            &err
        ));
    }

    #[test]
    fn remote_non_manifest_preflight_error_stays_blocking() {
        let err = anyhow::anyhow!(
            "ato internal preflight failed: preflight collection failed: failed to route manifest"
        );

        assert!(!is_non_blocking_remote_preflight_error(
            "koh0920/flatnotes",
            &err
        ));
    }

    #[test]
    fn consent_config_inputs_allow_selection() {
        assert!(CONSENT_HTML.contains("-webkit-user-select: text;"));
        assert!(CONSENT_HTML.contains("user-select: text;"));
    }

    #[test]
    fn launch_paste_script_uses_safe_text_insertion() {
        let script = launch_paste_script("a'b\nc");

        assert!(script.contains(r#"const text = "a'b\nc";"#));
        assert!(script.contains("active.setRangeText(text, start, end, 'end');"));
        assert!(script.contains("inputType: 'insertText'"));
    }
}

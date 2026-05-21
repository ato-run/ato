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
//! pending target, opens the boot wizard, and only spawns the real
//! AppWindow after `orchestrator::resolve_and_start_guest` returns a
//! ready session.
//!
//! Wizards are intentionally NOT registered in `OpenContentWindows`.
//! They are launch chrome, not destination content — the Card Switcher
//! should not list a half-formed AppWindow's wizard. The user-facing
//! AppWindow that follows a successful approve flow registers itself
//! the normal way via `open_app_window`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use capsule_wire::config::ConfigKind;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context, Entity, IntoElement, Pixels, Render,
    Size, WeakEntity, Window, WindowBounds, WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use serde::Serialize;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::localization::{compose_init_script, resolve_locale};
use crate::state::GuestRoute;
use crate::system_capsule::ipc as system_ipc;
use crate::system_capsule::static_resolver::resolve_system_capsule_protocol_response;
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

const LAUNCH_SCHEME: &str = "capsule-launch";
const LAUNCH_SLUG: &str = "ato-launch";

/// Pending capsule-launch target — set when `open_consent_window_for_route`
/// opens the consent wizard, consumed by `ato_launch::dispatch` on
/// Approve (spawns the real AppWindow) or cleared on Cancel.
///
/// Per-launch request tracker keyed by `preview_id` (which the consent
/// wizard already round-trips through its IPC approve payload). Uses a map
/// so multiple simultaneous launches do not overwrite each other.
#[derive(Default, Debug, Clone)]
pub struct PendingLaunches(pub std::collections::HashMap<String, StashedLaunch>);

/// Snapshot stored when the consent wizard opens so the Approve handler
/// can reconstruct the launch intent without touching the HTML.
#[derive(Debug, Clone)]
pub struct StashedLaunch {
    pub route: GuestRoute,
    pub requested_client: crate::state::session::SessionClientKind,
}

impl gpui::Global for PendingLaunches {}

/// Config key/value pairs collected from the consent form and passed
/// to `open_app_window` → `AppCapsuleShell::new` → `resolve_and_start_guest`.
/// Set by `ato_launch::dispatch(Approve)` before `open_app_window`.
/// Read and cleared inside `open_app_window` so other code paths
/// (focus_dispatcher direct opens, WebLinkView nav) get empty configs.
#[derive(Default, Debug, Clone)]
pub struct PendingLaunchConfigs(pub Vec<(String, String)>);

impl gpui::Global for PendingLaunchConfigs {}

/// Tracks the transient boot wizard opened during a capsule boot flow:
///
/// - `boot_window`: the in-flight boot progress wizard
///   (`open_boot_window`).
/// - `abort_flag`: shared with the background launch task so AbortBoot
///   can suppress a late successful session and stop it immediately.
///
/// Set by `start_boot_launch` after the boot window opens. Consumed by
/// `ato_launch::dispatch(AbortBoot)` to close the boot wizard and mark
/// the in-flight launch as aborted.
#[derive(Default, Debug, Clone)]
pub struct BootWindowSlot {
    pub boot_window: Option<AnyWindowHandle>,
    pub abort_flag: Option<Arc<AtomicBool>>,
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
    /// Default value to prefill in the form (from the capsule manifest).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preflight_error: Option<String>,
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

/// Weak handle to the most recently opened consent wizard shell.
/// Used by AODD automation entrypoints to open and scroll the demo
/// configuration panel without relying on macOS Accessibility focus.
#[derive(Default, Clone)]
pub struct ActiveConsentShell(pub Option<WeakEntity<LaunchWindowShell>>);

impl gpui::Global for ActiveConsentShell {}

/// Weak handle to the most recently opened GitHub Run wizard shell.
/// Used by `inject_github_candidates` and AODD automation to deliver
/// candidate lookup results into the wizard WebView.
#[derive(Default, Clone)]
pub struct ActiveGithubRunShell(pub Option<WeakEntity<LaunchWindowShell>>);

impl gpui::Global for ActiveGithubRunShell {}

pub struct LaunchWindowShell {
    _webview: WebView,
    window_size: Size<Pixels>,
    paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(LaunchWindowShell, paste);

impl WebViewPasteShell for LaunchWindowShell {
    fn active_paste_target(&self) -> Option<&WebView> {
        Some(&self._webview)
    }
}

impl Render for LaunchWindowShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_webview_bounds(window);
        // White backdrop in case the HTML is still painting.
        // track_focus is required so GPUI routes NativePaste actions here
        // even when the WKWebView child has OS first-responder.
        paste_render_wrap!(
            div().size_full().bg(rgb(0xffffff)),
            cx,
            &self.paste.focus_handle
        )
    }
}

impl LaunchWindowShell {
    fn sync_webview_bounds(&mut self, window: &mut Window) {
        let current = window.bounds().size;
        if current == self.window_size {
            return;
        }
        let _ = self._webview.set_bounds(Rect {
            position: LogicalPosition::new(0i32, 0i32).into(),
            size: LogicalSize::new(
                f32::from(current.width) as u32,
                f32::from(current.height) as u32,
            )
            .into(),
        });
        self.window_size = current;
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

    /// Push a textual launch event line into the boot wizard detail/log view.
    pub fn push_detail(&self, detail: &str) {
        let detail_json = serde_json::to_string(detail).unwrap_or_else(|_| "\"\"".to_string());
        let script = format!(
            "typeof window.__atoDetail==='function'&&window.__atoDetail({})",
            detail_json
        );
        let _ = self._webview.evaluate_script(&script);
    }

    /// Show a terminal failure in the boot wizard without opening an
    /// AppWindow. Used when launch orchestration fails before a ready
    /// session exists.
    pub fn show_failure(&self, error: &str) {
        let error_json =
            serde_json::to_string(error).unwrap_or_else(|_| "\"Launch failed\"".to_string());
        let script = format!(
            "typeof window.__atoFail==='function'&&window.__atoFail({})",
            error_json
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

    pub fn open_config_panel(&self) {
        let _ = self
            ._webview
            .evaluate_script("typeof openConfigPanel==='function'&&openConfigPanel()");
    }

    pub fn scroll_config_panel_to_bottom(&self) {
        let _ = self._webview.evaluate_script(
            "const el=document.getElementById('panel-config-body'); if (el) { el.scrollTop = el.scrollHeight; }",
        );
    }

    /// Deliver candidate lookup results into the GitHub Run wizard WebView.
    /// `result` is a JSON value shaped as `{ok:true,candidates:[...]}` or
    /// `{ok:false,error:"..."}`. Guards with `typeof` so a missed call is silent.
    pub fn inject_github_candidates(&self, result: &serde_json::Value) {
        let json = serde_json::to_string(result).unwrap_or_else(|_| "null".to_string());
        let script = format!(
            "typeof window.__ato_github_candidates_result==='function'&&window.__ato_github_candidates_result({})",
            json
        );
        let _ = self._webview.evaluate_script(&script);
    }

    pub fn inject_cli_inference_result(&self, result: &serde_json::Value) {
        let json = serde_json::to_string(result).unwrap_or_else(|_| "null".to_string());
        let script = format!(
            "typeof window.__ato_cli_inference_result==='function'&&window.__ato_cli_inference_result({})",
            json
        );
        let _ = self._webview.evaluate_script(&script);
    }
}

fn open_wizard(
    cx: &mut App,
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
        let launch_url = format!("{LAUNCH_SCHEME}://localhost/");
        let queue_for_ipc = queue.clone();
        let webview = WebViewBuilder::new()
            .with_asynchronous_custom_protocol(LAUNCH_SCHEME.to_string(), |_id, req, responder| {
                let response =
                    resolve_system_capsule_protocol_response(LAUNCH_SLUG, req.uri().path());
                responder.respond(response);
            })
            .with_url(&launch_url)
            .with_initialization_script(&composed)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_ipc))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Launch wizard WebView");
        let shell = cx.new(|cx| LaunchWindowShell {
            _webview: webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        });
        // Give GPUI focus to LaunchWindowShell so NativePaste/NativeCopy
        // key bindings dispatch here even when WKWebView has OS first-responder.
        window.focus(&shell.read(cx).paste.focus_handle.clone(), cx);
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
const GITHUB_RUN_W: f32 = 520.0;
const GITHUB_RUN_H: f32 = 480.0;

/// Spawn the consent wizard with no specific target — used for AODD
/// screenshot generation and standalone MCP testing.
/// Injects a minimal demo preview so the UI renders rather than showing
/// the loading spinner indefinitely.
pub fn open_consent_window(cx: &mut App) -> Result<()> {
    let mut demo_fields: Vec<serde_json::Value> = [
        ("OPENAI_API_KEY", "OpenAI API Key", "sk-..."),
        ("ANTHROPIC_API_KEY", "Anthropic API Key", "sk-ant-..."),
        ("GOOGLE_API_KEY", "Google API Key", "AIza..."),
        ("GROQ_API_KEY", "Groq API Key", "gsk_..."),
        ("MISTRAL_API_KEY", "Mistral API Key", "mis_..."),
        ("COHERE_API_KEY", "Cohere API Key", "co_..."),
        ("XAI_API_KEY", "xAI API Key", "xai-..."),
        ("AZURE_OPENAI_KEY", "Azure OpenAI Key", "aoai-..."),
        (
            "AZURE_OPENAI_ENDPOINT",
            "Azure OpenAI Endpoint",
            "https://example.openai.azure.com",
        ),
        ("LANGFUSE_SECRET_KEY", "Langfuse Secret Key", "sk-lf-..."),
        ("LANGFUSE_PUBLIC_KEY", "Langfuse Public Key", "pk-lf-..."),
        ("CUSTOM_MODEL_NAME", "Custom Model Name", "my-model"),
    ]
    .into_iter()
    .map(|(name, label, placeholder)| {
        serde_json::json!({
            "name": name,
            "label": label,
            "kind": "secret",
            "placeholder": placeholder,
            "already_configured": false,
        })
    })
    .collect();
    demo_fields.push(serde_json::json!({
        "name": "MODEL",
        "label": "Model",
        "kind": "enum",
        "choices": ["gpt-4o-mini", "gpt-4.1-mini", "gpt-4.1", "o4-mini"],
        "already_configured": true,
    }));
    let demo_preview = serde_json::json!({
        "preview_id": "demo",
        "loading": false,
        "name": "chat",
        "handle": "koh0920/byok-ai-chat",
        "capsule_id": "byok-ai-chat",
        "capsule_version": "0.3.4",
        "visited_targets": ["app"],
        "requirements": [
            {
                "type": "secrets_required",
                "target": "app",
                "fields": demo_fields,
            },
            {
                "type": "consent_required",
                "scoped_id": "byok-ai-chat",
                "version": "0.3.4",
                "target_label": "app",
                "policy_segment_hash": "blake3:1a087e1a47d13e659d9c65d7cb88e854308dd777aa292296d50c3816a4ccdaf5",
                "provisioning_policy_hash": "blake3:4cbe69639a92f8d0537b87b8cbb25527df222f30ea842c73e332f655ccc5adee",
                 "summary": "Capsule      : byok-ai-chat@0.3.4\nTarget       : app (runtime=source, driver=node)\nNetwork      : api.openai.com, localhost:3000, 127.0.0.1:3000\nNetwork IDs  : cidr:10.0.0.0/8\nNetwork Note : CIDR/SPIFFE entries are preview-only here; Deno/NodeCompat only apply host/IP allow-net targets.\nRead Only    : None\nRead Write   : None\nSecrets      : OPENAI_API_KEY\nPolicy Hash  : blake3:1a087e1a47d13e659d9c65d7cb88e854308dd777aa292296d50c3816a4ccdaf5\nProvisioning : blake3:4cbe69639a92f8d0537b87b8cbb25527df222f30ea842c73e332f655ccc5adee\nCommand      : npm run dev\nWorking Directory: /",
            },
        ],
    });
    let init_script = format!(
        "window.__ATO_LAUNCH_PREVIEW={};",
        serde_json::to_string(&demo_preview).unwrap_or_else(|_| "null".to_string())
    );
    open_consent_wizard_inner(cx, Some(init_script)).map(|_| ())
}

/// Real launch entrypoint: open the consent wizard for a concrete
/// `GuestRoute`. Stashes the route under `PendingLaunches` keyed by
/// `preview_id` so the broker's Approve handler can find the launch
/// intent on user confirmation.
///
/// Opens the wizard immediately with a loading state, then spawns a
/// background task to run `ato internal preflight` and hydrate the
/// WebView with real capsule identity + requirements.
pub fn open_consent_window_for_route(cx: &mut App, route: GuestRoute) -> Result<()> {
    open_consent_window_for_route_with_client(
        cx,
        route,
        crate::state::session::SessionClientKind::AtoWindow,
    )
}

/// Variant that specifies which display client to attach after approval.
pub fn open_consent_window_for_route_with_client(
    cx: &mut App,
    route: GuestRoute,
    requested_client: crate::state::session::SessionClientKind,
) -> Result<()> {
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

    let stashed = StashedLaunch {
        route,
        requested_client,
    };
    let mut launches = cx.global_mut::<PendingLaunches>();
    launches.0.insert(preview_id.clone(), stashed);

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
        preflight_error: None,
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
                                    default: f.default,
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
                preflight_error: None,
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
                preflight_error: preflight_failed.then(|| format!("{err:#}")),
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

/// Normalize a user-supplied GitHub repo input into the canonical
/// `github.com/{owner}/{repo}` handle format.
///
/// Accepts full URLs (`https://github.com/owner/repo`), `github.com/owner/repo`,
/// and bare `owner/repo` shorthands. Strips trailing slashes and extra path
/// segments (branch refs, sub-paths) beyond the two-segment owner/repo.
pub fn normalize_github_handle(repo: &str) -> String {
    let s = repo.trim();
    let s = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("github.com/"))
        .unwrap_or(s);
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2 {
        format!("github.com/{}/{}", parts[0], parts[1])
    } else {
        format!("github.com/{s}")
    }
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
    let composed = format!(
        "window.__ATO_LAUNCH_SCREEN='consent';\n{}",
        compose_init_script(locale, init_script.as_deref())
    );
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
        let launch_url = format!("{LAUNCH_SCHEME}://localhost/");
        let webview = WebViewBuilder::new()
            .with_asynchronous_custom_protocol(LAUNCH_SCHEME.to_string(), |_id, req, responder| {
                let response =
                    resolve_system_capsule_protocol_response(LAUNCH_SLUG, req.uri().path());
                responder.respond(response);
            })
            .with_url(&launch_url)
            .with_initialization_script(&composed)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_closure))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the consent WebView");
        let shell = cx.new(|cx| LaunchWindowShell {
            _webview: webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        });
        if let Ok(mut slot) = shell_slot_inner.lock() {
            *slot = Some(shell.clone());
        }
        window.focus(&shell.read(cx).paste.focus_handle.clone(), cx);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    system_ipc::spawn_drain_loop(cx, queue, *handle);
    let shell = shell_slot
        .lock()
        .unwrap()
        .take()
        .expect("LaunchWindowShell entity must be populated by open_window closure");
    cx.set_global(ActiveConsentShell(Some(shell.downgrade())));
    Ok((*handle, shell))
}

pub fn open_active_consent_config_panel(cx: &mut App) -> Result<()> {
    let Some(shell) = cx
        .try_global::<ActiveConsentShell>()
        .and_then(|slot| slot.0.clone())
        .and_then(|shell| shell.upgrade())
    else {
        return Err(anyhow::anyhow!("no active consent shell"));
    };
    shell.read(cx).open_config_panel();
    Ok(())
}

pub fn scroll_active_consent_config_panel_to_bottom(cx: &mut App) -> Result<()> {
    let Some(shell) = cx
        .try_global::<ActiveConsentShell>()
        .and_then(|slot| slot.0.clone())
        .and_then(|shell| shell.upgrade())
    else {
        return Err(anyhow::anyhow!("no active consent shell"));
    };
    shell.read(cx).scroll_config_panel_to_bottom();
    Ok(())
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

/// Open the `Run from GitHub` wizard window (screen: github_run).
/// Presents repository input, candidate lookup, detail inspection, and
/// the create/edit TOML path — all before the consent review step.
pub fn open_github_run_window(cx: &mut App) -> Result<AnyWindowHandle> {
    let bounds = Bounds::centered(None, size(px(GITHUB_RUN_W), px(GITHUB_RUN_H)), cx);
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let locale = resolve_locale(crate::config::load_config().general.language);
    let composed = format!(
        "window.__ATO_LAUNCH_SCREEN='github_run';\n{}",
        compose_init_script(locale, None)
    );
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
        let launch_url = format!("{LAUNCH_SCHEME}://localhost/");
        let webview = WebViewBuilder::new()
            .with_asynchronous_custom_protocol(LAUNCH_SCHEME.to_string(), |_id, req, responder| {
                let response =
                    resolve_system_capsule_protocol_response(LAUNCH_SLUG, req.uri().path());
                responder.respond(response);
            })
            .with_url(&launch_url)
            .with_initialization_script(&composed)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_closure))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the github_run WebView");
        let shell = cx.new(|cx| LaunchWindowShell {
            _webview: webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        });
        if let Ok(mut slot) = shell_slot_inner.lock() {
            *slot = Some(shell.clone());
        }
        window.focus(&shell.read(cx).paste.focus_handle.clone(), cx);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    system_ipc::spawn_drain_loop(cx, queue, *handle);
    let shell = shell_slot
        .lock()
        .unwrap()
        .take()
        .expect("LaunchWindowShell entity must be populated by open_window closure");
    cx.set_global(ActiveGithubRunShell(Some(shell.downgrade())));
    Ok(*handle)
}

pub fn start_boot_launch(
    cx: &mut App,
    route: GuestRoute,
    configs: Vec<(String, String)>,
    boot_handle: AnyWindowHandle,
    requested_client: crate::state::session::SessionClientKind,
) {
    let abort_flag = Arc::new(AtomicBool::new(false));
    let boot_shell_weak = cx
        .try_global::<PendingBootShell>()
        .and_then(|g| g.0.clone());
    cx.set_global(PendingBootShell(None));
    cx.set_global(BootWindowSlot {
        boot_window: Some(boot_handle),
        abort_flag: Some(Arc::clone(&abort_flag)),
    });
    if let Some(shell) = boot_shell_weak.as_ref().and_then(|weak| weak.upgrade()) {
        let _ = shell.update(cx, |shell, _cx| {
            shell.push_detail("Launching capsule");
            shell.push_detail("Preparing secure runtime and dependency resolution");
        });
    }

    let Some(handle) = launch_handle_for_route(&route) else {
        show_boot_failure(
            cx,
            &boot_shell_weak,
            "This route cannot be launched as a capsule.",
        );
        return;
    };

    let secret_store = crate::config::load_secrets();
    let secrets: Vec<_> = secret_store.secrets_for_capsule(&handle);
    let launch_configs_for_result = configs.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let (progress_tx, progress_rx) = std::sync::mpsc::channel::<u8>();
    let handle_for_thread = handle.clone();
    let abort_for_thread = Arc::clone(&abort_flag);
    std::thread::spawn(move || {
        let result = crate::orchestrator::resolve_and_start_guest(
            &handle_for_thread,
            &secrets,
            &configs,
            Some(Box::new(move |step| {
                let _ = progress_tx.send(step);
            })),
        );
        if abort_for_thread.load(Ordering::Acquire) {
            if let Ok(ref session) = result {
                let _ = crate::orchestrator::stop_guest_session(&session.session_id);
            }
            return;
        }
        let _ = tx.send(result);
    });

    let async_app = cx.to_async();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    async_app
        .foreground_executor()
        .spawn(async move {
            loop {
                be.timer(Duration::from_millis(100)).await;

                let mut steps = Vec::new();
                while let Ok(step) = progress_rx.try_recv() {
                    steps.push(step);
                }
                if !steps.is_empty() {
                    let shell_for_steps = boot_shell_weak.clone();
                    aa.update(move |cx: &mut App| {
                        if let Some(shell) = shell_for_steps.and_then(|weak| weak.upgrade()) {
                            for step in steps {
                                let _ = shell.update(cx, |shell, _cx| {
                                    shell.push_step(step);
                                    let msg = match step {
                                        0 => "Validating launch plan",
                                        1 => "Resolving capsule targets",
                                        2 => "Starting capsule session",
                                        3 => "Connecting to capsule endpoint",
                                        _ => "Processing launch step",
                                    };
                                    shell.push_detail(msg);
                                });
                            }
                        }
                    });
                }

                match rx.try_recv() {
                    Ok(result) => {
                        let route_for_open = route.clone();
                        let shell_for_result = boot_shell_weak.clone();
                        let aborted = abort_flag.load(Ordering::Acquire);
                        aa.update(move |cx: &mut App| {
                            if aborted {
                                if let Ok(session) = result {
                                    stop_session_async(session.session_id);
                                }
                                close_boot_window_handle(cx, boot_handle);
                                return;
                            }

                            match result {
                                Ok(session) => {
                                    let session_id = session.session_id.clone();
                                    if let Some(shell) =
                                        shell_for_result.as_ref().and_then(|weak| weak.upgrade())
                                    {
                                        let _ = shell.update(cx, |shell, _cx| {
                                            shell.push_detail(
                                                "Capsule session started successfully",
                                            );
                                        });
                                    }

                                    if requested_client
                                        == crate::state::session::SessionClientKind::OsBrowser
                                    {
                                        // OsBrowser path: open in system browser,
                                        // register an OsBrowser client in SessionRegistry.
                                        if let Some(ref url) = session.local_url {
                                            let _ = crate::ui::open_external_url(url);
                                        }
                                        let mut registry = cx
                                            .global_mut::<crate::state::session::SessionRegistry>();
                                        use crate::state::session::{
                                            CapsuleLaunchContext, CapsuleOpenSource, CapsuleSession,
                                            LaunchVia, SessionClient, SessionClientId,
                                            SessionClientKind, SessionClientState,
                                            SessionProcessState,
                                        };
                                        use std::time::SystemTime;
                                        let caps_session = CapsuleSession {
                                            session_id: session.session_id.clone(),
                                            handle: session.handle.clone(),
                                            canonical_handle: session.canonical_handle.clone(),
                                            title: session
                                                .snapshot_label
                                                .clone()
                                                .unwrap_or_else(|| session.handle.clone()),
                                            process_state: SessionProcessState::Ready,
                                            local_url: session.local_url.clone(),
                                            healthcheck_url: session.healthcheck_url.clone(),
                                            launch_context: CapsuleLaunchContext {
                                                handle_or_url: session.handle.clone(),
                                                target: Some(session.target_label.clone()),
                                                launch_configs: launch_configs_for_result.clone(),
                                                requested_client: SessionClientKind::OsBrowser,
                                                source: CapsuleOpenSource::NavigateToUrl,
                                            },
                                            launch_via: LaunchVia::Desktop,
                                            created_at: SystemTime::now(),
                                            last_seen_at: SystemTime::now(),
                                        };
                                        registry.register_session(caps_session);
                                        let client = SessionClient {
                                            client_id: SessionClientId::next(),
                                            session_id: session.session_id.clone(),
                                            client_kind: SessionClientKind::OsBrowser,
                                            window_id: None,
                                            pane_id: None,
                                            state: SessionClientState::External,
                                            attached_at: SystemTime::now(),
                                            last_seen_at: SystemTime::now(),
                                        };
                                        registry.attach_client(client);
                                        close_boot_window_handle(cx, boot_handle);
                                        record_start_history(&route_for_open);
                                    } else {
                                        match crate::window::orchestrator::open_ready_capsule_window(
                                            cx,
                                            route_for_open.clone(),
                                            session,
                                            launch_configs_for_result.clone(),
                                        ) {
                                            Ok(app_handle) => {
                                                close_boot_window_handle(cx, boot_handle);
                                                let _ = app_handle.update(cx, |_, window, _| {
                                                    window.activate_window()
                                                });
                                                record_start_history(&route_for_open);
                                            }
                                            Err(err) => {
                                                stop_session_async(session_id);
                                                if let Some(shell) = shell_for_result
                                                    .as_ref()
                                                    .and_then(|weak| weak.upgrade())
                                                {
                                                    let _ = shell.update(cx, |shell, _cx| {
                                                        shell.push_detail(
                                                            "Failed to create app window from session",
                                                        );
                                                    });
                                                }
                                                show_boot_failure(
                                                    cx,
                                                    &shell_for_result,
                                                    &format!("App window creation failed: {err}"),
                                                );
                                            }
                                        }
                                    }
                                }
                                Err(err) => {
                                    let message =
                                        crate::window::app_capsule_shell::describe_launch_error(
                                            &err,
                                        );
                                    if let Some(shell) =
                                        shell_for_result.as_ref().and_then(|weak| weak.upgrade())
                                    {
                                        let _ = shell.update(cx, |shell, _cx| {
                                            shell.push_detail("Capsule launch returned an error");
                                        });
                                    }
                                    show_boot_failure(cx, &shell_for_result, &message);
                                }
                            }
                        });
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        if !abort_flag.load(Ordering::Acquire) {
                            let shell_for_result = boot_shell_weak.clone();
                            aa.update(move |cx: &mut App| {
                                if let Some(shell) =
                                    shell_for_result.as_ref().and_then(|weak| weak.upgrade())
                                {
                                    let _ = shell.update(cx, |shell, _cx| {
                                        shell.push_detail(
                                            "Launch worker disconnected before returning a result",
                                        );
                                    });
                                }
                                show_boot_failure(
                                    cx,
                                    &shell_for_result,
                                    "Launch task stopped before returning a result.",
                                );
                            });
                        }
                        break;
                    }
                    Err(TryRecvError::Empty) => {
                        if abort_flag.load(Ordering::Acquire) {
                            break;
                        }
                    }
                }
            }
        })
        .detach();
}

fn launch_handle_for_route(route: &GuestRoute) -> Option<String> {
    match route {
        GuestRoute::CapsuleHandle { handle, .. } | GuestRoute::CapsuleUrl { handle, .. } => {
            Some(handle.clone())
        }
        GuestRoute::Capsule { session, .. } => Some(session.clone()),
        _ => None,
    }
}

fn show_boot_failure(
    cx: &mut App,
    shell_weak: &Option<WeakEntity<LaunchWindowShell>>,
    message: &str,
) {
    tracing::error!(error = %message, "ato_launch: capsule boot failed");
    if let Some(shell) = shell_weak.as_ref().and_then(|weak| weak.upgrade()) {
        let _ = shell.update(cx, |shell, _cx| shell.show_failure(message));
    }
}

fn close_boot_window_handle(cx: &mut App, boot_handle: AnyWindowHandle) {
    let _ = boot_handle.update(cx, |_, window, _| window.remove_window());
    cx.set_global(BootWindowSlot::default());
    tracing::info!("ato_launch: boot wizard closed");
}

fn stop_session_async(session_id: String) {
    std::thread::spawn(move || {
        if let Err(err) = crate::orchestrator::stop_guest_session(&session_id) {
            tracing::warn!(
                session_id = %session_id,
                error = %err,
                "ato_launch: failed to stop abandoned session"
            );
        }
    });
}

fn record_start_history(route: &GuestRoute) {
    if let GuestRoute::CapsuleHandle { handle, label }
    | GuestRoute::CapsuleUrl { handle, label, .. } = route
    {
        let mut store = crate::system_capsule::ato_start::StartPageHistoryStore::load();
        store.record_open(handle, label);
        if let Err(err) = store.save() {
            tracing::warn!(error = %err, "ato_launch: failed to save start history");
        }
    }
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
    let composed = format!(
        "window.__ATO_LAUNCH_SCREEN='boot';\n{}",
        compose_init_script(locale, init_script.as_deref())
    );
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
        let launch_url = format!("{LAUNCH_SCHEME}://localhost/");
        let webview = WebViewBuilder::new()
            .with_asynchronous_custom_protocol(LAUNCH_SCHEME.to_string(), |_id, req, responder| {
                let response =
                    resolve_system_capsule_protocol_response(LAUNCH_SLUG, req.uri().path());
                responder.respond(response);
            })
            .with_url(&launch_url)
            .with_initialization_script(&composed)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_closure))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the boot WebView");
        let shell = cx.new(|cx| LaunchWindowShell {
            _webview: webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        });
        if let Ok(mut slot) = shell_slot_inner.lock() {
            *slot = Some(shell.clone());
        }
        window.focus(&shell.read(cx).paste.focus_handle.clone(), cx);
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
            "ato internal preflight failed: preflight collection failed: manifest path does not exist: github.com/owner/repo"
        );

        assert!(is_non_blocking_remote_preflight_error(
            "github.com/owner/repo",
            &err
        ));
        assert!(is_non_blocking_remote_preflight_error(
            "capsule://github.com/Koh0920/cupbear",
            &err
        ));
        assert!(is_non_blocking_remote_preflight_error(
            "capsule://ato.run/koh0920/byok-ai-chat",
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
        assert!(!is_non_blocking_remote_preflight_error(
            "crates/ato-cli/samples/foo",
            &err
        ));
        assert!(!is_non_blocking_remote_preflight_error(
            "koh0920/flatnotes",
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
        let html = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/system/ato-launch/consent.html"
        ))
        .expect("legacy consent html should be readable");
        assert!(html.contains("-webkit-user-select: text;"));
        assert!(html.contains("user-select: text;"));
    }

    #[test]
    fn consent_preview_supports_network_identity_allowlists() {
        let html = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/system/ato-launch/consent.html"
        ))
        .expect("legacy consent html should be readable");
        assert!(html.contains("networkIds"));
        assert!(html.contains("case 'network_ids':"));
        assert!(html.contains("Network IDs"));
    }

    #[test]
    fn launch_paste_script_uses_safe_text_insertion() {
        let script = crate::window::webview_paste::paste_script("a'b\nc");

        assert!(script.contains(r#"const text = "a'b\nc";"#));
        assert!(script.contains("active.setRangeText(text, start, end, 'end');"));
        assert!(script.contains("inputType: 'insertText'"));
    }
}

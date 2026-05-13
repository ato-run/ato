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

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context, IntoElement, Render, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

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

pub struct LaunchWindowShell {
    _webview: WebView,
}

impl Render for LaunchWindowShell {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // White backdrop in case the HTML is still painting.
        div().size_full().bg(rgb(0xffffff))
    }
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
        let mut builder = WebViewBuilder::new()
            .with_html(html)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_ipc))
            .with_bounds(webview_rect);
        if let Some(script) = init_script {
            builder = builder.with_initialization_script(script);
        }
        let webview = builder
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
/// screenshot generation. The wizard JS falls back to its hardcoded
/// demo identity (`WasedaP2P` / `ato.app/shell://wasedap2p`).
pub fn open_consent_window(cx: &mut App) -> Result<()> {
    open_wizard(cx, CONSENT_HTML, CONSENT_W, CONSENT_H, None).map(|_| ())
}

/// Real launch entrypoint: open the consent wizard for a concrete
/// `GuestRoute`. Stashes the route under `PendingLaunchTarget` so the
/// broker's Approve handler can spawn the real AppWindow on user
/// confirmation, and injects an `__ATO_LAUNCH` global so the consent
/// HTML renders the actual capsule identity (name / handle / display URL).
pub fn open_consent_window_for_route(cx: &mut App, route: GuestRoute) -> Result<()> {
    let (display_name, display_handle, display_url) = match &route {
        GuestRoute::CapsuleHandle { handle, label } => {
            let pretty_name = label
                .split(['/', '@', '-', '_'])
                .filter(|s| !s.is_empty())
                .next_back()
                .unwrap_or(label.as_str())
                .to_string();
            (
                pretty_name,
                handle.clone(),
                format!("capsule://{}", handle),
            )
        }
        GuestRoute::ExternalUrl(url) => (
            url.host_str().unwrap_or("external").to_string(),
            url.as_str().to_string(),
            url.as_str().to_string(),
        ),
        // LocalCapsule / Terminal not surfaced through the consent
        // wizard in Phase 1 — the wizard is only on the NavigateToUrl
        // capsule:// path.
        other => (
            format!("{:?}", other),
            "unknown".to_string(),
            "unknown".to_string(),
        ),
    };

    cx.set_global(PendingLaunchTarget(Some(route)));

    // JSON-escape the strings for safe interpolation into JS.
    let payload = serde_json::json!({
        "name":   display_name,
        "handle": display_handle,
        "url":    display_url,
    });
    let init_script = format!(
        "window.__ATO_LAUNCH = {};",
        serde_json::to_string(&payload).unwrap_or_else(|_| "null".to_string())
    );

    open_wizard(cx, CONSENT_HTML, CONSENT_W, CONSENT_H, Some(init_script)).map(|_| ())
}

/// Spawn the boot progress wizard. Returns the `AnyWindowHandle` so the
/// caller can store it in `BootWindowSlot` for later programmatic close.
pub fn open_boot_window(cx: &mut App) -> Result<AnyWindowHandle> {
    open_wizard(cx, BOOT_HTML, BOOT_W, BOOT_H, None)
}

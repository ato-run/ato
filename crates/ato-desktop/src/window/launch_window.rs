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
//! Phase 1 ships these as standalone demonstrable shells. They are
//! NOT yet wired into `orchestrator::resolve_and_start_guest`; the
//! AppWindow spawn path stays as-is. Phase 2 will gate every
//! `LocalCapsule` route on a consent decision and drive the boot
//! window from real orchestrator events.
//!
//! Wizards are intentionally NOT registered in `OpenContentWindows`.
//! They are launch chrome, not destination content — the Card Switcher
//! should not list a half-formed AppWindow's wizard. The user-facing
//! AppWindow that follows a successful approve flow registers itself
//! the normal way via `open_app_window`.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Bounds, Context, IntoElement, Render, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::system_capsule::ipc as system_ipc;

const CONSENT_HTML: &str = include_str!("../../assets/system/ato-launch/consent.html");
const BOOT_HTML: &str = include_str!("../../assets/system/ato-launch/boot.html");

pub struct LaunchWindowShell {
    _webview: WebView,
}

impl Render for LaunchWindowShell {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Pale-violet backdrop in case the HTML is still painting.
        div().size_full().bg(rgb(0xf7f4ff))
    }
}

fn open_wizard(cx: &mut App, html: &'static str, w: f32, h: f32) -> Result<()> {
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
        let webview = WebViewBuilder::new()
            .with_html(html)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_ipc))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Launch wizard WebView");
        let shell = cx.new(|_cx| LaunchWindowShell { _webview: webview });
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    system_ipc::spawn_drain_loop(cx, queue, *handle);
    Ok(())
}

/// Spawn the consent wizard. Sized for the identity list + env-var
/// rows + action buttons described in `consent.html`.
pub fn open_consent_window(cx: &mut App) -> Result<()> {
    open_wizard(cx, CONSENT_HTML, 620.0, 640.0)
}

/// Spawn the boot progress wizard. Narrower than consent — just the
/// capsule badge + steps list + abort button.
pub fn open_boot_window(cx: &mut App) -> Result<()> {
    open_wizard(cx, BOOT_HTML, 500.0, 540.0)
}

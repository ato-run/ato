//! Layer 7 scaffolding — open a separate top-level GPUI window for
//! the **Launcher** surface. The full migration (extracting today's
//! `DesktopShell` as `LauncherShell`, stripping running-app rendering
//! from the Launcher, and moving the `N kept warm` retention pill into
//! the Launcher header — see #170) is a multi-step rename that happens
//! in follow-up commits on this branch. This file lands the spawn
//! primitive so the experimental `OpenLauncherWindow` action has a
//! target.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Bounds, Context, IntoElement, Render, WindowBounds, WindowDecorations,
    WindowOptions,
};
use gpui_component::TitleBar;

/// Placeholder Launcher shell — replaced by the renamed
/// `LauncherShell` (extracted from today's `DesktopShell`) once the
/// rename portion of #170 lands.
pub struct LauncherShellPlaceholder;

impl Render for LauncherShellPlaceholder {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x09090b))
            .text_color(rgb(0xd4d4d8))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .child("Launcher placeholder (Focus View redesign — #170)"),
            )
            .child(
                div()
                    .text_sm()
                    .opacity(0.7)
                    .child("Cmd-Shift-K to refocus this window from any AppWindow"),
            )
    }
}

/// Open a Launcher window when `ATO_DESKTOP_MULTI_WINDOW=1`. The real
/// Launcher migration moves today's `DesktopShell` (rail + chrome +
/// settings panel + retention pill) into this window in follow-up
/// commits. For now the window is a placeholder so the orchestrator
/// pattern can be exercised end-to-end.
pub fn open_launcher_window(cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(1100.0), px(760.0)), cx);
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let shell = cx.new(|_cx| LauncherShellPlaceholder);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    Ok(())
}

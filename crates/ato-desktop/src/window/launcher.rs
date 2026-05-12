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
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context, IntoElement, Render, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;

/// Process-wide slot for the currently-open Launcher window. The
/// Control Bar's Settings / Store buttons dispatch
/// `OpenLauncherWindow`; on a 2nd+ click we want to focus the
/// existing window (bring it to the front) rather than spawn a new
/// one on top.
#[derive(Default)]
pub struct LauncherWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for LauncherWindowSlot {}

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
        // Same light theme as the redesign reference. Real Launcher
        // content (rail + capsule store + settings tabs + retention
        // pill) migrates in once the DesktopShell → LauncherShell
        // rename portion of #170 lands.
        div()
            .size_full()
            .bg(rgb(0xfafafa))
            .text_color(rgb(0x52525b))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .text_color(rgb(0x18181b))
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

/// Open the Launcher window, or focus it if one is already open.
/// First click: opens a new window. Second-and-later clicks: bring
/// the existing window to the foreground (no new window spawned).
/// If the user closed the previous Launcher (red traffic light), the
/// next click opens a fresh one — `app::on_window_closed` clears the
/// slot for us.
pub fn open_launcher_window(cx: &mut App) -> Result<()> {
    // Focus path: an existing handle is tracked. Try to activate it.
    let existing = cx.global::<LauncherWindowSlot>().0;
    if let Some(handle) = existing {
        let result = handle.update(cx, |_, window, _| window.activate_window());
        match result {
            Ok(()) => return Ok(()),
            Err(_) => {
                // Stale handle — window was closed without our
                // cleanup hook running. Drop it and fall through.
                cx.set_global(LauncherWindowSlot(None));
            }
        }
    }

    let bounds = Bounds::centered(None, size(px(1100.0), px(760.0)), cx);
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    let handle = cx.open_window(options, |window, cx| {
        let shell = cx.new(|_cx| LauncherShellPlaceholder);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    cx.set_global(LauncherWindowSlot(Some(*handle)));
    Ok(())
}

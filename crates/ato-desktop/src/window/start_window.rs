//! StartWindow — a standalone "compose a new window" surface,
//! conceptually separate from the Launcher. Every invocation spawns
//! a fresh window: there is no slot-based focus-reuse, so the Card
//! Switcher's "新しいウィンドウ" tile creates a NEW StartWindow each
//! time. The body is the start-view content shared with the Launcher
//! (see `launcher::render_start_view`).
//!
//! Distinct from Launcher:
//!   - Launcher = settings host (Cmd-,) — slot-tracked, focus on
//!     second invocation, single-instance.
//!   - StartWindow = composition surface for a new window — every
//!     click in the Card Switcher creates a fresh instance.
//!
//! Lifecycle: the standard macOS title bar (traffic lights) closes
//! the window; quitting the process auto-cleans. No on_window_closed
//! bookkeeping is needed because no slot tracks the window.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Bounds, Context, IntoElement, Render, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;

pub struct StartWindowShell;

impl Render for StartWindowShell {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Body is the same Start view the Launcher renders when
        // `LauncherViewState::Start` is active. Delegating to a
        // shared helper keeps the visual surface aligned between the
        // two windows without coupling their lifecycles.
        div()
            .size_full()
            .bg(rgb(0xf5f3ff))
            .text_color(rgb(0x18181b))
            .flex()
            .flex_col()
            .items_center()
            .child(crate::window::launcher::render_start_view())
    }
}

/// Spawn a fresh StartWindow. Always creates a new window — there is
/// no slot or focus-reuse pathway here. Callers (Card Switcher new-
/// window tile, OpenStartWindow action handler) invoke this directly
/// instead of going through `dispatch_action` so that opening the
/// window does not depend on the dispatch queue surviving any close-
/// soon-after on the caller side.
pub fn open_start_window(cx: &mut App) -> Result<()> {
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
        let shell = cx.new(|_cx| StartWindowShell);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    // Register the new window in the cross-window content set so the
    // Control Bar Card Switcher badge increments. Eviction is handled
    // in `app::on_window_closed` keyed by the GPUI WindowId.
    cx.global_mut::<crate::state::OpenContentWindows>()
        .insert(handle.window_id().as_u64());
    Ok(())
}

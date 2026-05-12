//! Layer 3 scaffolding — spawn a borderless, floating Control Bar
//! window paired with each `AppWindow`. The Focus View redesign glues
//! this window to its parent via
//! `[parent addChildWindow:bar ordered:NSWindowAbove]` so the OS
//! handles co-movement, Spaces membership, fullscreen transitions, and
//! z-order automatically (see spike #168).
//!
//! This commit lands the spawn primitive and a placeholder content
//! view. The actual `addChildWindow` plumbing — which requires walking
//! from `gpui::Window` → `NSView` → `NSWindow` via `raw_window_handle`
//! — ships in a follow-up commit on this branch alongside the
//! orchestrator registry that pairs each `AppWindowId` with its parent
//! handle. Until that lands, the bar is a free-floating, focus-light
//! window rather than a true child.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Bounds, Context, IntoElement, Render, WindowBounds, WindowDecorations,
    WindowOptions,
};

/// Visual placeholder until layer 4 (#172) replaces the body with the
/// four real affordances (settings cog / URL pill / store / window
/// list).
pub struct ControlBarShellPlaceholder;

impl Render for ControlBarShellPlaceholder {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x18181b))
            .text_color(rgb(0xfafafa))
            .rounded_xl()
            .border_1()
            .border_color(rgb(0x27272a))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_xs()
                    .opacity(0.8)
                    .child("Control Bar placeholder — #171"),
            )
    }
}

/// Open a borderless floating Control Bar window. Sized
/// 520×56, intended to anchor at the bottom-center of its parent
/// `AppWindow` once `addChildWindow:` plumbing lands (see module-level
/// comment).
pub fn open_control_bar_window(cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(520.0), px(56.0)), cx);
    let options = WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let shell = cx.new(|_cx| ControlBarShellPlaceholder);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    Ok(())
}

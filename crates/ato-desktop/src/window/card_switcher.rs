//! Layer 5 scaffolding — borderless full-bleed translucent NSWindow
//! that surfaces an iOS-style task list of open `AppWindow`s. Invoked
//! by the Window-list icon on the Control Bar (#172) or by the
//! Card-Switcher gesture (#174).
//!
//! Real card content keys off `AppState::app_window_mru_order` (#167)
//! and uses `WKWebView.takeSnapshotWithConfiguration:` for each
//! window's preview. Snapshot acquisition, click-to-focus, swipe-up to
//! close, and Esc-to-dismiss are deferred to a follow-up commit that
//! also wires the per-window WebViewManager migration.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, px, rgb, size, App, Bounds, Context, IntoElement, Render, WindowBounds,
    WindowDecorations, WindowKind, WindowOptions,
};

/// Placeholder body for the Card Switcher overlay. Real cards are
/// `WKWebView` snapshots rendered in MRU order.
pub struct CardSwitcherShellPlaceholder;

impl Render for CardSwitcherShellPlaceholder {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .size_full()
            // 70% black backdrop — real implementation will be
            // translucent over the running app's pixels.
            .bg(hsla(0.0, 0.0, 0.0, 0.7))
            .text_color(rgb(0xfafafa))
            .flex()
            .items_center()
            .justify_center()
            .gap_6()
            .child(card_placeholder("WasedaP2P (most recent)"))
            .child(card_placeholder("example.com"))
            .child(card_placeholder("Launcher"))
    }
}

fn card_placeholder(label: &'static str) -> impl IntoElement {
    div()
        .w(px(320.0))
        .h(px(200.0))
        .bg(rgb(0x18181b))
        .border_1()
        .border_color(rgb(0x3f3f46))
        .rounded_xl()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .child(label)
}

/// Open the Card Switcher overlay as a third top-level window. The
/// long-term shape is a full-bleed translucent NSWindow with
/// `setLevel:NSPopUpMenuWindowLevel` painted above every Control Bar;
/// this commit lands the spawn primitive only — sizing, translucency,
/// MRU data binding, and dismissal handling ship in follow-ups.
pub fn open_card_switcher_window(cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(1200.0), px(700.0)), cx);
    let options = WindowOptions {
        titlebar: None,
        focus: true,
        show: true,
        kind: WindowKind::PopUp,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let shell = cx.new(|_cx| CardSwitcherShellPlaceholder);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    Ok(())
}

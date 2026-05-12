//! Layer 5 — borderless full-bleed translucent NSWindow that surfaces
//! an iOS-style task list of open `AppWindow`s. Reads the cross-window
//! `AppWindowRegistry` global at spawn time to render real MRU
//! entries; falls back to a single empty-state card when no windows
//! are open. Real WKWebView snapshots in the cards await the
//! per-window WebViewManager migration.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, px, rgb, size, App, Bounds, Context, FontWeight, IntoElement, Render, SharedString,
    WindowBounds, WindowDecorations, WindowKind, WindowOptions,
};

use crate::state::{AppWindowRegistry, GuestRoute};

/// Snapshot of one registry entry for rendering in the switcher.
/// Detached from `AppWindow` so the switcher's data is immutable for
/// the lifetime of the overlay window.
#[derive(Clone)]
struct CardEntry {
    title: SharedString,
    subtitle: SharedString,
}

pub struct CardSwitcherShellPlaceholder {
    cards: Vec<CardEntry>,
}

impl CardSwitcherShellPlaceholder {
    fn new(cards: Vec<CardEntry>) -> Self {
        Self { cards }
    }
}

impl Render for CardSwitcherShellPlaceholder {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let backdrop = div()
            .size_full()
            .bg(hsla(0.0, 0.0, 0.0, 0.7))
            .text_color(rgb(0xfafafa))
            .flex()
            .items_center()
            .justify_center()
            .gap_6();
        if self.cards.is_empty() {
            backdrop.child(empty_state_card())
        } else {
            let mut row = backdrop;
            for (idx, card) in self.cards.iter().enumerate() {
                row = row.child(mru_card(card.clone(), idx == 0));
            }
            row
        }
    }
}

fn mru_card(card: CardEntry, is_active: bool) -> impl IntoElement {
    let border_color = if is_active {
        rgb(0x6366f1)
    } else {
        rgb(0x3f3f46)
    };
    div()
        .w(px(320.0))
        .h(px(200.0))
        .bg(rgb(0x18181b))
        .border_2()
        .border_color(border_color)
        .rounded_xl()
        .p(px(16.0))
        .flex()
        .flex_col()
        .justify_between()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight(600.0))
                .child(card.title),
        )
        .child(
            div()
                .text_xs()
                .opacity(0.7)
                .child(card.subtitle),
        )
}

fn empty_state_card() -> impl IntoElement {
    div()
        .w(px(360.0))
        .h(px(200.0))
        .bg(rgb(0x18181b))
        .border_1()
        .border_color(rgb(0x3f3f46))
        .rounded_xl()
        .p(px(20.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight(600.0))
                .child("No app windows yet"),
        )
        .child(
            div()
                .text_xs()
                .opacity(0.7)
                .child("Open one via host_dispatch_action OpenAppWindowExperiment"),
        )
}

/// Convert a route into a (title, subtitle) pair for card rendering.
/// Mirrors the same logic AppWindowShell uses, kept duplicated to
/// avoid a cross-window dependency on the App view module.
fn entry_from_route(route: &GuestRoute) -> CardEntry {
    let (title, subtitle) = match route {
        GuestRoute::CapsuleHandle { handle, label } => (label.clone(), handle.clone()),
        GuestRoute::CapsuleUrl { label, url, .. } => (label.clone(), url.to_string()),
        GuestRoute::ExternalUrl(url) => (
            url.host_str()
                .map(|h| h.to_string())
                .unwrap_or_else(|| url.as_str().to_string()),
            url.as_str().to_string(),
        ),
        GuestRoute::Capsule { session, .. } => {
            (session.clone(), format!("capsule://{session}/"))
        }
        GuestRoute::Terminal { session_id } => (
            format!("terminal/{session_id}"),
            format!("terminal://{session_id}/"),
        ),
    };
    CardEntry {
        title: SharedString::from(title),
        subtitle: SharedString::from(subtitle),
    }
}

/// Read the MRU snapshot from the global `AppWindowRegistry` and open
/// the Card Switcher overlay populated with one card per registered
/// AppWindow. Without an `AppWindowRegistry` global, falls back to
/// the empty-state card.
pub fn open_card_switcher_window(cx: &mut App) -> Result<()> {
    let cards: Vec<CardEntry> = {
        let registry = cx.global::<AppWindowRegistry>();
        registry
            .mru_order()
            .into_iter()
            .filter_map(|id| registry.get(id))
            .map(|w| entry_from_route(&w.route))
            .collect()
    };

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
        let shell = cx.new(|_cx| CardSwitcherShellPlaceholder::new(cards));
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    Ok(())
}

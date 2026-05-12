//! Layer 5 — borderless full-bleed translucent NSWindow that surfaces
//! an iOS-style task list of open `AppWindow`s. Reads the cross-window
//! `AppWindowRegistry` global at spawn time to render real MRU
//! entries; falls back to a single empty-state card when no windows
//! are open. Real WKWebView snapshots in the cards await the
//! per-window WebViewManager migration.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, px, rgb, size, AnyWindowHandle, App, Bounds, Context, FocusHandle, Focusable,
    FontWeight, IntoElement, MouseButton, Render, SharedString, WindowBounds, WindowDecorations,
    WindowKind, WindowOptions,
};

use crate::state::{AppWindowRegistry, GuestRoute};

/// Process-wide slot for the currently-open Card Switcher window so
/// the Control Bar's switcher button can behave as a toggle: a
/// second click closes the open switcher instead of stacking a new
/// overlay on top.
#[derive(Default)]
pub struct CardSwitcherWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for CardSwitcherWindowSlot {}

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
    focus_handle: FocusHandle,
}

impl CardSwitcherShellPlaceholder {
    fn new(cards: Vec<CardEntry>, cx: &mut Context<Self>) -> Self {
        Self {
            cards,
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Focusable for CardSwitcherShellPlaceholder {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CardSwitcherShellPlaceholder {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Backdrop catches:
        //   - left mouse down → close the switcher
        //   - Escape key      → close the switcher
        // Cards inside the row call `cx.stop_propagation()` on
        // mouse down so clicking a card does NOT also close — that
        // pathway is reserved for the future "switch to this
        // AppWindow" select action.
        let backdrop = div()
            .id("card-switcher-backdrop")
            .track_focus(&self.focus_handle)
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                cx.set_global(CardSwitcherWindowSlot(None));
                window.remove_window();
            })
            .on_key_down(|event, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.set_global(CardSwitcherWindowSlot(None));
                    window.remove_window();
                }
            })
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
                row = row.child(mru_card(card.clone(), idx, idx == 0));
            }
            row
        }
    }
}

fn mru_card(card: CardEntry, idx: usize, is_active: bool) -> impl IntoElement {
    let border_color = if is_active {
        rgb(0x6366f1)
    } else {
        rgb(0x3f3f46)
    };
    div()
        .id(("card", idx))
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
        // Cards swallow the click so clicking a card does not bubble
        // up to the backdrop's close handler. Future iteration will
        // route the click into "switch to this AppWindow".
        .on_mouse_down(MouseButton::Left, |_, _window, cx| {
            cx.stop_propagation();
        })
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

/// Toggle the Card Switcher overlay. If one is already open
/// (tracked via the `CardSwitcherWindowSlot` global), this closes
/// it. Otherwise it reads the MRU snapshot from `AppWindowRegistry`
/// and opens a fresh overlay. The Control Bar's switcher button
/// dispatches through here so a second click dismisses the overlay
/// instead of stacking another one on top.
pub fn open_card_switcher_window(cx: &mut App) -> Result<()> {
    // If a switcher is already tracked, try to close it. Whether
    // close succeeds or the handle is stale (window was already
    // dismissed via backdrop / Escape), we clear the slot so the
    // next click reopens cleanly. When we successfully closed an
    // existing window, this is a toggle-off — return early.
    let existing = cx.global::<CardSwitcherWindowSlot>().0;
    if let Some(handle) = existing {
        let close_result = handle.update(cx, |_, window, _| window.remove_window());
        cx.set_global(CardSwitcherWindowSlot(None));
        if close_result.is_ok() {
            return Ok(());
        }
        // Handle was stale (window already gone) — fall through to
        // open a fresh switcher.
    }

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
    let handle = cx.open_window(options, |window, cx| {
        let shell = cx.new(|cx| CardSwitcherShellPlaceholder::new(cards, cx));
        // Focus the backdrop so the `on_key_down` Escape handler
        // fires without the user having to click first.
        let focus = shell.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    cx.set_global(CardSwitcherWindowSlot(Some(*handle)));
    Ok(())
}

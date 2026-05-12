//! Layer 5 — borderless full-bleed light-wash NSWindow that surfaces
//! the task list of open `AppWindow`s. Reads the cross-window
//! `AppWindowRegistry` global at spawn time. Layout follows the
//! `.tmp/window-list.png` reference:
//!   - Header strip with icon + title + subtitle
//!   - Row of portrait preview cards (one per open AppWindow)
//!   - Trailing "+ 新しいウィンドウ" tile
//!   - Footer instruction line
//!   - Bottom dock with the same apps as small circular tiles
//! Real WKWebView snapshots inside the cards await the per-window
//! WebViewManager migration; for now the preview area carries a
//! handle-coloured gradient placeholder.
//!
//! Lifecycle is unchanged from the previous iteration: backdrop click
//! and Escape close the switcher; cards stop_propagation so card
//! clicks do not bubble.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, px, rgb, size, AnyWindowHandle, App, Bounds, Context, FocusHandle, Focusable,
    FontWeight, IntoElement, MouseButton, Render, SharedString, WindowBounds, WindowDecorations,
    WindowKind, WindowOptions,
};
use gpui_component::{Icon, IconName};

use crate::app::OpenAppWindowExperiment;
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
    /// First grapheme cluster of the title, uppercased — used as the
    /// fallback "logo" letter when no branded asset is available.
    initial: SharedString,
    /// Deterministic accent colour derived from the handle/title so
    /// each card reads as visually distinct without needing real
    /// branded assets. RGB integer.
    accent: u32,
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
        // Foreground children stop_propagation on mouse down so clicks
        // on cards / dock / new-window tile do not bubble.
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
            // Light wash backdrop with a very subtle violet tint to
            // match the reference's pale-violet cast. The two layered
            // semi-transparent fills emulate the reference's soft
            // top-to-bottom gradient without needing a true gradient
            // primitive.
            .bg(rgb(0xf5f3ff))
            .text_color(rgb(0x18181b))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(28.0));

        let header = header_block(self.cards.len());
        let card_row = card_row(&self.cards);
        let footer = footer_text();
        let dock = bottom_dock(&self.cards);

        backdrop
            .child(header)
            .child(card_row)
            .child(footer)
            .child(dock)
    }
}

fn header_block(window_count: usize) -> impl IntoElement {
    let subtitle = if window_count == 0 {
        SharedString::from("まだウィンドウはありません。新しいウィンドウから始めましょう。")
    } else {
        SharedString::from(format!(
            "現在のセッションにある {window_count} つのアプリと、操作中のものを切り替えます。"
        ))
    };
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .w(px(48.0))
                .h(px(48.0))
                .rounded_xl()
                .bg(rgb(0xe0e7ff))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::GalleryVerticalEnd)
                        .size(px(24.0))
                        .text_color(rgb(0x4f46e5)),
                ),
        )
        .child(
            div()
                .text_size(px(22.0))
                .font_weight(FontWeight(700.0))
                .text_color(rgb(0x18181b))
                .child("開いているウィンドウ"),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(0x71717a))
                .child(subtitle),
        )
}

fn card_row(cards: &[CardEntry]) -> impl IntoElement {
    let mut row = div()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(16.0))
        // Cards swallow clicks so the row's content does not
        // bubble to the backdrop. The row container itself does not
        // intercept clicks — empty gaps still close the switcher.
        ;
    for (idx, card) in cards.iter().enumerate() {
        row = row.child(preview_card(card.clone(), idx, idx == 0));
    }
    row.child(new_window_card())
}

fn preview_card(card: CardEntry, idx: usize, is_active: bool) -> impl IntoElement {
    // Portrait card matching the reference's window-list layout:
    //   - header strip: app icon (accent circle with initial) + name
    //   - middle: preview placeholder (very-light accent wash)
    //   - bottom: pill chip with handle/subtitle
    let card_border = if is_active {
        rgb(0x6366f1)
    } else {
        rgb(0xe4e4e7)
    };
    let accent = rgb(card.accent);
    let accent_wash = hsla_from_rgb_with_alpha(card.accent, 0.10);

    div()
        .id(("card", idx))
        .w(px(220.0))
        .h(px(300.0))
        .bg(rgb(0xffffff))
        .border_2()
        .border_color(card_border)
        .rounded_2xl()
        .overflow_hidden()
        .flex()
        .flex_col()
        // Card click is reserved for "switch to this AppWindow" (to
        // land in a future iteration). For now it merely swallows the
        // event so the backdrop close does not fire.
        .on_mouse_down(MouseButton::Left, |_, _window, cx| {
            cx.stop_propagation();
        })
        // Header row
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(10.0))
                .child(
                    div()
                        .w(px(26.0))
                        .h(px(26.0))
                        .rounded_md()
                        .bg(accent)
                        .text_color(rgb(0xffffff))
                        .text_xs()
                        .font_weight(FontWeight(700.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(card.initial.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .font_weight(FontWeight(600.0))
                        .text_color(rgb(0x18181b))
                        .overflow_hidden()
                        .child(card.title.clone()),
                ),
        )
        // Preview placeholder area — fills remaining height, accent
        // wash, faint dotted-style gradient feel via a soft inner box
        .child(
            div()
                .flex_1()
                .mx(px(12.0))
                .my(px(4.0))
                .rounded_lg()
                .bg(accent_wash)
                .border_1()
                .border_color(hsla_from_rgb_with_alpha(card.accent, 0.18))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::SquareTerminal)
                        .size(px(28.0))
                        .text_color(hsla_from_rgb_with_alpha(card.accent, 0.55)),
                ),
        )
        // Bottom subtitle pill
        .child(
            div()
                .px(px(12.0))
                .py(px(10.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x71717a))
                        .overflow_hidden()
                        .child(card.subtitle.clone()),
                ),
        )
}

/// Trailing tile inside the Card Switcher row that opens a fresh
/// AppWindow. Click dispatches `OpenAppWindowExperiment` (the same
/// action the Control Bar / MCP route through) and dismisses the
/// switcher so the new window is immediately interactable. Sized to
/// match `preview_card` so the row reads as a uniform tile strip.
fn new_window_card() -> impl IntoElement {
    div()
        .id("new-window-card")
        .w(px(220.0))
        .h(px(300.0))
        .bg(rgb(0xfafafa))
        .border_2()
        .border_color(rgb(0xe4e4e7))
        .rounded_2xl()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.0))
        .cursor_pointer()
        .hover(|s| {
            s.bg(rgb(0xf4f4f5))
                .border_color(rgb(0x6366f1))
        })
        // Swallow the click so it does not bubble to the backdrop's
        // close-on-click handler, dispatch the open action, then
        // remove ourselves — order matters: the dispatch is queued
        // onto the global action queue before the window goes away.
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(OpenAppWindowExperiment), cx);
            cx.set_global(CardSwitcherWindowSlot(None));
            window.remove_window();
        })
        .child(
            div()
                .w(px(56.0))
                .h(px(56.0))
                .rounded_full()
                .bg(rgb(0xeef2ff))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::Plus)
                        .size(px(28.0))
                        .text_color(rgb(0x4f46e5)),
                ),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight(600.0))
                .text_color(rgb(0x18181b))
                .child("新しいウィンドウ"),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(0x71717a))
                .child("カプセルまたは URL を入力"),
        )
}

fn footer_text() -> impl IntoElement {
    div()
        .text_xs()
        .text_color(rgb(0x71717a))
        .child("クリックで切り替え、Esc で閉じる")
}

fn bottom_dock(cards: &[CardEntry]) -> impl IntoElement {
    // Bottom strip of small circular tiles — one per open AppWindow
    // plus a trailing "+" tile that mirrors the new-window-card
    // affordance for users who prefer to grab the dock rather than
    // the large card. Sits in a pill-shaped container with a soft
    // white background, matching the reference.
    let mut dock = div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .px(px(16.0))
        .py(px(10.0))
        .rounded_full()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7));
    for (idx, card) in cards.iter().enumerate() {
        dock = dock.child(dock_tile(card.clone(), idx));
    }
    dock.child(dock_new_window_tile())
}

fn dock_tile(card: CardEntry, idx: usize) -> impl IntoElement {
    let accent = rgb(card.accent);
    div()
        .id(("dock", idx))
        .w(px(40.0))
        .h(px(40.0))
        .rounded_lg()
        .bg(accent)
        .text_color(rgb(0xffffff))
        .text_sm()
        .font_weight(FontWeight(700.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, _window, cx| {
            cx.stop_propagation();
        })
        .child(card.initial.clone())
}

fn dock_new_window_tile() -> impl IntoElement {
    div()
        .id("dock-new-window")
        .w(px(40.0))
        .h(px(40.0))
        .rounded_lg()
        .bg(rgb(0xeef2ff))
        .text_color(rgb(0x4f46e5))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(rgb(0xe0e7ff)))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(OpenAppWindowExperiment), cx);
            cx.set_global(CardSwitcherWindowSlot(None));
            window.remove_window();
        })
        .child(Icon::new(IconName::Plus).size(px(18.0)))
}

/// Convert a route into a (title, subtitle, initial, accent) tuple
/// for card rendering. Mirrors the same logic AppWindowShell uses,
/// kept duplicated to avoid a cross-window dependency on the App
/// view module. Accent colour is a deterministic hash of the handle
/// so cards stay visually distinct without bundled brand assets.
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
    let initial = title
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());
    let accent = accent_for(&title);
    CardEntry {
        title: SharedString::from(title),
        subtitle: SharedString::from(subtitle),
        initial: SharedString::from(initial),
        accent,
    }
}

/// Deterministic accent colour for a given title — picks from a
/// curated set of brand-safe pastel-leaning hues so each app card
/// reads as visually distinct without ever looking garish.
fn accent_for(title: &str) -> u32 {
    const PALETTE: &[u32] = &[
        0x4f46e5, // indigo
        0x0ea5e9, // sky
        0x10b981, // emerald
        0xf59e0b, // amber
        0xef4444, // red
        0xa855f7, // violet
        0xec4899, // pink
        0x14b8a6, // teal
    ];
    let hash = title
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    PALETTE[(hash as usize) % PALETTE.len()]
}

/// Build an hsla colour derived from an RGB integer, with the given
/// alpha. Used to compose translucent fills tied to a card's accent
/// without hand-tuning each combination.
fn hsla_from_rgb_with_alpha(rgb_int: u32, alpha: f32) -> gpui::Hsla {
    let r = ((rgb_int >> 16) & 0xff) as f32 / 255.0;
    let g = ((rgb_int >> 8) & 0xff) as f32 / 255.0;
    let b = (rgb_int & 0xff) as f32 / 255.0;
    // Convert RGB → HSL via standard formula; gpui's `hsla` consumes
    // hue in 0..1.
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let (h, s) = if max == min {
        (0.0, 0.0)
    } else {
        let d = max - min;
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        let h = if max == r {
            ((g - b) / d) + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            ((b - r) / d) + 2.0
        } else {
            ((r - g) / d) + 4.0
        } / 6.0;
        (h, s)
    };
    hsla(h, s, l, alpha)
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

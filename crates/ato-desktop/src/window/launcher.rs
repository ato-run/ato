//! Layer 7 — Launcher window (the "Start" surface for ato-desktop).
//! Renders the layout described by `.tmp/start-window.png`:
//!   - Header (icon puck + 新しいウィンドウ title + subtitle)
//!   - Search bar (URL / command palette entry point)
//!   - 4 quick-action tiles (URLで開く / ローカル / ストア / 新しく始める)
//!   - Two-column block: 最近のセッション (rows) and ストア (cards)
//!   - Bottom dock matching the Card Switcher dock
//!   - Footer instruction line
//!
//! Data inside the recent-sessions and store sections is placeholder
//! for now — wiring to live `AppState` retention + capsule discovery
//! lands in a follow-up. The window content is non-functional but the
//! `OpenAppWindowExperiment` dispatch is wired on the relevant tiles
//! so users can act on the obvious affordances.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, px, rgb, size, AnyWindowHandle, App, Bounds, Context, FontWeight, IntoElement,
    MouseButton, Render, SharedString, WindowBounds, WindowDecorations, WindowOptions,
};
use gpui_component::{Icon, IconName, TitleBar};

use crate::app::OpenAppWindowExperiment;

/// Process-wide slot for the currently-open Launcher window. The
/// Control Bar's Settings / Store buttons dispatch
/// `OpenLauncherWindow`; on a 2nd+ click we want to focus the
/// existing window (bring it to the front) rather than spawn a new
/// one on top.
#[derive(Default)]
pub struct LauncherWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for LauncherWindowSlot {}

/// Which view the Launcher is currently rendering. The Control Bar's
/// Settings cog dispatches `ShowSettings`, which flips this global to
/// `Settings`. The Launcher's `observe_global` subscription wakes the
/// render loop so the view swap happens automatically.
#[derive(Default, Clone, Copy, PartialEq)]
pub enum LauncherView {
    #[default]
    Start,
    Settings,
}

#[derive(Default, Clone, Copy)]
pub struct LauncherViewState(pub LauncherView);
impl gpui::Global for LauncherViewState {}

pub struct LauncherShellPlaceholder;

impl LauncherShellPlaceholder {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Re-render when the launcher view request flips (e.g. Control
        // Bar dispatches ShowSettings). Same pattern as the Control
        // Bar's AppWindowRegistry subscription.
        cx.observe_global::<LauncherViewState>(|_view, cx| {
            cx.notify();
        })
        .detach();
        Self
    }
}

impl Render for LauncherShellPlaceholder {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.global::<LauncherViewState>().0;
        let root = div()
            .size_full()
            .bg(rgb(0xf5f3ff))
            .text_color(rgb(0x18181b))
            .flex()
            .flex_col()
            .items_center();
        match view {
            LauncherView::Start => root.child(render_start_view()),
            LauncherView::Settings => root.child(settings_content()),
        }
    }
}

/// Render the Start view body. Shared with `StartWindow` so the two
/// surfaces stay visually aligned — Launcher hosts it when its view
/// state is `Start`, StartWindow always hosts it.
pub(crate) fn render_start_view() -> impl IntoElement {
    div()
        .w(px(960.0))
        .px(px(40.0))
        .py(px(32.0))
        .flex()
        .flex_col()
        .gap(px(24.0))
        .child(header_block())
        .child(search_bar())
        .child(quick_actions_row())
        .child(two_column_block())
        .child(bottom_dock())
        .child(footer_text())
}

fn header_block() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.0))
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
                    Icon::new(IconName::Plus)
                        .size(px(24.0))
                        .text_color(rgb(0x4f46e5)),
                ),
        )
        .child(
            div()
                .text_size(px(22.0))
                .font_weight(FontWeight(700.0))
                .text_color(rgb(0x18181b))
                .child("新しいウィンドウ"),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(0x71717a))
                .child("カプセルを開く、または URL やコマンドを入力します。"),
        )
}

fn search_bar() -> impl IntoElement {
    div()
        .w_full()
        .h(px(48.0))
        .px(px(16.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .rounded_full()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .child(
            Icon::new(IconName::Search)
                .size(px(18.0))
                .text_color(rgb(0x71717a)),
        )
        .child(
            div()
                .flex_1()
                .text_color(rgb(0xa1a1aa))
                .text_sm()
                .child("URLでひらく、またはコマンドを入力"),
        )
        .child(
            div()
                .h(px(22.0))
                .px(px(8.0))
                .rounded_md()
                .bg(rgb(0xf4f4f5))
                .border_1()
                .border_color(rgb(0xe4e4e7))
                .text_xs()
                .font_weight(FontWeight(600.0))
                .text_color(rgb(0x71717a))
                .flex()
                .items_center()
                .child("⌘ K"),
        )
}

fn quick_actions_row() -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .gap(px(12.0))
        .child(quick_action(
            IconName::Globe,
            "URLで開く",
            "Web URLを開く",
            0x0ea5e9,
            QuickAction::OpenUrl,
        ))
        .child(quick_action(
            IconName::Folder,
            "ローカルから開く",
            "ファイルやフォルダ",
            0xf59e0b,
            QuickAction::OpenLocal,
        ))
        .child(quick_action(
            IconName::Search,
            "ストアで探す",
            "公開カプセル",
            0xa855f7,
            QuickAction::OpenStore,
        ))
        .child(quick_action(
            IconName::Plus,
            "新しく始める",
            "空のセッション",
            0x4f46e5,
            QuickAction::NewSession,
        ))
}

#[derive(Copy, Clone)]
enum QuickAction {
    OpenUrl,
    OpenLocal,
    OpenStore,
    NewSession,
}

fn quick_action(
    icon: IconName,
    title: &'static str,
    subtitle: &'static str,
    accent: u32,
    action: QuickAction,
) -> impl IntoElement {
    let accent_rgb = rgb(accent);
    let accent_wash = hsla_with_alpha(accent, 0.12);
    div()
        .id(SharedString::from(format!("quick-{title}")))
        .flex_1()
        .h(px(96.0))
        .px(px(14.0))
        .py(px(14.0))
        .rounded_xl()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .flex()
        .flex_col()
        .justify_between()
        .cursor_pointer()
        .hover(|s| s.border_color(accent_rgb).bg(rgb(0xfafafa)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| match action {
            QuickAction::NewSession => {
                window.dispatch_action(Box::new(OpenAppWindowExperiment), cx);
            }
            // Other actions are still wiring placeholders — dispatch
            // OpenAppWindowExperiment as a sensible fallback so the
            // tile is not a dead click.
            _ => {
                window.dispatch_action(Box::new(OpenAppWindowExperiment), cx);
            }
        })
        .child(
            div()
                .w(px(32.0))
                .h(px(32.0))
                .rounded_md()
                .bg(accent_wash)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).size(px(18.0)).text_color(accent_rgb)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight(600.0))
                        .text_color(rgb(0x18181b))
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x71717a))
                        .child(subtitle),
                ),
        )
}

fn two_column_block() -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .gap(px(16.0))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(section_header("最近のセッション"))
                .child(recent_sessions_list()),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(section_header("ストア"))
                .child(store_cards_list()),
        )
}

fn section_header(title: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight(700.0))
                .text_color(rgb(0x52525b))
                .child(title),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(0xa1a1aa))
                .child("すべて見る"),
        )
}

fn recent_sessions_list() -> impl IntoElement {
    let mut col = div().flex().flex_col().gap(px(6.0));
    for (name, handle, accent) in [
        ("WasedaP2P", "github.com/Koh0920/WasedaP2P", 0x4f46e5),
        ("CodeLab", "github.com/ato-run/codelab", 0x0ea5e9),
        ("QueryX", "github.com/ato-run/queryx", 0x10b981),
        ("Local AI Chat", "local://ai-chat", 0xa855f7),
    ] {
        col = col.child(recent_session_row(name, handle, accent));
    }
    col
}

fn recent_session_row(
    name: &'static str,
    handle: &'static str,
    accent: u32,
) -> impl IntoElement {
    let accent_rgb = rgb(accent);
    let initial: SharedString = SharedString::from(
        name.chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string()),
    );
    div()
        .id(SharedString::from(format!("recent-{name}")))
        .w_full()
        .h(px(56.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .rounded_lg()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .cursor_pointer()
        .hover(|s| s.border_color(accent_rgb))
        .child(
            div()
                .w(px(32.0))
                .h(px(32.0))
                .rounded_md()
                .bg(accent_rgb)
                .text_color(rgb(0xffffff))
                .text_xs()
                .font_weight(FontWeight(700.0))
                .flex()
                .items_center()
                .justify_center()
                .child(initial),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight(600.0))
                        .text_color(rgb(0x18181b))
                        .child(name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x71717a))
                        .child(handle),
                ),
        )
        .child(
            Icon::new(IconName::ChevronRight)
                .size(px(16.0))
                .text_color(rgb(0xa1a1aa)),
        )
}

fn store_cards_list() -> impl IntoElement {
    let mut col = div().flex().flex_col().gap(px(6.0));
    for (name, sub, accent) in [
        ("データ可視化", "BI / ダッシュボード", 0x14b8a6),
        ("メモリストア", "ベクトル & グラフ", 0xec4899),
        ("音声メモ", "ローカル文字起こし", 0xef4444),
        ("ストアを開く", "公開カプセル一覧", 0x4f46e5),
    ] {
        col = col.child(store_card_row(name, sub, accent));
    }
    col
}

fn store_card_row(
    name: &'static str,
    sub: &'static str,
    accent: u32,
) -> impl IntoElement {
    let accent_rgb = rgb(accent);
    let accent_wash = hsla_with_alpha(accent, 0.12);
    div()
        .id(SharedString::from(format!("store-{name}")))
        .w_full()
        .h(px(56.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .rounded_lg()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .cursor_pointer()
        .hover(|s| s.border_color(accent_rgb))
        .child(
            div()
                .w(px(32.0))
                .h(px(32.0))
                .rounded_md()
                .bg(accent_wash)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::SquareTerminal)
                        .size(px(16.0))
                        .text_color(accent_rgb),
                ),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight(600.0))
                        .text_color(rgb(0x18181b))
                        .child(name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x71717a))
                        .child(sub),
                ),
        )
        .child(
            Icon::new(IconName::ChevronRight)
                .size(px(16.0))
                .text_color(rgb(0xa1a1aa)),
        )
}

fn bottom_dock() -> impl IntoElement {
    let mut dock = div()
        .self_center()
        .flex()
        .items_center()
        .gap(px(12.0))
        .px(px(16.0))
        .py(px(10.0))
        .rounded_full()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7));
    for (name, accent) in [
        ("WasedaP2P", 0x4f46e5),
        ("CodeLab", 0x0ea5e9),
        ("QueryX", 0x10b981),
        ("Local AI Chat", 0xa855f7),
    ] {
        dock = dock.child(dock_tile(name, accent));
    }
    dock.child(dock_new_window_tile())
}

fn dock_tile(name: &'static str, accent: u32) -> impl IntoElement {
    let initial: SharedString = SharedString::from(
        name.chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string()),
    );
    div()
        .id(SharedString::from(format!("dock-{name}")))
        .w(px(40.0))
        .h(px(40.0))
        .rounded_lg()
        .bg(rgb(accent))
        .text_color(rgb(0xffffff))
        .text_sm()
        .font_weight(FontWeight(700.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .child(initial)
}

fn dock_new_window_tile() -> impl IntoElement {
    div()
        .id("launcher-dock-new")
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
            window.dispatch_action(Box::new(OpenAppWindowExperiment), cx);
        })
        .child(Icon::new(IconName::Plus).size(px(18.0)))
}

fn footer_text() -> impl IntoElement {
    div()
        .self_center()
        .text_xs()
        .text_color(rgb(0x71717a))
        .child("Tabキーでカプセルを切り替え、新しいウィンドウは ⌘N で開けます")
}

// ---------- Settings view ----------

fn settings_content() -> impl IntoElement {
    // Full-bleed two-column layout. Left nav rail + main panel,
    // contained inside a single rounded card so the settings surface
    // visually sits as one element on the violet wash, matching the
    // reference. Total max width ~960 to stay readable inside the
    // 1100px Launcher window.
    div()
        .w(px(960.0))
        .h_full()
        .py(px(24.0))
        .child(
            div()
                .h_full()
                .rounded_2xl()
                .bg(rgb(0xffffff))
                .border_1()
                .border_color(rgb(0xe4e4e7))
                .overflow_hidden()
                .flex()
                .child(settings_nav())
                .child(settings_main_panel()),
        )
}

fn settings_nav() -> impl IntoElement {
    let mut col = div()
        .w(px(180.0))
        .h_full()
        .px(px(12.0))
        .py(px(16.0))
        .bg(rgb(0xfafafa))
        .border_r_1()
        .border_color(rgb(0xe4e4e7))
        .flex()
        .flex_col()
        .gap(px(2.0));
    // The first nav item is a "back to start" affordance — clicking
    // it flips LauncherViewState back to Start so the user can return
    // to the start view without closing the window.
    col = col.child(nav_back_to_start());
    col = col.child(div().h(px(8.0)));
    for (label, active) in [
        ("一般", false),
        ("セキュリティ", true),
        ("ターミナル", false),
        ("ネットワーク", false),
        ("ランタイム", false),
        ("ストレージ", false),
        ("ストア", false),
        ("同期", false),
        ("このアプリについて", false),
    ] {
        col = col.child(nav_item(label, active));
    }
    col
}

fn nav_back_to_start() -> impl IntoElement {
    div()
        .id("nav-back-start")
        .w_full()
        .h(px(32.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded_md()
        .cursor_pointer()
        .text_xs()
        .text_color(rgb(0x71717a))
        .hover(|s| s.bg(rgb(0xf4f4f5)))
        .on_mouse_down(MouseButton::Left, |_, _window, cx| {
            cx.set_global(LauncherViewState(LauncherView::Start));
        })
        .child(
            Icon::new(IconName::ChevronLeft)
                .size(px(14.0))
                .text_color(rgb(0x71717a)),
        )
        .child("スタートに戻る")
}

fn nav_item(label: &'static str, active: bool) -> impl IntoElement {
    let (bg, text, weight) = if active {
        (rgb(0xeef2ff), rgb(0x4f46e5), FontWeight(600.0))
    } else {
        (rgb(0xfafafa), rgb(0x52525b), FontWeight(500.0))
    };
    div()
        .id(SharedString::from(format!("nav-{label}")))
        .w_full()
        .h(px(36.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .rounded_md()
        .bg(bg)
        .text_color(text)
        .text_sm()
        .font_weight(weight)
        .cursor_pointer()
        .hover(|s| if active { s } else { s.bg(rgb(0xf4f4f5)) })
        .child(label)
}

fn settings_main_panel() -> impl IntoElement {
    div()
        .flex_1()
        .h_full()
        .px(px(28.0))
        .py(px(24.0))
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap(px(20.0))
        .child(
            div()
                .text_size(px(20.0))
                .font_weight(FontWeight(700.0))
                .text_color(rgb(0x18181b))
                .child("設定"),
        )
        .child(security_summary_block())
        .child(startup_behaviour_block())
        .child(publishers_block())
        .child(privacy_cards_row())
}

fn security_summary_block() -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .gap(px(12.0))
        // Main security card (60%)
        .child(
            div()
                .flex_1()
                .px(px(16.0))
                .py(px(16.0))
                .rounded_xl()
                .bg(rgb(0xfafafa))
                .border_1()
                .border_color(rgb(0xe4e4e7))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x71717a))
                        .child("セキュリティレベル"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            div()
                                .w(px(36.0))
                                .h(px(36.0))
                                .rounded_full()
                                .bg(rgb(0xeef2ff))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    Icon::new(IconName::CircleCheck)
                                        .size(px(20.0))
                                        .text_color(rgb(0x4f46e5)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(18.0))
                                        .font_weight(FontWeight(700.0))
                                        .text_color(rgb(0x18181b))
                                        .child("標準"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(0x71717a))
                                        .child("推奨されるバランスのとれた設定です"),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x4f46e5))
                        .child("レベルを変更する →"),
                ),
        )
        .child(security_stats_column())
}

fn security_stats_column() -> impl IntoElement {
    div()
        .w(px(260.0))
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(stat_row("Files processed", "1,240"))
        .child(stat_row("Network checks", "308"))
        .child(stat_row("Security score", "92/100"))
}

fn stat_row(label: &'static str, value: &'static str) -> impl IntoElement {
    div()
        .w_full()
        .h(px(40.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .justify_between()
        .rounded_lg()
        .bg(rgb(0xfafafa))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .child(
            div()
                .text_xs()
                .text_color(rgb(0x71717a))
                .child(label),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight(700.0))
                .text_color(rgb(0x18181b))
                .child(value),
        )
}

fn startup_behaviour_block() -> impl IntoElement {
    div()
        .w_full()
        .px(px(16.0))
        .py(px(16.0))
        .rounded_xl()
        .bg(rgb(0xfafafa))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .flex()
        .flex_col()
        .gap(px(12.0))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight(700.0))
                .text_color(rgb(0x18181b))
                .child("起動時の挙動"),
        )
        .child(toggle_row("ログインで自動起動", true))
        .child(toggle_row("最後のセッションを復元", true))
        .child(toggle_row("ネットワークアクセスをデフォルトで許可", false))
}

fn toggle_row(label: &'static str, enabled: bool) -> impl IntoElement {
    let (track_bg, knob_align) = if enabled {
        (rgb(0x4f46e5), "right")
    } else {
        (rgb(0xd4d4d8), "left")
    };
    let knob = div()
        .w(px(16.0))
        .h(px(16.0))
        .rounded_full()
        .bg(rgb(0xffffff));
    let track = if knob_align == "right" {
        div()
            .w(px(36.0))
            .h(px(20.0))
            .rounded_full()
            .bg(track_bg)
            .px(px(2.0))
            .flex()
            .items_center()
            .justify_end()
            .child(knob)
    } else {
        div()
            .w(px(36.0))
            .h(px(20.0))
            .rounded_full()
            .bg(track_bg)
            .px(px(2.0))
            .flex()
            .items_center()
            .justify_start()
            .child(knob)
    };
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_sm()
                .text_color(rgb(0x18181b))
                .child(label),
        )
        .child(track)
}

fn publishers_block() -> impl IntoElement {
    div()
        .w_full()
        .px(px(16.0))
        .py(px(16.0))
        .rounded_xl()
        .bg(rgb(0xfafafa))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight(700.0))
                        .text_color(rgb(0x18181b))
                        .child("信頼済みの発行元"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x4f46e5))
                        .child("管理 →"),
                ),
        )
        .child(publisher_row("Ato Labs", 0x4f46e5))
        .child(publisher_row("Waseda University", 0x0ea5e9))
        .child(publisher_row("DataMesh Inc.", 0x10b981))
}

fn publisher_row(name: &'static str, accent: u32) -> impl IntoElement {
    let initial: SharedString = SharedString::from(
        name.chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string()),
    );
    div()
        .w_full()
        .h(px(36.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .w(px(24.0))
                .h(px(24.0))
                .rounded_md()
                .bg(rgb(accent))
                .text_color(rgb(0xffffff))
                .text_xs()
                .font_weight(FontWeight(700.0))
                .flex()
                .items_center()
                .justify_center()
                .child(initial),
        )
        .child(
            div()
                .flex_1()
                .text_sm()
                .text_color(rgb(0x18181b))
                .child(name),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(0x10b981))
                .child("検証済み"),
        )
}

fn privacy_cards_row() -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .gap(px(12.0))
        .child(privacy_card(
            IconName::CircleCheck,
            "プライバシー保護",
            "ローカル処理を優先",
            0x4f46e5,
        ))
        .child(privacy_card(
            IconName::HardDrive,
            "データ保護",
            "送信前に同意を要求",
            0x0ea5e9,
        ))
        .child(privacy_card(
            IconName::Folder,
            "ローカル優先",
            "オフラインでも利用可",
            0x10b981,
        ))
}

fn privacy_card(
    icon: IconName,
    title: &'static str,
    subtitle: &'static str,
    accent: u32,
) -> impl IntoElement {
    let accent_rgb = rgb(accent);
    let accent_wash = hsla_with_alpha(accent, 0.12);
    div()
        .flex_1()
        .h(px(80.0))
        .px(px(14.0))
        .py(px(12.0))
        .rounded_xl()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(
            div()
                .w(px(36.0))
                .h(px(36.0))
                .rounded_md()
                .bg(accent_wash)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).size(px(20.0)).text_color(accent_rgb)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight(700.0))
                        .text_color(rgb(0x18181b))
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x71717a))
                        .child(subtitle),
                ),
        )
}

// ---------- shared ----------

/// Build an hsla colour derived from an RGB integer, with the given
/// alpha. Mirrors the helper in `card_switcher.rs` — kept duplicated
/// to avoid a cross-window dependency on that module's accent helpers.
fn hsla_with_alpha(rgb_int: u32, alpha: f32) -> gpui::Hsla {
    let r = ((rgb_int >> 16) & 0xff) as f32 / 255.0;
    let g = ((rgb_int >> 8) & 0xff) as f32 / 255.0;
    let b = (rgb_int & 0xff) as f32 / 255.0;
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

/// Open the Launcher window, or focus it if one is already open.
/// First click: opens a new window. Second-and-later clicks: bring
/// the existing window to the foreground (no new window spawned).
/// If the user closed the previous Launcher (red traffic light), the
/// next click opens a fresh one — `app::on_window_closed` clears the
/// slot for us.
pub fn open_launcher_window(cx: &mut App) -> Result<()> {
    let existing = cx.global::<LauncherWindowSlot>().0;
    if let Some(handle) = existing {
        let result = handle.update(cx, |_, window, _| window.activate_window());
        match result {
            Ok(()) => return Ok(()),
            Err(_) => {
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
        let shell = cx.new(|cx| LauncherShellPlaceholder::new(cx));
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    cx.set_global(LauncherWindowSlot(Some(*handle)));
    // Register in the cross-window content registry so the Control
    // Bar badge increments AND the Card Switcher renders a card.
    use crate::window::content_windows::{
        ContentWindowEntry, ContentWindowKind, OpenContentWindows,
    };
    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::Launcher,
            title: gpui::SharedString::from("Launcher"),
            subtitle: gpui::SharedString::from("設定 · 検索 · ストア"),
            last_focused_at: std::time::Instant::now(),
        },
    );
    Ok(())
}

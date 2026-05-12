//! Layer 2 — spawn a top-level GPUI window per `AppWindow`. Renders
//! a richer placeholder dashboard matching the redesign reference so
//! the multi-window UX can be evaluated visually before the real
//! WKWebView attaches.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, linear_color_stop, linear_gradient, px, rgb, size, App, Bounds, Context, FontWeight,
    IntoElement, Render, SharedString, WindowBounds, WindowDecorations, WindowOptions,
};
use gpui_component::{Icon, IconName, TitleBar};

use crate::state::GuestRoute;

/// Mock dashboard view rendered inside each spawned `AppWindow`.
/// Picks up the visual language of the redesign reference (light
/// gradient backdrop, rounded cards, side panels) so the multi-window
/// scaffolding can be assessed against the mockups without the real
/// guest WebView attached.
pub struct AppWindowShell {
    title: SharedString,
    route_label: SharedString,
}

impl AppWindowShell {
    pub fn new(route: &GuestRoute) -> Self {
        Self {
            title: SharedString::from(short_title_from_route(route)),
            route_label: SharedString::from(route.label()),
        }
    }

    pub fn route_label(&self) -> SharedString {
        self.route_label.clone()
    }
}

fn short_title_from_route(route: &GuestRoute) -> String {
    match route {
        GuestRoute::CapsuleHandle { label, .. } | GuestRoute::CapsuleUrl { label, .. } => {
            label.clone()
        }
        GuestRoute::ExternalUrl(url) => url
            .host_str()
            .map(|h| h.to_string())
            .unwrap_or_else(|| url.as_str().to_string()),
        GuestRoute::Capsule { session, .. } => session.clone(),
        GuestRoute::Terminal { session_id } => format!("terminal/{session_id}"),
    }
}

impl Render for AppWindowShell {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = self.title.clone();
        let route_label = self.route_label.clone();
        div()
            .size_full()
            .bg(linear_gradient(
                135.0,
                linear_color_stop(hsla(210.0 / 360.0, 0.65, 0.94, 1.0), 0.0),
                linear_color_stop(hsla(345.0 / 360.0, 0.55, 0.95, 1.0), 1.0),
            ))
            .text_color(rgb(0x18181b))
            .flex()
            .flex_col()
            .p(px(24.0))
            .gap(px(20.0))
            .child(top_card_row())
            .child(title_block(title, route_label))
            .child(bottom_panel_row())
    }
}

fn top_card_row() -> impl IntoElement {
    div()
        .flex()
        .gap(px(16.0))
        .items_stretch()
        .child(preview_card("CodeLab", IconName::SquareTerminal, 0x6366f1))
        .child(preview_card("Discover", IconName::ChartPie, 0x10b981))
        .child(div().flex_1())
        .child(safety_summary_card())
}

fn preview_card(label: &'static str, icon: IconName, accent: u32) -> impl IntoElement {
    div()
        .w(px(180.0))
        .h(px(110.0))
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .rounded(px(12.0))
        .p(px(12.0))
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(
            div()
                .flex()
                .gap(px(8.0))
                .items_center()
                .child(
                    div()
                        .w(px(28.0))
                        .h(px(28.0))
                        .rounded(px(8.0))
                        .bg(rgb(accent))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(icon).size(px(16.0)).text_color(rgb(0xffffff))),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight(600.0))
                        .child(label),
                ),
        )
        .child(div().flex_1())
        .child(
            div()
                .h(px(32.0))
                .rounded(px(6.0))
                .bg(rgb(0xf4f4f5)),
        )
}

fn safety_summary_card() -> impl IntoElement {
    let row = |label: &'static str, value: &'static str, accent: u32| {
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .w(px(8.0))
                    .h(px(8.0))
                    .rounded_full()
                    .bg(rgb(accent)),
            )
            .child(div().flex_1().text_sm().child(label))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight(600.0))
                    .text_color(rgb(0x52525b))
                    .child(value),
            )
    };
    div()
        .w(px(260.0))
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .rounded(px(12.0))
        .p(px(14.0))
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    Icon::new(IconName::Globe)
                        .size(px(16.0))
                        .text_color(rgb(0x6366f1)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight(600.0))
                        .child("Safety summary"),
                ),
        )
        .child(row("Network shield", "正常", 0x10b981))
        .child(row("Files protected", "1,240", 0x6366f1))
        .child(row("Security score", "92/100", 0xf59e0b))
}

fn title_block(title: SharedString, route_label: SharedString) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .text_3xl()
                .font_weight(FontWeight(700.0))
                .child(title),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(0x71717a))
                .child(route_label),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(0xa1a1aa))
                .child("App window placeholder — real WKWebView attaches in a follow-up"),
        )
}

fn bottom_panel_row() -> impl IntoElement {
    div()
        .flex_1()
        .flex()
        .gap(px(16.0))
        .child(stat_panel(
            "ネットワーク",
            &[("Peers", "128"), ("Documents", "368"), ("Videos", "96")],
            0x6366f1,
        ))
        .child(stat_panel(
            "転送状況",
            &[("Upload", "1.2 TB"), ("Download", "688 GB"), ("Protected", "1,240")],
            0x10b981,
        ))
        .child(stat_panel(
            "ストレージ",
            &[("Documents", "32%"), ("Videos", "28%"), ("Images", "18%")],
            0xf59e0b,
        ))
        .child(terminal_panel())
}

fn stat_panel(
    title: &'static str,
    rows: &[(&'static str, &'static str)],
    accent: u32,
) -> gpui::Div {
    let mut body = div()
        .flex_1()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .rounded(px(12.0))
        .p(px(14.0))
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .w(px(8.0))
                        .h(px(8.0))
                        .rounded_full()
                        .bg(rgb(accent)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight(600.0))
                        .child(title),
                ),
        );
    for (label, value) in rows {
        body = body.child(
            div()
                .flex()
                .items_center()
                .text_sm()
                .text_color(rgb(0x52525b))
                .child(div().flex_1().child(*label))
                .child(
                    div()
                        .font_weight(FontWeight(600.0))
                        .text_color(rgb(0x18181b))
                        .child(*value),
                ),
        );
    }
    body
}

fn terminal_panel() -> impl IntoElement {
    div()
        .w(px(260.0))
        .bg(rgb(0x18181b))
        .rounded(px(12.0))
        .p(px(14.0))
        .flex()
        .flex_col()
        .gap(px(4.0))
        .text_color(rgb(0xe4e4e7))
        .text_xs()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .text_sm()
                .font_weight(FontWeight(600.0))
                .text_color(rgb(0xfafafa))
                .child(
                    Icon::new(IconName::SquareTerminal)
                        .size(px(14.0))
                        .text_color(rgb(0x10b981)),
                )
                .child(div().child("ターミナル")),
        )
        .child(div().text_color(rgb(0x71717a)).child("> ato sync --watch"))
        .child(div().text_color(rgb(0x10b981)).child("> Scanning..."))
        .child(div().text_color(rgb(0xa1a1aa)).child("> Secure connection: OK"))
        .child(div().text_color(rgb(0xa1a1aa)).child("> Syncing 120 files"))
        .child(div().text_color(rgb(0x10b981)).child("> Completed"))
}

/// Open a new top-level GPUI window hosting the placeholder
/// `AppWindowShell` for the given guest route, paired with its
/// Control Bar. The Control Bar position is offset toward the top of
/// the App Window so the two visually read as a unit until the real
/// `addChildWindow:` plumbing lands.
pub fn open_app_window(cx: &mut App, route: GuestRoute) -> Result<()> {
    // Compute bounds explicitly rather than `Bounds::centered` so we
    // can reserve breathing room ABOVE the AppWindow for the floating
    // Control Bar — otherwise the bar sits flush against the macOS
    // menu bar and visually fuses with system chrome.
    let display = cx.primary_display();
    let app_w = px(1100.0);
    let app_h = px(720.0);
    // Bar window is ~92px tall (60 bar + 2*16 padding); we want at
    // least the bar's height plus a visual gap between menu bar and
    // bar, plus a gap between bar and the parent title bar.
    let top_reserve = px(140.0);
    let app_bounds = match display {
        Some(d) => {
            let display_bounds = d.bounds();
            let left = display_bounds.origin.x
                + (display_bounds.size.width - app_w) / 2.0;
            let top = display_bounds.origin.y + top_reserve;
            Bounds {
                origin: gpui::point(left, top),
                size: size(app_w, app_h),
            }
        }
        None => Bounds::centered(None, size(app_w, app_h), cx),
    };
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(app_bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    let route_for_view = route.clone();
    cx.open_window(options, move |window, cx| {
        let shell = cx.new(|_cx| AppWindowShell::new(&route_for_view));
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    // Pair every spawned app window with its Control Bar window,
    // positioned just above the app window's top edge so the two
    // surfaces read as a single composition. `addChildWindow:` will
    // replace this static positioning with true OS-managed tracking
    // once the lower-level plumbing lands.
    let route_for_bar = route.clone();
    if let Err(err) = super::control_bar::open_control_bar_window_at(cx, app_bounds, route_for_bar) {
        tracing::error!(error = %err, "failed to open control bar window");
    }
    Ok(())
}

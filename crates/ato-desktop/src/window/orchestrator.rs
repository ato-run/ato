//! Layer 2 — spawn a top-level GPUI window per `AppWindow`. Renders
//! a richer placeholder dashboard matching the redesign reference so
//! the multi-window UX can be evaluated visually before the real
//! WKWebView attaches.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, linear_color_stop, linear_gradient, px, rgb, size, AnyWindowHandle, App, Bounds,
    Context, FontWeight, IntoElement, Render, SharedString, WindowBounds, WindowDecorations,
    WindowOptions,
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

/// Canonical URL string for the Control Bar URL field when this
/// AppWindow is the MRU front entry. The bar's URL is the
/// identifier of the FEATURE, not the page — see the architectural
/// note in `web_link_view.rs`. So:
///   - ExternalUrl(...) → `ato://web-viewer` (the WebLinkView is a
///     Desktop built-in; the in-page chrome shows the real https URL)
///   - CapsuleHandle / CapsuleUrl / Capsule → `capsule://<...>`
///     since the bar's capsule glyph + the capsule:// scheme are
///     the user's primary identity cue for capsule apps
///   - Terminal → `terminal://<session_id>/`
fn url_for_route(route: &GuestRoute) -> SharedString {
    use crate::state::GuestRoute as R;
    let s = match route {
        R::ExternalUrl(_) => "ato://web-viewer".to_string(),
        R::CapsuleHandle { handle, .. } => format!("capsule://{handle}"),
        R::CapsuleUrl { handle, url, .. } => format!("capsule://{handle}/{url}"),
        R::Capsule { session, .. } => format!("capsule://{session}/"),
        R::Terminal { session_id } => format!("terminal://{session_id}/"),
    };
    SharedString::from(s)
}

/// (title, subtitle) pair used by the cross-window content registry
/// to render Card Switcher entries for AppWindows. Subtitle is the
/// handle / URL / session ID so users can disambiguate two cards with
/// the same display label.
fn title_subtitle_for_route(route: &GuestRoute) -> (SharedString, SharedString) {
    use crate::state::GuestRoute as R;
    let (title, subtitle) = match route {
        R::CapsuleHandle { handle, label } => (label.clone(), handle.clone()),
        R::CapsuleUrl { label, url, .. } => (label.clone(), url.to_string()),
        R::ExternalUrl(url) => (
            url.host_str()
                .map(|h| h.to_string())
                .unwrap_or_else(|| url.as_str().to_string()),
            url.as_str().to_string(),
        ),
        R::Capsule { session, .. } => (session.clone(), format!("capsule://{session}/")),
        R::Terminal { session_id } => (
            format!("terminal/{session_id}"),
            format!("terminal://{session_id}/"),
        ),
    };
    (SharedString::from(title), SharedString::from(subtitle))
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
            .bg(rgb(0xfafafa))
            .text_color(rgb(0x18181b))
            .flex()
            .flex_col()
            .p(px(20.0))
            .gap(px(16.0))
            .child(hero_banner(title, route_label))
            .child(bottom_panel_row())
    }
}

/// Banner that owns the upper half of the AppWindow surface. Strong
/// blue→pink diagonal gradient with preview cards + Safety summary
/// floating at the top and the capsule title block anchored at
/// bottom-left, matching the reference mockup's "WasedaP2P" hero.
fn hero_banner(title: SharedString, route_label: SharedString) -> impl IntoElement {
    div()
        .h(px(300.0))
        .w_full()
        .rounded(px(16.0))
        .relative()
        .overflow_hidden()
        .bg(linear_gradient(
            135.0,
            linear_color_stop(hsla(210.0 / 360.0, 0.70, 0.92, 1.0), 0.0),
            linear_color_stop(hsla(345.0 / 360.0, 0.60, 0.94, 1.0), 1.0),
        ))
        // Cards row floating at top
        .child(
            div()
                .absolute()
                .top(px(20.0))
                .left(px(20.0))
                .right(px(20.0))
                .flex()
                .gap(px(14.0))
                .items_stretch()
                .child(preview_card(
                    "CodeLab",
                    IconName::SquareTerminal,
                    0x6366f1,
                    code_preview_body().into_any_element(),
                ))
                .child(preview_card(
                    "Discover",
                    IconName::ChartPie,
                    0x10b981,
                    chart_preview_body().into_any_element(),
                ))
                .child(div().flex_1())
                .child(safety_summary_card()),
        )
        // Title anchored at bottom-left of the banner
        .child(
            div()
                .absolute()
                .bottom(px(20.0))
                .left(px(28.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .text_size(px(36.0))
                        .font_weight(FontWeight(700.0))
                        .text_color(rgb(0x18181b))
                        .child(title),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x52525b))
                        .child("安全・シンプル・つながる"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x71717a))
                        .child(route_label),
                ),
        )
}

fn preview_card(
    label: &'static str,
    icon: IconName,
    accent: u32,
    body: gpui::AnyElement,
) -> impl IntoElement {
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
                        .w(px(24.0))
                        .h(px(24.0))
                        .rounded(px(6.0))
                        .bg(rgb(accent))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(icon).size(px(14.0)).text_color(rgb(0xffffff))),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight(600.0))
                        .child(label),
                ),
        )
        .child(div().flex_1().child(body))
}

/// Mock "code preview" body — six short rectangles of varying width
/// and colour suggesting a syntax-highlighted code block. Replaces
/// the earlier blank grey rect on the CodeLab preview card.
fn code_preview_body() -> impl IntoElement {
    let line = |segments: &[(f32, u32)]| -> gpui::Div {
        let mut row = div().flex().gap(px(4.0)).h(px(6.0));
        for (w, color) in segments {
            row = row.child(div().w(px(*w)).h_full().rounded(px(2.0)).bg(rgb(*color)));
        }
        row
    };
    div()
        .size_full()
        .bg(rgb(0x0f172a))
        .rounded(px(6.0))
        .p(px(6.0))
        .flex()
        .flex_col()
        .gap(px(4.0))
        // Each line: (width_px, color_rgb). Colours mimic a typical
        // editor theme (keyword purple, ident white, string orange,
        // comment gray, number cyan).
        .child(line(&[(18.0, 0xa78bfa), (40.0, 0xe2e8f0)]))
        .child(line(&[(30.0, 0x60a5fa), (8.0, 0xe2e8f0), (24.0, 0xfb923c)]))
        .child(line(&[(8.0, 0xe2e8f0), (50.0, 0x94a3b8)]))
        .child(line(&[(14.0, 0xa78bfa), (12.0, 0xe2e8f0), (20.0, 0x22d3ee)]))
        .child(line(&[(36.0, 0xe2e8f0)]))
}

/// Mock "chart preview" body — five vertical bars of varying height
/// suggesting a bar chart. Replaces the earlier blank grey rect on
/// the Discover preview card.
fn chart_preview_body() -> impl IntoElement {
    let bar = |h: f32, color: u32| -> gpui::Div {
        div()
            .w(px(10.0))
            .h(px(h))
            .rounded(px(2.0))
            .bg(rgb(color))
    };
    div()
        .size_full()
        .bg(rgb(0xeff6ff))
        .rounded(px(6.0))
        .p(px(6.0))
        .flex()
        .items_end()
        .justify_between()
        .child(bar(14.0, 0x60a5fa))
        .child(bar(26.0, 0x3b82f6))
        .child(bar(20.0, 0x6366f1))
        .child(bar(36.0, 0x10b981))
        .child(bar(22.0, 0x10b981))
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

#[allow(dead_code)]
fn title_block(title: SharedString, route_label: SharedString) -> impl IntoElement {
    // Retained for reference / future use. The active layout puts
    // the title block inside the hero banner via `hero_banner`.
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
}

fn bottom_panel_row() -> impl IntoElement {
    div()
        .flex_1()
        .flex()
        .gap(px(16.0))
        .child(network_panel())
        .child(transfer_panel())
        .child(storage_panel())
        .child(terminal_panel())
}

/// Reusable panel chrome — title strip + body. Mirrors the
/// reference mockup's card styling.
fn panel_card(accent: u32, title: &'static str) -> gpui::Div {
    div()
        .flex_1()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .rounded(px(12.0))
        .p(px(14.0))
        .flex()
        .flex_col()
        .gap(px(8.0))
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
        )
}

/// `ネットワーク` panel — a small radial peer dot cluster suggesting
/// a P2P mesh, plus a legend mapping colours to peer categories.
fn network_panel() -> gpui::Div {
    let legend_row =
        |color: u32, label: &'static str, value: &'static str| -> gpui::Div {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_xs()
                .text_color(rgb(0x52525b))
                .child(
                    div()
                        .w(px(6.0))
                        .h(px(6.0))
                        .rounded_full()
                        .bg(rgb(color)),
                )
                .child(div().flex_1().child(label))
                .child(
                    div()
                        .font_weight(FontWeight(600.0))
                        .text_color(rgb(0x18181b))
                        .child(value),
                )
        };
    panel_card(0x6366f1, "ネットワーク")
        .child(
            div()
                .flex()
                .gap(px(10.0))
                .items_stretch()
                .child(peer_cluster_graph())
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(legend_row(0x6366f1, "Peers", "128"))
                        .child(legend_row(0xf472b6, "Documents", "368"))
                        .child(legend_row(0x10b981, "Videos", "96"))
                        .child(legend_row(0x60a5fa, "Images", "312"))
                        .child(legend_row(0xa1a1aa, "Others", "64")),
                ),
        )
}

/// Tiny peer-mesh dot graph — colours match the legend.
fn peer_cluster_graph() -> gpui::Div {
    let dot = |x: f32, y: f32, color: u32, sz: f32| -> gpui::Div {
        div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(sz))
            .h(px(sz))
            .rounded_full()
            .bg(rgb(color))
    };
    div()
        .relative()
        .w(px(96.0))
        .h(px(96.0))
        // central node
        .child(dot(42.0, 42.0, 0x6366f1, 12.0))
        // satellites
        .child(dot(10.0, 18.0, 0x60a5fa, 8.0))
        .child(dot(72.0, 12.0, 0xf472b6, 8.0))
        .child(dot(78.0, 56.0, 0x10b981, 8.0))
        .child(dot(20.0, 72.0, 0xa1a1aa, 8.0))
        .child(dot(54.0, 80.0, 0xf472b6, 6.0))
        .child(dot(4.0, 50.0, 0x10b981, 6.0))
}

/// `転送状況` panel — three labelled progress bars matching the
/// reference's Upload / Download / Files-protected rows.
fn transfer_panel() -> gpui::Div {
    let bar = |label: &'static str,
               value: &'static str,
               filled: f32,
               accent: u32|
     -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(rgb(0x52525b))
                    .child(div().flex_1().child(label))
                    .child(
                        div()
                            .font_weight(FontWeight(600.0))
                            .text_color(rgb(0x18181b))
                            .child(value),
                    ),
            )
            .child(
                div()
                    .relative()
                    .h(px(6.0))
                    .w_full()
                    .rounded_full()
                    .bg(rgb(0xe4e4e7))
                    .child(
                        div()
                            .h(px(6.0))
                            .w(gpui::relative(filled))
                            .rounded_full()
                            .bg(rgb(accent)),
                    ),
            )
    };
    panel_card(0x10b981, "転送状況")
        .child(bar("Upload 1.2 TB", "68%", 0.68, 0x10b981))
        .child(bar("Download 688 GB", "42%", 0.42, 0x3b82f6))
        .child(bar("Files protected 1,240", "88%", 0.88, 0x6366f1))
}

/// `ストレージ` panel — horizontal stacked bar (donut approximation
/// since GPUI lacks built-in arc rendering) + legend.
fn storage_panel() -> gpui::Div {
    let legend_row =
        |color: u32, label: &'static str, value: &'static str| -> gpui::Div {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_xs()
                .text_color(rgb(0x52525b))
                .child(
                    div()
                        .w(px(8.0))
                        .h(px(8.0))
                        .rounded(px(2.0))
                        .bg(rgb(color)),
                )
                .child(div().flex_1().child(label))
                .child(
                    div()
                        .font_weight(FontWeight(600.0))
                        .text_color(rgb(0x18181b))
                        .child(value),
                )
        };
    panel_card(0xf59e0b, "ストレージ")
        .child(
            div()
                .h(px(12.0))
                .w_full()
                .rounded_full()
                .overflow_hidden()
                .flex()
                .child(div().w(gpui::relative(0.32)).h_full().bg(rgb(0x6366f1)))
                .child(div().w(gpui::relative(0.28)).h_full().bg(rgb(0x3b82f6)))
                .child(div().w(gpui::relative(0.18)).h_full().bg(rgb(0xf59e0b)))
                .child(div().w(gpui::relative(0.22)).h_full().bg(rgb(0xa1a1aa))),
        )
        .child(legend_row(0x6366f1, "Documents", "32%"))
        .child(legend_row(0x3b82f6, "Videos", "28%"))
        .child(legend_row(0xf59e0b, "Images", "18%"))
        .child(legend_row(0xa1a1aa, "Others", "22%"))
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
/// Control Bar. Returns the AppWindow's `AnyWindowHandle` so callers
/// (e.g. `app::run`'s Focus-mode automation dispatcher) can route
/// keyboard actions to it.
pub fn open_app_window(cx: &mut App, route: GuestRoute) -> Result<AnyWindowHandle> {
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
    // Register this AppWindow in the cross-window MRU registry so
    // the Card Switcher (#173) can surface real entries. Population
    // happens BEFORE `cx.open_window` so that even if the GPUI open
    // fails the registry stays consistent — we'd remove on failure.
    let app_window_id = cx
        .global_mut::<crate::state::AppWindowRegistry>()
        .open(route.clone());

    let route_for_view = route.clone();
    let app_handle = cx.open_window(options, move |window, cx| {
        // Branch on route kind:
        //   - ExternalUrl → WebLinkView (Wry WebView + browser
        //     chrome). This is the Desktop's built-in browser view
        //     for https links — NOT a capsule.
        //   - Everything else → AppWindowShell hero placeholder for
        //     now. Real capsule runtime will replace this once the
        //     per-window WebViewManager migration lands.
        match route_for_view.clone() {
            crate::state::GuestRoute::ExternalUrl(url) => {
                let shell = cx.new(|cx| {
                    crate::window::web_link_view::WebLinkViewShell::new(url, window, cx)
                });
                cx.new(|cx| gpui_component::Root::new(shell, window, cx))
            }
            _ => {
                let shell = cx.new(|_cx| AppWindowShell::new(&route_for_view));
                cx.new(|cx| gpui_component::Root::new(shell, window, cx))
            }
        }
    })?;

    // Stamp the GPUI WindowId on the registry entry so
    // `cx.on_window_closed` in `app.rs` can map a close event back
    // to the registry slot and evict the entry. Without this the
    // registry would grow monotonically as AppWindows come and go.
    let gpui_window_id = app_handle.window_id().as_u64();
    if let Some(entry) = cx
        .global_mut::<crate::state::AppWindowRegistry>()
        .get_mut(app_window_id)
    {
        entry.gpui_window_id = Some(gpui_window_id);
    }
    // Register in the cross-window content registry so the Control
    // Bar badge increments AND the Card Switcher renders a card for
    // this AppWindow. Title/subtitle derive from the route.
    use crate::window::content_windows::{
        ContentWindowEntry, ContentWindowKind, OpenContentWindows,
    };
    let (entry_title, entry_subtitle) = title_subtitle_for_route(&route);
    let entry_url = url_for_route(&route);
    cx.global_mut::<OpenContentWindows>().insert(
        gpui_window_id,
        ContentWindowEntry {
            handle: *app_handle,
            kind: ContentWindowKind::AppWindow { route: route.clone() },
            title: entry_title,
            subtitle: entry_subtitle,
            url: entry_url,
            last_focused_at: std::time::Instant::now(),
        },
    );

    // The Control Bar is intentionally NOT spawned as a child of
    // this AppWindow. It has a separate lifecycle (Focus-mode shell
    // singleton, opened once at startup by `open_focus_control_bar`)
    // so that closing an AppWindow does not also tear the bar down
    // via macOS's `addChildWindow:` auto-close contract. Tracking
    // co-movement is sacrificed in exchange for lifecycle clarity:
    // the user has a single, persistent Control Bar across all
    // AppWindow open/close cycles.
    let _ = app_bounds; // bar positioning moved out of this function
    Ok(*app_handle)
}

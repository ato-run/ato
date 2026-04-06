use gpui::prelude::*;
use gpui::{div, hsla, linear_color_stop, linear_gradient, point, px, rgb, BoxShadow, Div, FontWeight, IntoElement};

use crate::state::AppState;

use super::share::short_workspace_label;

const NAV_ITEM_SIZE: f32 = 36.0;
const APP_ICON_SIZE: f32 = 22.0;

pub(super) fn render_workspace_rail(state: &AppState) -> impl IntoElement {
    let active = state.active_workspace;
    let workspaces = state.workspaces.clone();

    div()
        .w(px(52.0))
        .min_w(px(52.0))
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .py_3()
        .gap_1()
        // Glass-heavy background matching mock: rgba(30, 30, 36, 0.85)
        .bg(hsla(240.0 / 360.0, 0.09, 0.13, 0.85))
        .border_r_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.06))
        // Workspace nav items
        .children(workspaces.into_iter().enumerate().map(move |(i, ws)| {
            let is_active = ws.id == active;
            let label = short_workspace_label(&ws.title);
            let hue = workspace_hue(i);
            render_nav_item(is_active, &label, hue)
        }))
        // Separator
        .child(render_nav_separator())
        // Git branch visualization
        .child(render_branch_section())
        // Spacer to push settings to bottom
        .child(div().flex_1())
        // Settings icon at bottom
        .child(render_settings_nav_item())
}

fn render_nav_item(active: bool, label: &str, hue: f32) -> Div {
    let item = div()
        .w(px(NAV_ITEM_SIZE))
        .h(px(NAV_ITEM_SIZE))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .relative();

    let item = if active {
        item.bg(hsla(217.0 / 360.0, 0.60, 0.50, 0.15))
            // Active indicator bar on the left
            .child(
                div()
                    .absolute()
                    .left(px(-8.0))
                    .top_1_2()
                    .mt(px(-9.0))
                    .w(px(3.0))
                    .h(px(18.0))
                    .rounded_r(px(3.0))
                    .bg(rgb(0x3b82f6)),
            )
    } else {
        item
    };

    item.child(render_app_icon(label, hue, active))
}

fn render_app_icon(label: &str, hue: f32, active: bool) -> Div {
    let saturation = if active { 0.65 } else { 0.50 };
    let lightness = if active { 0.55 } else { 0.42 };

    div()
        .w(px(APP_ICON_SIZE))
        .h(px(APP_ICON_SIZE))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(linear_gradient(
            135.,
            linear_color_stop(hsla(hue / 360.0, saturation, lightness, 1.0), 0.),
            linear_color_stop(
                hsla(
                    ((hue + 30.0) % 360.0) / 360.0,
                    saturation * 0.9,
                    lightness * 0.85,
                    1.0,
                ),
                1.,
            ),
        ))
        .shadow(vec![BoxShadow {
            color: hsla(hue / 360.0, saturation, lightness, 0.35),
            offset: point(px(0.), px(2.)),
            blur_radius: px(6.),
            spread_radius: px(0.),
        }])
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.10))
        .text_size(px(9.0))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(0xffffff))
        .child(label.to_string())
}

fn render_nav_separator() -> Div {
    div()
        .w(px(24.0))
        .h(px(1.0))
        .bg(hsla(0.0, 0.0, 1.0, 0.06))
        .my_1p5()
}

/// Git branch visualization matching the mock — a branch icon + connecting lines + dots.
fn render_branch_section() -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(3.0))
        // Branch icon
        .child(
            div()
                .text_size(px(14.0))
                .text_color(rgb(0xa78bfa))
                .child("⑂"),
        )
        // Connecting line
        .child(
            div()
                .w(px(1.0))
                .h(px(12.0))
                .bg(hsla(0.0, 0.0, 1.0, 0.10)),
        )
        // Filled branch dot (active)
        .child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .rounded_full()
                .bg(rgb(0xa78bfa))
                .cursor_pointer()
                .shadow(vec![BoxShadow {
                    color: hsla(270.0 / 360.0, 0.73, 0.73, 0.3),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(4.),
                    spread_radius: px(0.),
                }]),
        )
        // Connecting line
        .child(
            div()
                .w(px(1.0))
                .h(px(12.0))
                .bg(hsla(0.0, 0.0, 1.0, 0.10)),
        )
        // Hollow branch dot (inactive)
        .child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .rounded_full()
                .border_1()
                .border_color(rgb(0xa78bfa))
                .cursor_pointer(),
        )
}

fn render_settings_nav_item() -> Div {
    div()
        .w(px(NAV_ITEM_SIZE))
        .h(px(NAV_ITEM_SIZE))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .child(
            div()
                .w(px(APP_ICON_SIZE))
                .h(px(APP_ICON_SIZE))
                .rounded(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0.0, 0.0, 1.0, 0.06))
                .text_size(px(14.0))
                .text_color(hsla(0.0, 0.0, 1.0, 0.32))
                .child("⚙"),
        )
}

fn workspace_hue(index: usize) -> f32 {
    // Cycle through visually distinct hues matching the mock's palette
    const HUES: &[f32] = &[
        217.0, // blue
        270.0, // purple
        160.0, // green
        45.0,  // amber
        0.0,   // red
        25.0,  // orange
    ];
    HUES[index % HUES.len()]
}

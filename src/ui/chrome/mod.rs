mod window_controls;

use gpui::prelude::*;
use gpui::{
    div, hsla, point, px, rgb, BoxShadow, FontWeight, IntoElement, Window, WindowControlArea,
};

use crate::state::{AppState, ShellMode};

use self::window_controls::{default_window_control_buttons, render_window_controls};
use super::share::render_user_avatar;
use super::CHROME_HEIGHT;

pub(super) fn render_command_chrome(
    _window: &mut Window,
    state: &AppState,
    command_bar: bool,
    task_title: &str,
    active_route: &str,
) -> impl IntoElement {
    div()
        .h(px(CHROME_HEIGHT))
        .px(px(14.0))
        .flex()
        .items_center()
        .gap_3()
        // Glass-heavy background matching mock: rgba(30, 30, 36, 0.85) + backdrop-blur
        .bg(hsla(240.0 / 360.0, 0.09, 0.13, 0.85))
        .border_b_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.06))
        .child(render_window_controls(default_window_control_buttons()))
        .child(render_user_avatar())
        .child(
            div()
                .flex_1()
                .flex()
                .justify_center()
                .window_control_area(WindowControlArea::Drag)
                .child(render_omnibar(command_bar, task_title, active_route)),
        )
        .child(render_overview_toggle(state))
}

fn render_omnibar(command_bar: bool, _task_title: &str, _active_route: &str) -> impl IntoElement {
    div()
        .h(px(30.0))
        .w_full()
        .max_w(px(560.0))
        .rounded(px(8.0))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .bg(if command_bar {
            hsla(221.0 / 360.0, 0.18, 0.20, 0.98)
        } else {
            hsla(0.0, 0.0, 1.0, 0.05)
        })
        .border_1()
        .border_color(if command_bar {
            hsla(217.0 / 360.0, 0.88, 0.61, 0.44)
        } else {
            hsla(0.0, 0.0, 1.0, 0.06)
        })
        .when(command_bar, |this| {
            this.shadow(vec![BoxShadow {
                color: hsla(217.0 / 360.0, 0.88, 0.61, 0.25),
                offset: point(px(0.), px(0.)),
                blur_radius: px(12.),
                spread_radius: px(3.),
            }])
        })
        // Search icon
        .child(
            div()
                .text_size(px(13.0))
                .text_color(if command_bar {
                    hsla(217.0 / 360.0, 0.88, 0.60, 1.0)
                } else {
                    hsla(0.0, 0.0, 1.0, 0.32)
                })
                .child("⌕"),
        )
        // Placeholder text
        .child(
            div()
                .flex_1()
                .text_size(px(12.5))
                .font_weight(FontWeight(400.0))
                .text_color(hsla(0.0, 0.0, 1.0, 0.32))
                .child("Search files, run commands, or ask AI\u{2026}".to_string()),
        )
        // Shortcut badge
        .when(!command_bar, |this| {
            this.child(
                div()
                    .rounded(px(4.0))
                    .bg(hsla(0.0, 0.0, 1.0, 0.06))
                    .px(px(5.0))
                    .py(px(1.0))
                    .text_size(px(10.0))
                    .text_color(hsla(0.0, 0.0, 1.0, 0.32))
                    .child("⌘ K"),
            )
        })
}

fn render_overview_toggle(state: &AppState) -> impl IntoElement {
    let active = matches!(state.shell_mode, ShellMode::Overview);

    div()
        .w(px(30.0))
        .h(px(30.0))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .border_1()
        .border_color(if active {
            hsla(217.0 / 360.0, 0.60, 0.50, 0.3)
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .bg(if active {
            hsla(217.0 / 360.0, 0.60, 0.50, 0.15)
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .child(render_overview_icon(active))
}

/// Two stacked mini-window rectangles with traffic-light dots,
/// matching the mock's overview toggle icon.
fn render_overview_icon(active: bool) -> impl IntoElement {
    let border_color = if active {
        rgb(0x3b82f6)
    } else {
        hsla(0.0, 0.0, 1.0, 0.32).into()
    };

    let dot_color = if active {
        rgb(0x3b82f6)
    } else {
        hsla(0.0, 0.0, 1.0, 0.32).into()
    };

    div()
        .w(px(20.0))
        .h(px(16.0))
        .relative()
        // Back window
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .w(px(14.0))
                .h(px(11.0))
                .rounded(px(2.5))
                .border_1()
                .border_color(border_color)
                // Dots inside back window
                .child(
                    div()
                        .absolute()
                        .top(px(2.0))
                        .left(px(3.0))
                        .flex()
                        .gap(px(1.5))
                        .child(div().size(px(2.5)).rounded_full().bg(dot_color))
                        .child(div().size(px(2.5)).rounded_full().bg(dot_color))
                        .child(div().size(px(2.5)).rounded_full().bg(dot_color)),
                ),
        )
        // Front window
        .child(
            div()
                .absolute()
                .top(px(5.0))
                .left(px(5.0))
                .w(px(14.0))
                .h(px(11.0))
                .rounded(px(2.5))
                .border_1()
                .border_color(border_color)
                // Dots inside front window
                .child(
                    div()
                        .absolute()
                        .top(px(2.0))
                        .left(px(3.0))
                        .flex()
                        .gap(px(1.5))
                        .child(div().size(px(2.5)).rounded_full().bg(dot_color))
                        .child(div().size(px(2.5)).rounded_full().bg(dot_color))
                        .child(div().size(px(2.5)).rounded_full().bg(dot_color)),
                ),
        )
}

mod window_controls;

use gpui::prelude::*;
use gpui::{
    div, hsla, point, px, rgb, BoxShadow, Entity, FontWeight, IntoElement, MouseButton, Window,
};
use gpui_component::input::{Input, InputState};

use crate::app::{FocusCommandBar, NavigateToUrl, SelectTask, ShowSettings};
use crate::state::{AppState, OmnibarSuggestion, OmnibarSuggestionAction, ShellMode};

use self::window_controls::{default_window_control_buttons, render_window_controls};
use super::CHROME_HEIGHT;

pub(super) fn render_command_chrome(
    _window: &mut Window,
    state: &AppState,
    omnibar: &Entity<InputState>,
    omnibar_value: &str,
    suggestions: &[OmnibarSuggestion],
    command_bar: bool,
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
        .child(
            div()
                .flex_1()
                .flex()
                .justify_center()
                .child(render_omnibar(omnibar, omnibar_value, suggestions, command_bar)),
        )
        .child(render_overview_toggle(state))
}

fn render_omnibar(
    omnibar: &Entity<InputState>,
    omnibar_value: &str,
    suggestions: &[OmnibarSuggestion],
    command_bar: bool,
) -> impl IntoElement {
    let input_text = if command_bar {
        rgb(0xffffff)
    } else {
        rgb(0xffffff)
    };
    let placeholder_text = rgb(0xc6cbd2);
    let show_placeholder = omnibar_value.is_empty();

    div()
        .relative()
        .w_full()
        .max_w(px(560.0))
        .child(
            div()
                .h(px(30.0))
                .w_full()
                .rounded(px(8.0))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .cursor_text()
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.dispatch_action(Box::new(FocusCommandBar), cx);
                })
                .text_color(input_text)
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
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(if command_bar {
                            hsla(217.0 / 360.0, 0.88, 0.60, 1.0)
                        } else {
                            rgb(0xc6cbd2).into()
                        })
                        .child("⌕"),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .h_full()
                        .child(
                            Input::new(omnibar)
                                .flex_1()
                                .appearance(false)
                                .bordered(false)
                                .focus_bordered(false)
                                .disabled(!command_bar)
                                .bg(hsla(0.0, 0.0, 0.0, 0.0))
                                .text_size(px(12.5))
                                .font_weight(FontWeight(400.0))
                                .text_color(input_text),
                        )
                        .when(show_placeholder, |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .bottom_0()
                                    .flex()
                                    .items_center()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight(400.0))
                                    .text_color(placeholder_text)
                                    .child("Search files, run commands, or enter URL..."),
                            )
                        }),
                )
                .when(!command_bar, |this| {
                    this.child(
                        div()
                            .rounded(px(4.0))
                            .bg(hsla(0.0, 0.0, 1.0, 0.06))
                            .px(px(5.0))
                            .py(px(1.0))
                            .text_size(px(10.0))
                            .text_color(rgb(0xc6cbd2))
                            .child("⌘ K"),
                    )
                }),
        )
        .when(command_bar && !suggestions.is_empty(), |this| {
            this.child(render_omnibar_suggestions(suggestions))
        })
}

fn render_omnibar_suggestions(suggestions: &[OmnibarSuggestion]) -> impl IntoElement {
    div()
        .absolute()
        .top(px(36.0))
        .left_0()
        .right_0()
        .rounded(px(12.0))
        .bg(hsla(224.0 / 360.0, 0.14, 0.12, 0.98))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.08))
        .shadow(vec![BoxShadow {
            color: hsla(0.0, 0.0, 0.0, 0.28),
            offset: point(px(0.), px(10.)),
            blur_radius: px(28.),
            spread_radius: px(0.),
        }])
        .p_1()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .children(suggestions.iter().cloned().enumerate().map(|(index, suggestion)| {
            render_omnibar_suggestion(index, suggestion)
        }))
}

fn render_omnibar_suggestion(index: usize, suggestion: OmnibarSuggestion) -> impl IntoElement {
    let title = suggestion.title.clone();
    let detail = suggestion.detail.clone();

    div()
        .id(("omnibar-suggestion", index))
        .rounded(px(10.0))
        .px_3()
        .py_2()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(217.0 / 360.0, 0.60, 0.50, 0.14)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| match &suggestion.action {
            OmnibarSuggestionAction::Navigate { url } => {
                window.dispatch_action(Box::new(NavigateToUrl { url: url.clone() }), cx);
            }
            OmnibarSuggestionAction::SelectTask { task_id } => {
                window.dispatch_action(Box::new(SelectTask { task_id: *task_id }), cx);
            }
            OmnibarSuggestionAction::ShowSettings => {
                window.dispatch_action(Box::new(ShowSettings), cx);
            }
        })
        .child(
            div()
                .text_size(px(12.5))
                .font_weight(FontWeight(500.0))
                .text_color(rgb(0xe6e8ec))
                .child(title),
        )
        .child(
            div()
                .text_size(px(10.5))
                .text_color(rgb(0x8d929c))
                .child(detail),
        )
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


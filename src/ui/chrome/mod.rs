mod window_controls;

use gpui::prelude::*;
use gpui::{div, hsla, point, px, BoxShadow, Entity, FontWeight, IntoElement, MouseButton, Window};
use gpui_component::input::{Input, InputState};

use crate::app::{BrowserBack, BrowserForward, BrowserReload, FocusCommandBar, NavigateToUrl, SelectTask, ShowSettings};
use crate::state::{AppState, GuestRoute, OmnibarSuggestion, OmnibarSuggestionAction, ShellMode};

use self::window_controls::{default_window_control_buttons, render_window_controls};
use super::theme::Theme;
use super::CHROME_HEIGHT;

pub(super) fn render_command_chrome(
    _window: &mut Window,
    state: &AppState,
    omnibar: &Entity<InputState>,
    omnibar_value: &str,
    suggestions: &[OmnibarSuggestion],
    command_bar: bool,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .h(px(CHROME_HEIGHT))
        .px(px(14.0))
        .flex()
        .items_center()
        .gap_3()
        .bg(theme.panel_bg)
        .border_b_1()
        .border_color(theme.panel_border)
        .child(render_window_controls(default_window_control_buttons()))
        .child(render_nav_buttons(state, theme))
        .child(div().flex_1().flex().justify_center().child(render_omnibar(
            omnibar,
            omnibar_value,
            suggestions,
            command_bar,
            theme,
        )))
        .child(render_active_route_status(state, theme))
        .child(render_overview_toggle(state, theme))
}

fn render_nav_buttons(state: &AppState, theme: &Theme) -> impl IntoElement {
    let has_web_pane = state
        .active_web_pane()
        .map(|p| matches!(p.route, GuestRoute::ExternalUrl(_)))
        .unwrap_or(false);
    let enabled_color = theme.text_secondary;
    let disabled_color = theme.text_tertiary;
    let color = if has_web_pane {
        enabled_color
    } else {
        disabled_color
    };

    div()
        .flex()
        .items_center()
        .gap(px(2.0))
        .child(render_nav_button(
            "nav-back",
            "◀",
            color,
            has_web_pane,
            theme,
            |_, window, cx| {
                window.dispatch_action(Box::new(BrowserBack), cx);
            },
        ))
        .child(render_nav_button(
            "nav-forward",
            "▶",
            color,
            has_web_pane,
            theme,
            |_, window, cx| {
                window.dispatch_action(Box::new(BrowserForward), cx);
            },
        ))
        .child(render_nav_button(
            "nav-reload",
            "↻",
            color,
            has_web_pane,
            theme,
            |_, window, cx| {
                window.dispatch_action(Box::new(BrowserReload), cx);
            },
        ))
}

fn render_nav_button(
    id: &'static str,
    label: &'static str,
    color: gpui::Hsla,
    enabled: bool,
    theme: &Theme,
    on_click: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let hover_bg = theme.surface_hover;

    div()
        .id(id)
        .w(px(26.0))
        .h(px(26.0))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.0))
        .text_color(color)
        .when(enabled, move |this| {
            this.cursor_pointer()
                .hover(move |style| style.bg(hover_bg))
                .on_mouse_down(MouseButton::Left, on_click)
        })
        .child(label)
}

fn render_omnibar(
    omnibar: &Entity<InputState>,
    omnibar_value: &str,
    suggestions: &[OmnibarSuggestion],
    command_bar: bool,
    theme: &Theme,
) -> impl IntoElement {
    let show_placeholder = omnibar_value.is_empty();
    let omnibar_text = theme.omnibar_text;
    let placeholder_color = theme.omnibar_placeholder;
    let rest_bg = theme.omnibar_rest_bg;
    let active_bg = theme.omnibar_active_bg;
    let rest_border = theme.omnibar_rest_border;
    let active_border = theme.omnibar_active_border;
    let icon_rest = theme.omnibar_icon_rest;
    let icon_active = theme.omnibar_icon_active;
    let shadow_color = theme.accent_border;
    let kbd_bg = theme.omnibar_rest_bg;
    let kbd_text = theme.omnibar_placeholder;

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
                .text_color(omnibar_text)
                .bg(if command_bar { active_bg } else { rest_bg })
                .border_1()
                .border_color(if command_bar {
                    active_border
                } else {
                    rest_border
                })
                .when(command_bar, move |this| {
                    this.shadow(vec![BoxShadow {
                        color: shadow_color,
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(12.),
                        spread_radius: px(3.),
                    }])
                })
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(if command_bar { icon_active } else { icon_rest })
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
                                .text_color(omnibar_text),
                        )
                        .when(show_placeholder, move |this| {
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
                                    .text_color(placeholder_color)
                                    .child("Search files, run commands, or enter URL..."),
                            )
                        }),
                )
                .when(!command_bar, move |this| {
                    this.child(
                        div()
                            .rounded(px(4.0))
                            .bg(kbd_bg)
                            .px(px(5.0))
                            .py(px(1.0))
                            .text_size(px(10.0))
                            .text_color(kbd_text)
                            .child("⌘ K"),
                    )
                }),
        )
        .when(command_bar && !suggestions.is_empty(), |this| {
            this.child(render_omnibar_suggestions(suggestions, theme))
        })
}

fn render_omnibar_suggestions(
    suggestions: &[OmnibarSuggestion],
    theme: &Theme,
) -> impl IntoElement {
    let dropdown_bg = theme.omnibar_dropdown_bg;
    let dropdown_border = theme.omnibar_dropdown_border;

    div()
        .absolute()
        .top(px(36.0))
        .left_0()
        .right_0()
        .rounded(px(12.0))
        .bg(dropdown_bg)
        .border_1()
        .border_color(dropdown_border)
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
        .children(
            suggestions
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, suggestion)| render_omnibar_suggestion(index, suggestion, theme)),
        )
}

fn render_omnibar_suggestion(
    index: usize,
    suggestion: OmnibarSuggestion,
    theme: &Theme,
) -> impl IntoElement {
    let title = suggestion.title.clone();
    let detail = suggestion.detail.clone();
    let hover_bg = theme.omnibar_suggestion_hover;
    let title_color = theme.omnibar_suggestion_title;
    let detail_color = theme.omnibar_suggestion_detail;

    div()
        .id(("omnibar-suggestion", index))
        .rounded(px(10.0))
        .px_3()
        .py_2()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .cursor_pointer()
        .hover(move |style| style.bg(hover_bg))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            match &suggestion.action {
                OmnibarSuggestionAction::Navigate { url } => {
                    window.dispatch_action(Box::new(NavigateToUrl { url: url.clone() }), cx);
                }
                OmnibarSuggestionAction::SelectTask { task_id } => {
                    window.dispatch_action(Box::new(SelectTask { task_id: *task_id }), cx);
                }
                OmnibarSuggestionAction::ShowSettings => {
                    window.dispatch_action(Box::new(ShowSettings), cx);
                }
                OmnibarSuggestionAction::LaunchCapsule { handle } => {
                    window.dispatch_action(
                        Box::new(NavigateToUrl {
                            url: handle.clone(),
                        }),
                        cx,
                    );
                }
            }
        })
        .child(
            div()
                .text_size(px(12.5))
                .font_weight(FontWeight(500.0))
                .text_color(title_color)
                .child(title),
        )
        .child(
            div()
                .text_size(px(10.5))
                .text_color(detail_color)
                .child(detail),
        )
}

fn render_overview_toggle(state: &AppState, theme: &Theme) -> impl IntoElement {
    let active = matches!(state.shell_mode, ShellMode::Overview);
    let accent_border = theme.accent_border;
    let accent_subtle = theme.accent_subtle;

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
            accent_border
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .bg(if active {
            accent_subtle
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .child(render_overview_icon(active, theme))
}

fn render_active_route_status(state: &AppState, theme: &Theme) -> impl IntoElement {
    let Some(active) = state.active_capsule_pane().or_else(|| {
        state
            .active_web_pane()
            .map(|pane| crate::state::ActiveCapsulePane {
                pane_id: pane.pane_id,
                title: pane.title,
                route: pane.route,
                session: pane.session,
                source_label: pane.source_label,
                trust_state: pane.trust_state,
                restricted: pane.restricted,
                snapshot_label: pane.snapshot_label,
                canonical_handle: pane.canonical_handle,
                session_id: pane.session_id,
                adapter: pane.adapter,
                manifest_path: pane.manifest_path,
                runtime_label: pane.runtime_label,
                display_strategy: pane.display_strategy,
                log_path: pane.log_path,
                local_url: pane.local_url,
                healthcheck_url: pane.healthcheck_url,
                invoke_url: pane.invoke_url,
                served_by: pane.served_by,
            })
    }) else {
        return div().w(px(0.0));
    };

    let mut tags = Vec::new();

    match active.session {
        crate::state::WebSessionState::Resolving => tags.push(("Resolving".to_string(), true)),
        crate::state::WebSessionState::Materializing => {
            tags.push(("Materializing".to_string(), true))
        }
        crate::state::WebSessionState::Launching => tags.push(("Launching".to_string(), true)),
        _ => {}
    }

    if let Some(source) = active.source_label {
        tags.push((source, false));
    }
    if let Some(runtime) = active.runtime_label {
        tags.push((runtime, false));
    }
    if let Some(display_strategy) = active.display_strategy {
        tags.push((display_strategy, false));
    }
    if let Some(trust) = active.trust_state {
        tags.push((trust, false));
    }
    if active.restricted {
        tags.push(("restricted".to_string(), true));
    }
    if let Some(snapshot) = active.snapshot_label {
        tags.push((snapshot, false));
    }

    if tags.is_empty() {
        return div().w(px(0.0));
    }

    div().flex().items_center().gap(px(6.0)).children(
        tags.into_iter()
            .take(4)
            .map(|(label, emphasized)| render_status_chip(&label, emphasized, theme)),
    )
}

fn render_status_chip(label: &str, emphasized: bool, theme: &Theme) -> impl IntoElement {
    let bg = if emphasized {
        theme.accent_subtle
    } else {
        theme.omnibar_rest_bg
    };
    let border = if emphasized {
        theme.accent_border
    } else {
        theme.omnibar_rest_border
    };
    let text = if emphasized {
        theme.text_primary
    } else {
        theme.omnibar_placeholder
    };

    div()
        .rounded(px(999.0))
        .px(px(8.0))
        .py(px(3.0))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_size(px(10.5))
        .text_color(text)
        .child(label.to_string())
}

/// Two stacked mini-window rectangles with traffic-light dots,
/// matching the mock's overview toggle icon.
fn render_overview_icon(active: bool, theme: &Theme) -> impl IntoElement {
    let icon_color = if active {
        theme.accent
    } else {
        theme.text_tertiary
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
                .border_color(icon_color)
                .child(
                    div()
                        .absolute()
                        .top(px(2.0))
                        .left(px(3.0))
                        .flex()
                        .gap(px(1.5))
                        .child(div().size(px(2.5)).rounded_full().bg(icon_color))
                        .child(div().size(px(2.5)).rounded_full().bg(icon_color))
                        .child(div().size(px(2.5)).rounded_full().bg(icon_color)),
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
                .border_color(icon_color)
                .child(
                    div()
                        .absolute()
                        .top(px(2.0))
                        .left(px(3.0))
                        .flex()
                        .gap(px(1.5))
                        .child(div().size(px(2.5)).rounded_full().bg(icon_color))
                        .child(div().size(px(2.5)).rounded_full().bg(icon_color))
                        .child(div().size(px(2.5)).rounded_full().bg(icon_color)),
                ),
        )
}

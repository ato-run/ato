mod window_controls;

use gpui::prelude::*;
use gpui::{div, hsla, point, px, BoxShadow, Entity, FontWeight, IntoElement, MouseButton, Window};
use gpui_component::input::{Input, InputState};

use crate::app::{
    BrowserBack, BrowserForward, BrowserReload, FocusCommandBar, NavigateToUrl, SelectTask,
    ShowSettings, ToggleRouteMetadataPopover,
};
use crate::state::{
    AppState, GuestRoute, OmnibarSuggestion, OmnibarSuggestionAction, WebSessionState,
};

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
}

fn render_nav_buttons(state: &AppState, theme: &Theme) -> impl IntoElement {
    let is_external_url = state
        .active_web_pane()
        .map(|p| matches!(p.route, GuestRoute::ExternalUrl(_)))
        .unwrap_or(false);
    // Reload works on any web pane (capsule reload restarts the session)
    let has_any_web_pane =
        state.active_web_pane().is_some() || state.active_capsule_pane().is_some();
    let enabled_color = theme.text_secondary;
    let disabled_color = theme.text_tertiary;
    let nav_color = if is_external_url {
        enabled_color
    } else {
        disabled_color
    };
    let reload_color = if has_any_web_pane {
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
            nav_color,
            is_external_url,
            theme,
            |_, window, cx| {
                window.dispatch_action(Box::new(BrowserBack), cx);
            },
        ))
        .child(render_nav_button(
            "nav-forward",
            "▶",
            nav_color,
            is_external_url,
            theme,
            |_, window, cx| {
                window.dispatch_action(Box::new(BrowserForward), cx);
            },
        ))
        .child(render_nav_button(
            "nav-reload",
            "↻",
            reload_color,
            has_any_web_pane,
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

/// Visual variant of the route-status chip in the chrome bar.
///
/// Previously the chrome rendered up to four inline pills covering
/// source/runtime/trust/snapshot/transient state — that read as
/// noise on the right edge for users who only care about the
/// metadata when something looks wrong. The chip is now a single
/// click target whose icon depends on the route's lifecycle: a calm
/// info glyph when the session is healthy, a half-circle while the
/// guest is materializing, and a dedicated warning glyph when the
/// launch has failed. Click → `ToggleRouteMetadataPopover` reveals
/// the full metadata in an anchored popover.
#[derive(Copy, Clone, Eq, PartialEq)]
enum RouteChipVariant {
    Info,
    Loading,
    Error,
}

fn render_active_route_status(state: &AppState, theme: &Theme) -> impl IntoElement {
    let session = state
        .active_capsule_pane()
        .map(|pane| pane.session)
        .or_else(|| state.active_web_pane().map(|pane| pane.session));

    let Some(session) = session else {
        return div().w(px(0.0)).into_any_element();
    };

    let variant = match session {
        WebSessionState::LaunchFailed => RouteChipVariant::Error,
        WebSessionState::Resolving
        | WebSessionState::Materializing
        | WebSessionState::Launching => RouteChipVariant::Loading,
        _ => RouteChipVariant::Info,
    };
    let pressed = state.route_metadata_popover_open;

    render_route_chip(variant, pressed, theme).into_any_element()
}

fn render_route_chip(variant: RouteChipVariant, pressed: bool, theme: &Theme) -> impl IntoElement {
    // Error tones are inlined here rather than added to the global
    // Theme — the chrome's chip is currently the only error-coded
    // surface, so threading new fields through both light and dark
    // themes would be premature.
    let error_bg = hsla(0.0 / 360.0, 0.65, 0.55, 0.10);
    let error_border = hsla(0.0 / 360.0, 0.55, 0.55, 0.45);
    let error_fg = hsla(0.0 / 360.0, 0.65, 0.50, 1.0);

    let (icon, tone_bg, tone_border, tone_fg) = match variant {
        RouteChipVariant::Info => (
            "ⓘ",
            theme.omnibar_rest_bg,
            theme.omnibar_rest_border,
            theme.text_secondary,
        ),
        RouteChipVariant::Loading => ("◐", theme.accent_subtle, theme.accent_border, theme.accent),
        RouteChipVariant::Error => ("⚠", error_bg, error_border, error_fg),
    };

    let bg = if pressed {
        theme.surface_pressed
    } else {
        tone_bg
    };

    div()
        .id("route-metadata-chip")
        .rounded(px(999.0))
        .w(px(22.0))
        .h(px(22.0))
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(tone_border)
        .bg(bg)
        .text_size(px(12.0))
        .text_color(tone_fg)
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(ToggleRouteMetadataPopover), cx);
        })
        .child(icon)
}

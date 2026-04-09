use gpui::prelude::*;
use gpui::{div, hsla, px, Div, FontWeight, MouseButton};
use gpui_component::scroll::ScrollableElement;

use super::super::theme::Theme;
use crate::app::ToggleTheme;
use crate::state::{AppState, ThemeMode};

pub(super) fn render_settings_panel(body: &str, state: &AppState, theme: &Theme) -> Div {
    let body_text = body.to_string();

    div()
        .w(px(360.0))
        .min_w(px(260.0))
        .bg(theme.settings_panel_bg)
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(
            div()
                .flex_1()
                .overflow_y_scrollbar()
                .p_4()
                .flex()
                .flex_col()
                .gap_4()
                // Appearance section
                .child(
                    div()
                        .rounded(px(12.0))
                        .bg(theme.settings_card_bg)
                        .border_1()
                        .border_color(theme.settings_card_border)
                        .p_4()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight(600.0))
                                .text_color(theme.text_primary)
                                .child("Appearance"),
                        )
                        .child(
                            div()
                                .flex()
                                .rounded(px(8.0))
                                .bg(theme.settings_body_bg)
                                .border_1()
                                .border_color(theme.settings_card_border)
                                .overflow_hidden()
                                .child(theme_chip(
                                    "Light",
                                    state.theme_mode == ThemeMode::Light,
                                    theme,
                                ))
                                .child(theme_chip(
                                    "Dark",
                                    state.theme_mode == ThemeMode::Dark,
                                    theme,
                                )),
                        ),
                )
                // Diagnostics section
                .child(
                    div()
                        .rounded(px(12.0))
                        .bg(theme.settings_card_bg)
                        .border_1()
                        .border_color(theme.settings_card_border)
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_weight(FontWeight(600.0))
                                        .text_color(theme.text_primary)
                                        .child("Agent diagnostics"),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .line_height(px(18.0))
                                        .text_color(theme.text_disabled)
                                        .child(
                                            "Companion native pane for host-side state and diagnostics.",
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .rounded(px(10.0))
                                .bg(theme.settings_body_bg)
                                .border_1()
                                .border_color(theme.settings_body_border)
                                .p_4()
                                .text_sm()
                                .line_height(px(22.0))
                                .text_color(theme.text_disabled)
                                .child(body_text),
                        ),
                ),
        )
}

fn theme_chip(label: &'static str, active: bool, theme: &Theme) -> impl IntoElement {
    let accent = theme.accent;
    let accent_subtle = theme.accent_subtle;
    let text_secondary = theme.text_secondary;

    div()
        .px(px(12.0))
        .py(px(4.0))
        .cursor_pointer()
        .text_size(px(11.0))
        .font_weight(FontWeight(500.0))
        .bg(if active {
            accent_subtle
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .text_color(if active { accent } else { text_secondary })
        .when(!active, |this| {
            this.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.dispatch_action(Box::new(ToggleTheme), cx);
            })
        })
        .child(label)
}

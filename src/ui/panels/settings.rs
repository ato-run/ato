use gpui::prelude::*;
use gpui::{div, hsla, px, rgb, Div, FontWeight};
use gpui_component::scroll::ScrollableElement;

pub(super) fn render_settings_panel(body: &str) -> Div {
    let body_text = body.to_string();

    div()
        .w(px(360.0))
        .min_w(px(260.0))
        .bg(rgb(0x1f1f25))
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(
            div()
                .flex_1()
                .overflow_y_scrollbar()
                .p_4()
                .child(
                    div()
                        .rounded(px(12.0))
                        .bg(hsla(0.0, 0.0, 1.0, 0.03))
                        .border_1()
                        .border_color(hsla(0.0, 0.0, 1.0, 0.06))
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
                                        .text_color(rgb(0xe6e8ec))
                                        .child("Agent diagnostics"),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .line_height(px(18.0))
                                        .text_color(rgb(0x8d929c))
                                        .child("Companion native pane for host-side state and diagnostics."),
                                ),
                        )
                        .child(
                            div()
                                .rounded(px(10.0))
                                .bg(hsla(0.0, 0.0, 0.0, 0.18))
                                .border_1()
                                .border_color(hsla(0.0, 0.0, 1.0, 0.05))
                                .p_4()
                                .text_sm()
                                .line_height(px(22.0))
                                .text_color(rgb(0x8d929c))
                                .child(body_text),
                        ),
                ),
        )
}

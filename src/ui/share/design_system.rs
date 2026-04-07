use gpui::prelude::*;
use gpui::{
    div, hsla, linear_color_stop, linear_gradient, point, px, rgb, BoxShadow, Div, FontWeight,
};
/// Preview card for the overview overlay, with traffic-light dots, hero gradient,
/// tile grid, and text lines matching the mock's miniature previews.
pub(in crate::ui) fn render_preview_card(active: bool, title: &str, preview: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .h(px(150.0))
                .rounded(px(10.0))
                .bg(hsla(240.0 / 360.0, 0.10, 0.17, 1.0))
                .border_1()
                .border_color(hsla(0.0, 0.0, 1.0, 0.06))
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.35),
                    offset: point(px(0.), px(4.)),
                    blur_radius: px(20.),
                    spread_radius: px(0.),
                }])
                .overflow_hidden()
                // Mini title bar with traffic-light dots
                .child(
                    div()
                        .h(px(14.0))
                        .bg(hsla(0.0, 0.0, 1.0, 0.04))
                        .border_b_1()
                        .border_color(hsla(0.0, 0.0, 1.0, 0.06))
                        .flex()
                        .items_center()
                        .px(px(6.0))
                        .gap(px(3.0))
                        .child(
                            div()
                                .flex()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .size(px(4.0))
                                        .rounded_full()
                                        .bg(hsla(0.0, 0.0, 1.0, 0.15)),
                                )
                                .child(
                                    div()
                                        .size(px(4.0))
                                        .rounded_full()
                                        .bg(hsla(0.0, 0.0, 1.0, 0.15)),
                                )
                                .child(
                                    div()
                                        .size(px(4.0))
                                        .rounded_full()
                                        .bg(hsla(0.0, 0.0, 1.0, 0.15)),
                                ),
                        )
                        .child(
                            div()
                                .ml_1()
                                .flex_1()
                                .h(px(6.0))
                                .rounded(px(2.0))
                                .bg(hsla(0.0, 0.0, 1.0, 0.04)),
                        ),
                )
                // Content body
                .child(
                    div()
                        .flex_1()
                        .p(px(8.0))
                        .flex()
                        .gap_1()
                        // Left pane
                        .child(
                            div()
                                .flex_1()
                                .rounded(px(3.0))
                                .bg(hsla(0.0, 0.0, 1.0, 0.02))
                                .border_1()
                                .border_color(hsla(0.0, 0.0, 1.0, 0.06))
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .h(px(6.0))
                                        .bg(hsla(0.0, 0.0, 1.0, 0.03))
                                        .border_b_1()
                                        .border_color(hsla(0.0, 0.0, 1.0, 0.03)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .p_1()
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.0))
                                        .justify_center()
                                        .child(render_preview_line(0.60))
                                        .child(render_preview_line(0.90))
                                        .child(render_preview_line(0.70))
                                        .child(render_preview_line(0.85)),
                                ),
                        )
                        // Right pane
                        .child(
                            div()
                                .flex_1()
                                .rounded(px(3.0))
                                .bg(hsla(0.0, 0.0, 1.0, 0.02))
                                .border_1()
                                .border_color(hsla(0.0, 0.0, 1.0, 0.06))
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .h(px(6.0))
                                        .bg(hsla(0.0, 0.0, 1.0, 0.03))
                                        .border_b_1()
                                        .border_color(hsla(0.0, 0.0, 1.0, 0.03)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .p_1()
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .gap(px(2.0))
                                        .justify_center()
                                        .child(div().w_4_5().h(px(20.0)).rounded(px(2.0)).bg(
                                            if active {
                                                linear_gradient(
                                                    135.,
                                                    linear_color_stop(
                                                        hsla(217.0 / 360.0, 0.88, 0.60, 0.15),
                                                        0.,
                                                    ),
                                                    linear_color_stop(
                                                        hsla(270.0 / 360.0, 0.73, 0.73, 0.15),
                                                        1.,
                                                    ),
                                                )
                                            } else {
                                                linear_gradient(
                                                    135.,
                                                    linear_color_stop(
                                                        hsla(0.0, 0.0, 1.0, 0.04),
                                                        0.,
                                                    ),
                                                    linear_color_stop(
                                                        hsla(0.0, 0.0, 1.0, 0.06),
                                                        1.,
                                                    ),
                                                )
                                            },
                                        ))
                                        .child(
                                            div()
                                                .w_full()
                                                .grid()
                                                .grid_cols(2)
                                                .gap(px(2.0))
                                                .child(render_preview_cell())
                                                .child(render_preview_cell())
                                                .child(render_preview_cell())
                                                .child(render_preview_cell()),
                                        )
                                        .child(render_preview_line(0.80))
                                        .child(render_preview_line(0.60)),
                                ),
                        ),
                ),
        )
        .child(
            div()
                .text_size(px(14.0))
                .font_weight(FontWeight(600.0))
                .text_color(rgb(0xf0f0f2))
                .child(title.to_string()),
        )
        .child(
            div()
                .text_xs()
                .text_color(hsla(0.0, 0.0, 1.0, 0.55))
                .child(preview.to_string()),
        )
}

/// A thin shimmer line used in miniature preview cards.
fn render_preview_line(width_fraction: f32) -> Div {
    let width_pct = (width_fraction * 100.0) as u32;
    div()
        .h(px(2.0))
        .rounded(px(1.0))
        .bg(hsla(0.0, 0.0, 1.0, 0.06))
        // Width set as fraction of parent
        .when(width_pct <= 60, |this| this.w_3_5())
        .when(width_pct > 60 && width_pct <= 70, |this| this.w_2_3())
        .when(width_pct > 70 && width_pct <= 85, |this| this.w_4_5())
        .when(width_pct > 85, |this| this.w_full())
}

/// A small cell in the mini-preview grid.
fn render_preview_cell() -> Div {
    div()
        .h(px(8.0))
        .rounded(px(1.0))
        .bg(hsla(0.0, 0.0, 1.0, 0.04))
}

#[allow(dead_code)]
pub(in crate::ui) fn preview_tile(color: gpui::Rgba) -> Div {
    div()
        .h(px(42.0))
        .rounded_md()
        .bg(linear_gradient(
            135.,
            linear_color_stop(color, 0.),
            linear_color_stop(color, 1.),
        ))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.05))
}

use super::super::theme::Theme;
use gpui::prelude::*;
use gpui::{div, linear_color_stop, linear_gradient, point, px, BoxShadow, Div, FontWeight};

/// Preview card for the overview overlay, with traffic-light dots, hero gradient,
/// tile grid, and text lines matching the mock's miniature previews.
pub(in crate::ui) fn render_preview_card(
    active: bool,
    title: &str,
    preview: &str,
    theme: &Theme,
) -> Div {
    let card_bg = theme.preview_card_bg;
    let card_border = theme.border_subtle;
    let chrome_bg = theme.preview_chrome_bg;
    let pane_bg = theme.stage_bg;
    let pane_border = theme.border_subtle;
    let header_bg = theme.preview_chrome_bg;
    let line_color = theme.border_default;
    let cell_color = theme.surface_hover;
    let title_color = theme.text_primary;
    let subtitle_color = theme.text_tertiary;
    let accent_subtle = theme.accent_subtle;

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .h(px(150.0))
                .rounded(px(10.0))
                .bg(card_bg)
                .border_1()
                .border_color(card_border)
                .shadow(vec![BoxShadow {
                    color: gpui::hsla(0.0, 0.0, 0.0, 0.35),
                    offset: point(px(0.), px(4.)),
                    blur_radius: px(20.),
                    spread_radius: px(0.),
                }])
                .overflow_hidden()
                // Mini title bar with traffic-light dots
                .child(
                    div()
                        .h(px(14.0))
                        .bg(chrome_bg)
                        .border_b_1()
                        .border_color(card_border)
                        .flex()
                        .items_center()
                        .px(px(6.0))
                        .gap(px(3.0))
                        .child(
                            div()
                                .flex()
                                .gap(px(2.0))
                                .child(div().size(px(4.0)).rounded_full().bg(card_border))
                                .child(div().size(px(4.0)).rounded_full().bg(card_border))
                                .child(div().size(px(4.0)).rounded_full().bg(card_border)),
                        )
                        .child(
                            div()
                                .ml_1()
                                .flex_1()
                                .h(px(6.0))
                                .rounded(px(2.0))
                                .bg(chrome_bg),
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
                                .bg(pane_bg)
                                .border_1()
                                .border_color(pane_border)
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .h(px(6.0))
                                        .bg(header_bg)
                                        .border_b_1()
                                        .border_color(pane_border),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .p_1()
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.0))
                                        .justify_center()
                                        .child(render_preview_line(0.60, line_color))
                                        .child(render_preview_line(0.90, line_color))
                                        .child(render_preview_line(0.70, line_color))
                                        .child(render_preview_line(0.85, line_color)),
                                ),
                        )
                        // Right pane
                        .child(
                            div()
                                .flex_1()
                                .rounded(px(3.0))
                                .bg(pane_bg)
                                .border_1()
                                .border_color(pane_border)
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .h(px(6.0))
                                        .bg(header_bg)
                                        .border_b_1()
                                        .border_color(pane_border),
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
                                                        gpui::hsla(217.0 / 360.0, 0.88, 0.60, 0.15),
                                                        0.,
                                                    ),
                                                    linear_color_stop(
                                                        gpui::hsla(270.0 / 360.0, 0.73, 0.73, 0.15),
                                                        1.,
                                                    ),
                                                )
                                            } else {
                                                linear_gradient(
                                                    135.,
                                                    linear_color_stop(accent_subtle, 0.),
                                                    linear_color_stop(accent_subtle, 1.),
                                                )
                                            },
                                        ))
                                        .child(
                                            div()
                                                .w_full()
                                                .grid()
                                                .grid_cols(2)
                                                .gap(px(2.0))
                                                .child(render_preview_cell(cell_color))
                                                .child(render_preview_cell(cell_color))
                                                .child(render_preview_cell(cell_color))
                                                .child(render_preview_cell(cell_color)),
                                        )
                                        .child(render_preview_line(0.80, line_color))
                                        .child(render_preview_line(0.60, line_color)),
                                ),
                        ),
                ),
        )
        .child(
            div()
                .text_size(px(14.0))
                .font_weight(FontWeight(600.0))
                .text_color(title_color)
                .child(title.to_string()),
        )
        .child(
            div()
                .text_xs()
                .text_color(subtitle_color)
                .child(preview.to_string()),
        )
}

/// A thin shimmer line used in miniature preview cards.
fn render_preview_line(width_fraction: f32, color: gpui::Hsla) -> Div {
    let width_pct = (width_fraction * 100.0) as u32;
    div()
        .h(px(2.0))
        .rounded(px(1.0))
        .bg(color)
        .when(width_pct <= 60, |this| this.w_3_5())
        .when(width_pct > 60 && width_pct <= 70, |this| this.w_2_3())
        .when(width_pct > 70 && width_pct <= 85, |this| this.w_4_5())
        .when(width_pct > 85, |this| this.w_full())
}

/// A small cell in the mini-preview grid.
fn render_preview_cell(color: gpui::Hsla) -> Div {
    div().h(px(8.0)).rounded(px(1.0)).bg(color)
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
        .border_color(gpui::hsla(0.0, 0.0, 1.0, 0.05))
}

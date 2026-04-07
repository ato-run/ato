use gpui::prelude::*;
use gpui::{div, hsla, px, IntoElement};

pub(in crate::ui) fn render_launcher_panel() -> impl IntoElement {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_4()
        .size_full()
        .bg(hsla(240.0 / 360.0, 0.08, 0.10, 1.0))
        .child(
            div()
                .text_size(px(24.0))
                .font_weight(gpui::FontWeight(500.0))
                .text_color(hsla(0.0, 0.0, 1.0, 0.8))
                .child("What do you want to create?"),
        )
        .child(
            div()
                .text_size(px(14.0))
                .text_color(hsla(0.0, 0.0, 1.0, 0.4))
                .child("Press ⌘ K to search commands or open a capsule."),
        )
}

use gpui::prelude::*;
use gpui::{div, hsla, linear_color_stop, linear_gradient, point, px, rgb, BoxShadow, IntoElement};

/// User avatar styled as a gradient circle matching the mock's
/// `linear-gradient(135deg, #6366f1, #a78bfa)` with a centered person icon.
pub(in crate::ui) fn render_user_avatar() -> impl IntoElement {
    div()
        .w(px(26.0))
        .h(px(26.0))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(linear_gradient(
            135.,
            linear_color_stop(rgb(0x6366f1), 0.),
            linear_color_stop(rgb(0xa78bfa), 1.),
        ))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.10))
        .shadow(vec![BoxShadow {
            color: hsla(250.0 / 360.0, 0.6, 0.6, 0.25),
            offset: point(px(0.), px(2.)),
            blur_radius: px(8.),
            spread_radius: px(0.),
        }])
        .overflow_hidden()
        .text_size(px(12.0))
        .text_color(rgb(0xffffff))
        .child("👤")
}

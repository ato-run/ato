use gpui::prelude::*;
use gpui::{div, hsla, IntoElement};

/// Pane icon matching the mock's pane-icon style (13px, tertiary color).
pub(in crate::ui) fn render_capsule_icon() -> impl IntoElement {
    div()
        .text_size(gpui::px(13.0))
        .text_color(hsla(0.0, 0.0, 1.0, 0.32))
        .child("◈")
}

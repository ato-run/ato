use gpui::prelude::*;
use gpui::{div, px, rgb, Div};
use gpui_component::setting::{SettingGroup, SettingItem, SettingPage, Settings};

use crate::ui::share::render_pane_header;

pub(super) fn render_settings_panel(title: String, body: &str) -> Div {
    let body_text = body.to_string();

    div()
        .w(px(360.0))
        .min_w(px(260.0))
        .bg(rgb(0x1f1f25))
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(render_pane_header(title, None, "native".to_string()))
        .child(
            div().flex_1().overflow_hidden().child(
                Settings::new("capability-inspector").page(
                    SettingPage::new("Diagnostics").group(
                        SettingGroup::new()
                            .title("Agent diagnostics")
                            .item(SettingItem::render(move |_opts, _window, _cx| {
                                div()
                                    .p_4()
                                    .text_sm()
                                    .line_height(px(22.0))
                                    .text_color(rgb(0x8d929c))
                                    .child(body_text.clone())
                            })),
                    ),
                ),
            ),
        )
}

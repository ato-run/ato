use std::fs;

use gpui::prelude::*;
use gpui::{div, px, Div, FontWeight};
use gpui_component::scroll::ScrollableElement;

use crate::state::{CapsuleStatusPane, WebSessionState};

use super::super::theme::Theme;

pub(super) fn render_capsule_runtime_panel(capsule: &CapsuleStatusPane, theme: &Theme) -> Div {
    let log_tail = capsule
        .log_path
        .as_deref()
        .map(read_log_tail)
        .unwrap_or_else(|| "No runtime log available yet.".to_string());

    div()
        .flex_1()
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
                .child(
                    div()
                        .rounded(px(12.0))
                        .bg(theme.settings_card_bg)
                        .border_1()
                        .border_color(theme.settings_card_border)
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight(600.0))
                                .text_color(theme.text_primary)
                                .child("Capsule session"),
                        )
                        .child(render_row("Handle", &capsule.route.to_string(), theme))
                        .child(render_optional_row(
                            "Runtime",
                            capsule.runtime_label.as_deref(),
                            theme,
                        ))
                        .child(render_optional_row(
                            "Display",
                            capsule.display_strategy.as_deref(),
                            theme,
                        ))
                        .child(render_row(
                            "State",
                            session_state_label(&capsule.session),
                            theme,
                        ))
                        .child(render_optional_row(
                            "URL",
                            capsule.local_url.as_deref(),
                            theme,
                        ))
                        .child(render_optional_row(
                            "Health",
                            capsule.healthcheck_url.as_deref(),
                            theme,
                        ))
                        .child(render_optional_row(
                            "Invoke",
                            capsule.invoke_url.as_deref(),
                            theme,
                        ))
                        .child(render_optional_row(
                            "Log",
                            capsule.log_path.as_deref(),
                            theme,
                        )),
                )
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
                                .text_size(px(12.0))
                                .font_weight(FontWeight(600.0))
                                .text_color(theme.text_primary)
                                .child("Runtime log"),
                        )
                        .child(
                            div()
                                .rounded(px(10.0))
                                .bg(theme.settings_body_bg)
                                .border_1()
                                .border_color(theme.settings_body_border)
                                .p_4()
                                .text_size(px(11.0))
                                .line_height(px(18.0))
                                .text_color(theme.text_secondary)
                                .child(log_tail),
                        ),
                ),
        )
}

fn render_row(label: &str, value: &str, theme: &Theme) -> Div {
    div()
        .flex()
        .justify_between()
        .gap_3()
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme.text_disabled)
                .child(label.to_string()),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme.text_primary)
                .child(value.to_string()),
        )
}

fn render_optional_row(label: &str, value: Option<&str>, theme: &Theme) -> Div {
    render_row(label, value.unwrap_or("n/a"), theme)
}

fn read_log_tail(path: &str) -> String {
    let Ok(raw) = fs::read_to_string(path) else {
        return format!("Unable to read runtime log at {path}");
    };
    let lines = raw.lines().rev().take(30).collect::<Vec<_>>();
    if lines.is_empty() {
        return "Runtime started, but the log is still empty.".to_string();
    }
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

fn session_state_label(session: &WebSessionState) -> &'static str {
    match session {
        WebSessionState::Detached => "Detached",
        WebSessionState::Resolving => "Resolving",
        WebSessionState::Materializing => "Materializing",
        WebSessionState::Launching => "Launching",
        WebSessionState::Mounted => "Mounted",
        WebSessionState::Closed => "Closed",
        WebSessionState::LaunchFailed => "Failed",
    }
}

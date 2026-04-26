use gpui::prelude::*;
use gpui::{div, hsla, px, Div, FontWeight};
use gpui_component::scroll::ScrollableElement;

use crate::state::{AppState, ConsoleLevel, ConsoleLogEntry, NetworkLogEntry};

use super::super::theme::Theme;

pub(super) fn render_dev_console_panel(state: &AppState, theme: &Theme) -> Div {
    div()
        .w(px(380.0))
        .min_w(px(280.0))
        .bg(theme.settings_panel_bg)
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(render_header(theme))
        .child(
            div()
                .flex_1()
                .overflow_y_scrollbar()
                .p_4()
                .flex()
                .flex_col()
                .gap_4()
                .child(render_console_section(state, theme))
                .child(render_network_section(state, theme))
                .child(render_application_section(state, theme)),
        )
}

fn render_header(theme: &Theme) -> Div {
    div()
        .px_4()
        .py(px(10.0))
        .border_b_1()
        .border_color(theme.border_subtle)
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(12.0))
                .font_weight(FontWeight(600.0))
                .text_color(theme.text_primary)
                .child("Developer Console"),
        )
        .child(
            div()
                .text_size(px(10.5))
                .text_color(theme.text_disabled)
                .child("cmd+opt+i to close"),
        )
}

fn render_console_section(state: &AppState, theme: &Theme) -> Div {
    let logs = &state.console_logs;
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
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(theme.text_primary)
                        .child("Console"),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_disabled)
                        .child(format!("{} entries", logs.len())),
                ),
        )
        .child(
            div()
                .rounded(px(10.0))
                .bg(theme.settings_body_bg)
                .border_1()
                .border_color(theme.settings_body_border)
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .children(if logs.is_empty() {
                    vec![render_empty_placeholder("No console output yet.", theme)
                        .into_any_element()]
                } else {
                    logs.iter()
                        .rev()
                        .take(200)
                        .map(|entry| render_console_row(entry, theme).into_any_element())
                        .collect()
                }),
        )
}

fn render_console_row(entry: &ConsoleLogEntry, theme: &Theme) -> Div {
    let (badge_bg, badge_text, msg_color) = console_level_colors(&entry.level, theme);
    let label_text = entry.source_label.as_deref().unwrap_or("guest").to_string();

    div()
        .rounded(px(6.0))
        .px_2()
        .py(px(4.0))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .rounded(px(999.0))
                        .px(px(6.0))
                        .py(px(1.0))
                        .bg(badge_bg)
                        .text_size(px(9.5))
                        .text_color(badge_text)
                        .child(entry.level.as_str().to_string()),
                )
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme.text_disabled)
                        .child(label_text),
                ),
        )
        .child(
            div()
                .text_size(px(11.0))
                .line_height(px(17.0))
                .text_color(msg_color)
                .child(entry.message.clone()),
        )
}

fn console_level_colors(
    level: &ConsoleLevel,
    theme: &Theme,
) -> (gpui::Hsla, gpui::Hsla, gpui::Hsla) {
    match level {
        ConsoleLevel::Error => (
            hsla(0.0, 0.85, 0.45, 0.15),
            hsla(0.0, 0.85, 0.55, 1.0),
            hsla(0.0, 0.70, 0.55, 1.0),
        ),
        ConsoleLevel::Warn => (
            hsla(38.0 / 360.0, 0.90, 0.50, 0.15),
            hsla(38.0 / 360.0, 0.90, 0.50, 1.0),
            hsla(38.0 / 360.0, 0.70, 0.45, 1.0),
        ),
        ConsoleLevel::Info => (theme.accent_subtle, theme.accent, theme.text_secondary),
        ConsoleLevel::Debug => (
            theme.surface_hover,
            theme.text_tertiary,
            theme.text_tertiary,
        ),
        ConsoleLevel::Log => (
            theme.surface_hover,
            theme.text_disabled,
            theme.text_secondary,
        ),
    }
}

fn render_network_section(state: &AppState, theme: &Theme) -> Div {
    let logs = &state.network_logs;
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
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(theme.text_primary)
                        .child("Network"),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_disabled)
                        .child(format!("{} requests", logs.len())),
                ),
        )
        .child(
            div()
                .rounded(px(10.0))
                .bg(theme.settings_body_bg)
                .border_1()
                .border_color(theme.settings_body_border)
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .children(if logs.is_empty() {
                    vec![render_empty_placeholder("No network requests yet.", theme)
                        .into_any_element()]
                } else {
                    logs.iter()
                        .rev()
                        .take(100)
                        .map(|entry| render_network_row(entry, theme).into_any_element())
                        .collect()
                }),
        )
}

fn render_network_row(entry: &NetworkLogEntry, theme: &Theme) -> Div {
    let method_color = method_color(&entry.method);
    let (status_bg, status_text) = status_colors(entry.status, theme);
    let url_display = truncate_url(&entry.url, 48);

    div()
        .rounded(px(6.0))
        .px_2()
        .py(px(4.0))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .rounded(px(999.0))
                        .px(px(6.0))
                        .py(px(1.0))
                        .bg(method_color)
                        .text_size(px(9.5))
                        .text_color(theme.settings_panel_bg)
                        .child(entry.method.clone()),
                )
                .child(if let Some(status) = entry.status {
                    div()
                        .rounded(px(999.0))
                        .px(px(6.0))
                        .py(px(1.0))
                        .bg(status_bg)
                        .text_size(px(9.5))
                        .text_color(status_text)
                        .child(status.to_string())
                        .into_any_element()
                } else {
                    div()
                        .text_size(px(9.5))
                        .text_color(theme.text_disabled)
                        .child("pending")
                        .into_any_element()
                })
                .when_some(entry.duration_ms, |d, ms| {
                    d.child(
                        div()
                            .text_size(px(10.0))
                            .text_color(theme.text_disabled)
                            .child(format!("{ms}ms")),
                    )
                }),
        )
        .child(
            div()
                .text_size(px(10.5))
                .line_height(px(16.0))
                .text_color(theme.text_secondary)
                .child(url_display),
        )
}

fn method_color(method: &str) -> gpui::Hsla {
    match method {
        "GET" => hsla(217.0 / 360.0, 0.75, 0.45, 1.0),
        "POST" => hsla(142.0 / 360.0, 0.70, 0.38, 1.0),
        "PUT" | "PATCH" => hsla(38.0 / 360.0, 0.85, 0.45, 1.0),
        "DELETE" => hsla(0.0, 0.80, 0.48, 1.0),
        _ => hsla(270.0 / 360.0, 0.60, 0.50, 1.0),
    }
}

fn status_colors(status: Option<u16>, theme: &Theme) -> (gpui::Hsla, gpui::Hsla) {
    match status {
        None => (theme.surface_hover, theme.text_disabled),
        Some(0) => (hsla(0.0, 0.80, 0.45, 0.15), hsla(0.0, 0.80, 0.55, 1.0)),
        Some(s) if s >= 500 => (hsla(0.0, 0.80, 0.45, 0.15), hsla(0.0, 0.80, 0.55, 1.0)),
        Some(s) if s >= 400 => (
            hsla(38.0 / 360.0, 0.85, 0.50, 0.15),
            hsla(38.0 / 360.0, 0.85, 0.48, 1.0),
        ),
        Some(s) if s >= 300 => (theme.accent_subtle, theme.accent),
        _ => (
            hsla(142.0 / 360.0, 0.65, 0.40, 0.15),
            hsla(142.0 / 360.0, 0.65, 0.42, 1.0),
        ),
    }
}

fn truncate_url(url: &str, max_len: usize) -> String {
    if url.len() <= max_len {
        url.to_string()
    } else {
        format!("{}…", &url[..max_len])
    }
}

fn render_application_section(state: &AppState, theme: &Theme) -> Div {
    let inspector = state.active_capsule_inspector();
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
                .child("Application"),
        )
        .child(
            div()
                .rounded(px(10.0))
                .bg(theme.settings_body_bg)
                .border_1()
                .border_color(theme.settings_body_border)
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .children(if let Some(insp) = inspector {
                    vec![
                        render_app_row("Handle", &insp.handle, theme).into_any_element(),
                        render_app_row("Session", &format!("{:?}", insp.session_state), theme)
                            .into_any_element(),
                        render_app_row("Adapter", insp.adapter.as_deref().unwrap_or("—"), theme)
                            .into_any_element(),
                        render_app_row(
                            "Session ID",
                            insp.session_id.as_deref().unwrap_or("—"),
                            theme,
                        )
                        .into_any_element(),
                        render_app_row(
                            "Source",
                            insp.source_label.as_deref().unwrap_or("—"),
                            theme,
                        )
                        .into_any_element(),
                        render_app_row("Trust", insp.trust_state.as_deref().unwrap_or("—"), theme)
                            .into_any_element(),
                    ]
                } else {
                    vec![render_empty_placeholder(
                        "Focus a capsule tab to see application info.",
                        theme,
                    )
                    .into_any_element()]
                }),
        )
}

fn render_app_row(label: &str, value: &str, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(10.5))
                .text_color(theme.text_disabled)
                .child(label.to_string()),
        )
        .child(
            div()
                .text_size(px(11.0))
                .line_height(px(17.0))
                .text_color(theme.text_secondary)
                .child(value.to_string()),
        )
}

fn render_empty_placeholder(msg: &str, theme: &Theme) -> Div {
    div()
        .p_3()
        .text_size(px(11.0))
        .line_height(px(18.0))
        .text_color(theme.text_disabled)
        .child(msg.to_string())
}

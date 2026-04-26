use gpui::prelude::*;
use gpui::{div, px, Div, FontWeight};
use gpui_component::scroll::ScrollableElement;

use crate::state::{ActivityTone, AppState, CapsuleInspectorView};

use super::super::theme::Theme;

pub(super) fn render_capsule_inspector_panel(state: &AppState, theme: &Theme) -> Div {
    let Some(inspector) = state.active_capsule_inspector() else {
        return render_empty_state(theme);
    };

    div()
        .w(px(380.0))
        .min_w(px(280.0))
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
                .child(render_metadata_card(&inspector, theme))
                .child(render_log_card(&inspector, theme)),
        )
}

fn render_empty_state(theme: &Theme) -> Div {
    div()
        .w(px(380.0))
        .min_w(px(280.0))
        .bg(theme.settings_panel_bg)
        .flex()
        .flex_col()
        .justify_center()
        .items_center()
        .p_6()
        .child(
            div()
                .max_w(px(240.0))
                .flex()
                .flex_col()
                .gap_2()
                .text_center()
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(theme.text_primary)
                        .child("Capsule inspector"),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .line_height(px(18.0))
                        .text_color(theme.text_disabled)
                        .child(
                            "Focus a capsule tab to inspect handle metadata, launch stages, and permission/runtime logs.",
                        ),
                ),
        )
}

fn render_metadata_card(inspector: &CapsuleInspectorView, theme: &Theme) -> Div {
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
                        .child("Capsule inspector"),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_disabled)
                        .child(inspector.title.clone()),
                ),
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
                .child(render_metadata_row("Handle", &inspector.handle, theme))
                .child(render_optional_row(
                    "Canonical",
                    inspector.canonical_handle.as_deref(),
                    theme,
                ))
                .child(render_optional_row(
                    "Source",
                    inspector.source_label.as_deref(),
                    theme,
                ))
                .child(render_optional_row(
                    "Runtime",
                    inspector.runtime_label.as_deref(),
                    theme,
                ))
                .child(render_optional_row(
                    "Display",
                    inspector.display_strategy.as_deref(),
                    theme,
                ))
                .child(render_optional_row(
                    "Trust",
                    inspector.trust_state.as_deref(),
                    theme,
                ))
                .child(render_metadata_row(
                    "Restricted",
                    if inspector.restricted { "yes" } else { "no" },
                    theme,
                ))
                .child(render_optional_row(
                    "Snapshot",
                    inspector.snapshot_label.as_deref(),
                    theme,
                ))
                .child(render_metadata_row(
                    "Session",
                    session_state_label(inspector),
                    theme,
                ))
                .child(render_optional_row(
                    "Session ID",
                    inspector.session_id.as_deref(),
                    theme,
                ))
                .child(render_optional_row(
                    "Adapter",
                    inspector.adapter.as_deref(),
                    theme,
                ))
                .child(render_optional_row(
                    "Manifest",
                    inspector.manifest_path.as_deref(),
                    theme,
                ))
                .child(render_optional_row(
                    "URL",
                    inspector.local_url.as_deref(),
                    theme,
                ))
                .child(render_optional_row(
                    "Health",
                    inspector.healthcheck_url.as_deref(),
                    theme,
                ))
                .child(render_optional_row(
                    "Invoke",
                    inspector.invoke_url.as_deref(),
                    theme,
                ))
                .child(render_optional_row(
                    "Served By",
                    inspector.served_by.as_deref(),
                    theme,
                ))
                .child(render_optional_row(
                    "Log",
                    inspector.log_path.as_deref(),
                    theme,
                )),
        )
}

fn render_log_card(inspector: &CapsuleInspectorView, theme: &Theme) -> Div {
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
                        .child("Capsule log"),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_disabled)
                        .child(format!("{} entries", inspector.logs.len())),
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
                .gap_2()
                .children(if inspector.logs.is_empty() {
                    vec![render_empty_log(theme).into_any_element()]
                } else {
                    inspector
                        .logs
                        .iter()
                        .rev()
                        .map(|entry| {
                            render_log_row(
                                entry.stage.as_str(),
                                entry.message.as_str(),
                                &entry.tone,
                                theme,
                            )
                            .into_any_element()
                        })
                        .collect()
                }),
        )
}

fn render_empty_log(theme: &Theme) -> Div {
    div()
        .p_3()
        .text_size(px(11.0))
        .line_height(px(18.0))
        .text_color(theme.text_disabled)
        .child("No capsule activity recorded yet.")
}

fn render_log_row(stage: &str, message: &str, tone: &ActivityTone, theme: &Theme) -> Div {
    let badge_bg = match tone {
        ActivityTone::Info => theme.accent_subtle,
        ActivityTone::Warning => theme.surface_hover,
        ActivityTone::Error => theme.surface_pressed,
    };
    let badge_text = match tone {
        ActivityTone::Info => theme.accent,
        ActivityTone::Warning => theme.text_secondary,
        ActivityTone::Error => theme.text_primary,
    };

    div()
        .rounded(px(8.0))
        .bg(theme.settings_panel_bg)
        .border_1()
        .border_color(theme.settings_body_border)
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div().flex().items_center().gap_2().child(
                div()
                    .rounded(px(999.0))
                    .px(px(7.0))
                    .py(px(2.0))
                    .bg(badge_bg)
                    .text_size(px(10.0))
                    .text_color(badge_text)
                    .child(stage.to_string()),
            ),
        )
        .child(
            div()
                .text_size(px(11.0))
                .line_height(px(18.0))
                .text_color(theme.text_secondary)
                .child(message.to_string()),
        )
}

fn render_metadata_row(label: &str, value: &str, theme: &Theme) -> Div {
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
                .line_height(px(18.0))
                .text_color(theme.text_primary)
                .child(value.to_string()),
        )
}

fn render_optional_row(label: &str, value: Option<&str>, theme: &Theme) -> Div {
    render_metadata_row(label, value.unwrap_or("n/a"), theme)
}

fn session_state_label(inspector: &CapsuleInspectorView) -> &'static str {
    use crate::state::WebSessionState;

    match inspector.session_state {
        WebSessionState::Detached => "detached",
        WebSessionState::Resolving => "resolving",
        WebSessionState::Materializing => "materializing",
        WebSessionState::Launching => "launching",
        WebSessionState::Mounted => "mounted",
        WebSessionState::Closed => "closed",
        WebSessionState::LaunchFailed => "failed",
    }
}

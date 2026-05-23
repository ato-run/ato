use gpui::prelude::*;
use gpui::{div, hsla, px, AnyElement, FontWeight, IntoElement, MouseButton, Styled};

use super::super::theme::Theme;
use crate::install_lifecycle_dashboard::{DashboardCache, InstalledAppDashboardItem};

pub(in crate::ui) fn render_installed_apps_section(
    items: &[InstalledAppDashboardItem],
    theme: &Theme,
) -> AnyElement {
    if items.is_empty() {
        return div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0))
            .py(px(16.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(theme.text_tertiary)
                    .child("No installed apps yet."),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_disabled)
                    .child("Install one with:  ato install <publisher>/<slug>"),
            )
            .into_any_element();
    }

    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight(600.0))
                .text_color(theme.text_tertiary)
                .px(px(4.0))
                .child(format!("Installed Apps ({})", items.len())),
        )
        .children(
            items
                .iter()
                .map(|item| render_app_card(item, theme))
                .collect::<Vec<_>>(),
        )
        .into_any_element()
}

fn render_app_card(item: &InstalledAppDashboardItem, theme: &Theme) -> gpui::Div {
    let handle = item.capsule_handle.clone();
    let version = item.version.clone();
    let profile_count = item.profiles.len();
    let profile_info = if profile_count == 1 {
        format!(
            "{} revision{}",
            item.profiles[0].revisions_count,
            if item.profiles[0].revisions_count != 1 {
                "s"
            } else {
                ""
            }
        )
    } else {
        format!("{} profiles", profile_count)
    };
    let ipk = item
        .profiles
        .first()
        .map(|p| p.install_profile_key.clone())
        .unwrap_or_default();

    let running = !item.running_sessions_hint.is_empty();

    let ipk_short = if ipk.len() > 16 {
        format!("{}...", &ipk[..16])
    } else {
        ipk.clone()
    };

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .child(
            div()
                .w(px(32.0))
                .h(px(32.0))
                .rounded(px(7.0))
                .bg(hsla(210.0 / 360.0, 0.5, 0.45, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .text_color(gpui::white())
                .text_size(px(13.0))
                .font_weight(FontWeight::BOLD)
                .child(
                    handle
                        .chars()
                        .next()
                        .unwrap_or('?')
                        .to_uppercase()
                        .to_string(),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .flex_1()
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight(500.0))
                        .text_color(theme.text_primary)
                        .child(handle),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(format!("v{} · {}", version, profile_info)),
                )
                .child(
                    div()
                        .text_size(px(9.0))
                        .text_color(theme.text_disabled)
                        .child(ipk_short),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .when(running, |this| {
                    this.child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(hsla(
                        130.0 / 360.0,
                        0.7,
                        0.5,
                        1.0,
                    )))
                })
                .child(
                    div()
                        .px(px(10.0))
                        .py(px(4.0))
                        .rounded(px(6.0))
                        .bg(theme.accent)
                        .text_color(gpui::white())
                        .text_size(px(11.0))
                        .font_weight(FontWeight(500.0))
                        .cursor_pointer()
                        .child("Launch")
                        .on_mouse_down(MouseButton::Left, move |_event, _window, _cx| {
                            let ipk = ipk.clone();
                            let ato_bin =
                                match crate::orchestrator::resolve_ato_binary() {
                                    Ok(p) => p,
                                    Err(e) => {
                                        tracing::error!(
                                            "cannot resolve ato binary: {e}"
                                        );
                                        return;
                                    }
                                };
                            std::thread::spawn(move || {
                                if let Err(e) =
                                    crate::install_lifecycle_dashboard::spawn_launch(
                                        &ato_bin,
                                        &ipk,
                                    )
                                {
                                    tracing::error!(
                                        "launch installed app {ipk}: {e}"
                                    );
                                }
                                DashboardCache::refresh();
                            });
                        }),
                ),
        )
}

use gpui::prelude::*;
use gpui::{
    AnyElement, ClipboardItem, FontWeight, IntoElement, MouseButton, Styled, div, hsla, px,
};

use super::super::theme::Theme;
use crate::app::SelectInstalledProfile;
use crate::install_lifecycle_dashboard::{
    InstalledAppDashboardItem, InstalledProfileDashboardItem, InstalledRevisionDashboardItem,
};

pub(in crate::ui) fn render_installed_app_detail_panel(
    selected: Option<&InstalledAppDashboardItem>,
    selected_profile_id: Option<&str>,
    theme: &Theme,
) -> AnyElement {
    match selected {
        None => render_no_selection(theme),
        Some(item) => {
            let resolved_profile = selected_profile_id
                .and_then(|pid| item.profiles.iter().find(|p| p.profile_id == pid))
                .or_else(|| item.profiles.first());

            div()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .size_full()
                .child(render_header(item, theme))
                .child(render_profiles(item, selected_profile_id, theme))
                .when_some(resolved_profile, |this, profile| {
                    this.child(render_revisions(item, profile, theme))
                })
                .into_any_element()
        }
    }
}

fn render_no_selection(theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .size_full()
        .gap(px(8.0))
        .text_size(px(13.0))
        .text_color(theme.text_tertiary)
        .child("Select an installed app to view details.")
        .into_any_element()
}

fn render_header(item: &InstalledAppDashboardItem, theme: &Theme) -> gpui::Div {
    let running = !item.running_sessions_hint.is_empty();
    let running_label = if running { "Running" } else { "" };
    let running_color = if running {
        hsla(130.0 / 360.0, 0.7, 0.5, 1.0)
    } else {
        hsla(0.0, 0.0, 0.0, 0.0)
    };

    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(theme.text_primary)
                        .child(format!("{}/{}", item.publisher, item.slug)),
                )
                .when(running, |this| {
                    this.child(
                        div()
                            .px(px(6.0))
                            .py(px(2.0))
                            .rounded(px(4.0))
                            .bg(hsla(130.0 / 360.0, 0.7, 0.5, 0.15))
                            .text_size(px(9.0))
                            .text_color(running_color)
                            .child(running_label),
                    )
                }),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme.text_secondary)
                .child(format!("v{}", item.version)),
        )
        .child(div().flex().flex_col().gap(px(2.0)).child(labeled_field(
            "capsule",
            &item.capsule_handle,
            theme,
        )))
        .child(labeled_field("app", &item.installed_app_id, theme))
        .when(!item.installed_at.is_empty(), |this| {
            this.child(labeled_field("installed", &item.installed_at, theme))
        })
        .when(!item.updated_at.is_empty(), |this| {
            this.child(labeled_field("updated", &item.updated_at, theme))
        })
}

fn labeled_field(label: &str, value: &str, theme: &Theme) -> gpui::Div {
    let label = label.to_string();
    let value = value.to_string();
    div()
        .flex()
        .flex_row()
        .gap(px(6.0))
        .text_size(px(10.0))
        .child(
            div()
                .w(px(56.0))
                .text_color(theme.text_secondary)
                .child(label),
        )
        .child(div().text_color(theme.text_primary).child(value))
}

fn render_profiles(
    item: &InstalledAppDashboardItem,
    selected_profile_id: Option<&str>,
    theme: &Theme,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight(600.0))
                .text_color(theme.text_tertiary)
                .child("Profiles"),
        )
        .children(
            item.profiles
                .iter()
                .map(|profile| render_profile_card(item, profile, selected_profile_id, theme))
                .collect::<Vec<_>>(),
        )
}

fn render_profile_card(
    item: &InstalledAppDashboardItem,
    profile: &InstalledProfileDashboardItem,
    selected_profile_id: Option<&str>,
    theme: &Theme,
) -> gpui::Div {
    let is_selected = selected_profile_id == Some(profile.profile_id.as_str());
    let installed_app_id = item.installed_app_id.clone();
    let profile_id = profile.profile_id.clone();
    let ipk = profile.install_profile_key.clone();

    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .px(px(8.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .when(is_selected, |this| {
            this.bg(hsla(217.0 / 360.0, 0.75, 0.45, 0.10))
                .border_1()
                .border_color(hsla(217.0 / 360.0, 0.75, 0.45, 0.20))
        })
        .when(!is_selected, |this| {
            this.bg(theme.pane_bg_top)
                .cursor_pointer()
                .hover(|style| style.bg(hsla(60.0 / 360.0, 0.05, 0.96, 1.0)))
        })
        .when(!is_selected, |this| {
            this.on_mouse_down(MouseButton::Left, {
                let installed_app_id = installed_app_id.clone();
                let profile_id = profile_id.clone();
                move |_, window, cx| {
                    window.dispatch_action(
                        Box::new(SelectInstalledProfile {
                            installed_app_id: installed_app_id.clone(),
                            profile_id: profile_id.clone(),
                        }),
                        cx,
                    );
                }
            })
        })
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight(500.0))
                        .text_color(theme.text_primary)
                        .child(profile.profile_id.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.0))
                        .child(copy_label(&ipk, "Copy IPK")),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(profile_field("ipk", &ipk, theme))
                .when_some(profile.current_revision_id.as_ref(), |this, rev| {
                    this.child(profile_field("current", rev, theme))
                })
                .child(profile_field(
                    "revisions",
                    &profile.revisions_count.to_string(),
                    theme,
                ))
                .when_some(profile.current_output_dir.as_ref(), |this, dir| {
                    this.child(profile_field("output", dir, theme))
                }),
        )
}

fn profile_field(label: &str, value: &str, theme: &Theme) -> gpui::Div {
    let label = label.to_string();
    let value = value.to_string();
    div()
        .flex()
        .flex_row()
        .gap(px(4.0))
        .text_size(px(9.0))
        .child(
            div()
                .w(px(40.0))
                .text_color(theme.text_disabled)
                .child(label),
        )
        .child(
            div()
                .text_color(theme.text_secondary)
                .overflow_hidden()
                .text_ellipsis()
                .child(value),
        )
}

fn render_revisions(
    _item: &InstalledAppDashboardItem,
    profile: &InstalledProfileDashboardItem,
    theme: &Theme,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight(600.0))
                .text_color(theme.text_tertiary)
                .child("Revisions"),
        )
        .children(
            profile
                .revisions
                .iter()
                .map(|rev| render_revision_row(rev, theme))
                .collect::<Vec<_>>(),
        )
}

fn render_revision_row(rev: &InstalledRevisionDashboardItem, theme: &Theme) -> gpui::Div {
    let rev_id_short = if rev.revision_id.len() > 16 {
        format!("{}...", &rev.revision_id[..16])
    } else {
        rev.revision_id.clone()
    };

    let rev_id = rev.revision_id.clone();
    let output_dir = rev.output_dir.clone();

    let current_color = hsla(217.0 / 360.0, 0.75, 0.45, 1.0);
    let pinned_color = hsla(43.0 / 360.0, 0.90, 0.44, 1.0);

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .py(px(4.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .when(rev.is_current, |this| {
                    this.child(
                        div()
                            .text_size(px(10.0))
                            .font_weight(FontWeight(600.0))
                            .text_color(current_color)
                            .child("*"),
                    )
                })
                .when(!rev.is_current, |this| this.child(div().w(px(10.0))))
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme.text_primary)
                        .child(rev_id_short),
                ),
        )
        .child(div().flex_1())
        .when(rev.is_current, |this| {
            this.child(
                div()
                    .px(px(4.0))
                    .py(px(1.0))
                    .rounded(px(3.0))
                    .bg(hsla(217.0 / 360.0, 0.75, 0.45, 0.12))
                    .text_size(px(8.0))
                    .text_color(current_color)
                    .child("current"),
            )
        })
        .when(rev.is_pinned, |this| {
            this.child(
                div()
                    .px(px(4.0))
                    .py(px(1.0))
                    .rounded(px(3.0))
                    .bg(hsla(43.0 / 360.0, 0.90, 0.44, 0.12))
                    .text_size(px(8.0))
                    .text_color(pinned_color)
                    .child("pinned"),
            )
        })
        .when_some(rev.finalized_at.as_ref(), |this, finalized| {
            this.child(
                div()
                    .text_size(px(8.0))
                    .text_color(theme.text_disabled)
                    .child(finalized.clone()),
            )
        })
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(4.0))
                .child(copy_label(&rev_id, "Copy ID"))
                .child(copy_label(&output_dir, "Copy dir")),
        )
}

fn copy_label(value: &str, label: &str) -> gpui::AnyElement {
    let value_str = value.to_string();
    let label_str = label.to_string();
    div()
        .px(px(4.0))
        .py(px(1.0))
        .rounded(px(3.0))
        .bg(hsla(60.0 / 360.0, 0.05, 0.93, 1.0))
        .text_size(px(8.0))
        .text_color(hsla(217.0 / 360.0, 0.75, 0.45, 1.0))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(217.0 / 360.0, 0.75, 0.45, 0.12)))
        .child(label_str)
        .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(value_str.clone()));
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use crate::install_lifecycle_dashboard::{
        InstalledAppDashboardItem, InstalledProfileDashboardItem, InstalledRevisionDashboardItem,
    };

    fn make_dummy_item(installed_app_id: &str) -> InstalledAppDashboardItem {
        InstalledAppDashboardItem {
            installed_app_id: installed_app_id.to_string(),
            publisher: "acme".to_string(),
            slug: "hello".to_string(),
            capsule_handle: "acme/hello".to_string(),
            version: "1.0.0".to_string(),
            installed_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            profiles: vec![InstalledProfileDashboardItem {
                profile_id: "default".to_string(),
                install_profile_key: "ipk_test".to_string(),
                current_revision_id: Some("rev_001".to_string()),
                revisions_count: 1,
                latest_finalized_at: Some("2026-01-02T00:00:00Z".to_string()),
                current_output_dir: Some("/tmp/output".to_string()),
                revisions: vec![InstalledRevisionDashboardItem {
                    revision_id: "rev_001".to_string(),
                    is_current: true,
                    is_pinned: false,
                    finalized_at: Some("2026-01-02T00:00:00Z".to_string()),
                    output_dir: "/tmp/output".to_string(),
                }],
            }],
            running_sessions_hint: vec![],
        }
    }

    fn resolve_selected_app<'a>(
        items: &'a [InstalledAppDashboardItem],
        selected_id: Option<&str>,
    ) -> Option<&'a InstalledAppDashboardItem> {
        selected_id
            .and_then(|id| items.iter().find(|item| item.installed_app_id == id))
            .or_else(|| items.first().filter(|_| selected_id.is_none()))
    }

    fn resolve_selected_profile<'a>(
        item: &'a InstalledAppDashboardItem,
        selected_profile_id: Option<&str>,
    ) -> Option<&'a InstalledProfileDashboardItem> {
        selected_profile_id
            .and_then(|pid| item.profiles.iter().find(|p| p.profile_id == pid))
            .or_else(|| item.profiles.first())
    }

    #[test]
    fn resolve_selected_app_returns_selected() {
        let items = vec![make_dummy_item("app_aaa"), make_dummy_item("app_bbb")];
        let result = resolve_selected_app(&items, Some("app_bbb"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().installed_app_id, "app_bbb");
    }

    #[test]
    fn resolve_selected_app_falls_back_to_first() {
        let items = vec![make_dummy_item("app_aaa"), make_dummy_item("app_bbb")];
        let result = resolve_selected_app(&items, None);
        assert!(result.is_some());
        assert_eq!(result.unwrap().installed_app_id, "app_aaa");
    }

    #[test]
    fn resolve_selected_app_returns_none_for_missing() {
        let items = vec![make_dummy_item("app_aaa")];
        // Production: Some(missing) does NOT fall back to first item
        let result = resolve_selected_app(&items, Some("app_missing"));
        assert!(result.is_none());
    }

    #[test]
    fn resolve_selected_profile_prefers_default() {
        let item = InstalledAppDashboardItem {
            installed_app_id: "app_test".to_string(),
            publisher: "acme".to_string(),
            slug: "hello".to_string(),
            capsule_handle: "acme/hello".to_string(),
            version: "1.0.0".to_string(),
            installed_at: "".to_string(),
            updated_at: "".to_string(),
            profiles: vec![
                InstalledProfileDashboardItem {
                    profile_id: "default".to_string(),
                    install_profile_key: "ipk_default".to_string(),
                    current_revision_id: None,
                    revisions_count: 0,
                    latest_finalized_at: None,
                    current_output_dir: None,
                    revisions: vec![],
                },
                InstalledProfileDashboardItem {
                    profile_id: "prod".to_string(),
                    install_profile_key: "ipk_prod".to_string(),
                    current_revision_id: None,
                    revisions_count: 0,
                    latest_finalized_at: None,
                    current_output_dir: None,
                    revisions: vec![],
                },
            ],
            running_sessions_hint: vec![],
        };
        let result = resolve_selected_profile(&item, None);
        assert_eq!(result.map(|p| p.profile_id.as_str()), Some("default"));

        let result2 = resolve_selected_profile(&item, Some("prod"));
        assert_eq!(result2.map(|p| p.profile_id.as_str()), Some("prod"));
    }

    #[test]
    fn resolve_selected_app_empty_returns_none() {
        let items: Vec<InstalledAppDashboardItem> = vec![];
        let result = resolve_selected_app(&items, None);
        assert!(result.is_none());

        let result2 = resolve_selected_app(&items, Some("anything"));
        assert!(result2.is_none());
    }

    #[test]
    fn resolve_selected_profile_falls_back_to_first_no_default() {
        let item = InstalledAppDashboardItem {
            installed_app_id: "app_test".to_string(),
            publisher: "acme".to_string(),
            slug: "hello".to_string(),
            capsule_handle: "acme/hello".to_string(),
            version: "1.0.0".to_string(),
            installed_at: "".to_string(),
            updated_at: "".to_string(),
            profiles: vec![
                InstalledProfileDashboardItem {
                    profile_id: "prod".to_string(),
                    install_profile_key: "ipk_prod".to_string(),
                    current_revision_id: None,
                    revisions_count: 0,
                    latest_finalized_at: None,
                    current_output_dir: None,
                    revisions: vec![],
                },
                InstalledProfileDashboardItem {
                    profile_id: "staging".to_string(),
                    install_profile_key: "ipk_staging".to_string(),
                    current_revision_id: None,
                    revisions_count: 0,
                    latest_finalized_at: None,
                    current_output_dir: None,
                    revisions: vec![],
                },
            ],
            running_sessions_hint: vec![],
        };
        let result = resolve_selected_profile(&item, None);
        assert_eq!(
            result.map(|p| p.profile_id.as_str()),
            Some("prod"),
            "should fall back to first profile when no 'default' exists"
        );
    }

    #[test]
    fn resolve_selected_profile_empty_returns_none() {
        let item = InstalledAppDashboardItem {
            installed_app_id: "app_test".to_string(),
            publisher: "acme".to_string(),
            slug: "hello".to_string(),
            capsule_handle: "acme/hello".to_string(),
            version: "1.0.0".to_string(),
            installed_at: "".to_string(),
            updated_at: "".to_string(),
            profiles: vec![],
            running_sessions_hint: vec![],
        };
        let result = resolve_selected_profile(&item, None);
        assert!(result.is_none(), "empty profiles -> None (no panic)");
    }
}

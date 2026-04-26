use std::collections::HashMap;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, img, linear_color_stop, linear_gradient, point, px, BoxShadow, Div, FontWeight, Image,
    InteractiveElement, IntoElement, MouseButton,
};
use gpui_component::{Icon, IconName};

use super::theme::Theme;
use crate::app::{NewTab, SelectTask, ShowSettings};
use crate::state::{AppState, SidebarTaskIconSpec, SidebarTaskItem, SystemPageIcon};

const NAV_ITEM_SIZE: f32 = 36.0;
const APP_ICON_SIZE: f32 = 22.0;

#[derive(Clone)]
pub(super) enum FaviconState {
    Loading,
    Ready(Arc<Image>),
    Failed,
}

pub(super) fn render_task_rail(
    state: &AppState,
    favicon_cache: &HashMap<String, FaviconState>,
    theme: &Theme,
) -> impl IntoElement {
    let tasks = state.sidebar_task_items();
    let panel_bg = theme.panel_bg;
    let panel_border = theme.panel_border;

    div()
        .w(px(52.0))
        .min_w(px(52.0))
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .py_3()
        .gap_1()
        .bg(panel_bg)
        .border_r_1()
        .border_color(panel_border)
        .children(
            tasks
                .into_iter()
                .enumerate()
                .map(move |(i, task)| render_nav_item(task, i, favicon_cache, theme)),
        )
        .child(render_nav_separator(theme))
        .child(render_new_tab_button(theme))
        .child(div().flex_1())
        .child(render_settings_nav_item(theme))
}

pub(super) fn favicon_request_url(origin: &str) -> Option<String> {
    let parsed = url::Url::parse(origin).ok()?;
    match parsed.scheme() {
        "http" | "https" => Some(format!("{origin}/favicon.ico")),
        _ => None,
    }
}

fn render_nav_item(
    task: SidebarTaskItem,
    index: usize,
    favicon_cache: &HashMap<String, FaviconState>,
    theme: &Theme,
) -> Div {
    let task_id = task.id;
    let accent_subtle = theme.accent_subtle;
    let accent = theme.accent;

    let item = div()
        .w(px(NAV_ITEM_SIZE))
        .h(px(NAV_ITEM_SIZE))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .relative()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.dispatch_action(Box::new(SelectTask { task_id }), cx);
        });

    let item = if task.is_active {
        item.bg(accent_subtle).child(
            div()
                .absolute()
                .left(px(-8.0))
                .top_1_2()
                .mt(px(-9.0))
                .w(px(3.0))
                .h(px(18.0))
                .rounded_r(px(3.0))
                .bg(accent),
        )
    } else {
        item
    };

    item.child(render_app_icon(
        task.icon,
        index,
        task.is_active,
        favicon_cache,
        theme,
    ))
}

fn render_app_icon(
    icon: SidebarTaskIconSpec,
    hue_index: usize,
    active: bool,
    favicon_cache: &HashMap<String, FaviconState>,
    theme: &Theme,
) -> Div {
    match icon {
        SidebarTaskIconSpec::Monogram(label) => {
            render_monogram_icon(&label, workspace_hue(hue_index), active, theme)
        }
        SidebarTaskIconSpec::ExternalUrl { origin } => match favicon_cache.get(&origin) {
            Some(FaviconState::Ready(image)) => render_favicon_icon(image.clone(), active, theme),
            Some(FaviconState::Loading) | Some(FaviconState::Failed) | None => {
                render_globe_icon(active, theme)
            }
        },
        SidebarTaskIconSpec::SystemIcon(page_type) => render_system_icon(page_type, active, theme),
    }
}

fn render_monogram_icon(label: &str, hue: f32, active: bool, theme: &Theme) -> Div {
    let saturation = if active { 0.65 } else { 0.50 };
    let lightness = if active { 0.55 } else { 0.42 };
    let border_color = theme.border_default;

    div()
        .w(px(APP_ICON_SIZE))
        .h(px(APP_ICON_SIZE))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(linear_gradient(
            135.,
            linear_color_stop(gpui::hsla(hue / 360.0, saturation, lightness, 1.0), 0.),
            linear_color_stop(
                gpui::hsla(
                    ((hue + 30.0) % 360.0) / 360.0,
                    saturation * 0.9,
                    lightness * 0.85,
                    1.0,
                ),
                1.,
            ),
        ))
        .shadow(vec![BoxShadow {
            color: gpui::hsla(hue / 360.0, saturation, lightness, 0.35),
            offset: point(px(0.), px(2.)),
            blur_radius: px(6.),
            spread_radius: px(0.),
        }])
        .border_1()
        .border_color(border_color)
        .text_size(px(9.0))
        .font_weight(FontWeight::BOLD)
        .text_color(gpui::hsla(0.0, 0.0, 1.0, 1.0))
        .child(label.to_string())
}

fn render_favicon_icon(image: Arc<Image>, active: bool, theme: &Theme) -> Div {
    let bg = if active {
        theme.accent_subtle
    } else {
        theme.surface_hover
    };
    let border_color = theme.border_default;

    div()
        .w(px(APP_ICON_SIZE))
        .h(px(APP_ICON_SIZE))
        .rounded(px(5.0))
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .border_1()
        .border_color(border_color)
        .child(img(image).size_full())
}

fn render_globe_icon(active: bool, theme: &Theme) -> Div {
    let bg = if active {
        theme.accent_subtle
    } else {
        theme.surface_hover
    };
    let border_color = theme.border_default;
    let text_color = if active {
        theme.accent
    } else {
        theme.text_tertiary
    };

    div()
        .w(px(APP_ICON_SIZE))
        .h(px(APP_ICON_SIZE))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .border_1()
        .border_color(border_color)
        .text_color(text_color)
        .text_size(px(12.0))
        .font_weight(FontWeight::BOLD)
        .child("◎")
}

fn render_system_icon(page_type: SystemPageIcon, active: bool, theme: &Theme) -> Div {
    let (label, hue) = match page_type {
        SystemPageIcon::Console => (">_", 270.0),    // purple
        SystemPageIcon::Terminal => ("$", 160.0),    // green
        SystemPageIcon::Launcher => ("◆", 217.0),    // blue
        SystemPageIcon::Inspector => ("i", 45.0),    // yellow
        SystemPageIcon::CapsuleStatus => ("⊙", 0.0), // red
    };

    let saturation = if active { 0.55 } else { 0.40 };
    let lightness = if active { 0.50 } else { 0.38 };
    let bg = gpui::hsla(
        hue / 360.0,
        saturation,
        lightness,
        if active { 0.25 } else { 0.15 },
    );
    let text_color = gpui::hsla(hue / 360.0, saturation + 0.1, lightness + 0.2, 1.0);
    let border_color = theme.border_default;

    div()
        .w(px(APP_ICON_SIZE))
        .h(px(APP_ICON_SIZE))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .border_1()
        .border_color(border_color)
        .text_color(text_color)
        .text_size(px(10.0))
        .font_weight(FontWeight::BOLD)
        .child(label)
}

fn render_nav_separator(theme: &Theme) -> Div {
    div()
        .w(px(24.0))
        .h(px(1.0))
        .bg(theme.border_subtle)
        .my_1p5()
}

#[allow(dead_code)]
fn render_branch_section() -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(3.0))
        .child(
            div()
                .text_size(px(14.0))
                .text_color(gpui::rgb(0xa78bfa))
                .child("⑂"),
        )
        .child(
            div()
                .w(px(1.0))
                .h(px(12.0))
                .bg(gpui::hsla(0.0, 0.0, 1.0, 0.10)),
        )
        .child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .rounded_full()
                .bg(gpui::rgb(0xa78bfa))
                .cursor_pointer()
                .shadow(vec![BoxShadow {
                    color: gpui::hsla(270.0 / 360.0, 0.73, 0.73, 0.3),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(4.),
                    spread_radius: px(0.),
                }]),
        )
        .child(
            div()
                .w(px(1.0))
                .h(px(12.0))
                .bg(gpui::hsla(0.0, 0.0, 1.0, 0.10)),
        )
        .child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .rounded_full()
                .border_1()
                .border_color(gpui::rgb(0xa78bfa))
                .cursor_pointer(),
        )
}

fn render_settings_nav_item(theme: &Theme) -> Div {
    let bg = theme.surface_hover;
    let icon_color = theme.text_tertiary;

    div()
        .w(px(NAV_ITEM_SIZE))
        .h(px(NAV_ITEM_SIZE))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.dispatch_action(Box::new(ShowSettings), cx);
        })
        .child(
            div()
                .w(px(APP_ICON_SIZE))
                .h(px(APP_ICON_SIZE))
                .rounded(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(bg)
                .text_size(px(14.0))
                .text_color(icon_color)
                .child("⚙"),
        )
}

fn render_new_tab_button(theme: &Theme) -> Div {
    let border_color = theme.border_strong;
    let icon_color = theme.text_secondary;

    div()
        .w(px(NAV_ITEM_SIZE))
        .h(px(NAV_ITEM_SIZE))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.dispatch_action(Box::new(NewTab), cx);
        })
        .child(
            div()
                .w(px(APP_ICON_SIZE))
                .h(px(APP_ICON_SIZE))
                .rounded(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .border_1()
                .border_color(border_color)
                .text_color(icon_color)
                .child(Icon::new(IconName::Plus).size(px(16.0)).into_any_element()),
        )
}

fn workspace_hue(index: usize) -> f32 {
    const HUES: &[f32] = &[217.0, 270.0, 160.0, 45.0, 0.0, 25.0];
    HUES[index % HUES.len()]
}

#[cfg(test)]
mod tests {
    use super::favicon_request_url;

    #[test]
    fn favicon_request_is_built_from_origin() {
        assert_eq!(
            favicon_request_url("https://example.com"),
            Some("https://example.com/favicon.ico".to_string())
        );
        assert_eq!(
            favicon_request_url("http://localhost:3000"),
            Some("http://localhost:3000/favicon.ico".to_string())
        );
        assert_eq!(favicon_request_url("file:///tmp/app"), None);
    }
}

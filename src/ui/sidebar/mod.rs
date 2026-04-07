use std::collections::HashMap;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, hsla, img, linear_color_stop, linear_gradient, point, px, rgb, BoxShadow, Div, FontWeight,
    Image, InteractiveElement, IntoElement, MouseButton,
};
use gpui_component::{Icon, IconName};

use crate::app::{NewTab, SelectTask, ShowSettings};
use crate::state::{AppState, SidebarTaskIconSpec, SidebarTaskItem};

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
) -> impl IntoElement {
    let tasks = state.sidebar_task_items();

    div()
        .w(px(52.0))
        .min_w(px(52.0))
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .py_3()
        .gap_1()
        .bg(hsla(240.0 / 360.0, 0.09, 0.13, 0.85))
        .border_r_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.06))
        .children(
            tasks
                .into_iter()
                .enumerate()
                .map(move |(i, task)| render_nav_item(task, i, favicon_cache)),
        )
        .child(render_nav_separator())
        .child(render_new_tab_button())
        // .child(render_branch_section())
        .child(div().flex_1())
        .child(render_settings_nav_item())
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
) -> Div {
    let task_id = task.id;
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
        item.bg(hsla(217.0 / 360.0, 0.60, 0.50, 0.15)).child(
            div()
                .absolute()
                .left(px(-8.0))
                .top_1_2()
                .mt(px(-9.0))
                .w(px(3.0))
                .h(px(18.0))
                .rounded_r(px(3.0))
                .bg(rgb(0x3b82f6)),
        )
    } else {
        item
    };

    item.child(render_app_icon(
        task.icon,
        index,
        task.is_active,
        favicon_cache,
    ))
}

fn render_app_icon(
    icon: SidebarTaskIconSpec,
    hue_index: usize,
    active: bool,
    favicon_cache: &HashMap<String, FaviconState>,
) -> Div {
    match icon {
        SidebarTaskIconSpec::Monogram(label) => {
            render_monogram_icon(&label, workspace_hue(hue_index), active)
        }
        SidebarTaskIconSpec::ExternalUrl { origin } => match favicon_cache.get(&origin) {
            Some(FaviconState::Ready(image)) => render_favicon_icon(image.clone(), active),
            Some(FaviconState::Loading) | Some(FaviconState::Failed) | None => {
                render_globe_icon(active)
            }
        },
    }
}

fn render_monogram_icon(label: &str, hue: f32, active: bool) -> Div {
    let saturation = if active { 0.65 } else { 0.50 };
    let lightness = if active { 0.55 } else { 0.42 };

    div()
        .w(px(APP_ICON_SIZE))
        .h(px(APP_ICON_SIZE))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(linear_gradient(
            135.,
            linear_color_stop(hsla(hue / 360.0, saturation, lightness, 1.0), 0.),
            linear_color_stop(
                hsla(
                    ((hue + 30.0) % 360.0) / 360.0,
                    saturation * 0.9,
                    lightness * 0.85,
                    1.0,
                ),
                1.,
            ),
        ))
        .shadow(vec![BoxShadow {
            color: hsla(hue / 360.0, saturation, lightness, 0.35),
            offset: point(px(0.), px(2.)),
            blur_radius: px(6.),
            spread_radius: px(0.),
        }])
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.10))
        .text_size(px(9.0))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(0xffffff))
        .child(label.to_string())
}

fn render_favicon_icon(image: Arc<Image>, active: bool) -> Div {
    div()
        .w(px(APP_ICON_SIZE))
        .h(px(APP_ICON_SIZE))
        .rounded(px(5.0))
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .bg(if active {
            hsla(217.0 / 360.0, 0.60, 0.50, 0.16)
        } else {
            hsla(0.0, 0.0, 1.0, 0.05)
        })
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.10))
        .child(img(image).size_full())
}

fn render_globe_icon(active: bool) -> Div {
    div()
        .w(px(APP_ICON_SIZE))
        .h(px(APP_ICON_SIZE))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(if active {
            hsla(217.0 / 360.0, 0.60, 0.50, 0.15)
        } else {
            hsla(0.0, 0.0, 1.0, 0.06)
        })
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.10))
        .text_color(if active {
            rgb(0x93c5fd)
        } else {
            hsla(0.0, 0.0, 1.0, 0.45).into()
        })
        .text_size(px(12.0))
        .font_weight(FontWeight::BOLD)
        .child("◎")
}

fn render_nav_separator() -> Div {
    div()
        .w(px(24.0))
        .h(px(1.0))
        .bg(hsla(0.0, 0.0, 1.0, 0.06))
        .my_1p5()
}

fn render_branch_section() -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(3.0))
        .child(
            div()
                .text_size(px(14.0))
                .text_color(rgb(0xa78bfa))
                .child("⑂"),
        )
        .child(div().w(px(1.0)).h(px(12.0)).bg(hsla(0.0, 0.0, 1.0, 0.10)))
        .child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .rounded_full()
                .bg(rgb(0xa78bfa))
                .cursor_pointer()
                .shadow(vec![BoxShadow {
                    color: hsla(270.0 / 360.0, 0.73, 0.73, 0.3),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(4.),
                    spread_radius: px(0.),
                }]),
        )
        .child(div().w(px(1.0)).h(px(12.0)).bg(hsla(0.0, 0.0, 1.0, 0.10)))
        .child(
            div()
                .w(px(8.0))
                .h(px(8.0))
                .rounded_full()
                .border_1()
                .border_color(rgb(0xa78bfa))
                .cursor_pointer(),
        )
}

fn render_settings_nav_item() -> Div {
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
                .bg(hsla(0.0, 0.0, 1.0, 0.06))
                .text_size(px(14.0))
                .text_color(hsla(0.0, 0.0, 1.0, 0.32))
                .child("⚙"),
        )
}

fn render_new_tab_button() -> Div {
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
                .border_color(hsla(0.0, 0.0, 1.0, 0.15))
                .text_color(hsla(0.0, 0.0, 1.0, 0.55))
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

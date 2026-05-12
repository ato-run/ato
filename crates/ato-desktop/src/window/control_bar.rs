//! Floating Control Bar window — light pill bar with four
//! affordances (Settings · URL pill · Card Switcher · Store). Matches
//! the redesign reference mockups.
//!
//! Real `addChildWindow:ordered:` parent-child attachment is still
//! TBD on this branch — for now the orchestrator passes initial
//! parent bounds so the bar opens anchored to the top of its app
//! window. OS-managed co-movement lands with the raw `objc2_app_kit`
//! plumbing pass.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, point, px, rgb, size, AnyWindowHandle, App, Bounds, BoxShadow, Context, FontWeight,
    IntoElement, MouseButton, Pixels, Render, SharedString, WindowBackgroundAppearance,
    WindowBounds, WindowDecorations, WindowKind, WindowOptions,
};
use gpui_component::{Icon, IconName};

use crate::app::{OpenCardSwitcher, OpenLauncherWindow, ShowSettings};
use crate::state::GuestRoute;

const BAR_WIDTH: f32 = 840.0;
const BAR_HEIGHT: f32 = 60.0;
const WINDOW_PAD: f32 = 16.0;
const BAR_GAP_ABOVE_APP: f32 = 12.0;

pub struct ControlBarShellPlaceholder {
    url_display: SharedString,
}

impl ControlBarShellPlaceholder {
    pub fn new(route: &GuestRoute) -> Self {
        Self {
            url_display: SharedString::from(display_url_from_route(route)),
        }
    }
}

fn display_url_from_route(route: &GuestRoute) -> String {
    match route {
        GuestRoute::ExternalUrl(url) => url.as_str().to_string(),
        GuestRoute::CapsuleHandle { handle, .. } => format!("ato.app/shell://{handle}"),
        GuestRoute::CapsuleUrl { handle, url, .. } => {
            format!("ato.app/shell://{handle} → {url}")
        }
        GuestRoute::Capsule { session, .. } => format!("capsule://{session}/"),
        GuestRoute::Terminal { session_id } => format!("terminal://{session_id}/"),
    }
}

impl Render for ControlBarShellPlaceholder {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let url = self.url_display.clone();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(bar_pill(url))
    }
}

fn bar_pill(url: SharedString) -> impl IntoElement {
    div()
        .w(px(BAR_WIDTH))
        .h(px(BAR_HEIGHT))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap_2()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .rounded(px(BAR_HEIGHT / 2.0))
        .shadow(vec![BoxShadow {
            color: hsla(0.0, 0.0, 0.0, 0.10),
            offset: point(px(0.0), px(8.0)),
            blur_radius: px(24.0),
            spread_radius: px(0.0),
        }])
        .child(pill_button(
            "settings",
            Some(IconName::Settings),
            Some("設定"),
            ActionTarget::Settings,
        ))
        .child(url_pill(url))
        .child(pill_button(
            "card-switcher",
            Some(IconName::GalleryVerticalEnd),
            None,
            ActionTarget::CardSwitcher,
        ))
        .child(pill_button(
            "store",
            Some(IconName::Inbox),
            Some("ストア"),
            ActionTarget::Store,
        ))
}

#[derive(Copy, Clone)]
enum ActionTarget {
    Settings,
    Store,
    CardSwitcher,
}

fn pill_button(
    id: &'static str,
    icon: Option<IconName>,
    label: Option<&'static str>,
    target: ActionTarget,
) -> impl IntoElement {
    let mut body = div()
        .id(id)
        .h(px(40.0))
        .px(px(if label.is_some() { 14.0 } else { 12.0 }))
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(20.0))
        .bg(rgb(0xfafafa))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .text_color(rgb(0x18181b))
        .text_sm()
        .font_weight(FontWeight(500.0))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(0xf4f4f5)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| match target {
            ActionTarget::Settings => {
                window.dispatch_action(Box::new(OpenLauncherWindow), cx);
                window.dispatch_action(Box::new(ShowSettings), cx);
            }
            ActionTarget::Store => {
                window.dispatch_action(Box::new(OpenLauncherWindow), cx);
            }
            ActionTarget::CardSwitcher => {
                window.dispatch_action(Box::new(OpenCardSwitcher), cx);
            }
        });
    if let Some(icon) = icon {
        body = body.child(Icon::new(icon).size(px(16.0)));
    }
    if let Some(label) = label {
        body = body.child(div().child(label));
    }
    body
}

fn url_pill(url: SharedString) -> impl IntoElement {
    div()
        .id("url-pill")
        .flex_1()
        .h(px(40.0))
        .px(px(14.0))
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(20.0))
        .bg(rgb(0xfafafa))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .text_color(rgb(0x18181b))
        .text_sm()
        .child(Icon::new(IconName::Globe).size(px(16.0)))
        .child(div().flex_1().overflow_hidden().child(url))
        .child(Icon::new(IconName::ChevronDown).size(px(14.0)))
}

/// Open the bar anchored above a parent app window's bounds. Returns
/// the new bar's `AnyWindowHandle` so the orchestrator can pass it
/// to `crate::window::macos::attach_as_child` together with the
/// parent handle — that's where the real `addChildWindow:ordered:`
/// plumbing happens.
pub fn open_control_bar_window_at(
    cx: &mut App,
    parent_bounds: Bounds<Pixels>,
    route: GuestRoute,
) -> Result<AnyWindowHandle> {
    let bar_w = px(BAR_WIDTH + 2.0 * WINDOW_PAD);
    let bar_h = px(BAR_HEIGHT + 2.0 * WINDOW_PAD);
    // Center horizontally on the parent; sit just above the parent's top edge.
    let origin = gpui::Point {
        x: parent_bounds.origin.x + (parent_bounds.size.width - bar_w) / 2.0,
        y: parent_bounds.origin.y - bar_h + px(BAR_GAP_ABOVE_APP),
    };
    let bounds = Bounds {
        origin,
        size: size(bar_w, bar_h),
    };
    open_control_bar_inner(cx, bounds, route)
}

/// Standalone bar opener — keeps the legacy code path callable
/// (e.g. AODD scripts) without parent bounds. Centers on screen.
pub fn open_control_bar_window(cx: &mut App) -> Result<AnyWindowHandle> {
    let bar_w = px(BAR_WIDTH + 2.0 * WINDOW_PAD);
    let bar_h = px(BAR_HEIGHT + 2.0 * WINDOW_PAD);
    let bounds = Bounds::centered(None, size(bar_w, bar_h), cx);
    open_control_bar_inner(
        cx,
        bounds,
        GuestRoute::ExternalUrl(
            url::Url::parse("https://ato.run/").expect("https://ato.run/ is a valid URL"),
        ),
    )
}

fn open_control_bar_inner(
    cx: &mut App,
    bounds: Bounds<Pixels>,
    route: GuestRoute,
) -> Result<AnyWindowHandle> {
    let options = WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: true,
        is_resizable: false,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    let handle = cx.open_window(options, move |window, cx| {
        let shell = cx.new(|_cx| ControlBarShellPlaceholder::new(&route));
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    Ok(*handle)
}

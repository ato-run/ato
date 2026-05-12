//! Floating Control Bar window — a white pill with five regions
//! (Settings · URL pill · Card Switcher · Store · info indicators).
//! Reproduces the reference mockup at `.tmp/control-bar.png`:
//! - Opaque white pill with a soft multi-layer drop shadow
//! - Transparent window backdrop so the shadow blurs through to the
//!   desktop / app behind without a coloured halo
//! - Icon affordances are bare (no fill, no border) — they sit
//!   directly on the pill background; only the URL chip carries its
//!   own light tint
//! - URL text in muted zinc-grey rather than near-black
//! - Two small ⓘ info dots at the right edge

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, point, px, rgb, size, svg, AnyWindowHandle, App, Bounds, BoxShadow, Context,
    FontWeight, IntoElement, MouseButton, Pixels, Render, SharedString,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions,
};
use gpui_component::{Icon, IconName};

use crate::app::{OpenCardSwitcher, OpenLauncherWindow, OpenStoreWindow, ShowSettings};
use crate::state::GuestRoute;

const BAR_WIDTH: f32 = 720.0;
const BAR_HEIGHT: f32 = 56.0;
/// Padding around the pill inside the host window — gives the
/// multi-layer drop shadow room to render. Without this the shadow
/// is clipped flush against the NSWindow edge and the pill loses
/// the floating quality the reference mockup carries.
const WINDOW_PAD: f32 = 32.0;
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
        // Outer wrapper occupies the entire host window — including
        // the transparent padding ring — and centres the pill so the
        // drop shadow has equal room on every side.
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(bar_pill(self.url_display.clone()))
    }
}

fn bar_pill(url: SharedString) -> impl IntoElement {
    div()
        .w(px(BAR_WIDTH))
        .h(px(BAR_HEIGHT))
        .px(px(6.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .bg(rgb(0xffffff))
        .rounded(px(BAR_HEIGHT / 2.0))
        // Hairline border — barely perceptible against the white
        // fill, mirrors the reference's near-invisible edge.
        .border_1()
        .border_color(hsla(0.0, 0.0, 0.0, 0.04))
        // Three-layer drop shadow stacked from tight-and-near-opaque
        // to wide-and-very-soft. Reads as a single natural shadow
        // rather than a single hard offset.
        .shadow(vec![
            BoxShadow {
                color: hsla(0.0, 0.0, 0.0, 0.04),
                offset: point(px(0.0), px(1.0)),
                blur_radius: px(2.0),
                spread_radius: px(0.0),
            },
            BoxShadow {
                color: hsla(0.0, 0.0, 0.0, 0.06),
                offset: point(px(0.0), px(8.0)),
                blur_radius: px(16.0),
                spread_radius: px(0.0),
            },
            BoxShadow {
                color: hsla(0.0, 0.0, 0.0, 0.08),
                offset: point(px(0.0), px(24.0)),
                blur_radius: px(40.0),
                spread_radius: px(-8.0),
            },
        ])
        .child(pill_button(
            "settings",
            Some(PillIcon::Builtin(IconName::Settings)),
            Some("設定"),
            ActionTarget::Settings,
        ))
        .child(url_pill(url))
        .child(pill_button(
            "card-switcher",
            Some(PillIcon::Builtin(IconName::GalleryVerticalEnd)),
            None,
            ActionTarget::CardSwitcher,
        ))
        .child(pill_button(
            "store",
            Some(PillIcon::Custom("icons/shopping-bag.svg")),
            Some("ストア"),
            ActionTarget::Store,
        ))
        .child(info_dots())
}

#[derive(Copy, Clone)]
enum ActionTarget {
    Settings,
    Store,
    CardSwitcher,
}

/// Icon source for `pill_button`. `Builtin` uses gpui_component's
/// auto-generated `IconName` set; `Custom` resolves an SVG path via
/// the app's asset source (local `assets/` first, gpui_component
/// bundle fallback).
#[derive(Clone)]
enum PillIcon {
    Builtin(IconName),
    Custom(&'static str),
}

fn pill_button(
    id: &'static str,
    icon: Option<PillIcon>,
    label: Option<&'static str>,
    target: ActionTarget,
) -> impl IntoElement {
    // Bare icon affordance — no fill, no border — sits directly on
    // the pill's white background. Reveals a subtle zinc-100 fill on
    // hover so the click target is still discoverable. Mirrors the
    // reference mockup where Settings / Card Switcher / Store look
    // identical to the pill surface at rest.
    let mut body = div()
        .id(id)
        .h(px(36.0))
        .px(px(if label.is_some() { 12.0 } else { 10.0 }))
        .flex()
        .items_center()
        .gap(px(6.0))
        .rounded(px(18.0))
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
                window.dispatch_action(Box::new(OpenStoreWindow), cx);
            }
            ActionTarget::CardSwitcher => {
                window.dispatch_action(Box::new(OpenCardSwitcher), cx);
            }
        });
    match icon {
        Some(PillIcon::Builtin(name)) => {
            body = body.child(
                Icon::new(name)
                    .size(px(16.0))
                    .text_color(rgb(0x3f3f46)),
            );
        }
        Some(PillIcon::Custom(path)) => {
            body = body.child(
                svg()
                    .path(SharedString::from(path))
                    .size(px(16.0))
                    .text_color(rgb(0x3f3f46)),
            );
        }
        None => {}
    }
    if let Some(label) = label {
        body = body.child(div().child(label));
    }
    body
}

fn url_pill(url: SharedString) -> impl IntoElement {
    // The URL chip is the only inner affordance with its own fill —
    // a barely-there zinc-50 tint plus a hairline border so the
    // address sits on a recessed surface. URL text is zinc-600
    // (muted) rather than near-black, matching the reference.
    div()
        .id("url-pill")
        .flex_1()
        .h(px(36.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(18.0))
        .bg(rgb(0xfafafa))
        .border_1()
        .border_color(rgb(0xeeeeee))
        .text_color(rgb(0x52525b))
        .text_sm()
        .child(
            Icon::new(IconName::Globe)
                .size(px(15.0))
                .text_color(rgb(0x71717a)),
        )
        .child(div().flex_1().overflow_hidden().child(url))
        .child(
            Icon::new(IconName::ChevronDown)
                .size(px(13.0))
                .text_color(rgb(0x71717a)),
        )
}

fn info_dots() -> impl IntoElement {
    // Two small ⓘ info dots tucked into the right edge of the pill,
    // matching the reference. Decorative for now — future iteration
    // can wire one to "what's running here?" and the other to a
    // help overlay.
    let dot = || {
        div()
            .w(px(20.0))
            .h(px(20.0))
            .flex()
            .items_center()
            .justify_center()
            .child(
                Icon::new(IconName::Info)
                    .size(px(13.0))
                    .text_color(rgb(0xa1a1aa)),
            )
    };
    div()
        .px(px(4.0))
        .flex()
        .items_center()
        .gap(px(2.0))
        .child(dot())
        .child(dot())
}

/// Open the bar anchored above a parent app window's bounds.
pub fn open_control_bar_window_at(
    cx: &mut App,
    parent_bounds: Bounds<Pixels>,
    route: GuestRoute,
) -> Result<AnyWindowHandle> {
    let win_w = px(BAR_WIDTH + 2.0 * WINDOW_PAD);
    let win_h = px(BAR_HEIGHT + 2.0 * WINDOW_PAD);
    let origin = gpui::Point {
        x: parent_bounds.origin.x + (parent_bounds.size.width - win_w) / 2.0,
        y: parent_bounds.origin.y - win_h + px(BAR_GAP_ABOVE_APP),
    };
    let bounds = Bounds {
        origin,
        size: size(win_w, win_h),
    };
    open_control_bar_inner(cx, bounds, route)
}

/// Standalone bar opener — keeps the legacy code path callable
/// without parent bounds.
pub fn open_control_bar_window(cx: &mut App) -> Result<AnyWindowHandle> {
    let win_w = px(BAR_WIDTH + 2.0 * WINDOW_PAD);
    let win_h = px(BAR_HEIGHT + 2.0 * WINDOW_PAD);
    let bounds = Bounds::centered(None, size(win_w, win_h), cx);
    open_control_bar_inner(
        cx,
        bounds,
        GuestRoute::ExternalUrl(
            url::Url::parse("https://ato.run/").expect("https://ato.run/ is a valid URL"),
        ),
    )
}

/// Open the Focus-mode Control Bar as a process-lifetime singleton.
pub fn open_focus_control_bar(cx: &mut App) -> Result<AnyWindowHandle> {
    let win_w = px(BAR_WIDTH + 2.0 * WINDOW_PAD);
    let win_h = px(BAR_HEIGHT + 2.0 * WINDOW_PAD);
    let bounds = match cx.primary_display() {
        Some(d) => {
            let display_bounds = d.bounds();
            let left =
                display_bounds.origin.x + (display_bounds.size.width - win_w) / 2.0;
            let top = display_bounds.origin.y + px(24.0);
            Bounds {
                origin: gpui::point(left, top),
                size: size(win_w, win_h),
            }
        }
        None => Bounds::centered(None, size(win_w, win_h), cx),
    };
    open_control_bar_inner(
        cx,
        bounds,
        GuestRoute::CapsuleHandle {
            handle: "wasedap2p".to_string(),
            label: "WasedaP2P".to_string(),
        },
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
        // Pure transparent so the drop shadow renders cleanly over
        // whatever is behind the window (desktop / Store / AppWindow).
        // No system vibrancy — the pill is opaque white and that is
        // what should read as the bar.
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    let handle = cx.open_window(options, move |window, cx| {
        let shell = cx.new(|_cx| ControlBarShellPlaceholder::new(&route));
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    Ok(*handle)
}

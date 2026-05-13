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

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, px, rgb, size, svg, AnyElement, AnyWindowHandle, App, Bounds, Context, Entity,
    FontWeight, IntoElement, MouseButton, Pixels, Render, SharedString,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Icon, IconName};

use crate::app::{
    NavigateToUrl, OpenCardSwitcher, OpenLauncherWindow, OpenStoreWindow, ShowSettings,
};
use crate::state::GuestRoute;
use crate::window::content_windows::OpenContentWindows;

const BAR_WIDTH: f32 = 720.0;
const BAR_HEIGHT: f32 = 56.0;
/// Host NSWindow is sized flush to the pill — the rectangle of the
/// window and the rectangle of the pill are the same; the only
/// transparent area is the four rounded-corner cut-outs created by
/// the pill's `rounded(BAR_HEIGHT / 2)`. Drop shadow is not declared
/// on `bar_pill` because it would be clipped at the window edge.
const BAR_GAP_ABOVE_APP: f32 = 12.0;

pub struct ControlBarShellPlaceholder {
    /// Editable URL field in the centre of the bar. Wraps
    /// `gpui_component::input::InputState` so we can subscribe to
    /// PressEnter and read the value at render time for the icon
    /// scheme decision.
    omnibar: Entity<InputState>,
    /// Track focus so the MRU → omnibar sync does NOT clobber the
    /// user's in-progress typing. Flipped by the InputEvent::Focus
    /// / InputEvent::Blur subscription.
    omnibar_focused: bool,
}

impl ControlBarShellPlaceholder {
    pub fn new(
        route: &GuestRoute,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = display_url_from_route(route);
        let omnibar = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://… または capsule://… を入力")
                .default_value(initial.clone())
        });

        // PressEnter → dispatch NavigateToUrl. The Focus-mode handler
        // registered in `app::run` decides whether to spawn an
        // ExternalUrl AppWindow (web) or a CapsuleHandle AppWindow
        // (capsule://) based on the scheme.
        // Change events bump notify so the leading icon (Globe vs
        // capsule) re-evaluates as the user types.
        cx.subscribe_in(
            &omnibar,
            window,
            |this: &mut Self, omnibar, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let url = omnibar.read(cx).value().to_string();
                    if !url.is_empty() {
                        window.dispatch_action(Box::new(NavigateToUrl { url }), cx);
                    }
                }
                InputEvent::Change => {
                    cx.notify();
                }
                InputEvent::Focus => {
                    this.omnibar_focused = true;
                }
                InputEvent::Blur => {
                    this.omnibar_focused = false;
                    cx.notify();
                }
            },
        )
        .detach();

        // Re-render whenever the OpenContentWindows set changes so
        // the Card Switcher badge reflects the live count.
        cx.observe_global::<OpenContentWindows>(|_view, cx| {
            cx.notify();
        })
        .detach();

        Self {
            omnibar,
            omnibar_focused: false,
        }
    }
}

fn display_url_from_route(route: &GuestRoute) -> String {
    match route {
        GuestRoute::ExternalUrl(url) => url.as_str().to_string(),
        GuestRoute::CapsuleHandle { handle, .. } => format!("capsule://{handle}"),
        GuestRoute::CapsuleUrl { handle, url, .. } => {
            format!("capsule://{handle} → {url}")
        }
        GuestRoute::Capsule { session, .. } => format!("capsule://{session}/"),
        GuestRoute::Terminal { session_id } => format!("terminal://{session_id}/"),
    }
}

impl Render for ControlBarShellPlaceholder {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let window_count = cx.global::<OpenContentWindows>().len();

        // Mirror the MRU-front window's URL into the omnibar while
        // the user is NOT focused on the field. Typing wins until
        // Blur. Comparing against the current value avoids triggering
        // a needless set_value on every render.
        if !self.omnibar_focused {
            let target = cx
                .global::<OpenContentWindows>()
                .mru_order()
                .into_iter()
                .next()
                .map(|e| e.url.clone());
            if let Some(target) = target {
                let current: SharedString =
                    self.omnibar.read(cx).value().to_string().into();
                if current != target {
                    self.omnibar.update(cx, |state, cx| {
                        state.set_value(target.clone(), window, cx);
                    });
                }
            }
        }

        // Read the current input value so the leading icon swaps
        // between Globe and the custom capsule glyph based on the
        // typed scheme. Cheap read — no clone of InputState.
        let is_capsule = self
            .omnibar
            .read(cx)
            .value()
            .trim_start()
            .starts_with("capsule://");
        bar_pill(self.omnibar.clone(), is_capsule, window_count)
    }
}

fn bar_pill(
    omnibar: Entity<InputState>,
    is_capsule: bool,
    window_count: usize,
) -> impl IntoElement {
    div()
        .size_full()
        .px(px(6.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .bg(rgb(0xffffff))
        // `rounded_full` forces a true capsule shape regardless of the
        // pill's actual rendered height (gpui clamps fractional
        // pixel heights, so `rounded(BAR_HEIGHT / 2.0)` was reading
        // as slightly-too-small at the corners and the curve never
        // fully met).
        .rounded_full()
        // Hairline border — barely perceptible against the white
        // fill, mirrors the reference's near-invisible edge.
        .border_1()
        .border_color(hsla(0.0, 0.0, 0.0, 0.04))
        .child(pill_button(
            "settings",
            Some(PillIcon::Builtin(IconName::Settings)),
            Some("設定"),
            ActionTarget::Settings,
            None,
        ))
        .child(url_pill(omnibar, is_capsule))
        .child(pill_button(
            "card-switcher",
            Some(PillIcon::Builtin(IconName::GalleryVerticalEnd)),
            None,
            ActionTarget::CardSwitcher,
            Some(window_count),
        ))
        .child(pill_button(
            "store",
            Some(PillIcon::Custom("icons/shopping-bag.svg")),
            Some("ストア"),
            ActionTarget::Store,
            None,
        ))
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
    badge: Option<usize>,
) -> impl IntoElement {
    // Bare icon affordance — no fill, no border — sits directly on
    // the pill's white background. Reveals a subtle zinc-100 fill on
    // hover so the click target is still discoverable. Mirrors the
    // reference mockup where Settings / Card Switcher / Store look
    // identical to the pill surface at rest. `.relative()` makes the
    // button the positioning parent for any `.absolute()` overlay
    // (currently: the Card Switcher window-count badge).
    let mut body = div()
        .id(id)
        .relative()
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
    // Window-count badge — only shown when count > 0. Positioned at
    // the button's top-right corner, slightly outdented so it reads
    // as a separate chip rather than an inset label. Zinc-900 fill
    // with white text matches the reference mockup language used for
    // other "live count" affordances elsewhere in the shell.
    if let Some(count) = badge.filter(|n| *n > 0) {
        body = body.child(
            div()
                .absolute()
                .top(px(-2.0))
                .right(px(-2.0))
                .w(px(16.0))
                .h(px(16.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(0x18181b))
                .text_color(rgb(0xffffff))
                .text_size(px(10.0))
                .font_weight(FontWeight(700.0))
                .child(SharedString::from(count.to_string())),
        );
    }
    body
}

fn url_pill(omnibar: Entity<InputState>, is_capsule: bool) -> impl IntoElement {
    // The URL chip is the only inner affordance with its own fill —
    // a barely-there zinc-50 tint plus a hairline border so the
    // address sits on a recessed surface. Now editable — wraps
    // gpui_component::Input which routes keystrokes / clipboard / IME
    // through the standard text-input machinery. Leading icon swaps
    // between Globe (web) and a custom capsule glyph based on the
    // current value's scheme; this is recomputed every render via
    // the `is_capsule` flag the caller passes in.
    let leading_icon: AnyElement = if is_capsule {
        svg()
            .path(SharedString::from("icons/capsule.svg"))
            .size(px(15.0))
            .text_color(rgb(0x4f46e5))
            .into_any_element()
    } else {
        Icon::new(IconName::Globe)
            .size(px(15.0))
            .text_color(rgb(0x71717a))
            .into_any_element()
    };
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
        .child(leading_icon)
        .child(
            div().flex_1().h_full().flex().items_center().child(
                Input::new(&omnibar)
                    .appearance(false)
                    .bordered(false)
                    .focus_bordered(false)
                    .bg(hsla(0.0, 0.0, 0.0, 0.0))
                    .text_size(px(13.0))
                    .text_color(rgb(0x52525b)),
            ),
        )
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
    let win_w = px(BAR_WIDTH);
    let win_h = px(BAR_HEIGHT);
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
    let win_w = px(BAR_WIDTH);
    let win_h = px(BAR_HEIGHT);
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
    let win_w = px(BAR_WIDTH);
    let win_h = px(BAR_HEIGHT);
    let bounds = match cx.primary_display() {
        Some(d) => {
            let display_bounds = d.bounds();
            let left =
                display_bounds.origin.x + (display_bounds.size.width - win_w) / 2.0;
            let top = display_bounds.origin.y + px(36.0);
            Bounds {
                origin: gpui::point(left, top),
                size: size(win_w, win_h),
            }
        }
        None => Bounds::centered(None, size(win_w, win_h), cx),
    };
    // Default the URL input to https://ato.run so cold-launches read
    // as "you're looking at the Store" — matches the Focus-mode boot
    // path that opens the Store window as the initial content
    // surface. Users can type a `capsule://...` to navigate
    // elsewhere; the leading icon switches automatically.
    let initial_route = url::Url::parse("https://ato.run/")
        .map(GuestRoute::ExternalUrl)
        .unwrap_or_else(|_| GuestRoute::CapsuleHandle {
            handle: "wasedap2p".to_string(),
            label: "WasedaP2P".to_string(),
        });
    open_control_bar_inner(cx, bounds, initial_route)
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
        let shell = cx.new(|cx| ControlBarShellPlaceholder::new(&route, window, cx));
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    // Round the NSWindow's contentView layer so the underlying
    // rectangle does not leak through at the corners of the pill.
    // Without this the pill's gpui-side `rounded_full()` reveals the
    // rectangular NSWindow underneath whenever the backdrop happens
    // to share the pill's white fill (e.g. when the Store sits below
    // the bar). macOS treats the rounded contentView as the window
    // shape for clicking, screen-grabs, and shadow casting.
    #[cfg(target_os = "macos")]
    super::macos::round_window_corners(cx, *handle, (BAR_HEIGHT / 2.0) as f64);
    Ok(*handle)
}

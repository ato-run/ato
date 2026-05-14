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
//! TODO: add the avatar icon on the right end

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, px, rgb, size, svg, AnyElement, AnyWindowHandle, App, Bounds, Context, Entity,
    FontWeight, IntoElement, MouseButton, Pixels, Render, SharedString, Window,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Icon, IconName};

use crate::app::{
    NavigateToUrl, OpenCardSwitcher, OpenDockWindow, OpenStoreWindow,
    ShowSettings,
};
use crate::config::ControlBarMode;
use crate::localization::{resolve_locale, tr, LocaleCode};
use crate::state::GuestRoute;
use crate::window::content_windows::OpenContentWindows;

const BAR_WIDTH: f32 = 720.0;
const BAR_HEIGHT: f32 = 56.0;
const COMPACT_BAR_WIDTH: f32 = 360.0;
const COMPACT_HEIGHT: f32 = 10.0;
/// Host NSWindow is sized flush to the pill — the rectangle of the
/// window and the rectangle of the pill are the same; the only
/// transparent area is the four rounded-corner cut-outs created by
/// the pill's `rounded(BAR_HEIGHT / 2)`. Drop shadow is not declared
/// on `bar_pill` because it would be clipped at the window edge.
const BAR_GAP_ABOVE_APP: f32 = 12.0;

#[derive(Default)]
pub struct ControlBarController {
    pub handle: Option<AnyWindowHandle>,
    shell: Option<Entity<ControlBarShellPlaceholder>>,
    mode: ControlBarMode,
    /// Mode to restore when transitioning out of Hidden via show/toggle.
    previous_mode: ControlBarMode,
    expanded: bool,
}

impl gpui::Global for ControlBarController {}

impl ControlBarController {
    pub fn new(mode: ControlBarMode) -> Self {
        let previous_mode = if matches!(mode, ControlBarMode::Hidden) {
            ControlBarMode::AutoHide
        } else {
            mode
        };
        Self {
            handle: None,
            shell: None,
            mode,
            previous_mode,
            expanded: matches!(mode, ControlBarMode::Floating),
        }
    }

    pub fn mode(&self) -> ControlBarMode {
        self.mode
    }

    pub fn is_visible(&self) -> bool {
        self.handle.is_some() && !matches!(self.mode, ControlBarMode::Hidden)
    }

    fn set_window(
        &mut self,
        handle: AnyWindowHandle,
        shell: Entity<ControlBarShellPlaceholder>,
    ) {
        self.handle = Some(handle);
        self.shell = Some(shell);
    }

    pub fn clear_window(&mut self, handle: AnyWindowHandle) {
        if self.handle == Some(handle) {
            self.handle = None;
            self.shell = None;
        }
    }

    pub fn set_mode(&mut self, mode: ControlBarMode) {
        // Save the mode we're leaving as the restore point, unless
        // we're already Hidden (avoid remembering Hidden as previous).
        if !matches!(self.mode, ControlBarMode::Hidden) {
            self.previous_mode = self.mode;
        }
        self.mode = mode;
        self.expanded = matches!(mode, ControlBarMode::Floating);
    }

    pub fn expand(&mut self) {
        if matches!(self.mode, ControlBarMode::AutoHide) {
            self.expanded = true;
        }
    }

    /// Force-expand the bar regardless of current mode.  Used by
    /// `focus_control_bar_input` so that Cmd+L works even in
    /// CompactPill mode (the bar expands temporarily, then collapses
    /// on omnibar blur like AutoHide).
    pub fn force_expand(&mut self) {
        if !matches!(self.mode, ControlBarMode::Floating | ControlBarMode::Hidden) {
            self.expanded = true;
        }
    }

    fn collapse(&mut self) {
        if matches!(self.mode, ControlBarMode::AutoHide | ControlBarMode::CompactPill) {
            self.expanded = false;
        }
    }

    fn should_render_expanded(&self) -> bool {
        // `expanded` is set to `true` by `set_mode` for Floating, by
        // `expand()` for AutoHide, and by `force_expand()` for AutoHide and
        // CompactPill (e.g. via Cmd+L).  It is the single source of truth
        // for whether the bar should currently show the full pill layout.
        self.expanded
    }
}

pub fn install_control_bar_controller(cx: &mut App) {
    let mode = crate::config::load_config().desktop.control_bar.mode;
    cx.set_global(ControlBarController::new(mode));
}

pub fn control_bar_mode(cx: &App) -> ControlBarMode {
    cx.global::<ControlBarController>().mode()
}

pub fn set_control_bar_mode(cx: &mut App, mode: ControlBarMode) -> Result<Option<AnyWindowHandle>> {
    let old_handle = {
        let controller = cx.global_mut::<ControlBarController>();
        controller.set_mode(mode);
        controller.handle
    };
    if let Some(handle) = old_handle {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
    if matches!(mode, ControlBarMode::Hidden) {
        return Ok(None);
    }
    open_focus_control_bar(cx).map(Some)
}

pub fn show_control_bar(cx: &mut App) -> Result<AnyWindowHandle> {
    let existing = cx.global::<ControlBarController>().handle;
    if let Some(handle) = existing {
        if handle.update(cx, |_, window, _| window.activate_window()).is_ok() {
            cx.global_mut::<ControlBarController>().expand();
            resize_bar_window(cx, true);
            return Ok(handle);
        }
        let mode = cx.global::<ControlBarController>().mode();
        cx.set_global(ControlBarController::new(mode));
    }

    if matches!(cx.global::<ControlBarController>().mode(), ControlBarMode::Hidden) {
        let restore = cx.global::<ControlBarController>().previous_mode;
        cx.global_mut::<ControlBarController>().set_mode(restore);
    }
    open_focus_control_bar(cx)
}

pub fn hide_control_bar(cx: &mut App) {
    let handle = {
        let controller = cx.global_mut::<ControlBarController>();
        controller.set_mode(ControlBarMode::Hidden);
        let h = controller.handle;
        // Clear immediately so show_control_bar can't find a stale handle
        // before the async on_window_closed event fires.
        controller.handle = None;
        controller.shell = None;
        h
    };
    if let Some(handle) = handle {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

pub fn toggle_control_bar(cx: &mut App) -> Result<Option<AnyWindowHandle>> {
    if cx.global::<ControlBarController>().is_visible() {
        hide_control_bar(cx);
        Ok(None)
    } else {
        show_control_bar(cx).map(Some)
    }
}

pub fn focus_control_bar_input(cx: &mut App) -> Result<AnyWindowHandle> {
    let handle = show_control_bar(cx)?;
    cx.global_mut::<ControlBarController>().force_expand();
    resize_bar_window(cx, true);
    if let Some(shell) = cx.global::<ControlBarController>().shell.clone() {
        let _ = handle.update(cx, |_, window, cx| {
            window.activate_window();
            shell.update(cx, |shell, cx| shell.focus_omnibar(window, cx));
        });
    }
    Ok(handle)
}

/// Resize the control bar NSWindow **from within a window event handler**
/// (on_mouse_move, on_hover, subscribe_in callbacks). These callbacks
/// already have a `&mut Window` from GPUI, so we use it directly rather
/// than going through `handle.update(cx, ...)`, which would silently fail
/// because GPUI removes the window from its map for the duration of an
/// update and nested updates on the same window return "window not found".
fn resize_bar_window_in_handler(window: &mut Window, expanded: bool) {
    let (new_w, new_h) = if expanded {
        (BAR_WIDTH, BAR_HEIGHT)
    } else {
        (COMPACT_BAR_WIDTH, COMPACT_HEIGHT)
    };
    #[cfg(target_os = "macos")]
    super::macos::resize_window_in_handler(window, new_w, new_h);
    #[cfg(not(target_os = "macos"))]
    let _ = (new_w, new_h, window);
}

/// Resize the control bar NSWindow **from outside a window event handler**
/// (show_control_bar, focus_control_bar_input, etc.) where no `&mut Window`
/// is available. Uses `handle.update(cx, ...)` to enter the window context.
fn resize_bar_window(cx: &mut App, expanded: bool) {
    let handle = match cx.global::<ControlBarController>().handle {
        Some(h) => h,
        None => return,
    };
    let (new_w, new_h) = if expanded {
        (BAR_WIDTH, BAR_HEIGHT)
    } else {
        (COMPACT_BAR_WIDTH, COMPACT_HEIGHT)
    };
    #[cfg(target_os = "macos")]
    super::macos::resize_window_to(cx, handle, new_w, new_h);
    #[cfg(not(target_os = "macos"))]
    let _ = (new_w, new_h, handle);
}

pub struct ControlBarShellPlaceholder {
    /// Editable URL field in the centre of the bar. Wraps
    /// `gpui_component::input::InputState` so we can subscribe to
    /// PressEnter and read the value at render time for the icon
    /// scheme decision.
    omnibar: Entity<InputState>,
    locale: LocaleCode,
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
        let locale = resolve_locale(crate::config::load_config().general.language);
        let placeholder = tr(locale, "control_bar.omnibar_placeholder");
        let omnibar = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
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
                    cx.global_mut::<ControlBarController>().collapse();
                    resize_bar_window_in_handler(window, false);
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
            locale,
            omnibar_focused: false,
        }
    }

    fn focus_omnibar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.omnibar.update(cx, |state, cx| state.focus(window, cx));
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
        let expanded = cx.global::<ControlBarController>().should_render_expanded();
        let omnibar_focused = self.omnibar_focused;

        div()
            .id("control-bar-hover-zone")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_move(|_event, window, cx| {
                let was_expanded = cx.global::<ControlBarController>().expanded;
                cx.global_mut::<ControlBarController>().expand();
                let now_expanded = cx.global::<ControlBarController>().expanded;
                if now_expanded && !was_expanded {
                    resize_bar_window_in_handler(window, true);
                }
                if let Some(shell) = cx.global::<ControlBarController>().shell.clone() {
                    shell.update(cx, |_shell, cx| cx.notify());
                }
            })
            .on_hover(move |hovered, window, cx| {
                if *hovered {
                    // mouseEntered: fires via NSTrackingArea regardless of
                    // whether any normal window has acceptsMouseMovedEvents.
                    // This is the reliable expand trigger when the control bar
                    // is the only window (no store/settings open), because in
                    // that case mouseMoved: — which drives on_mouse_move — is
                    // never delivered to the floating PopUp window alone.
                    let was_expanded = cx.global::<ControlBarController>().expanded;
                    cx.global_mut::<ControlBarController>().expand();
                    let now_expanded = cx.global::<ControlBarController>().expanded;
                    if now_expanded && !was_expanded {
                        resize_bar_window_in_handler(window, true);
                    }
                    if let Some(shell) = cx.global::<ControlBarController>().shell.clone() {
                        shell.update(cx, |_shell, cx| cx.notify());
                    }
                } else if !omnibar_focused {
                    // mouseExited: — collapse when the mouse leaves, unless
                    // the omnibar is focused (InputEvent::Blur handles that).
                    let was_expanded = cx.global::<ControlBarController>().expanded;
                    cx.global_mut::<ControlBarController>().collapse();
                    if was_expanded && !cx.global::<ControlBarController>().expanded {
                        resize_bar_window_in_handler(window, false);
                    }
                    if let Some(shell) = cx.global::<ControlBarController>().shell.clone() {
                        shell.update(cx, |_shell, cx| cx.notify());
                    }
                }
            })
            .child(if expanded {
                bar_pill(
                    self.omnibar.clone(),
                    is_capsule,
                    window_count,
                    self.locale,
                )
                .into_any_element()
            } else {
                compact_pill().into_any_element()
            })
    }
}

fn bar_pill(
    omnibar: Entity<InputState>,
    is_capsule: bool,
    window_count: usize,
    locale: LocaleCode,
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
            Some(tr(locale, "control_bar.settings").into()),
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
            Some(tr(locale, "control_bar.store").into()),
            ActionTarget::Store,
            None,
        ))
        .child(identity_button())
}

fn compact_pill() -> impl IntoElement {
    // Ultra-thin sliver — no text or icons. Just a visible hover target.
    // On hover the bar expands to full size via on_hover on the container.
    div()
        .w(px(COMPACT_BAR_WIDTH))
        .h(px(COMPACT_HEIGHT))
        .rounded_full()
        .bg(hsla(0.0, 0.0, 1.0, 0.90))
        .border_1()
        .border_color(hsla(0.0, 0.0, 0.0, 0.08))
}

fn identity_button() -> impl IntoElement {
    div()
        .id("identity")
        .w(px(36.0))
        .h(px(36.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(rgb(0xeef2ff))
        .border_1()
        .border_color(rgb(0xe0e7ff))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(0xe0e7ff)).border_color(rgb(0xc7d2fe)))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.dispatch_action(Box::new(OpenDockWindow), cx);
        })
        .child(
            svg()
                .path(SharedString::from("icons/identity.svg"))
                .size(px(18.0))
                .text_color(rgb(0x4f46e5)),
        )
}

#[derive(Copy, Clone)]
enum ActionTarget {
    Settings,
    Store,
    CardSwitcher,
    FocusUrl,
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
    label: Option<SharedString>,
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
                // Stage D: Launcher window retired. ShowSettings
                // opens the ato-settings system capsule window
                // directly — no Launcher round-trip needed.
                window.dispatch_action(Box::new(ShowSettings), cx);
            }
            ActionTarget::Store => {
                window.dispatch_action(Box::new(OpenStoreWindow), cx);
            }
            ActionTarget::CardSwitcher => {
                window.dispatch_action(Box::new(OpenCardSwitcher), cx);
            }
            ActionTarget::FocusUrl => {
                if let Err(err) = focus_control_bar_input(cx) {
                    tracing::error!(error = %err, "Control Bar URL focus failed");
                }
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

// fn info_dots() -> impl IntoElement {
//     // Two small ⓘ info dots tucked into the right edge of the pill,
//     // matching the reference. Decorative for now — future iteration
//     // can wire one to "what's running here?" and the other to a
//     // help overlay.
//     let dot = || {
//         div()
//             .w(px(20.0))
//             .h(px(20.0))
//             .flex()
//             .items_center()
//             .justify_center()
//             .child(
//                 Icon::new(IconName::Info)
//                     .size(px(13.0))
//                     .text_color(rgb(0xa1a1aa)),
//             )
//     };
//     div()
//         .px(px(4.0))
//         .flex()
//         .items_center()
//         .gap(px(2.0))
//         .child(dot())
//         .child(dot())
// }

/// Return the initial `(width, height)` for the control bar window.
/// In non-expanded modes (AutoHide, CompactPill) the window starts at
/// the compact dimensions so there is never a first-frame flash at the
/// full-bar size.
fn initial_bar_size(cx: &App) -> (Pixels, Pixels) {
    if cx.global::<ControlBarController>().should_render_expanded() {
        (px(BAR_WIDTH), px(BAR_HEIGHT))
    } else {
        (px(COMPACT_BAR_WIDTH), px(COMPACT_HEIGHT))
    }
}

/// Open the bar anchored above a parent app window's bounds.
pub fn open_control_bar_window_at(
    cx: &mut App,
    parent_bounds: Bounds<Pixels>,
    route: GuestRoute,
) -> Result<AnyWindowHandle> {
    let (win_w, win_h) = initial_bar_size(cx);
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
    let (win_w, win_h) = initial_bar_size(cx);
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
    if let Some(existing) = cx.global::<ControlBarController>().handle {
        if existing.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return Ok(existing);
        }
        cx.global_mut::<ControlBarController>().handle = None;
    }
    if matches!(
        cx.global::<ControlBarController>().mode(),
        ControlBarMode::Hidden
    ) {
        return Err(anyhow::anyhow!("Control Bar mode is hidden"));
    }
    let (win_w, win_h) = initial_bar_size(cx);
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
    let shell_slot: Rc<RefCell<Option<Entity<ControlBarShellPlaceholder>>>> =
        Rc::new(RefCell::new(None));
    let shell_slot_for_window = shell_slot.clone();
    let handle = cx.open_window(options, move |window, cx| {
        let shell = cx.new(|cx| ControlBarShellPlaceholder::new(&route, window, cx));
        *shell_slot_for_window.borrow_mut() = Some(shell.clone());
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    if let Some(shell) = shell_slot.borrow().clone() {
        cx.global_mut::<ControlBarController>()
            .set_window(*handle, shell);
    } else {
        cx.global_mut::<ControlBarController>().handle = Some(*handle);
    }
    // Round the NSWindow's contentView layer so the underlying
    // rectangle does not leak through at the corners of the pill.
    // Without this the pill's gpui-side `rounded_full()` reveals the
    // rectangular NSWindow underneath whenever the backdrop happens
    // to share the pill's white fill (e.g. when the Store sits below
    // the bar). macOS treats the rounded contentView as the window
    // shape for clicking, screen-grabs, and shadow casting.
    // Use the actual initial bar height so the radius is correct for
    // both the full-bar (56 px) and compact-pill (28 px) opening sizes.
    #[cfg(target_os = "macos")]
    {
        let initial_h = if cx.global::<ControlBarController>().should_render_expanded() {
            BAR_HEIGHT
        } else {
            COMPACT_HEIGHT
        };
        super::macos::round_window_corners(cx, *handle, (initial_h / 2.0) as f64);
    }
    Ok(*handle)
}

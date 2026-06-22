//! Floating Control Bar window — a white pill with the following regions:
//! [Settings] [URL Bar] | [Windows/Tray+badge] [Store] [Profile]
//! Restart and Stop are accessed via the Info popup menu.
//! - Opaque white pill with a soft multi-layer drop shadow
//! - Transparent window backdrop so the shadow blurs through to the
//!   desktop / app behind without a coloured halo
//! - Icon affordances are bare (no fill, no border) — they sit
//!   directly on the pill background; only the URL chip carries its
//!   own light tint
//! - URL text in muted zinc-grey rather than near-black
//! - Restart/Stop are icon-only, no text, no border
//! - Capsule icon in URL bar opens detailed Capsule Settings window
//! - Info icon opens lightweight context menu anchored below
//! - Star icon toggles pin state (outline/filled)

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, BoxShadow, ClipboardItem, Context, DispatchPhase,
    Entity, FontWeight, IntoElement, MouseButton, MouseDownEvent, MouseExitEvent, MouseUpEvent,
    Pixels, Render, ScrollWheelEvent, SharedString, Window, WindowBackgroundAppearance,
    WindowBounds, WindowDecorations, WindowKind, WindowOptions, canvas, div, hsla, point, px, rgb,
    size, svg, transparent_black,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Icon, IconName};

use crate::app::{
    FocusNextAppWindow, FocusPrevAppWindow, NavigateToUrl, OpenCardSwitcher, OpenContentWindowLogs,
    OpenContentWindowSettings, OpenDockWindow, OpenStoreWindow, ShowSettings,
    ToggleControlBarInfoPopup, ToggleStarCapsule,
};
use crate::config::{ControlBarMode, load_config, save_config};
use crate::localization::{LocaleCode, resolve_locale, tr};
use crate::state::GuestRoute;
use crate::window::content_windows::OpenContentWindows;
use crate::window::gestures::{GestureAction, GestureState};

const BAR_WIDTH: f32 = 720.0;
const BAR_HEIGHT: f32 = 56.0;
const COMPACT_BAR_WIDTH: f32 = 360.0;
const COMPACT_HEIGHT: f32 = 10.0;

#[derive(Default)]
pub struct ControlBarController {
    pub handle: Option<AnyWindowHandle>,
    pub(crate) shell: Option<Entity<ControlBarShellPlaceholder>>,
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

    fn set_window(&mut self, handle: AnyWindowHandle, shell: Entity<ControlBarShellPlaceholder>) {
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
        if matches!(
            self.mode,
            ControlBarMode::AutoHide | ControlBarMode::CompactPill
        ) {
            self.expanded = false;
        }
    }

    fn should_render_expanded(&self) -> bool {
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
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            cx.global_mut::<ControlBarController>().expand();
            resize_bar_window(cx, true);
            return Ok(handle);
        }
        let mode = cx.global::<ControlBarController>().mode();
        cx.set_global(ControlBarController::new(mode));
    }

    if matches!(
        cx.global::<ControlBarController>().mode(),
        ControlBarMode::Hidden
    ) {
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

fn resize_bar_window_in_handler(window: &mut Window, expanded: bool) {
    let (new_w, new_h) = if expanded {
        (BAR_WIDTH, BAR_HEIGHT)
    } else {
        (COMPACT_BAR_WIDTH, COMPACT_HEIGHT)
    };
    #[cfg(target_os = "macos")]
    super::macos::resize_window_in_handler(window, new_w, new_h);
    #[cfg(target_os = "windows")]
    super::windows::resize_window_in_handler(window, new_w, new_h);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = (new_w, new_h, window);
}

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
    #[cfg(target_os = "windows")]
    super::windows::resize_window_to(cx, handle, new_w, new_h);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = (new_w, new_h, handle);
}

pub struct ControlBarShellPlaceholder {
    omnibar: Entity<InputState>,
    locale: LocaleCode,
    omnibar_focused: bool,
    /// Track which capsule handles are starred (pinned).
    starred_handles: HashSet<String>,
    /// Trackpad / mouse gesture recognizer (#174).
    gesture_state: GestureState,
}

impl ControlBarShellPlaceholder {
    pub fn new(route: &GuestRoute, window: &mut gpui::Window, cx: &mut Context<Self>) -> Self {
        let initial = display_url_from_route(route);
        let locale = resolve_locale(crate::config::load_config().general.language);
        let placeholder = tr(locale, "control_bar.omnibar_placeholder");
        let omnibar = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(initial.clone())
        });

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
                    let was_expanded = cx.global::<ControlBarController>().expanded;
                    cx.global_mut::<ControlBarController>().collapse();
                    if was_expanded && !cx.global::<ControlBarController>().expanded {
                        resize_bar_window_in_handler(window, false);
                    }
                    cx.notify();
                }
            },
        )
        .detach();

        cx.observe_global::<OpenContentWindows>(|_view, cx| {
            cx.notify();
        })
        .detach();

        Self {
            omnibar,
            locale,
            omnibar_focused: false,
            starred_handles: load_config()
                .desktop
                .pinned_capsules
                .iter()
                .cloned()
                .collect(),
            gesture_state: GestureState::new(),
        }
    }

    /// Normalize the omnibar display value into a stable pin key.
    /// When the omnibar is focused, the user's explicit input always wins.
    /// Otherwise the frontmost managed capsule is preferred because the
    /// display value may be a decorated `capsule://{handle} → {url}` form.
    fn current_pin_key(&self, cx: &App) -> Option<String> {
        let value = self.omnibar.read(cx).value().trim().to_string();
        if self.omnibar_focused {
            return Self::parse_pin_key_from_value(&value);
        }
        if let Some(entry) = cx.global::<OpenContentWindows>().frontmost()
            && let Some(capsule) = &entry.capsule
        {
            return Some(format!("capsule://{}", capsule.active_handle()));
        }
        Self::parse_pin_key_from_value(&value)
    }

    fn parse_pin_key_from_value(value: &str) -> Option<String> {
        let raw = value.trim();
        let stripped = raw.strip_prefix("capsule://").filter(|s| !s.is_empty())?;
        let handle = match stripped.find(" → ") {
            Some(pos) => &stripped[..pos],
            None => stripped,
        };
        Some(format!("capsule://{}", handle.trim_end_matches('/')))
    }

    fn focus_omnibar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.omnibar.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Toggle the info popup open/closed. Called from the action handler
    /// in app.rs so it runs outside the render cycle.
    pub(crate) fn toggle_info_popup(&mut self, cx: &mut Context<Self>) {
        if close_info_popup_if_live(cx) {
            cx.notify();
            return;
        }

        let frontmost = cx.global::<OpenContentWindows>().frontmost();
        let model = frontmost
            .as_ref()
            .and_then(|entry| entry.capsule.as_ref())
            .map(|ctx| InfoPopupModel::Managed {
                window_id: entry_handle_to_window_id(frontmost.as_ref().unwrap()),
                title: ctx.title.clone(),
                handle: ctx.active_handle().to_string(),
                current_url: ctx.current_url.clone(),
                local_url: ctx.local_url.clone(),
                session_id: ctx.session_id.clone(),
                log_path: ctx.log_path.clone(),
            })
            .unwrap_or_else(|| {
                let entry = frontmost.as_ref();
                InfoPopupModel::Unmanaged {
                    title: entry
                        .map(|e| e.title.to_string())
                        .unwrap_or_else(|| "No window".to_string()),
                    url: entry.map(|e| e.url.to_string()).unwrap_or_default(),
                }
            });
        if let Err(err) = open_info_popup(cx, model, self.locale) {
            tracing::error!(error = %err, "Failed to open info popup");
        }
        cx.notify();
    }

    /// Toggle star/pin state for the current omnibar URL.
    pub(crate) fn toggle_star(&mut self, cx: &mut Context<Self>) {
        let key = match self.current_pin_key(cx) {
            Some(k) => k,
            None => return,
        };
        if self.starred_handles.contains(&key) {
            self.starred_handles.remove(&key);
        } else {
            self.starred_handles.insert(key);
        }
        let mut config = load_config();
        let mut sorted: Vec<String> = self.starred_handles.iter().cloned().collect();
        sorted.sort();
        config.desktop.pinned_capsules = sorted;
        save_config(&config);
        cx.notify();
    }
}

fn entry_handle_to_window_id(entry: &crate::window::content_windows::ContentWindowEntry) -> u64 {
    entry.handle.window_id().as_u64()
}

fn display_url_from_route(route: &GuestRoute) -> String {
    match route {
        GuestRoute::ExternalUrl(url) => url.as_str().to_string(),
        GuestRoute::CapsuleHandle { handle, .. } => format!("capsule://{handle}"),
        GuestRoute::LocalManifest(local) => format!("capsule://{}", local.source_handle),
        GuestRoute::CapsuleUrl { handle, url, .. } => {
            format!("capsule://{handle} → {url}")
        }
        GuestRoute::Capsule { session, .. } => format!("capsule://{session}/"),
        GuestRoute::Terminal { session_id } => format!("terminal://{session_id}/"),
    }
}

impl Render for ControlBarShellPlaceholder {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let window_count = cx.global::<OpenContentWindows>().len();

        if !self.omnibar_focused {
            let target = cx
                .global::<OpenContentWindows>()
                .mru_order()
                .into_iter()
                .next()
                .map(|e| e.url.clone());
            if let Some(target) = target {
                let current: SharedString = self.omnibar.read(cx).value().to_string().into();
                if current != target {
                    self.omnibar.update(cx, |state, cx| {
                        state.set_value(target.clone(), window, cx);
                    });
                }
            }
        }

        let is_capsule = self
            .omnibar
            .read(cx)
            .value()
            .trim_start()
            .starts_with("capsule://");
        let expanded = cx.global::<ControlBarController>().should_render_expanded();
        let omnibar_focused = self.omnibar_focused;

        let is_starred = self
            .current_pin_key(cx)
            .map(|key| self.starred_handles.contains(&key))
            .unwrap_or(false);

        div()
            .id("control-bar-hover-zone")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                let delta = event.delta.pixel_delta(px(20.0));
                if let Some(action) = this
                    .gesture_state
                    .on_scroll_delta(f32::from(delta.x), f32::from(delta.y))
                {
                    match action {
                        GestureAction::FocusPrev => {
                            window.dispatch_action(Box::new(FocusPrevAppWindow), cx)
                        }
                        GestureAction::FocusNext => {
                            window.dispatch_action(Box::new(FocusNextAppWindow), cx)
                        }
                        GestureAction::OpenCardSwitcher => {
                            window.dispatch_action(Box::new(OpenCardSwitcher), cx)
                        }
                    }
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, _cx| {
                    this.gesture_state
                        .on_mouse_down(f32::from(event.position.x), f32::from(event.position.y));
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, window, cx| {
                    if let Some(action) = this.gesture_state.on_mouse_up()
                        && matches!(action, GestureAction::OpenCardSwitcher)
                    {
                        window.dispatch_action(Box::new(OpenCardSwitcher), cx);
                    }
                }),
            )
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
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                    if let Some(action) = this
                        .gesture_state
                        .on_mouse_move(f32::from(event.position.x), f32::from(event.position.y))
                        && matches!(action, GestureAction::OpenCardSwitcher)
                    {
                        window.dispatch_action(Box::new(OpenCardSwitcher), cx);
                    }
                }),
            )
            .on_hover(move |hovered, window, cx| {
                if *hovered {
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
            .child(
                canvas(|_, _, _| {}, {
                    move |_, _, window, _cx| {
                        window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            if omnibar_focused {
                                return;
                            }
                            let was_expanded = cx.global::<ControlBarController>().expanded;
                            cx.global_mut::<ControlBarController>().collapse();
                            if was_expanded && !cx.global::<ControlBarController>().expanded {
                                resize_bar_window_in_handler(window, false);
                            }
                            if let Some(shell) = cx.global::<ControlBarController>().shell.clone() {
                                shell.update(cx, |_shell, cx| cx.notify());
                            }
                        });
                    }
                })
                .absolute()
                .size(px(0.0)),
            )
            .child(if expanded {
                bar_pill(
                    self.omnibar.clone(),
                    is_capsule,
                    is_starred,
                    window_count,
                    self.locale,
                )
                .into_any_element()
            } else {
                compact_pill().into_any_element()
            })
    }
}

/// Apply the platform-appropriate surface shape to a Control Bar pill.
///
/// macOS: the host window is transparent and rounded to a full pill, so the
/// inner surface is `rounded_full()` with a hairline border that defines the
/// floating pill against the desktop.
///
/// Windows: the host window is opaque with DWM-rounded corners (the full pill
/// shape is macOS-only — see `open_control_bar_inner`). Keep the inner surface
/// square and clip overflow so neither the opaque window background nor a
/// border halo can show in the gap between a rounded inner pill and the
/// DWM-rounded window edge — the very "blur/black around the bar" this is
/// meant to avoid.
#[cfg(target_os = "windows")]
fn bar_surface_shape(d: gpui::Div, _border_alpha: f32) -> gpui::Div {
    d.overflow_hidden()
}

#[cfg(not(target_os = "windows"))]
fn bar_surface_shape(d: gpui::Div, border_alpha: f32) -> gpui::Div {
    d.rounded_full()
        .border_1()
        .border_color(hsla(0.0, 0.0, 0.0, border_alpha))
}

fn bar_pill(
    omnibar: Entity<InputState>,
    is_capsule: bool,
    is_starred: bool,
    window_count: usize,
    locale: LocaleCode,
) -> impl IntoElement {
    bar_surface_shape(
        div()
            .size_full()
            .px(px(6.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .bg(rgb(0xffffff)),
        0.04,
    )
    .child(left_action_group(locale))
    .child(pill_separator())
    .child(url_pill(omnibar, is_capsule, is_starred))
    .child(pill_separator())
    .child(right_action_group(window_count, locale))
}

/// Left group: [Settings]
fn left_action_group(_locale: LocaleCode) -> impl IntoElement {
    div().flex().items_center().gap(px(2.0)).child(pill_button(
        "settings",
        Some(PillIcon::Builtin(IconName::Settings)),
        None,
        ActionTarget::Settings,
        None,
    ))
}

/// Right group: [Windows/Tray+badge] [Store] [My Dock]
fn right_action_group(window_count: usize, _locale: LocaleCode) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(2.0))
        .child(pill_button(
            "windows-tray",
            Some(PillIcon::Builtin(IconName::GalleryVerticalEnd)),
            None,
            ActionTarget::CardSwitcher,
            Some(window_count),
        ))
        .child(pill_button(
            "store",
            Some(PillIcon::Custom("icons/shopping-bag.svg")),
            None,
            ActionTarget::Store,
            None,
        ))
        .child(my_dock_button())
}

/// Thin vertical separator line between bar groups.
fn pill_separator() -> impl IntoElement {
    div()
        .w(px(1.0))
        .h(px(24.0))
        .flex_shrink_0()
        .bg(hsla(0.0, 0.0, 0.0, 0.06))
}

fn compact_pill() -> impl IntoElement {
    bar_surface_shape(
        div()
            .w(px(COMPACT_BAR_WIDTH))
            .h(px(COMPACT_HEIGHT))
            .bg(hsla(0.0, 0.0, 1.0, 0.90)),
        0.08,
    )
}

fn my_dock_button() -> impl IntoElement {
    div()
        .id("my-dock")
        .w(px(36.0))
        .h(px(36.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(18.0))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(0xf4f4f5)))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(OpenDockWindow), cx);
        })
        .child(
            Icon::new(IconName::LayoutDashboard)
                .size(px(16.0))
                .text_color(rgb(0x3f3f46)),
        )
}

#[derive(Copy, Clone)]
enum ActionTarget {
    Settings,
    Store,
    CardSwitcher,
}

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
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            // Stop propagation so the outer gesture zone does not record
            // this mouse-down. Without this, a hold >= 400 ms on the pill
            // button would cause the gesture's on_mouse_up handler to fire
            // a second OpenCardSwitcher dispatch, toggling the window
            // closed immediately after it opened.
            cx.stop_propagation();
            match target {
                ActionTarget::Settings => {
                    window.dispatch_action(Box::new(ShowSettings), cx);
                }
                ActionTarget::Store => {
                    window.dispatch_action(Box::new(OpenStoreWindow), cx);
                }
                ActionTarget::CardSwitcher => {
                    window.dispatch_action(Box::new(OpenCardSwitcher), cx);
                }
            }
        });
    match icon {
        Some(PillIcon::Builtin(name)) => {
            body = body.child(Icon::new(name).size(px(16.0)).text_color(rgb(0x3f3f46)));
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

/// The URL pill — the center focal element of the control bar.
/// Interior: [Capsule icon (clickable)] [Input text] [Info icon] [Star icon]
fn url_pill(omnibar: Entity<InputState>, is_capsule: bool, is_starred: bool) -> impl IntoElement {
    let leading_icon: AnyElement = if is_capsule {
        // Clickable capsule icon → opens Capsule Settings window
        div()
            .cursor_pointer()
            .hover(|s| s.opacity(0.7))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                let target_id = cx
                    .global::<OpenContentWindows>()
                    .frontmost()
                    .filter(|entry| entry.capsule.is_some())
                    .map(|entry| entry_handle_to_window_id(&entry));
                if let Some(window_id) = target_id {
                    window.dispatch_action(Box::new(OpenContentWindowSettings { window_id }), cx);
                }
            })
            .child(
                svg()
                    .path(SharedString::from("icons/capsule.svg"))
                    .size(px(15.0))
                    .text_color(rgb(0x4f46e5)),
            )
            .into_any_element()
    } else {
        Icon::new(IconName::Globe)
            .size(px(15.0))
            .text_color(rgb(0x71717a))
            .into_any_element()
    };

    let star_icon_name = if is_starred {
        IconName::StarFill
    } else {
        IconName::Star
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
        .child(info_icon_button())
        .child(star_icon_button(star_icon_name))
}

/// Info icon button — opens/closes the info popup anchored below the bar.
fn info_icon_button() -> impl IntoElement {
    div()
        .id("info-button")
        .w(px(24.0))
        .h(px(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(12.0))
        .cursor_pointer()
        .hover(|s| s.bg(hsla(0.0, 0.0, 0.0, 0.05)))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(ToggleControlBarInfoPopup), cx);
        })
        .child(
            Icon::new(IconName::Info)
                .size(px(13.0))
                .text_color(rgb(0x71717a)),
        )
}

/// Star icon button — toggles pin state (outline/filled).
fn star_icon_button(icon: IconName) -> impl IntoElement {
    div()
        .id("star-button")
        .w(px(24.0))
        .h(px(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(12.0))
        .cursor_pointer()
        .hover(|s| s.bg(hsla(0.0, 0.0, 0.0, 0.05)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(ToggleStarCapsule), cx);
        })
        .child(Icon::new(icon).size(px(13.0)).text_color(rgb(0x9ca3af)))
}

// ─── Info Popup ───────────────────────────────────────────────────────

/// Tracks the currently-open info popup window handle.
#[derive(Default)]
pub struct InfoPopupWindowSlot(pub Option<AnyWindowHandle>);

impl gpui::Global for InfoPopupWindowSlot {}

#[derive(Clone, Debug)]
enum InfoPopupModel {
    Managed {
        window_id: u64,
        title: String,
        handle: String,
        current_url: String,
        local_url: Option<String>,
        session_id: Option<String>,
        log_path: Option<String>,
    },
    Unmanaged {
        title: String,
        url: String,
    },
}

struct InfoPopupWindow {
    model: InfoPopupModel,
    locale: LocaleCode,
}

impl Render for InfoPopupWindow {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let locale = self.locale;
        div()
            .w(px(300.0))
            .flex()
            .flex_col()
            .rounded(px(14.0))
            .bg(rgb(0xffffff))
            .border_1()
            .border_color(hsla(0.0, 0.0, 0.0, 0.08))
            .shadow(vec![BoxShadow {
                color: hsla(0.0, 0.0, 0.0, 0.14),
                offset: point(px(0.0), px(8.0)),
                blur_radius: px(28.0),
                spread_radius: px(0.0),
            }])
            .overflow_hidden()
            .child(match &model {
                InfoPopupModel::Managed {
                    window_id,
                    title,
                    handle,
                    current_url,
                    local_url,
                    session_id: _,
                    log_path,
                } => info_popup_managed(
                    *window_id,
                    title,
                    handle,
                    current_url,
                    local_url,
                    log_path,
                    locale,
                    cx,
                )
                .into_any_element(),
                InfoPopupModel::Unmanaged { title, url } => {
                    info_popup_unmanaged(title, url, locale).into_any_element()
                }
            })
    }
}

fn info_popup_managed(
    window_id: u64,
    title: &str,
    handle: &str,
    _current_url: &str,
    local_url: &Option<String>,
    log_path: &Option<String>,
    locale: LocaleCode,
    _cx: &mut App,
) -> impl IntoElement {
    let show_logs = log_path.is_some();
    let has_local_url = local_url.is_some();
    let capsule_url = format!("capsule://{handle}");

    div()
        .flex()
        .flex_col()
        .child(info_popup_header(title, handle, locale))
        .child(info_popup_divider())
        .child(info_popup_item_enabled(
            &tr(locale, "control_bar.info.open_in_browser"),
            "open-browser",
            has_local_url,
            Some(IconName::Globe),
            {
                let url = local_url.clone();
                move |_win, _cx| {
                    if let Some(ref url) = url {
                        let _ = crate::proc_util::open_external_url(url);
                    }
                }
            },
        ))
        .child(info_popup_item_enabled(
            &tr(locale, "control_bar.info.open_headless"),
            "open-headless",
            false,
            Some(IconName::SquareTerminal),
            |_, _| {},
        ))
        .child(info_popup_divider())
        .child(info_popup_item_enabled(
            &tr(locale, "control_bar.info.copy_capsule_url"),
            "copy-capsule-url",
            true,
            Some(IconName::Copy),
            {
                let url = capsule_url.clone();
                move |_win, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                }
            },
        ))
        .child(info_popup_item_enabled(
            &tr(locale, "control_bar.info.copy_local_url"),
            "copy-local-url",
            has_local_url,
            Some(IconName::Copy),
            {
                let url = local_url.clone();
                move |_win, cx| {
                    if let Some(ref url) = url {
                        cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                    }
                }
            },
        ))
        .child(info_popup_item_enabled(
            &tr(locale, "control_bar.info.show_identity"),
            "show-execution-identity",
            false,
            Some(IconName::Search),
            |_, _| {},
        ))
        .child(info_popup_divider())
        .child(info_popup_item_enabled(
            &tr(locale, "control_bar.info.view_logs"),
            "view-logs",
            show_logs,
            Some(IconName::SquareTerminal),
            move |win, cx| {
                win.dispatch_action(Box::new(OpenContentWindowLogs { window_id }), cx);
            },
        ))
        .child(info_popup_item_enabled(
            &tr(locale, "control_bar.info.open_settings"),
            "open-settings",
            true,
            Some(IconName::Settings),
            move |win, cx| {
                win.dispatch_action(Box::new(OpenContentWindowSettings { window_id }), cx);
            },
        ))
}

fn info_popup_unmanaged(title: &str, url: &str, locale: LocaleCode) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .child(info_popup_header(title, url, locale))
        .child(info_popup_divider())
        .child(
            div()
                .p(px(14.0))
                .text_size(px(12.0))
                .text_color(rgb(0x6b7280))
                .child(tr(locale, "control_bar.info.unmanaged_desc")),
        )
}

fn info_popup_header(title: &str, subtitle: &str, locale: LocaleCode) -> impl IntoElement {
    div()
        .p(px(14.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(0x6b7280))
                .font_weight(FontWeight(600.0))
                .child(tr(locale, "control_bar.info.current_capsule")),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(rgb(0x111827))
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(0x6b7280))
                        .child(subtitle.to_string()),
                ),
        )
}

fn info_popup_divider() -> impl IntoElement {
    div().w_full().h(px(1.0)).bg(hsla(0.0, 0.0, 0.0, 0.06))
}

fn info_popup_item_enabled(
    label: &str,
    id: &str,
    enabled: bool,
    icon: Option<IconName>,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id = id.to_string();
    div()
        .id(id.clone())
        .px(px(14.0))
        .py(px(8.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .text_size(px(12.5))
        .text_color(if enabled {
            rgb(0x1f2937)
        } else {
            rgb(0x9ca3af)
        })
        .font_weight(if enabled {
            FontWeight(400.0)
        } else {
            FontWeight(300.0)
        })
        .when(enabled, |this| {
            this.cursor_pointer()
                .hover(|s| s.bg(rgb(0xf4f4f5)))
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    on_click(window, cx);
                    dismiss_info_popup(cx);
                })
        })
        .when_some(icon, |this, icon_name| {
            this.child(Icon::new(icon_name).size(px(13.0)).text_color(if enabled {
                rgb(0x6b7280)
            } else {
                rgb(0xd1d5db)
            }))
        })
        .child(label.to_string())
}

fn open_info_popup(
    cx: &mut App,
    model: InfoPopupModel,
    locale: LocaleCode,
) -> Result<AnyWindowHandle> {
    dismiss_info_popup(cx);

    let popup_size = size(px(300.0), px(440.0));

    let control_bar = match cx.global::<ControlBarController>().handle {
        Some(h) => h,
        None => return Err(anyhow::anyhow!("Control bar not open")),
    };

    let popup_bounds = match control_bar.update(cx, |_, window, _| window.bounds()) {
        Ok(bar_bounds) => {
            let left = bar_bounds.origin.x + bar_bounds.size.width - popup_size.width - px(156.0);
            let top = bar_bounds.origin.y + bar_bounds.size.height + px(6.0);
            Bounds {
                origin: point(left, top),
                size: popup_size,
            }
        }
        Err(_) => Bounds::centered(None, popup_size, cx),
    };

    let options = WindowOptions {
        titlebar: None,
        focus: true,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        window_bounds: Some(WindowBounds::Windowed(popup_bounds)),
        window_decorations: Some(WindowDecorations::Client),
        window_background: popup_background_appearance(),
        ..Default::default()
    };

    let handle = cx.open_window(options, move |window, cx| {
        let shell = cx.new(|_cx| InfoPopupWindow {
            model: model.clone(),
            locale,
        });
        cx.new(|cx| gpui_component::Root::new(shell, window, cx).bg(transparent_black()))
    })?;

    cx.set_global(InfoPopupWindowSlot(Some(*handle)));
    Ok(*handle)
}

pub(crate) fn dismiss_info_popup(cx: &mut App) {
    let _ = close_info_popup_if_live(cx);
}

#[cfg(target_os = "windows")]
fn popup_background_appearance() -> WindowBackgroundAppearance {
    // With GPUI DirectComposition disabled on Windows (so child WebView2
    // surfaces composite), a fully transparent popup window presents as
    // opaque black. Blurred keeps the floating pill readable while preserving
    // transparent rounded corners.
    WindowBackgroundAppearance::Blurred
}

#[cfg(not(target_os = "windows"))]
fn popup_background_appearance() -> WindowBackgroundAppearance {
    WindowBackgroundAppearance::Transparent
}

/// Background appearance for the Control Bar window.
///
/// On macOS the window is transparent so the rounded "pill" floats over the
/// desktop. On Windows DirectComposition is disabled (so child WebView2
/// surfaces composite), which means a transparent surface paints black and a
/// blurred one shows acrylic frost in the corners between the rounded content
/// and the DWM-rounded window. An opaque window avoids both — the bar reads as
/// a clean rounded rectangle (corners rounded via DWM).
#[cfg(target_os = "windows")]
fn control_bar_background_appearance() -> WindowBackgroundAppearance {
    WindowBackgroundAppearance::Opaque
}

#[cfg(not(target_os = "windows"))]
fn control_bar_background_appearance() -> WindowBackgroundAppearance {
    WindowBackgroundAppearance::Transparent
}

fn close_info_popup_if_live(cx: &mut App) -> bool {
    let Some(handle) = cx.global::<InfoPopupWindowSlot>().0 else {
        return false;
    };

    cx.set_global(InfoPopupWindowSlot(None));
    match handle.update(cx, |_, window, _| window.remove_window()) {
        Ok(_) => true,
        Err(err) => {
            tracing::debug!(error = %err, "Info popup handle was stale while dismissing");
            false
        }
    }
}

/// Return the initial `(width, height)` for the control bar window.
fn initial_bar_size(cx: &App) -> (Pixels, Pixels) {
    if cx.global::<ControlBarController>().should_render_expanded() {
        (px(BAR_WIDTH), px(BAR_HEIGHT))
    } else {
        (px(COMPACT_BAR_WIDTH), px(COMPACT_HEIGHT))
    }
}

/// Open the Focus-mode Control Bar as a process-lifetime singleton.
pub fn open_focus_control_bar(cx: &mut App) -> Result<AnyWindowHandle> {
    if let Some(existing) = cx.global::<ControlBarController>().handle {
        if existing
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
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
            let left = display_bounds.origin.x + (display_bounds.size.width - win_w) / 2.0;
            let top = display_bounds.origin.y + px(36.0);
            Bounds {
                origin: gpui::point(left, top),
                size: size(win_w, win_h),
            }
        }
        None => Bounds::centered(None, size(win_w, win_h), cx),
    };
    let initial_route = url::Url::parse("https://ato.run/")
        .map(GuestRoute::ExternalUrl)
        .unwrap_or_else(|_| GuestRoute::CapsuleHandle {
            handle: "wasedap2p".to_string(),
            label: "WasedaP2P".to_string(),
            community_toml_id: None,
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
        window_background: control_bar_background_appearance(),
        ..Default::default()
    };
    let shell_slot: Rc<RefCell<Option<Entity<ControlBarShellPlaceholder>>>> =
        Rc::new(RefCell::new(None));
    let shell_slot_for_window = shell_slot.clone();
    let handle = cx.open_window(options, move |window, cx| {
        let shell = cx.new(|cx| ControlBarShellPlaceholder::new(&route, window, cx));
        *shell_slot_for_window.borrow_mut() = Some(shell.clone());
        cx.new(|cx| gpui_component::Root::new(shell, window, cx).bg(transparent_black()))
    })?;
    if let Some(shell) = shell_slot.borrow().clone() {
        cx.global_mut::<ControlBarController>()
            .set_window(*handle, shell);
    } else {
        cx.global_mut::<ControlBarController>().handle = Some(*handle);
    }
    #[cfg(target_os = "macos")]
    {
        let initial_h = if cx.global::<ControlBarController>().should_render_expanded() {
            BAR_HEIGHT
        } else {
            COMPACT_HEIGHT
        };
        super::macos::round_window_corners(cx, *handle, (initial_h / 2.0) as f64);
        // #168 — attach the Control Bar as a child of the frontmost content
        // window so macOS orders them correctly and they move together.
        if let Some(entry) = cx.global::<OpenContentWindows>().frontmost()
            && let Err(err) = super::macos::attach_as_child(cx, entry.handle, *handle)
        {
            tracing::warn!(error = %err, "attach_as_child: could not attach Control Bar");
        }
    }
    #[cfg(target_os = "windows")]
    {
        // DirectComposition is disabled on Windows (so child WebView2 surfaces
        // composite), which means a GPUI window cannot present a
        // per-pixel-alpha transparent surface. Rather than a full pill (which
        // would require region-clipping that leaves visible blur/black around
        // the bar), the window is opaque and we simply round its corners via
        // DWM. The pill shape stays macOS-only.
        let initial_h = if cx.global::<ControlBarController>().should_render_expanded() {
            BAR_HEIGHT
        } else {
            COMPACT_HEIGHT
        };
        super::windows::round_window_corners(cx, *handle, (initial_h / 2.0) as f64);
        if let Some(entry) = cx.global::<OpenContentWindows>().frontmost() {
            if let Err(err) = super::windows::attach_as_child(cx, entry.handle, *handle) {
                tracing::warn!(error = %err, "attach_as_child: could not attach Control Bar");
            }
        }
        // Pin the bar to the always-on-top band so it stays visible in front
        // of the content windows it controls.
        super::windows::set_window_topmost(cx, *handle);
    }
    Ok(*handle)
}

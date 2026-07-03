//! Floating Shell Icon Bar — the Focus-mode top pill.
//!
//! Not a browser toolbar: there is no URL text, no back/forward/reload,
//! no settings button. The pill shows exactly
//! [Ato icon] | [open capsule icons…]
//!   - the fixed Ato icon opens/raises the Ato PWA Home (the control
//!     surface: login, Discover, Run, runner settings),
//!   - one icon per open capsule window; clicking switches to that
//!     capsule's window. Active capsule gets a tinted background,
//!     starting/error states get a small status dot.
//!
//! Capsule URLs / localhost origins are never rendered here.
//! Restart/Stop/logs remain reachable via the Info popup, which is now
//! an automation/debug surface (ToggleControlBarInfoPopup action).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, BoxShadow, ClipboardItem, Context, DispatchPhase, Entity,
    FontWeight, IntoElement, MouseButton, MouseDownEvent, MouseExitEvent, MouseUpEvent, Pixels,
    Render, ScrollWheelEvent, SharedString, Window, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowKind, WindowOptions, canvas, div, hsla, point, px, rgb, size,
    transparent_black,
};
use gpui_component::{Icon, IconName};

use crate::app::{
    FocusContentWindow, FocusNextAppWindow, FocusPrevAppWindow, NavigateToUrl, OpenCardSwitcher,
    OpenContentWindowLogs, OpenContentWindowSettings, ShowAtoHome,
};
use crate::config::{ControlBarMode, load_config, save_config};
use crate::localization::{LocaleCode, resolve_locale, tr};
use crate::remote_runs::RemoteRunsSnapshot;
use crate::window::content_windows::OpenContentWindows;
use crate::window::gestures::{GestureAction, GestureState};
use crate::window::shell_tabs::{self, ShellTab, ShellTabKind, ShellTabStatus};

const BAR_HEIGHT: f32 = 56.0;
/// Fixed width of the (transparent) bar window. The visible pill is
/// centered inside and sized by its icon count.
const BAR_WIDTH: f32 = 720.0;
const COMPACT_BAR_WIDTH: f32 = 360.0;
const COMPACT_HEIGHT: f32 = 10.0;

/// Horizontal padding inside the pill.
const BAR_PAD_X: f32 = 12.0;
/// Square hit-target per tab icon.
const TAB_BUTTON: f32 = 40.0;
/// Gap between adjacent items in the pill.
const TAB_GAP: f32 = 8.0;

#[derive(Default)]
pub struct ControlBarController {
    pub handle: Option<AnyWindowHandle>,
    pub(crate) shell: Option<Entity<ControlBarShellPlaceholder>>,
    mode: ControlBarMode,
    /// Mode to restore when transitioning out of Hidden via show/toggle.
    previous_mode: ControlBarMode,
    expanded: bool,
    /// Configured PWA origin, cached so hover/resize paths never touch
    /// disk. Used to classify home vs capsule tabs.
    app_base_url: String,
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
            app_base_url: crate::config::load_config().desktop.app_base_url,
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

/// Show and expand the icon bar. Historically this focused the omnibar
/// text input; the icon bar has no URL input (by design — arbitrary URL
/// navigation is not a user-facing affordance), so Cmd+L now just
/// surfaces the bar.
pub fn focus_control_bar_input(cx: &mut App) -> Result<AnyWindowHandle> {
    let handle = show_control_bar(cx)?;
    cx.global_mut::<ControlBarController>().force_expand();
    resize_bar_window(cx, true);
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    Ok(handle)
}

/// The bar window is a FIXED-size transparent strip; the visible pill
/// inside hugs its icons via GPUI layout. The native window is never
/// resized per tab count — external setFrame changes proved unreliable
/// to synchronize with GPUI's internal size, breaking hit-testing.
fn bar_size(_cx: &App, expanded: bool) -> (f32, f32) {
    if expanded {
        (BAR_WIDTH, BAR_HEIGHT)
    } else {
        (COMPACT_BAR_WIDTH, COMPACT_HEIGHT)
    }
}

/// Resize the bar from within one of its own event handlers.
///
/// macOS: the frame change is scheduled OUTSIDE the current GPUI update
/// (see `macos::resize_window_outside_update`) — a synchronous `setFrame`
/// here re-enters GPUI mid-borrow, drops the resize event, and leaves
/// hit-testing misaligned with what is on screen.
fn resize_bar_window_in_handler(window: &mut Window, cx: &App, expanded: bool) {
    let (new_w, new_h) = bar_size(cx, expanded);
    #[cfg(target_os = "macos")]
    if let Some(nswindow) = super::macos::ns_window_of(window) {
        super::macos::resize_window_outside_update(
            nswindow,
            cx.foreground_executor().clone(),
            new_w,
            new_h,
        );
    }
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
    let (new_w, new_h) = bar_size(cx, expanded);
    #[cfg(target_os = "macos")]
    if let Some(nswindow) = super::macos::ns_window_for(cx, handle) {
        super::macos::resize_window_outside_update(
            nswindow,
            cx.foreground_executor().clone(),
            new_w,
            new_h,
        );
    }
    #[cfg(target_os = "windows")]
    super::windows::resize_window_to(cx, handle, new_w, new_h);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = (new_w, new_h, handle);
}

pub struct ControlBarShellPlaceholder {
    locale: LocaleCode,
    /// Configured PWA origin, cached so the render pass never touches
    /// disk. Used to recognize the Ato Home window among open windows.
    app_base_url: String,
    /// Track which capsule handles are starred (pinned).
    starred_handles: HashSet<String>,
    /// Trackpad / mouse gesture recognizer (#174).
    gesture_state: GestureState,
}

impl ControlBarShellPlaceholder {
    pub fn new(_window: &mut gpui::Window, cx: &mut Context<Self>) -> Self {
        let config = crate::config::load_config();
        let locale = resolve_locale(config.general.language);

        cx.observe_global::<OpenContentWindows>(|_view, cx| {
            cx.notify();
        })
        .detach();
        cx.observe_global::<RemoteRunsSnapshot>(|_view, cx| {
            cx.notify();
        })
        .detach();
        cx.observe_global::<crate::launch_tracker::LaunchTrackerSnapshot>(|_view, cx| {
            cx.notify();
        })
        .detach();

        Self {
            locale,
            app_base_url: config.desktop.app_base_url.clone(),
            starred_handles: config.desktop.pinned_capsules.iter().cloned().collect(),
            gesture_state: GestureState::new(),
        }
    }

    /// Pin key for the frontmost managed capsule, if any. The icon bar
    /// has no URL input, so the frontmost capsule is the only candidate.
    fn current_pin_key(&self, cx: &App) -> Option<String> {
        let entry = cx.global::<OpenContentWindows>().frontmost()?;
        let capsule = entry.capsule.as_ref()?;
        Some(format!("capsule://{}", capsule.active_handle()))
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

impl Render for ControlBarShellPlaceholder {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let expanded = cx.global::<ControlBarController>().should_render_expanded();
        let tabs = shell_tabs::derive_shell_tabs(
            cx.global::<OpenContentWindows>(),
            &self.app_base_url,
            &cx.global::<RemoteRunsSnapshot>().runs,
            &cx.global::<crate::launch_tracker::LaunchTrackerSnapshot>()
                .launches,
        );

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
                    resize_bar_window_in_handler(window, cx, true);
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
                        resize_bar_window_in_handler(window, cx, true);
                    }
                    if let Some(shell) = cx.global::<ControlBarController>().shell.clone() {
                        shell.update(cx, |_shell, cx| cx.notify());
                    }
                } else {
                    let was_expanded = cx.global::<ControlBarController>().expanded;
                    cx.global_mut::<ControlBarController>().collapse();
                    if was_expanded && !cx.global::<ControlBarController>().expanded {
                        resize_bar_window_in_handler(window, cx, false);
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
                            let was_expanded = cx.global::<ControlBarController>().expanded;
                            cx.global_mut::<ControlBarController>().collapse();
                            if was_expanded && !cx.global::<ControlBarController>().expanded {
                                resize_bar_window_in_handler(window, cx, false);
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
                shell_icon_bar(tabs).into_any_element()
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

/// The expanded pill: [Ato icon] | [capsule tab icons…].
fn shell_icon_bar(tabs: Vec<ShellTab>) -> impl IntoElement {
    let mut capsule_tabs: Vec<ShellTab> = Vec::new();
    let mut home_tab: Option<ShellTab> = None;
    for tab in tabs {
        match tab.kind {
            ShellTabKind::AtoHome => home_tab = Some(tab),
            ShellTabKind::Capsule => capsule_tabs.push(tab),
        }
    }

    let mut bar = bar_surface_shape(
        div()
            .h(px(BAR_HEIGHT))
            .px(px(BAR_PAD_X))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(TAB_GAP))
            .bg(hsla(0.0, 0.0, 1.0, 0.92)),
        0.08,
    );
    if let Some(home) = home_tab {
        bar = bar.child(ato_home_button(home.is_active));
    }
    if !capsule_tabs.is_empty() {
        bar = bar.child(pill_separator());
        for tab in capsule_tabs {
            bar = bar.child(capsule_tab_button(tab));
        }
    }
    bar
}

/// Shared shape for every 40×40 icon slot in the pill: round hit target,
/// hover tint, active tint per the icon-bar design spec.
fn tab_slot(id: SharedString, is_active: bool) -> gpui::Stateful<gpui::Div> {
    let slot = div()
        .id(id)
        .relative()
        .w(px(TAB_BUTTON))
        .h(px(TAB_BUTTON))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(|s| s.bg(hsla(0.0, 0.0, 0.0, 0.05)));
    if is_active {
        slot.bg(hsla(0.63, 0.54, 0.51, 0.10))
    } else {
        slot
    }
}

/// The fixed leading Ato icon — opens/raises the Ato PWA Home surface.
/// Renders the brand PNG (black mark, transparent background) directly
/// via `img` — the GPUI svg path renders this particular asset inverted,
/// so the raster source is authoritative here. White borderless wrapper
/// keeps the slot aligned with the capsule avatars.
fn ato_home_button(is_active: bool) -> impl IntoElement {
    tab_slot(SharedString::from("shell-tab-ato-home"), is_active)
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            cx.stop_propagation();
            tracing::info!("shell icon bar: Ato tile clicked");
            window.dispatch_action(Box::new(ShowAtoHome), cx);
        })
        .child(
            div()
                .w(px(30.0))
                .h(px(30.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(0xffffff))
                .child(gpui::img("icons/ato.png").w(px(20.0)).h(px(20.0))),
        )
}

/// One open-capsule icon. Clicking raises that capsule's window.
/// No capsule icon asset is plumbed through the window registry yet, so
/// the avatar is the capsule's initial on a stable per-title tint;
/// status is a small dot badge (amber = starting, red = failed).
fn capsule_tab_button(tab: ShellTab) -> impl IntoElement {
    let slot_id = match (tab.window_id, tab.launch_id.as_deref(), tab.open_url.as_deref()) {
        (Some(window_id), _, _) => SharedString::from(format!("shell-tab-{window_id}")),
        // A launch-backed tab keeps its launch_id identity across the
        // starting → ready transition (even once it has an open_url).
        (None, Some(launch_id), _) => SharedString::from(format!("shell-tab-launch-{launch_id}")),
        (None, None, Some(url)) => SharedString::from(format!("shell-tab-remote-{url}")),
        (None, None, None) => return div().into_any_element(),
    };
    let hue = shell_tabs::avatar_hue(&tab.title);
    let dimmed = tab.status == ShellTabStatus::Starting;

    // Real Store icon when cached locally, else the letter avatar.
    let avatar = if let Some(icon_path) = tab.icon_path.clone() {
        div()
            .w(px(30.0))
            .h(px(30.0))
            .rounded_full()
            .overflow_hidden()
            .when(dimmed, |slot| slot.opacity(0.45))
            .child(gpui::img(icon_path).w(px(30.0)).h(px(30.0)))
            .into_any_element()
    } else {
        div()
            .w(px(30.0))
            .h(px(30.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(hsla(hue, 0.45, 0.55, if dimmed { 0.45 } else { 1.0 }))
            .text_color(rgb(0xffffff))
            .text_size(px(14.0))
            .font_weight(FontWeight(600.0))
            .child(tab.initial.clone())
            .into_any_element()
    };

    let badge_color = match tab.status {
        ShellTabStatus::Running => None,
        ShellTabStatus::Starting => Some(rgb(0xf59e0b)),
        ShellTabStatus::Error => Some(rgb(0xef4444)),
    };

    let window_id = tab.window_id;
    let open_url = tab.open_url.clone();
    let mut slot = tab_slot(slot_id, tab.is_active)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            if let Some(window_id) = window_id {
                window.dispatch_action(Box::new(FocusContentWindow { window_id }), cx);
            } else if let Some(url) = open_url.clone() {
                tracing::info!(url = %url, "shell icon bar: remote run tab clicked");
                window.dispatch_action(Box::new(NavigateToUrl { url }), cx);
            }
        })
        .child(avatar);

    if let Some(color) = badge_color {
        slot = slot.child(
            div()
                .absolute()
                .bottom(px(3.0))
                .right(px(3.0))
                .w(px(10.0))
                .h(px(10.0))
                .rounded_full()
                .border_1()
                .border_color(rgb(0xffffff))
                .bg(color),
        );
    }
    slot.into_any_element()
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
    let expanded = cx.global::<ControlBarController>().should_render_expanded();
    let (w, h) = bar_size(cx, expanded);
    (px(w), px(h))
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
    open_control_bar_inner(cx, bounds)
}

fn open_control_bar_inner(cx: &mut App, bounds: Bounds<Pixels>) -> Result<AnyWindowHandle> {
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
        let shell = cx.new(|cx| ControlBarShellPlaceholder::new(window, cx));
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
        // The bar is deliberately NOT an AppKit child of any content
        // window: its PopUp window level already keeps it above every
        // normal window, and a parent-child relationship caused a class
        // of bugs (level resets on attach, vanishing with a closing
        // parent, click-through activation raising the parent group
        // over freshly-focused windows).
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
        // Pin the bar to the always-on-top band so it stays visible in front
        // of the content windows it controls. No parent-child attachment —
        // the topmost band alone keeps it above content windows.
        super::windows::set_window_topmost(cx, *handle);
    }
    Ok(*handle)
}

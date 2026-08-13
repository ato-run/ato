//! Blocked-launch diagnostic placeholder — opened when a Shell Icon Bar
//! tab whose launch is `blocked` (consent / billing / capacity) is
//! clicked. Shows the capsule and the blocker kind so the state is never
//! an unexplained frozen "Starting" icon. A real resolution UI (consent
//! approval, checkout hand-off) is a later phase of the launch
//! unification plan.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, Context, FontWeight, IntoElement, MouseButton, Render,
    WindowBounds, WindowDecorations, WindowKind, WindowOptions, div, hsla, px, rgb, size,
};

/// Singleton slot so repeat clicks refresh instead of stacking popups.
#[derive(Default)]
pub struct LaunchBlockedPopupSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for LaunchBlockedPopupSlot {}

struct LaunchBlockedView {
    title: String,
    reason: String,
}

impl Render for LaunchBlockedView {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .bg(rgb(0xffffff))
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight(600.0))
                    .text_color(rgb(0x18181b))
                    .child(format!("{} is waiting", self.title)),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(0x52525b))
                    .child(format!("Blocked: {}", self.reason)),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(0x9ca3af))
                    .child("Resolve it from Ato Home, then run again."),
            )
            .child(
                div()
                    .id("launch-blocked-close")
                    .mt(px(4.0))
                    .px(px(16.0))
                    .py(px(6.0))
                    .rounded(px(8.0))
                    .bg(rgb(0xf4f4f5))
                    .border_1()
                    .border_color(hsla(0.0, 0.0, 0.0, 0.08))
                    .text_size(px(12.0))
                    .text_color(rgb(0x18181b))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(0xe4e4e7)))
                    .on_mouse_down(MouseButton::Left, |_, _window, cx| {
                        cx.stop_propagation();
                        close_launch_blocked_popup(cx);
                    })
                    .child("Close"),
            )
    }
}

fn close_launch_blocked_popup(cx: &mut App) {
    let Some(handle) = cx.global::<LaunchBlockedPopupSlot>().0 else {
        return;
    };
    cx.set_global(LaunchBlockedPopupSlot(None));
    let _ = handle.update(cx, |_, window, _| window.remove_window());
}

/// Open (or replace) the blocked-launch placeholder popup.
pub fn open_launch_blocked_popup(cx: &mut App, title: String, reason: String) -> Result<()> {
    close_launch_blocked_popup(cx);
    let win_size = size(px(340.0), px(160.0));
    let bounds = Bounds::centered(None, win_size, cx);
    let options = WindowOptions {
        titlebar: None,
        focus: true,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: true,
        is_resizable: false,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    let handle = cx.open_window(options, move |window, cx| {
        window.set_window_title(crate::window::WINDOW_TITLE);
        let view = cx.new(|_cx| LaunchBlockedView { title, reason });
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    })?;
    cx.set_global(LaunchBlockedPopupSlot(Some(*handle)));
    Ok(())
}

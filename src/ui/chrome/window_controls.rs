use gpui::prelude::*;
use gpui::{div, px, rgb, Hsla, IntoElement};

#[cfg(not(target_os = "macos"))]
use gpui::{point, BoxShadow, Div, WindowControlArea};

const MACOS_TRAFFIC_LIGHT_SPACER_WIDTH: f32 = 56.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WindowControlIntent {
    Close,
    Minimize,
    ToggleMaximize,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct WindowControlButtonSpec {
    pub intent: WindowControlIntent,
    pub color: Hsla,
}

pub(super) fn default_window_control_buttons() -> [WindowControlButtonSpec; 3] {
    [
        WindowControlButtonSpec {
            intent: WindowControlIntent::Close,
            color: rgb(0xff5f57).into(),
        },
        WindowControlButtonSpec {
            intent: WindowControlIntent::Minimize,
            color: rgb(0xfebc2e).into(),
        },
        WindowControlButtonSpec {
            intent: WindowControlIntent::ToggleMaximize,
            color: rgb(0x28c840).into(),
        },
    ]
}

pub(super) fn render_window_controls(
    #[cfg(not(target_os = "macos"))] buttons: impl IntoIterator<Item = WindowControlButtonSpec>,
    #[cfg(target_os = "macos")] _buttons: impl IntoIterator<Item = WindowControlButtonSpec>,
) -> impl IntoElement {
    #[cfg(target_os = "macos")]
    {
        return div()
            .w(px(MACOS_TRAFFIC_LIGHT_SPACER_WIDTH))
            .h(px(12.0))
            .flex_none();
    }

    #[cfg(not(target_os = "macos"))]
    div()
        .flex()
        .items_center()
        .gap_2()
        .children(buttons.into_iter().map(render_window_control_button))
}

#[cfg(not(target_os = "macos"))]
fn render_window_control_button(spec: WindowControlButtonSpec) -> impl IntoElement {
    // Keep the control as a plain Div so GPUI can register it as a native titlebar hit target.
    traffic_light(spec)
}

#[cfg(not(target_os = "macos"))]
fn traffic_light(spec: WindowControlButtonSpec) -> Div {
    div()
        .size_3()
        .rounded_full()
        .bg(spec.color)
        .shadow(vec![BoxShadow {
            color: spec.color.opacity(0.4),
            offset: point(px(0.), px(1.)),
            blur_radius: px(6.),
            spread_radius: px(0.),
        }])
        .window_control_area(window_control_area(spec.intent))
}

#[cfg(not(target_os = "macos"))]
fn window_control_area(intent: WindowControlIntent) -> WindowControlArea {
    match intent {
        WindowControlIntent::Close => WindowControlArea::Close,
        WindowControlIntent::Minimize => WindowControlArea::Min,
        WindowControlIntent::ToggleMaximize => WindowControlArea::Max,
    }
}

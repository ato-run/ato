//! Chrome window-control row (traffic lights).
//!
//! On macOS GPUI delegates to the native window-server proxy, so we
//! reserve a fixed-width spacer and let the OS draw the actual
//! buttons. On Linux / Windows we render the buttons ourselves and
//! tag each hitbox with `WindowControlArea::{Close,Min,Max}` so the
//! platform handles the click semantics (drag, double-click, etc.) —
//! only the visual treatment lives in Rust.
//!
//! ## gpui-html origin
//!
//! The visual layout (`flex items-center gap-2` row of three
//! `size-3 rounded-full` dots at `/80` opacity) was lowered from
//! `.tmp/gpui-html/titlebar.html` via `gpui-html compile --manifest`.
//! The generated chain is preserved in
//! `.tmp/gpui-html/titlebar.generated.rs` for reviewers; this file
//! replaces the inline `gpui::rgba(0xff5f57cc)` literals with
//! `theme.traffic_rose.opacity(0.8)` style lookups and adds the
//! hover transition the mockup expresses as `hover:bg-rose` (not in
//! gpui-html's static-lowering scope).

use gpui::prelude::*;
use gpui::{div, px, IntoElement};

#[cfg(not(target_os = "macos"))]
use gpui::{point, BoxShadow, Div, Hsla, Stateful, WindowControlArea};

use super::super::theme::Theme;

const MACOS_TRAFFIC_LIGHT_SPACER_WIDTH: f32 = 80.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WindowControlIntent {
    Close,
    Minimize,
    ToggleMaximize,
}

/// One traffic-light slot. `intent` drives platform hit-test
/// registration (Close / Min / Max). The color is resolved from
/// `Theme` at render time rather than baked in, so a dark/light
/// theme swap flows through without touching this module.
#[derive(Clone, Copy, Debug)]
pub(super) struct WindowControlButtonSpec {
    pub intent: WindowControlIntent,
}

pub(super) fn default_window_control_buttons() -> [WindowControlButtonSpec; 3] {
    [
        WindowControlButtonSpec {
            intent: WindowControlIntent::Close,
        },
        WindowControlButtonSpec {
            intent: WindowControlIntent::Minimize,
        },
        WindowControlButtonSpec {
            intent: WindowControlIntent::ToggleMaximize,
        },
    ]
}

pub(super) fn render_window_controls(
    #[cfg(not(target_os = "macos"))] buttons: impl IntoIterator<Item = WindowControlButtonSpec>,
    #[cfg(target_os = "macos")] _buttons: impl IntoIterator<Item = WindowControlButtonSpec>,
    #[cfg(not(target_os = "macos"))] theme: &Theme,
    #[cfg(target_os = "macos")] _theme: &Theme,
) -> impl IntoElement {
    #[cfg(target_os = "macos")]
    {
        return div()
            .w(px(MACOS_TRAFFIC_LIGHT_SPACER_WIDTH))
            .h(px(12.0))
            .flex_none();
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Layout chain mirrors the gpui-html lowering of
        //   <div class="flex items-center gap-2"> …three dots… </div>
        // (see .tmp/gpui-html/titlebar.generated.rs). The generated
        // packed-alpha literals are replaced with theme lookups +
        // hover state.
        let rose = theme.traffic_rose;
        let amber = theme.traffic_amber;
        let green = theme.traffic_green;
        div().flex().items_center().gap_2().children(
            buttons
                .into_iter()
                .map(|spec| render_window_control_button(spec, rose, amber, green)),
        )
    }
}

#[cfg(not(target_os = "macos"))]
fn render_window_control_button(
    spec: WindowControlButtonSpec,
    rose: Hsla,
    amber: Hsla,
    green: Hsla,
) -> Stateful<Div> {
    let (color, id) = match spec.intent {
        WindowControlIntent::Close => (rose, "traffic-light-close"),
        WindowControlIntent::Minimize => (amber, "traffic-light-min"),
        WindowControlIntent::ToggleMaximize => (green, "traffic-light-max"),
    };
    // Mockup: `bg-<token>/80 hover:bg-<token> transition-colors`.
    // gpui-html lowers `/80` to a packed-alpha literal; here we
    // express it as `color.opacity(0.8)` so the runtime theme owns
    // the final hex and hover reuses the same `Hsla` at full alpha.
    div()
        .id(id)
        .size_3()
        .rounded_full()
        .bg(color.opacity(0.8))
        .hover(move |s| s.bg(color))
        .shadow(vec![BoxShadow {
            color: color.opacity(0.4),
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

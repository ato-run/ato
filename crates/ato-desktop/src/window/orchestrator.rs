//! Layer 2 scaffolding — spawn a new top-level GPUI window per
//! `AppWindow`. The current iteration installs a placeholder view; the
//! Control Bar window (#171) and the per-window WebViewManager
//! migration (also scoped under #169 / follow-up commits) plug in here.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Bounds, Context, IntoElement, Render, SharedString, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;

use crate::state::GuestRoute;

/// Minimal placeholder shell installed in each spawned `AppWindow`.
/// Subsequent layers (#171 Control Bar window, #172 Control Bar UI,
/// #173 Card Switcher) replace the body with real content; WebView
/// attachment lands with the `WebViewManager` per-window singleton
/// migration on this branch.
pub struct AppWindowShell {
    route_label: SharedString,
}

impl AppWindowShell {
    pub fn new(route: &GuestRoute) -> Self {
        Self {
            route_label: SharedString::from(route.label()),
        }
    }
}

impl Render for AppWindowShell {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x111113))
            .text_color(rgb(0xd4d4d8))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .child("App window placeholder (Focus View redesign — #169)"),
            )
            .child(
                div()
                    .text_sm()
                    .opacity(0.7)
                    .child(self.route_label.clone()),
            )
    }
}

/// Open a new top-level GPUI window hosting the placeholder
/// `AppWindowShell` for the given guest route. Returns once the window
/// is realised; the GPUI `WindowHandle` is intentionally discarded for
/// now because the orchestrator registry that tracks (`AppWindowId` →
/// handle) lands with the close-lifecycle commit (also under #169).
///
/// The legacy single-window path in `app::run` is left untouched. This
/// helper is invoked from the experimental `OpenAppWindowExperiment`
/// action gated on `ATO_DESKTOP_MULTI_WINDOW=1`.
pub fn open_app_window(cx: &mut App, route: GuestRoute) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(900.0), px(680.0)), cx);
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    cx.open_window(options, move |window, cx| {
        let shell = cx.new(|_cx| AppWindowShell::new(&route));
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    Ok(())
}

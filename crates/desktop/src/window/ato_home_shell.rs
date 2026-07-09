//! AtoHomeShell — the dedicated window hosting the `ato-pwa` Home.
//!
//! NOT the web-viewer: the window hosts a single full-window
//! [`WebAppView`] pointed at the configured PWA origin — no tab strip,
//! no URL bar, no browser chrome in any build. The PWA is the Ato
//! control surface (login, Discover, Run, runner settings), so its
//! window is a first-class surface, not a wrapped browser tab.
//!
//! App launches initiated inside the PWA do NOT embed in this window:
//! `WebAppView` cancels non-web navigations (`capsule://…`, `ato://…`)
//! and re-dispatches them as `NavigateToUrl`, which routes through the
//! normal launch flow and opens an independent capsule AppWindow —
//! which in turn appears as its own icon in the Shell Icon Bar.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, WindowBounds, WindowDecorations, WindowOptions, px, size,
};
use gpui_component::TitleBar;
use url::Url;

use crate::window::web_app_view::WebAppView;

/// Tracks the singleton Ato Home window so repeat opens focus the
/// existing window instead of stacking duplicates.
#[derive(Default)]
pub struct AtoHomeWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for AtoHomeWindowSlot {}

/// Open (or focus) the dedicated Ato Home window on `url`.
pub fn open_ato_home_window(cx: &mut App, url: Url) -> Result<AnyWindowHandle> {
    let existing = cx.global::<AtoHomeWindowSlot>().0;
    if let Some(handle) = existing {
        match handle.update(cx, |_, window, _| window.activate_window()) {
            Ok(()) => return Ok(handle),
            Err(_) => cx.set_global(AtoHomeWindowSlot(None)),
        }
    }

    let win_size = size(px(1100.0), px(760.0));
    let bounds = match cx.primary_display() {
        Some(d) => {
            let db = d.bounds();
            let left = db.origin.x + (db.size.width - win_size.width) / 2.0;
            let top = db.origin.y + px(108.0);
            Bounds {
                origin: gpui::point(left, top),
                size: win_size,
            }
        }
        None => Bounds::centered(None, win_size, cx),
    };
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let handle = cx.open_window(options, move |window, cx| {
        window.set_window_title(crate::window::WINDOW_TITLE);
        let shell = cx.new(|cx| WebAppView::new("Ato Home window", url.clone(), window, cx));
        window.focus(&shell.read(cx).paste.focus_handle.clone(), cx);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    cx.set_global(AtoHomeWindowSlot(Some(*handle)));
    use crate::window::content_windows::{
        ContentWindowEntry, ContentWindowKind, OpenContentWindows,
    };
    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::Home,
            title: gpui::SharedString::from("Ato"),
            subtitle: gpui::SharedString::from("Home"),
            url: gpui::SharedString::from("capsule://desktop.ato.run/home"),
            capsule: None,
            last_focused_at: std::time::Instant::now(),
        },
    );
    Ok(*handle)
}

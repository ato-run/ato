//! Layer 3+4 — floating Control Bar window with the four affordances
//! (Settings · URL pill · Card Switcher · Store) rendered as
//! light-themed rounded pills, matching the redesign reference
//! mockups.
//!
//! The window itself is borderless and transparent so the pill bar
//! reads as a free-floating element. The actual
//! `addChildWindow:ordered:` parent-child attachment that locks the
//! bar to its parent app window is still TBD on this branch (see
//! module-level comment on the lower-level scaffolding in #171).

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, point, px, rgb, size, App, Bounds, BoxShadow, Context, FontWeight, IntoElement,
    MouseButton, Render, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind,
    WindowOptions,
};
use gpui_component::{Icon, IconName};

use crate::app::{OpenCardSwitcher, OpenLauncherWindow, ShowSettings};

const BAR_WIDTH: f32 = 840.0;
const BAR_HEIGHT: f32 = 60.0;
const WINDOW_PAD: f32 = 16.0;

/// Control Bar contents — four affordances laid out per the
/// redesign mockup:
///
/// `[ ⚙ 設定 ]   [ 🌐 ato.app/shell://… ▾ ]   [ ▦ ]   [ 📦 ストア ]`
///
/// The Card Switcher pill is rendered enabled (#173 scaffolding
/// reachable). Settings and Store dispatch to existing chrome
/// actions; the URL pill is presentation-only until the per-window
/// WebView migration lands.
pub struct ControlBarShellPlaceholder;

impl Render for ControlBarShellPlaceholder {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Outer transparent wrapper so the pill bar gets a true
        // floating look (the window background appearance is also
        // Transparent).
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(bar_pill())
    }
}

fn bar_pill() -> impl IntoElement {
    div()
        .w(px(BAR_WIDTH))
        .h(px(BAR_HEIGHT))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap_2()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .rounded(px(BAR_HEIGHT / 2.0))
        .shadow(vec![BoxShadow {
            color: hsla(0.0, 0.0, 0.0, 0.10),
            offset: point(px(0.0), px(8.0)),
            blur_radius: px(24.0),
            spread_radius: px(0.0),
        }])
        .child(pill_button(
            "settings",
            Some(IconName::Settings),
            Some("設定"),
            ActionTarget::Settings,
        ))
        .child(url_pill())
        .child(pill_button(
            "card-switcher",
            Some(IconName::GalleryVerticalEnd),
            None,
            ActionTarget::CardSwitcher,
        ))
        .child(pill_button(
            "store",
            Some(IconName::Inbox),
            Some("ストア"),
            ActionTarget::Store,
        ))
}

#[derive(Copy, Clone)]
enum ActionTarget {
    Settings,
    Store,
    CardSwitcher,
}

fn pill_button(
    id: &'static str,
    icon: Option<IconName>,
    label: Option<&'static str>,
    target: ActionTarget,
) -> impl IntoElement {
    let mut body = div()
        .id(id)
        .h(px(40.0))
        .px(px(if label.is_some() { 14.0 } else { 12.0 }))
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(20.0))
        .bg(rgb(0xfafafa))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .text_color(rgb(0x18181b))
        .text_sm()
        .font_weight(FontWeight(500.0))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(0xf4f4f5)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| match target {
            ActionTarget::Settings => {
                // Settings opens the Launcher (#170) with the settings
                // tab focused; the per-tab dispatch lands with the
                // DesktopShell → LauncherShell rename. For now we just
                // open the Launcher window.
                window.dispatch_action(Box::new(OpenLauncherWindow), cx);
                window.dispatch_action(Box::new(ShowSettings), cx);
            }
            ActionTarget::Store => {
                window.dispatch_action(Box::new(OpenLauncherWindow), cx);
            }
            ActionTarget::CardSwitcher => {
                window.dispatch_action(Box::new(OpenCardSwitcher), cx);
            }
        });
    if let Some(icon) = icon {
        body = body.child(Icon::new(icon).size(px(16.0)));
    }
    if let Some(label) = label {
        body = body.child(div().child(label));
    }
    body
}

fn url_pill() -> impl IntoElement {
    div()
        .id("url-pill")
        .flex_1()
        .h(px(40.0))
        .px(px(14.0))
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(20.0))
        .bg(rgb(0xfafafa))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .text_color(rgb(0x18181b))
        .text_sm()
        .child(Icon::new(IconName::Globe).size(px(16.0)))
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .child("ato.app/shell://wasedap2p"),
        )
        .child(Icon::new(IconName::ChevronDown).size(px(14.0)))
}

/// Open a borderless transparent Control Bar window centred on the
/// screen. Sized to fit `BAR_WIDTH + 2 * WINDOW_PAD` × `BAR_HEIGHT + 2
/// * WINDOW_PAD` so the drop shadow has room to render. Once
/// `addChildWindow:` plumbing lands the orchestrator will reposition
/// the bar to anchor at the parent app window's top-center.
pub fn open_control_bar_window(cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(
        None,
        size(
            px(BAR_WIDTH + 2.0 * WINDOW_PAD),
            px(BAR_HEIGHT + 2.0 * WINDOW_PAD),
        ),
        cx,
    );
    let options = WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: true,
        is_resizable: false,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let shell = cx.new(|_cx| ControlBarShellPlaceholder);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    Ok(())
}

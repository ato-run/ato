//! Quit prompt — the Windows landing surface when the last content
//! window closes.
//!
//! macOS keeps the Shell Icon Bar as the bar-only landing state, but on
//! Windows the bar is a taskbar-invisible toolwindow, so a bar-only
//! state would leave the process unreachable. Instead this small dialog
//! asks "Quit Ato?" — Quit shuts the app down; Reopen (or closing the
//! dialog) opens the PWA Home window again. No ato-start involved.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, Context, FontWeight, IntoElement, MouseButton, Render,
    WindowBounds, WindowDecorations, WindowKind, WindowOptions, div, hsla, px, rgb, size,
};

use crate::localization::{LocaleCode, resolve_locale, tr};

/// Tracks the open quit prompt so the close observer can tell "the
/// prompt itself closed" (= treat as Reopen) apart from a content
/// window closing.
#[derive(Default)]
pub struct QuitPromptWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for QuitPromptWindowSlot {}

struct QuitPromptView {
    locale: LocaleCode,
}

impl Render for QuitPromptView {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let locale = self.locale;
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(20.0))
            .bg(rgb(0xffffff))
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(FontWeight(600.0))
                    .text_color(rgb(0x18181b))
                    .child(tr(locale, "quit_prompt.title")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(prompt_button(
                        "quit-prompt-reopen",
                        tr(locale, "quit_prompt.reopen"),
                        true,
                        |_, cx| {
                            close_quit_prompt(cx);
                            if let Err(err) = crate::window::home::open_home_window(cx) {
                                tracing::error!(error = %err, "quit prompt: reopen Home failed");
                            }
                        },
                    ))
                    .child(prompt_button(
                        "quit-prompt-quit",
                        tr(locale, "quit_prompt.quit"),
                        false,
                        |_, cx| {
                            crate::window::begin_shutdown();
                            cx.quit();
                        },
                    )),
            )
    }
}

fn prompt_button(
    id: &'static str,
    label: String,
    primary: bool,
    on_click: impl Fn(&mut gpui::Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px(px(18.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .text_size(px(13.0))
        .cursor_pointer()
        .when(primary, |button| {
            button
                .bg(rgb(0x18181b))
                .text_color(rgb(0xffffff))
                .hover(|s| s.bg(rgb(0x3f3f46)))
        })
        .when(!primary, |button| {
            button
                .bg(rgb(0xf4f4f5))
                .text_color(rgb(0x18181b))
                .border_1()
                .border_color(hsla(0.0, 0.0, 0.0, 0.08))
                .hover(|s| s.bg(rgb(0xe4e4e7)))
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx);
        })
        .child(label)
}

fn close_quit_prompt(cx: &mut App) {
    let Some(handle) = cx.global::<QuitPromptWindowSlot>().0 else {
        return;
    };
    cx.set_global(QuitPromptWindowSlot(None));
    let _ = handle.update(cx, |_, window, _| window.remove_window());
}

/// Open (or focus) the quit prompt.
pub fn open_quit_prompt_window(cx: &mut App) -> Result<AnyWindowHandle> {
    if let Some(existing) = cx.global::<QuitPromptWindowSlot>().0 {
        if existing
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return Ok(existing);
        }
        cx.set_global(QuitPromptWindowSlot(None));
    }
    let locale = resolve_locale(crate::config::load_config().general.language);
    let win_size = size(px(360.0), px(150.0));
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
        let view = cx.new(|_cx| QuitPromptView { locale });
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    })?;
    cx.set_global(QuitPromptWindowSlot(Some(*handle)));
    Ok(*handle)
}

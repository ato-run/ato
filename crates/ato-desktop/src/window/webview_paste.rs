//! Shared copy/paste delegation for all Wry-WebView–hosted windows.
//!
//! # Usage
//!
//! 1. Add `paste: WebViewPasteSupport` to your shell struct.
//! 2. Implement `WebViewPasteShell` for your shell type.
//! 3. Use `impl_focusable_via_paste!(YourShell, paste)` to generate `Focusable`.
//! 4. In `render()`, wrap the root element with `paste_render_wrap!(element, cx, &self.paste.focus_handle)`.
//! 5. After creating the entity, call `window.focus(&entity.read(cx).paste.focus_handle.clone(), cx)`.
//!
//! # Why this is necessary
//!
//! macOS GPUI intercepts Cmd+V before WKWebView sees it (WKWebView is an OS-native
//! first responder, but GPUI key bindings take priority). The global no-op handlers
//! in `app.rs` swallow the action if no component-level handler fires.
//!
//! The fix: shell has `FocusHandle` + `Focusable`; render attaches the "WebViewShell"
//! key context with 4 `on_action` handlers; the paste handler calls `webview.focus()`
//! (gives WKWebView macOS first-responder) then injects the paste script.

use gpui::{Context, FocusHandle, Window};
use wry::WebView;

#[cfg(target_os = "macos")]
use wry::WebViewExtMacOS;

use crate::app::{NativeCopy, NativeCut, NativePaste, NativeSelectAll};

/// The GPUI key-context string shared by every WebView-hosting shell.
/// A single set of 4 key bindings in `app.rs` covers all shells.
pub const PASTE_CTX: &str = "WebViewShell";

/// Per-shell focus handle. Add this as a field named `paste` in your shell struct.
pub struct WebViewPasteSupport {
    pub focus_handle: FocusHandle,
}

impl WebViewPasteSupport {
    pub fn new<T: 'static>(cx: &mut Context<T>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}

/// Implement this trait on any shell that hosts a Wry WebView.
/// The four action handlers have working default implementations; only
/// `active_paste_target` is required.
pub trait WebViewPasteShell: Sized + 'static {
    /// Return a reference to the WebView to forward keyboard actions to.
    /// Return `None` during the window's boot phase (the webview may not
    /// exist yet); the action will silently no-op.
    fn active_paste_target(&self) -> Option<&WebView>;

    fn on_native_paste(&mut self, _: &NativePaste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = cx.read_from_clipboard() {
            if let Some(text) = item.text() {
                if let Some(webview) = self.active_paste_target() {
                    // Give WKWebView macOS first-responder so document.activeElement
                    // is accurate by the time the script runs.
                    #[cfg(target_os = "macos")]
                    let _ = webview.focus();
                    let script = paste_script(&text);
                    let _ = webview.evaluate_script(&script);
                }
            }
        }
    }

    fn on_native_copy(&mut self, _: &NativeCopy, _: &mut Window, _cx: &mut Context<Self>) {
        if let Some(webview) = self.active_paste_target() {
            #[cfg(target_os = "macos")]
            let _ = webview.focus();
            let _ = webview.evaluate_script("document.execCommand('copy')");
        }
    }

    fn on_native_cut(&mut self, _: &NativeCut, _: &mut Window, _cx: &mut Context<Self>) {
        if let Some(webview) = self.active_paste_target() {
            #[cfg(target_os = "macos")]
            let _ = webview.focus();
            let _ = webview.evaluate_script("document.execCommand('cut')");
        }
    }

    fn on_native_select_all(
        &mut self,
        _: &NativeSelectAll,
        _: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if let Some(webview) = self.active_paste_target() {
            let _ = webview.evaluate_script("document.execCommand('selectAll')");
        }
    }
}

/// Build the JS paste script that targets the last focused input element.
///
/// Falls back to `window.__ato_last_focused` when `document.activeElement`
/// has been reset to `<body>` by the time the deferred GPUI action fires.
/// HTML pages must install the `__ato_last_focused` tracker (via
/// `with_initialization_script` or inline `<script>`) to benefit from
/// the fallback.
pub fn paste_script(text: &str) -> String {
    let text_json = serde_json::to_string(text).expect("clipboard text should serialize");
    format!(
        r#"(() => {{
  const text = {text};
  const active = (document.activeElement && document.activeElement !== document.body)
    ? document.activeElement
    : (window.__ato_last_focused || null);
  const isTextInput = active && (
    active.tagName === 'TEXTAREA' ||
    (active.tagName === 'INPUT' && !['button','checkbox','color','file','hidden','image','radio','range','reset','submit'].includes((active.type || '').toLowerCase()))
  );
  if (!isTextInput || active.readOnly || active.disabled) {{ return; }}
  active.focus();
  const start = active.selectionStart ?? active.value.length;
  const end = active.selectionEnd ?? start;
  active.setRangeText(text, start, end, 'end');
  active.dispatchEvent(new InputEvent('input', {{ bubbles: true, inputType: 'insertText', data: text }}));
}})();"#,
        text = text_json,
    )
}

/// Generates a `Focusable` impl for a shell struct that holds a
/// `WebViewPasteSupport` field.
///
/// ```ignore
/// impl_focusable_via_paste!(MyShell, paste);
/// // expands to:
/// impl gpui::Focusable for MyShell {
///     fn focus_handle(&self, _cx: &gpui::App) -> gpui::FocusHandle {
///         self.paste.focus_handle.clone()
///     }
/// }
/// ```
#[macro_export]
macro_rules! impl_focusable_via_paste {
    ($T:ty, $field:ident) => {
        impl gpui::Focusable for $T {
            fn focus_handle(&self, _cx: &gpui::App) -> gpui::FocusHandle {
                self.$field.focus_handle.clone()
            }
        }
    };
}

/// Attaches the `"WebViewShell"` key context, focus tracking, and the four
/// copy/paste action handlers to a GPUI element expression.
///
/// ```ignore
/// paste_render_wrap!(div().size_full().bg(rgb(0xffffff)), cx, &self.paste.focus_handle)
/// // → div().size_full().bg(rgb(0xffffff))
/// //       .key_context("WebViewShell")
/// //       .track_focus(&self.paste.focus_handle)
/// //       .on_action(cx.listener(Self::on_native_paste))
/// //       ...
/// ```
#[macro_export]
macro_rules! paste_render_wrap {
    ($element:expr, $cx:expr, $focus:expr) => {
        $element
            .key_context($crate::window::webview_paste::PASTE_CTX)
            .track_focus($focus)
            .on_action($cx.listener(Self::on_native_paste))
            .on_action($cx.listener(Self::on_native_copy))
            .on_action($cx.listener(Self::on_native_cut))
            .on_action($cx.listener(Self::on_native_select_all))
    };
}

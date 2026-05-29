//! Windows-only window management primitives.
//!
//! Provides the same conceptual surface as `window/macos.rs` using Win32
//! APIs. All functions are no-ops (log + return) on hardware that does not
//! support the requested feature (e.g. `round_window_corners` silently
//! succeeds on Windows 10 where DWM rounded corners are not available).
//!
//! The path from `gpui::Window` to an `HWND` goes through the
//! `raw_window_handle` trait that GPUI implements on `Window`: the Win32
//! variant gives us the `hwnd` field as a `NonZero<isize>`.

#![cfg(target_os = "windows")]

use gpui::{AnyWindowHandle, App, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::Value;
use std::sync::mpsc::Sender;
use tracing::warn;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_HWNDPARENT,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_HIDE, SW_SHOW,
};

// ── HWND helpers ──────────────────────────────────────────────────────────────

fn hwnd_for_window(window: &Window) -> Option<HWND> {
    let rwh = match window.window_handle() {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, "window.window_handle() failed");
            return None;
        }
    };
    match rwh.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as HWND),
        other => {
            warn!(handle = ?other, "raw window handle is not Win32");
            None
        }
    }
}

fn hwnd_for(cx: &mut App, handle: AnyWindowHandle) -> Option<HWND> {
    handle
        .update(cx, |_view, window, _cx| hwnd_for_window(window))
        .ok()
        .flatten()
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Apply DWM rounded corners to the window (Windows 11+).
///
/// Uses `DwmSetWindowAttribute` with `DWMWA_WINDOW_CORNER_PREFERENCE = 33`
/// and value `DWMWCP_ROUND = 2`. Silently ignores failures on Windows 10
/// where this attribute is not supported.
pub fn round_window_corners(cx: &mut App, handle: AnyWindowHandle, _radius: f64) {
    use std::ffi::c_void;
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;

    let Some(hwnd) = hwnd_for(cx, handle) else {
        return;
    };

    // DWMWA_WINDOW_CORNER_PREFERENCE = 33 (Windows 11 SDK, stable value)
    // DWMWCP_ROUND = 2
    let corner_pref: u32 = 2;
    unsafe {
        // Ignore return value — not supported on Windows 10 and earlier.
        let _ = DwmSetWindowAttribute(
            hwnd,
            33,
            &corner_pref as *const u32 as *const c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

/// Resize the window associated with `handle` to `new_w × new_h` logical
/// pixels, keeping the current position. Call from outside a window event
/// handler (use [`resize_window_in_handler`] when you already hold
/// `&mut Window`).
pub fn resize_window_to(cx: &mut App, handle: AnyWindowHandle, new_w: f32, new_h: f32) {
    let result = handle.update(cx, |_view, window, _cx| {
        resize_window_in_handler(window, new_w, new_h);
    });
    if let Err(err) = result {
        warn!(error = ?err, "resize_window_to: handle update failed");
    }
}

/// Resize a window directly from a window event handler where
/// `handle.update()` on the same window would deadlock.
pub fn resize_window_in_handler(window: &mut Window, new_w: f32, new_h: f32) {
    let Some(hwnd) = hwnd_for_window(window) else {
        return;
    };
    unsafe {
        SetWindowPos(
            hwnd,
            0, // ignored when SWP_NOZORDER
            0, // ignored when SWP_NOMOVE
            0,
            new_w as i32,
            new_h as i32,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Hide the window inside a `window.on_window_should_close` handler.
pub fn hide_window_in_handler(window: &mut Window) {
    let Some(hwnd) = hwnd_for_window(window) else {
        return;
    };
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
}

/// Hide the window identified by `handle`.
pub fn hide_win_window(cx: &mut App, handle: AnyWindowHandle) {
    if let Some(hwnd) = hwnd_for(cx, handle) {
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Show (unhide) the window identified by `handle` and bring it to the
/// foreground.
pub fn show_win_window(cx: &mut App, handle: AnyWindowHandle) {
    if let Some(hwnd) = hwnd_for(cx, handle) {
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
        }
    }
}

/// Make `child` a logical child of `parent` by setting `GWLP_HWNDPARENT`.
///
/// Uses `SetWindowLongPtrW(child, GWLP_HWNDPARENT, parent_hwnd)` rather than
/// `SetParent` to avoid triggering a full WS_CHILD restyle. The child window
/// will be minimised/restored together with the parent.
///
/// Returns `Ok(())` on success; logs and returns `Err(String)` on failure so
/// the caller can decide whether to surface or ignore it.
pub fn attach_as_child(
    cx: &mut App,
    parent: AnyWindowHandle,
    child: AnyWindowHandle,
) -> Result<(), String> {
    let parent_hwnd =
        hwnd_for(cx, parent).ok_or_else(|| "parent HWND unavailable".to_string())?;
    let child_hwnd =
        hwnd_for(cx, child).ok_or_else(|| "child HWND unavailable".to_string())?;

    unsafe {
        SetWindowLongPtrW(child_hwnd, GWLP_HWNDPARENT, parent_hwnd as isize);
    }
    tracing::info!("SetWindowLongPtrW(GWLP_HWNDPARENT) attached child window to parent");
    Ok(())
}

/// Dispatch an asynchronous WebView2 screenshot for the window that
/// backs `handle`. Delegates to [`crate::automation::screenshot::take_screenshot`].
pub fn request_win_window_snapshot(
    cx: &mut App,
    handle: AnyWindowHandle,
    tx: Sender<Option<String>>,
) {
    // On Windows the screenshot is taken from the WebView2 handle, which is
    // owned by the AppCapsuleShell, not retrieved here via HWND. This function
    // sends `None` because the caller-side fallback already tries the
    // automation MCP screenshot path. A full HWND-based BitBlt implementation
    // can be added here when needed.
    let _ = cx;
    let _ = handle;
    let _ = tx.send(None);
}

/// Take a screenshot from a `wry::WebView` handle directly.
/// Delegates to the WebView2 CapturePreview implementation.
pub fn take_webview_screenshot(webview: &wry::WebView, tx: Sender<Result<Value, String>>) {
    crate::automation::screenshot::take_screenshot(webview, tx);
}

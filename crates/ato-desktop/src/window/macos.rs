//! macOS-only plumbing — `addChildWindow:ordered:NSWindowAbove`
//! glues the Control Bar window to its AppWindow so the OS handles
//! co-movement (drag, resize, Space migration, fullscreen toggling)
//! automatically. Spike #168 verifies the AppKit contract.
//!
//! The path from `gpui::Window` down to `NSWindow` goes through the
//! `raw_window_handle` trait that GPUI implements on `Window`: the
//! AppKit variant gives us a raw `*mut c_void` for the NSView, which
//! we cast to a typed `&NSView` and walk up to its containing
//! `NSWindow` via the standard `view.window()` method (objc2-app-kit
//! generates this as a safe, retained accessor).

#![cfg(target_os = "macos")]

use gpui::{AnyWindowHandle, App};
use objc2::rc::Retained;
use objc2_app_kit::{NSFloatingWindowLevel, NSView, NSWindow, NSWindowOrderingMode};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tracing::warn;

/// Walk from a `gpui::WindowHandle` to its underlying `NSWindow`.
/// Returns `None` if the handle is unknown, the platform window
/// reports a non-AppKit raw handle, or the NSView has no parent
/// window yet (which can happen before the first frame paints).
pub fn ns_window_for(cx: &mut App, handle: AnyWindowHandle) -> Option<Retained<NSWindow>> {
    handle
        .update(cx, |_view, window, _cx| {
            let rwh = match window.window_handle() {
                Ok(h) => h,
                Err(e) => {
                    warn!(error = %e, "window.window_handle() failed");
                    return None;
                }
            };
            match rwh.as_raw() {
                RawWindowHandle::AppKit(h) => {
                    // SAFETY: `ns_view` is documented as a valid
                    // pointer to an `NSView` owned by the platform
                    // window for the window's lifetime. We hold the
                    // gpui WindowHandle here, which keeps the window
                    // alive across this closure, so the view is live.
                    let view: &NSView =
                        unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
                    view.window()
                }
                other => {
                    warn!(handle = ?other, "raw window handle is not AppKit");
                    None
                }
            }
        })
        .ok()
        .flatten()
}

/// Make `child` a real AppKit child of `parent` via
/// `[parent addChildWindow:child ordered:NSWindowAbove]`. Also bumps
/// the child window's level to `NSFloatingWindowLevel` so it paints
/// above all normal-level windows — including the parent's title bar
/// — without leaving the parent's space group.
///
/// Returns `Ok(())` on success. Errors are logged at warn-level and
/// returned as `Err(String)` so the caller can decide whether to
/// surface the failure or treat it as best-effort.
pub fn attach_as_child(
    cx: &mut App,
    parent: AnyWindowHandle,
    child: AnyWindowHandle,
) -> Result<(), String> {
    let parent_win = ns_window_for(cx, parent).ok_or_else(|| {
        "parent NSWindow unavailable (window not realised yet?)".to_string()
    })?;
    let child_win = ns_window_for(cx, child).ok_or_else(|| {
        "child NSWindow unavailable (window not realised yet?)".to_string()
    })?;
    // SAFETY: both windows are retained for the duration of this call.
    // `addChildWindow:ordered:` is the documented AppKit API for
    // parent-child window relationships; objc2-app-kit marks it
    // unsafe because misuse (e.g. cyclic parenting) can crash AppKit.
    unsafe {
        child_win.setLevel(NSFloatingWindowLevel);
        parent_win.addChildWindow_ordered(&child_win, NSWindowOrderingMode::Above);
    }
    tracing::info!("addChildWindow attached Control Bar to AppWindow");
    Ok(())
}

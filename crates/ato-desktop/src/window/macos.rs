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
use objc2_app_kit::{
    NSColor, NSFloatingWindowLevel, NSView, NSWindow, NSWindowOrderingMode,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
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

/// Apply a `cornerRadius` to the NSWindow's contentView layer so the
/// window itself reads as a rounded shape, not a rectangle. Needed
/// when the gpui-side pill is rounded but the underlying NSWindow is
/// still rectangular — the rectangle leaks through at the corners
/// when the backdrop behind has the same colour as the pill.
pub fn round_window_corners(cx: &mut App, handle: AnyWindowHandle, radius: f64) {
    let result = handle.update(cx, |_view, window, _cx| {
        // Same `window_handle()` walk as `ns_window_for` — the match
        // arms constrain method resolution onto the
        // `HasWindowHandle` trait return type.
        let rwh = match window.window_handle() {
            Ok(h) => h,
            Err(e) => {
                warn!(error = %e, "round_window_corners: window_handle failed");
                return;
            }
        };
        let view: &NSView = match rwh.as_raw() {
            RawWindowHandle::AppKit(h) => unsafe {
                &*(h.ns_view.as_ptr() as *const NSView)
            },
            other => {
                warn!(handle = ?other, "round_window_corners: not AppKit");
                return;
            }
        };
        let nswindow: Retained<NSWindow> = match view.window() {
            Some(w) => w,
            None => {
                warn!("round_window_corners: view has no window yet");
                return;
            }
        };
        let content_view = match unsafe { nswindow.contentView() } {
            Some(v) => v,
            None => {
                warn!("round_window_corners: NSWindow has no contentView");
                return;
            }
        };
        // SAFETY: `wantsLayer = true` is the documented opt-in for
        // layer-backed views. We need a layer to set a corner radius
        // on, and `masksToBounds` to clip children inside the radius.
        unsafe {
            content_view.setWantsLayer(true);
            if let Some(layer) = content_view.layer() {
                layer.setCornerRadius(radius);
                layer.setMasksToBounds(true);
            } else {
                warn!("round_window_corners: contentView produced no layer");
            }
            // Make the window backing transparent so AppKit's
            // window-level shadow follows the rounded contentView
            // alpha mask instead of the full rectangle, and so the
            // four rounded-corner cut-outs above the mask are truly
            // transparent (no white halo, no rectangular boundary).
            nswindow.setOpaque(false);
            let clear = NSColor::clearColor();
            nswindow.setBackgroundColor(Some(&clear));
            // OS-level drop shadow renders OUTSIDE the window bounds
            // and follows the alpha mask. This gives the pill visible
            // separation from same-coloured backdrops (e.g. white
            // Store) without re-introducing a padded host window. We
            // re-enable + invalidate explicitly because some popup
            // window kinds disable hasShadow by default and we need
            // AppKit to recompute the shadow against the new rounded
            // alpha mask we just installed via cornerRadius.
            nswindow.setHasShadow(true);
            nswindow.invalidateShadow();
        }
    });
    if let Err(err) = result {
        warn!(error = ?err, "round_window_corners: handle update failed");
    }
}

/// Resize the NSWindow associated with `handle` to `new_w × new_h` logical
/// pixels, keeping the **top edge** and **horizontal centre** anchored so
/// the pill does not jump around the screen during expand/collapse
/// transitions. Also re-applies a `cornerRadius` of `new_h / 2` so the
/// rounded-pill shape stays correct at the new dimensions.
///
/// Uses `setFrame:display:animate:` with `animate = false` for an
/// immediate, synchronous resize — GPUI receives the resulting window-resize
/// notification on the next event-loop tick and re-renders the content area
/// at the new size.
pub fn resize_window_to(cx: &mut App, handle: AnyWindowHandle, new_w: f32, new_h: f32) {
    let result = handle.update(cx, |_view, window, _cx| {
        let rwh = match window.window_handle() {
            Ok(h) => h,
            Err(e) => {
                warn!(error = %e, "resize_window_to: window_handle failed");
                return;
            }
        };
        let view: &NSView = match rwh.as_raw() {
            RawWindowHandle::AppKit(h) => unsafe {
                &*(h.ns_view.as_ptr() as *const NSView)
            },
            other => {
                warn!(handle = ?other, "resize_window_to: not AppKit");
                return;
            }
        };
        let nswindow: Retained<NSWindow> = match view.window() {
            Some(w) => w,
            None => {
                warn!("resize_window_to: view has no window yet");
                return;
            }
        };
        // Cocoa screen coordinates have Y increasing upward and the window
        // origin at the bottom-left corner.  Compute the new frame so that
        // the top edge and horizontal centre stay fixed.
        let current = unsafe { nswindow.frame() };
        let top_y = current.origin.y + current.size.height;
        let center_x = current.origin.x + current.size.width / 2.0;
        let new_origin_x = center_x - new_w as f64 / 2.0;
        let new_origin_y = top_y - new_h as f64;
        let new_frame = NSRect::new(
            NSPoint::new(new_origin_x, new_origin_y),
            NSSize::new(new_w as f64, new_h as f64),
        );
        unsafe {
            nswindow.setFrame_display_animate(new_frame, true, false);
            // Re-apply corner radius for the new height so the pill keeps
            // its fully-rounded capsule shape at both expanded and compact
            // dimensions.
            if let Some(content_view) = nswindow.contentView() {
                content_view.setWantsLayer(true);
                if let Some(layer) = content_view.layer() {
                    layer.setCornerRadius(new_h as f64 / 2.0);
                    layer.setMasksToBounds(true);
                }
            }
            nswindow.invalidateShadow();
        }
    });
    if let Err(err) = result {
        warn!(error = ?err, "resize_window_to: handle update failed");
    }
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

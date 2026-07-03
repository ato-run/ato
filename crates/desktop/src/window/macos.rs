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

use gpui::{AnyWindowHandle, App, Window};
use objc2::rc::Retained;
use objc2::runtime::AnyClass;
use objc2_app_kit::{NSColor, NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tracing::warn;

/// NSWindow backing `window`, extracted WITHOUT going through
/// `handle.update` — usable inside that window's own event handlers,
/// where a same-window `update` would fail.
pub fn ns_window_of(window: &Window) -> Option<Retained<NSWindow>> {
    let rwh = match HasWindowHandle::window_handle(window) {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, "ns_window_of: window_handle failed");
            return None;
        }
    };
    match rwh.as_raw() {
        RawWindowHandle::AppKit(h) => {
            let view: &NSView = unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
            view.window()
        }
        other => {
            warn!(handle = ?other, "ns_window_of: not AppKit");
            None
        }
    }
}

/// Resize `nswindow` keeping its top edge and horizontal centre — but
/// OUTSIDE any GPUI update. `setFrame` synchronously re-enters GPUI's
/// window delegates (windowDidResize); doing that while an `App` borrow
/// is held logs "RefCell already borrowed", the resize event is dropped,
/// and GPUI's internal size goes stale — which breaks hit-testing (a
/// click on one icon lands on another). Scheduling the frame change on
/// the foreground executor lets AppKit deliver the resize while GPUI is
/// idle, so its layout and hit-test geometry stay correct.
pub fn resize_window_outside_update(
    nswindow: Retained<NSWindow>,
    executor: gpui::ForegroundExecutor,
    new_w: f32,
    new_h: f32,
) {
    executor
        .spawn(async move {
            let current = nswindow.frame();
            let top_y = current.origin.y + current.size.height;
            let center_x = current.origin.x + current.size.width / 2.0;
            let new_frame = NSRect::new(
                NSPoint::new(center_x - new_w as f64 / 2.0, top_y - new_h as f64),
                NSSize::new(new_w as f64, new_h as f64),
            );
            nswindow.setFrame_display_animate(new_frame, true, false);
            if let Some(content_view) = nswindow.contentView() {
                content_view.setWantsLayer(true);
                if let Some(layer) = content_view.layer() {
                    layer.setCornerRadius(new_h as f64 / 2.0);
                    layer.setMasksToBounds(true);
                }
            }
            nswindow.invalidateShadow();
        })
        .detach();
}

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
                    let view: &NSView = unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
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
            RawWindowHandle::AppKit(h) => unsafe { &*(h.ns_view.as_ptr() as *const NSView) },
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
        let content_view = match nswindow.contentView() {
            Some(v) => v,
            None => {
                warn!("round_window_corners: NSWindow has no contentView");
                return;
            }
        };
        // SAFETY: `wantsLayer = true` is the documented opt-in for
        // layer-backed views. We need a layer to set a corner radius
        // on, and `masksToBounds` to clip children inside the radius.
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
///
/// **Call this variant from OUTSIDE a window event handler** (e.g.,
/// `show_control_bar`, `focus_control_bar_input`). If you already have a
/// `&mut Window` from an event callback, use [`resize_window_in_handler`]
/// instead — calling `handle.update(cx, ...)` from within the same window's
/// own event handler silently fails because GPUI removes the window from the
/// map for the duration of the update.
pub fn resize_window_to(cx: &mut App, handle: AnyWindowHandle, new_w: f32, new_h: f32) {
    let result = handle.update(cx, |_view, window, _cx| {
        resize_window_in_handler(window, new_w, new_h);
    });
    if let Err(err) = result {
        warn!(error = ?err, "resize_window_to: handle update failed");
    }
}

/// Same resize logic as [`resize_window_to`] but uses a `&mut Window`
/// reference directly. **Use this from within window event handlers**
/// (`on_mouse_move`, `on_hover`, `subscribe_in` callbacks) where
/// `handle.update(cx, ...)` for the same window would fail because GPUI
/// has already removed the window from the map.
pub fn resize_window_in_handler(window: &mut Window, new_w: f32, new_h: f32) {
    let rwh = match window.window_handle() {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, "resize_window_in_handler: window_handle failed");
            return;
        }
    };
    let view: &NSView = match rwh.as_raw() {
        RawWindowHandle::AppKit(h) => unsafe { &*(h.ns_view.as_ptr() as *const NSView) },
        other => {
            warn!(handle = ?other, "resize_window_in_handler: not AppKit");
            return;
        }
    };
    let nswindow: Retained<NSWindow> = match view.window() {
        Some(w) => w,
        None => {
            warn!("resize_window_in_handler: view has no window yet");
            return;
        }
    };
    let current = nswindow.frame();
    let top_y = current.origin.y + current.size.height;
    let center_x = current.origin.x + current.size.width / 2.0;
    let new_origin_x = center_x - new_w as f64 / 2.0;
    let new_origin_y = top_y - new_h as f64;
    let new_frame = NSRect::new(
        NSPoint::new(new_origin_x, new_origin_y),
        NSSize::new(new_w as f64, new_h as f64),
    );
    nswindow.setFrame_display_animate(new_frame, true, false);
    if let Some(content_view) = nswindow.contentView() {
        content_view.setWantsLayer(true);
        if let Some(layer) = content_view.layer() {
            layer.setCornerRadius(new_h as f64 / 2.0);
            layer.setMasksToBounds(true);
        }
    }
    nswindow.invalidateShadow();
}

/// Hide the NSWindow backing `window` without destroying it.
/// Call this inside `window.on_window_should_close(...)` to implement
/// hide-instead-of-close — the GPUI window stays alive, so the next
/// dock / settings / switcher button click only needs to order the
/// window back on-screen without recreating the WebView or re-running
/// heavy initialisation.
pub fn hide_window_in_handler(window: &mut Window) {
    let rwh = match window.window_handle() {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, "hide_window_in_handler: window_handle failed");
            return;
        }
    };
    match rwh.as_raw() {
        RawWindowHandle::AppKit(h) => {
            let view: &NSView = unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
            if let Some(nswindow) = view.window() {
                nswindow.orderOut(None);
            }
        }
        other => {
            warn!(handle = ?other, "hide_window_in_handler: not AppKit");
        }
    }
}

// ── WKWebView screenshot helpers ──────────────────────────────────
//
// `screencapture -l <windowID>` cannot capture Metal-backed windows
// (GPUI renders via Metal). For these windows we fall through to
// WKWebView's own snapshot API, which captures the WebView content
// directly without needing CGWindowListCreateImage.

/// Find a WKWebView in the NSView hierarchy rooted at `content`.
/// Wry mounts the WKWebView as a child of the GPUI content view, so
/// we walk `contentView.subviews()` recursively looking for any view
/// whose class is `WKWebView` (or subclass).
fn find_wkwebview_in_content(content: &NSView) -> Option<Retained<NSView>> {
    use objc2::msg_send;

    static WK_CLASS: std::sync::OnceLock<Option<&'static AnyClass>> = std::sync::OnceLock::new();
    let wk_class = *WK_CLASS.get_or_init(|| {
        let name = c"WKWebView";
        AnyClass::get(name)
    });
    let wk_class = wk_class?;

    for sv in content.subviews().iter() {
        let is_wk: bool = unsafe { msg_send![&sv, isKindOfClass: wk_class] };
        if is_wk {
            return Some(sv);
        }
        if let Some(found) = find_wkwebview_in_content(&sv) {
            return Some(found);
        }
    }
    None
}

/// Dispatch a WKWebView screenshot request for the NSWindow that
/// backs `handle`. The result (a `data:image/png;base64,...` URL or
/// `None` on failure) is sent to `tx` when WKWebView's async snapshot
/// API completes.
///
/// On the main thread this calls `takeSnapshotWithConfiguration:` and
/// returns immediately — the completion handler runs asynchronously on
/// the main queue. No run-loop pumping is performed, so this is safe
/// to call from within GPUI event handlers.
pub fn request_wkwebview_snapshot(
    cx: &mut App,
    handle: AnyWindowHandle,
    tx: std::sync::mpsc::Sender<Option<String>>,
) {
    let Some(nswindow) = ns_window_for(cx, handle) else {
        let _ = tx.send(None);
        return;
    };
    let Some(content) = nswindow.contentView() else {
        let _ = tx.send(None);
        return;
    };
    let Some(wk_view) = find_wkwebview_in_content(&content) else {
        let _ = tx.send(None);
        return;
    };

    use base64::Engine;
    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
    use objc2_foundation::{NSDictionary, NSString};

    let handler = RcBlock::new(move |image: *mut NSImage, _error: *mut AnyObject| {
        let data_url = if !image.is_null() {
            let img = unsafe { &*image };
            let tiff = img.TIFFRepresentation();
            let rep = tiff
                .as_ref()
                .and_then(|t| NSBitmapImageRep::imageRepWithData(t));
            let empty = NSDictionary::<NSString, AnyObject>::new();
            let png = rep.as_ref().and_then(|r| unsafe {
                r.representationUsingType_properties(NSBitmapImageFileType::PNG, &empty)
            });
            png.map(|data| {
                let b64 = base64::engine::general_purpose::STANDARD.encode(data.to_vec());
                format!("data:image/png;base64,{}", b64)
            })
        } else {
            None
        };
        let _ = tx.send(data_url);
    });

    unsafe {
        let _: () = msg_send![
            &*wk_view,
            takeSnapshotWithConfiguration: std::ptr::null_mut::<AnyObject>(),
            completionHandler: &*handler
        ];
    }
}

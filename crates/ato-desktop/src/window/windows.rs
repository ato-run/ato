//! Windows-only window management primitives.
//!
//! Provides the same conceptual surface as `window/macos.rs` using Win32
//! APIs. All functions are no-ops (log + return) when the backing `HWND`
//! cannot be resolved or a Win32 call fails.
//!
//! The path from `gpui::Window` to an `HWND` goes through the
//! `raw_window_handle` trait that GPUI implements on `Window`: the Win32
//! variant gives us the `hwnd` field as a `NonZero<isize>`.

#![cfg(target_os = "windows")]

use gpui::{AnyWindowHandle, App, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::Value;
use std::io::Cursor;
use std::sync::mpsc::Sender;
use tracing::warn;
use windows_sys::Win32::Foundation::{GetLastError, HWND, RECT, SetLastError};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits, GetWindowDC, HBITMAP, HGDIOBJ, RGBQUAD,
    ReleaseDC, SRCCOPY, SelectObject,
};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWLP_HWNDPARENT, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, SW_HIDE, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, WS_EX_NOREDIRECTIONBITMAP,
};

// ── HWND helpers ──────────────────────────────────────────────────────────────

fn hwnd_for_window(window: &Window) -> Option<HWND> {
    // `Window::window_handle()` (inherent) returns `AnyWindowHandle`; we need
    // the `HasWindowHandle` trait method which returns `Result<WindowHandle<'_>>`.
    let rwh = match <Window as HasWindowHandle>::window_handle(window) {
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

/// Prepare a GPUI host window for child WebView composition on Windows by
/// removing WS_EX_NOREDIRECTIONBITMAP when present.
pub fn prepare_window_for_webview(window: &Window) {
    let Some(hwnd) = hwnd_for_window(window) else {
        return;
    };

    unsafe {
        SetLastError(0);
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if ex_style == 0 && GetLastError() != 0 {
            warn!("GetWindowLongPtrW(GWL_EXSTYLE) failed");
            return;
        }
        let desired = ex_style & !(WS_EX_NOREDIRECTIONBITMAP as isize);
        if desired == ex_style {
            return;
        }
        SetLastError(0);
        let prev = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, desired);
        if prev == 0 && GetLastError() != 0 {
            warn!("SetWindowLongPtrW(GWL_EXSTYLE) failed while preparing WebView host window");
            return;
        }
        tracing::debug!("cleared WS_EX_NOREDIRECTIONBITMAP on WebView host window");
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
/// where this attribute is not supported. The `_radius` argument is accepted
/// for parity with the macOS helper but DWM only offers a fixed system radius
/// (there is no per-window radius on Windows), so it is unused.
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
            std::ptr::null_mut(), // hWndInsertAfter: ignored when SWP_NOZORDER
            0,                    // ignored when SWP_NOMOVE
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

/// Pin the window to the always-on-top band so the floating Control Bar
/// stays above normal app windows (and other apps). GPUI's Windows backend
/// creates `WindowKind::PopUp` windows as `WS_EX_TOOLWINDOW` but does not
/// mark them topmost, so we set `HWND_TOPMOST` explicitly via `SetWindowPos`.
/// `SWP_NOMOVE | SWP_NOSIZE` keep the current rect; `SWP_NOACTIVATE` avoids
/// stealing focus.
pub fn set_window_topmost(cx: &mut App, handle: AnyWindowHandle) {
    let Some(hwnd) = hwnd_for(cx, handle) else {
        return;
    };
    unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
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
    let parent_hwnd = hwnd_for(cx, parent).ok_or_else(|| "parent HWND unavailable".to_string())?;
    let child_hwnd = hwnd_for(cx, child).ok_or_else(|| "child HWND unavailable".to_string())?;

    unsafe {
        // Per MSDN: SetWindowLongPtrW returns the previous value on success and
        // 0 on failure. However it can also return 0 legitimately (previous value
        // was 0), so we must clear LastError first and test it afterwards.
        SetLastError(0);
        let prev = SetWindowLongPtrW(child_hwnd, GWLP_HWNDPARENT, parent_hwnd as isize);
        if prev == 0 {
            let err = GetLastError();
            if err != 0 {
                return Err(format!(
                    "SetWindowLongPtrW(GWLP_HWNDPARENT) failed: Win32 error {err}"
                ));
            }
        }
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
    let Some(hwnd) = hwnd_for(cx, handle) else {
        let _ = tx.send(None);
        return;
    };
    let hwnd_value = hwnd as isize;
    std::thread::spawn(move || {
        let data_url = capture_hwnd_png_data_url(hwnd_value as HWND)
            .map_err(|error| {
                tracing::debug!(?error, "windows snapshot capture failed");
                error
            })
            .ok();
        let _ = tx.send(data_url);
    });
}

fn last_error_context(operation: &str) -> String {
    format!("{operation} failed: Win32 error {}", unsafe {
        GetLastError()
    })
}

fn capture_hwnd_png_data_url(hwnd: HWND) -> Result<String, String> {
    unsafe {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return Err(last_error_context("GetWindowRect"));
        }
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return Err(format!("window has invalid capture size {width}x{height}"));
        }

        let window_dc = GetWindowDC(hwnd);
        if window_dc.is_null() {
            return Err(last_error_context("GetWindowDC"));
        }

        let result = (|| {
            let memory_dc = CreateCompatibleDC(window_dc);
            if memory_dc.is_null() {
                return Err(last_error_context("CreateCompatibleDC"));
            }

            let result = (|| {
                let bitmap = CreateCompatibleBitmap(window_dc, width, height);
                if bitmap.is_null() {
                    return Err(last_error_context("CreateCompatibleBitmap"));
                }
                let result = capture_bitmap_to_png_data_url(
                    hwnd, window_dc, memory_dc, bitmap, width, height,
                );
                let _ = DeleteObject(bitmap as HGDIOBJ);
                result
            })();

            let _ = DeleteDC(memory_dc);
            result
        })();

        let _ = ReleaseDC(hwnd, window_dc);
        result
    }
}

fn capture_bitmap_to_png_data_url(
    hwnd: HWND,
    window_dc: windows_sys::Win32::Graphics::Gdi::HDC,
    memory_dc: windows_sys::Win32::Graphics::Gdi::HDC,
    bitmap: HBITMAP,
    width: i32,
    height: i32,
) -> Result<String, String> {
    use base64::Engine as _;

    let (previous, rendered) = unsafe {
        let prev = SelectObject(memory_dc, bitmap as HGDIOBJ);
        if prev.is_null() {
            return Err(last_error_context("SelectObject"));
        }
        let rendered = PrintWindow(hwnd, memory_dc, 2);
        (prev, rendered)
    };
    unsafe {
        if rendered == 0 && BitBlt(memory_dc, 0, 0, width, height, window_dc, 0, 0, SRCCOPY) == 0 {
            let _ = SelectObject(memory_dc, previous);
            return Err(last_error_context("PrintWindow/BitBlt"));
        }
    }

    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [RGBQUAD {
            rgbBlue: 0,
            rgbGreen: 0,
            rgbRed: 0,
            rgbReserved: 0,
        }; 1],
    };
    let mut bgra = vec![0u8; (width as usize) * (height as usize) * 4];
    unsafe {
        let rows = GetDIBits(
            memory_dc,
            bitmap,
            0,
            height as u32,
            bgra.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = SelectObject(memory_dc, previous);
        if rows == 0 {
            return Err(last_error_context("GetDIBits"));
        }
    }

    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }

    let image = image::RgbaImage::from_raw(width as u32, height as u32, bgra)
        .ok_or_else(|| "failed to construct RGBA image".to_string())?;
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|error| format!("PNG encode failed: {error}"))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    Ok(format!("data:image/png;base64,{encoded}"))
}

/// Take a screenshot from a `wry::WebView` handle directly.
/// Delegates to the WebView2 CapturePreview implementation.
pub fn take_webview_screenshot(webview: &wry::WebView, tx: Sender<Result<Value, String>>) {
    crate::automation::screenshot::take_screenshot(webview, tx);
}

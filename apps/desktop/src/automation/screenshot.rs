use serde_json::Value;
use std::sync::mpsc::Sender;

/// Schedule an asynchronous WebView screenshot.
///
/// The result is sent via `tx` when the platform's snapshot API completes.
/// Returns immediately (non-blocking).
#[cfg(target_os = "macos")]
pub fn take_screenshot(webview: &wry::WebView, tx: Sender<Result<Value, String>>) {
    use block2::RcBlock;
    use objc2::msg_send;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
    use objc2_foundation::NSDictionary;
    use wry::WebViewExtMacOS;

    // WryWebView is a subclass of WKWebView; msg_send! uses dynamic dispatch so we can
    // send takeSnapshotWithConfiguration:completionHandler: directly to the wry handle.
    let native_webview = webview.webview();

    let handler = RcBlock::new(
        move |image: *mut NSImage, _error: *mut objc2::runtime::AnyObject| {
            if image.is_null() {
                let _ = tx.send(Err("takeSnapshot returned nil image".into()));
                return;
            }
            let result: Result<Value, String> = unsafe {
                let img = &*image;
                match img.TIFFRepresentation() {
                    None => Err("NSImage.TIFFRepresentation() returned nil".into()),
                    Some(tiff) => match NSBitmapImageRep::imageRepWithData(&tiff) {
                        None => Err("NSBitmapImageRep.imageRepWithData() returned nil".into()),
                        Some(rep) => {
                            let empty_dict = NSDictionary::<
                                objc2_foundation::NSString,
                                objc2::runtime::AnyObject,
                            >::new();
                            match rep.representationUsingType_properties(
                                NSBitmapImageFileType::PNG,
                                &empty_dict,
                            ) {
                                None => Err("PNG representationUsingType returned nil".into()),
                                Some(data) => {
                                    use base64::Engine as _;
                                    let bytes: Vec<u8> = data.to_vec();
                                    let b64 =
                                        base64::engine::general_purpose::STANDARD.encode(&bytes);
                                    Ok(serde_json::json!({
                                        "data": b64,
                                        "mimeType": "image/png"
                                    }))
                                }
                            }
                        }
                    },
                }
            };
            let _ = tx.send(result);
        },
    );

    unsafe {
        let null_config: *mut objc2::runtime::AnyObject = std::ptr::null_mut();
        let _: () = msg_send![
            &*native_webview,
            takeSnapshotWithConfiguration: null_config,
            completionHandler: &*handler
        ];
    }
}

#[cfg(target_os = "windows")]
fn read_capture_stream_bytes(
    stream: &windows::Win32::System::Com::IStream,
) -> Result<Vec<u8>, String> {
    use std::slice;
    use windows::Win32::System::Com::StructuredStorage::GetHGlobalFromStream;
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    let hglobal = unsafe { GetHGlobalFromStream(stream) }
        .map_err(|err| format!("GetHGlobalFromStream failed: {err}"))?;
    let size = unsafe { GlobalSize(hglobal) };
    if size == 0 {
        return Ok(Vec::new());
    }

    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() {
        return Err(format!(
            "GlobalLock failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let bytes = unsafe { slice::from_raw_parts(ptr.cast::<u8>(), size).to_vec() };
    let _ = unsafe { GlobalUnlock(hglobal) };
    Ok(bytes)
}

#[cfg(target_os = "windows")]
pub fn take_screenshot(webview: &wry::WebView, tx: Sender<Result<Value, String>>) {
    use base64::Engine as _;
    use webview2_com::{CapturePreviewCompletedHandler, Microsoft::Web::WebView2::Win32::*};
    use windows::Win32::{
        Foundation::HGLOBAL, System::Com::StructuredStorage::CreateStreamOnHGlobal,
    };
    use wry::WebViewExtWindows;

    let stream = match unsafe { CreateStreamOnHGlobal(HGLOBAL::default(), true) } {
        Ok(stream) => stream,
        Err(err) => {
            let _ = tx.send(Err(format!("CreateStreamOnHGlobal failed: {err}")));
            return;
        }
    };

    let callback_tx = tx.clone();
    let stream_for_handler = stream.clone();
    let handler = CapturePreviewCompletedHandler::create(Box::new(move |result| {
        match result {
            Ok(()) => match read_capture_stream_bytes(&stream_for_handler) {
                Ok(bytes) if bytes.is_empty() => {
                    let _ = callback_tx.send(Err("CapturePreview returned an empty PNG".into()));
                }
                Ok(bytes) => {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                    let _ = callback_tx.send(Ok(serde_json::json!({
                        "data": b64,
                        "mimeType": "image/png"
                    })));
                }
                Err(err) => {
                    let _ = callback_tx.send(Err(err));
                }
            },
            Err(err) => {
                let _ = callback_tx.send(Err(format!("CapturePreview failed: {err}")));
            }
        }
        Ok(())
    }));

    if let Err(err) = unsafe {
        webview.webview().CapturePreview(
            COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
            &stream,
            &handler,
        )
    } {
        let _ = tx.send(Err(format!("failed to start CapturePreview: {err}")));
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn take_screenshot(_webview: &wry::WebView, tx: Sender<Result<Value, String>>) {
    let _ = tx.send(Err(
        "screenshot is not yet supported on this platform".into()
    ));
}

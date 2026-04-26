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

#[cfg(not(target_os = "macos"))]
pub fn take_screenshot(_webview: &wry::WebView, tx: Sender<Result<Value, String>>) {
    let _ = tx.send(Err(
        "screenshot is not yet supported on this platform".into()
    ));
}

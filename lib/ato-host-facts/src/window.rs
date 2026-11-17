//! Read-only ownership and visibility of local macOS windows.
use core_foundation_sys::{
    array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef},
    base::CFRelease,
    dictionary::{CFDictionaryGetValue, CFDictionaryRef},
    number::{CFNumberGetValue, kCFNumberSInt32Type},
    string::CFStringRef,
};
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(options: u32, relative_to: u32) -> CFArrayRef;
    static kCGWindowOwnerPID: CFStringRef;
    static kCGWindowLayer: CFStringRef;
}

pub fn visible_window(pid: u32) -> bool {
    // SAFETY: CoreGraphics owns the dictionary values for the retained array's
    // lifetime. Both documented keys contain CFNumbers. Only metadata is read;
    // there is no image capture or global UI automation.
    unsafe {
        let windows = CGWindowListCopyWindowInfo(1 | 16, 0);
        if windows.is_null() {
            return false;
        }
        let mut found = false;
        for i in 0..CFArrayGetCount(windows) {
            let item = CFArrayGetValueAtIndex(windows, i) as CFDictionaryRef;
            let owner = CFDictionaryGetValue(item, kCGWindowOwnerPID.cast());
            let layer = CFDictionaryGetValue(item, kCGWindowLayer.cast());
            let mut owner_value = 0_i32;
            let mut layer_value = -1_i32;
            if !owner.is_null()
                && !layer.is_null()
                && CFNumberGetValue(
                    owner.cast(),
                    kCFNumberSInt32Type,
                    (&mut owner_value as *mut i32).cast(),
                )
                && CFNumberGetValue(
                    layer.cast(),
                    kCFNumberSInt32Type,
                    (&mut layer_value as *mut i32).cast(),
                )
                && owner_value == pid as i32
                && layer_value == 0
            {
                found = true;
                break;
            }
        }
        CFRelease(windows.cast());
        found
    }
}

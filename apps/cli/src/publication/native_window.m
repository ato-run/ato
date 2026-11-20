// Owner-side operations on the one on-screen window of a verified Run.
#import <ApplicationServices/ApplicationServices.h>
#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <Vision/Vision.h>

bool ato_native_window_available(void) {
    if (@available(macOS 14.0, *)) {
        return AXIsProcessTrusted() && CGPreflightScreenCaptureAccess();
    }
    return false;
}

uint32_t ato_native_window_id(int32_t pid) {
    @autoreleasepool {
        if (pid <= 0) return 0;
        NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
        uint32_t match = 0;
        for (NSDictionary *item in windows) {
            if ([item[(__bridge NSString *)kCGWindowOwnerPID] intValue] == pid &&
                [item[(__bridge NSString *)kCGWindowLayer] intValue] == 0) {
                if (match != 0) return 0;
                match = [item[(__bridge NSString *)kCGWindowNumber] unsignedIntValue];
            }
        }
        return match;
    }
}

static NSString *recognize(CGImageRef image) {
    if (image == NULL) return nil;
    VNRecognizeTextRequest *request = [VNRecognizeTextRequest new];
    request.recognitionLevel = VNRequestTextRecognitionLevelAccurate;
    VNImageRequestHandler *handler = [[VNImageRequestHandler alloc] initWithCGImage:image options:@{}];
    if (![handler performRequests:@[request] error:nil]) return nil;
    NSMutableArray<NSString *> *lines = [NSMutableArray array];
    for (VNRecognizedTextObservation *observation in request.results) {
        NSString *text = [observation topCandidates:1].firstObject.string;
        if (text.length > 0) [lines addObject:text];
        if (lines.count >= 48) break;
    }
    return [lines componentsJoinedByString:@"\n"];
}

// Only a window captured with a desktop-independent filter reaches OCR.
// No image or text is sent to the workload or stored on disk by this bridge.
size_t ato_native_window_text(int32_t pid, uint32_t window, uint32_t timeout_ms,
                              uint8_t *output, size_t capacity) {
    @autoreleasepool {
        if (@available(macOS 14.0, *)) {
            if (window == 0 || timeout_ms == 0 || ato_native_window_id(pid) != window) return 0;
            dispatch_semaphore_t done = dispatch_semaphore_create(0);
            __block NSString *observed = nil;
            [SCShareableContent getShareableContentExcludingDesktopWindows:YES
                onScreenWindowsOnly:YES completionHandler:^(SCShareableContent *content, NSError *error) {
                    if (error != nil) {
                        dispatch_semaphore_signal(done);
                        return;
                    }
                    for (SCWindow *item in content.windows) {
                        if (item.windowID != window || item.owningApplication.processID != pid) continue;
                        SCContentFilter *filter = [[SCContentFilter alloc] initWithDesktopIndependentWindow:item];
                        SCStreamConfiguration *config = [SCStreamConfiguration new];
                        CGFloat scale = MIN(2.0, 1440.0 / MAX(item.frame.size.width, item.frame.size.height));
                        config.width = MAX(1, (size_t)(item.frame.size.width * scale));
                        config.height = MAX(1, (size_t)(item.frame.size.height * scale));
                        config.showsCursor = NO;
                        [SCScreenshotManager captureImageWithFilter:filter configuration:config
                            completionHandler:^(CGImageRef image, NSError *captureError) {
                                if (captureError == nil) observed = recognize(image);
                                dispatch_semaphore_signal(done);
                            }];
                        return;
                    }
                    dispatch_semaphore_signal(done);
                }];
            if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, timeout_ms * NSEC_PER_MSEC)) != 0 ||
                ato_native_window_id(pid) != window) return 0;
            NSData *text = [observed dataUsingEncoding:NSUTF8StringEncoding];
            if (text.length == 0 || text.length > capacity) return 0;
            memcpy(output, text.bytes, text.length);
            return text.length;
        }
        return 0;
    }
}

static id attribute(AXUIElementRef element, CFStringRef key) {
    CFTypeRef value = NULL;
    if (AXUIElementCopyAttributeValue(element, key, &value) != kAXErrorSuccess) return nil;
    return CFBridgingRelease(value);
}

static bool owned_axis(id item, pid_t pid) {
    pid_t owner = 0;
    return item != nil && CFGetTypeID((__bridge CFTypeRef)item) == AXUIElementGetTypeID() &&
           AXUIElementGetPid((__bridge AXUIElementRef)item, &owner) == kAXErrorSuccess && owner == pid;
}

// Only AXPress on one unambiguous enabled button in this Run's window.
// Never use the global event stream, focus, coordinates, or launcher UI.
bool ato_native_window_press(int32_t pid, uint32_t window, const uint8_t *label, size_t length,
                             uint32_t timeout_ms) {
    @autoreleasepool {
        if (window == 0 || timeout_ms == 0 || !AXIsProcessTrusted() || ato_native_window_id(pid) != window) return false;
        NSString *target = [[NSString alloc] initWithBytes:label length:length encoding:NSUTF8StringEncoding];
        if (target.length == 0) return false;
        NSTimeInterval deadline = NSProcessInfo.processInfo.systemUptime + timeout_ms / 1000.0;
        id app = CFBridgingRelease(AXUIElementCreateApplication(pid));
        AXUIElementSetMessagingTimeout((__bridge AXUIElementRef)app, 0.15);
        NSArray *windows = attribute((__bridge AXUIElementRef)app, kAXWindowsAttribute);
        if (![windows isKindOfClass:NSArray.class] || windows.count != 1) return false;
        NSMutableArray *pending = [NSMutableArray arrayWithObject:windows.firstObject];
        id match = nil;
        for (NSUInteger cursor = 0; cursor < pending.count; cursor++) {
            id item = pending[cursor];
            if (!owned_axis(item, pid) || NSProcessInfo.processInfo.systemUptime >= deadline) return false;
            AXUIElementRef node = (__bridge AXUIElementRef)item;
            AXUIElementSetMessagingTimeout(node, 0.15);
            if ([attribute(node, kAXRoleAttribute) isEqual:(__bridge NSString *)kAXButtonRole] &&
                [attribute(node, kAXTitleAttribute) isEqual:target]) {
                if (match != nil || ![attribute(node, kAXEnabledAttribute) isEqual:@YES]) return false;
                match = item;
            }
            CFIndex count = 0;
            if (AXUIElementGetAttributeValueCount(node, kAXChildrenAttribute, &count) == kAXErrorSuccess && count > 0) {
                if (count + pending.count > 128) return false;
                CFArrayRef children = NULL;
                if (AXUIElementCopyAttributeValues(node, kAXChildrenAttribute, 0, count, &children) != kAXErrorSuccess) return false;
                [pending addObjectsFromArray:CFBridgingRelease(children)];
            }
        }
        return match != nil && NSProcessInfo.processInfo.systemUptime < deadline &&
            ato_native_window_id(pid) == window &&
            AXUIElementPerformAction((__bridge AXUIElementRef)match, kAXPressAction) == kAXErrorSuccess;
    }
}

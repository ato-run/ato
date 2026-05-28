//! Spike for issue #168 — verify NSWindow `addChildWindow:ordered:` behavior
//! across Spaces, fullscreen, and multi-monitor.
//!
//! The Focus View multi-window redesign (#167–#174) places the Control Bar
//! as a borderless NSWindow attached to its parent app window via
//! `[parent addChildWindow:bar ordered:NSWindowAbove]`. macOS is documented
//! to keep the child synchronized with the parent across drag, Spaces, and
//! fullscreen, but the contract has historically broken with custom
//! titlebar / GPUI configs. This spike isolates the relationship from the
//! rest of the desktop shell so the assumption can be verified manually
//! before the Control Bar window scaffolding lands (#171).
//!
//! ## Run
//!
//! ```sh
//! cd crates/ato-desktop
//! cargo run --example nswindow_child_spike
//! ```
//!
//! ## Manual verification checklist
//!
//! After launch the screen shows a dark-grey 800×600 parent window with
//! traffic lights and a blue 400×60 borderless child anchored near the
//! parent's lower-left corner.
//!
//! 1. **Drag** the parent by its titlebar → the child follows pixel-for-pixel.
//! 2. **Switch Spaces** with Ctrl-→ / Ctrl-← (or Mission Control) → the
//!    child appears in the same Space as the parent.
//! 3. **Fullscreen** the parent (green button) and exit fullscreen → the
//!    child reappears in roughly the same screen position.
//! 4. **Move** the parent to another monitor → the child follows.
//! 5. **Close** the parent (red button) → the child is destroyed and the
//!    process exits cleanly (no orphan window remains in `Cmd-Tab` or in
//!    `cx.windows()` parlance).
//! 6. **Click** the parent → the parent becomes key. Hover over the
//!    child without clicking → the child does NOT become key.
//! 7. **Click** the child → the child becomes key but the parent stays
//!    in front of every other application (the addChildWindow contract).
//!
//! Capture observations as comments in the spike PR or as an issue
//! comment on #168. If any step fails, document the fallback (typically
//! observing `NSWorkspaceDidActivateApplicationNotification` and calling
//! `setFrame:` manually) and adjust the acceptance criteria of #171
//! accordingly.
//!
//! This example deliberately does NOT use GPUI. The spike question is
//! about AppKit primitives, and going through GPUI would conflate the
//! two layers. The real Control Bar implementation in #171 spawns its
//! window through GPUI's `cx.open_window` and then calls
//! `addChildWindow_ordered` via the same raw `objc2_app_kit` API used
//! here.

#![cfg(target_os = "macos")]

use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor,
    NSFloatingWindowLevel, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

fn main() {
    let mtm = MainThreadMarker::new().expect("NSWindow allocation must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let parent = make_parent(mtm);
    let child = make_child(mtm);

    parent.makeKeyAndOrderFront(None);
    // SAFETY: parent and child are owned by `Retained` for the lifetime
    // of `main`; the child window stays alive at least as long as the
    // parent because the parent retains it via `addChildWindow:`.
    unsafe {
        parent.addChildWindow_ordered(&child, NSWindowOrderingMode::Above);
    }

    app.activate();
    app.run();

    // `app.run()` blocks until the app terminates (parent close via
    // red traffic light triggers `windowShouldClose:` → app terminates
    // when the last window is gone). The Retained handles are dropped
    // here; child windows attached via `addChildWindow:` are released
    // by AppKit when the parent is released, so no manual cleanup is
    // required.
    drop(parent);
    drop(child);
}

fn make_parent(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let rect = NSRect::new(NSPoint::new(120.0, 120.0), NSSize::new(800.0, 600.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::Miniaturizable;
    // SAFETY: We hold `MainThreadMarker`; the alloc / init pair is the
    // documented constructor for `NSWindow`.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str(
        "nswindow_child_spike — parent (drag me, Space me, fullscreen me)",
    ));
    window.setBackgroundColor(Some(&NSColor::darkGrayColor()));
    // Keep our `Retained` handle valid after the user clicks the red
    // traffic light. AppKit's default is to release the NSWindow,
    // which would dangle the `Retained` and abort the spike.
    // SAFETY: We own the window via `Retained`; opting out of the
    // release-on-close behavior is safe under that ownership.
    unsafe { window.setReleasedWhenClosed(false) };
    window
}

fn make_child(mtm: MainThreadMarker) -> Retained<NSWindow> {
    // Anchored near the parent's lower-left; the spike script does not
    // try to track parent geometry — `addChildWindow:` does that.
    let rect = NSRect::new(NSPoint::new(220.0, 140.0), NSSize::new(400.0, 60.0));
    let style = NSWindowStyleMask::Borderless;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setBackgroundColor(Some(&NSColor::systemBlueColor()));
    window.setLevel(NSFloatingWindowLevel);
    // Transparent backdrop so the upcoming Control Bar can render its
    // own rounded-rect with shadow.
    window.setOpaque(false);
    // Make sure clicks hit the bar (default for borderless windows but
    // explicit here to mirror what the real Control Bar will configure
    // in #171).
    window.setIgnoresMouseEvents(false);
    window
}

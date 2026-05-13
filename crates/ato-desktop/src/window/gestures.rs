//! Layer 6 scaffolding — trackpad / mouse gestures on the Control Bar
//! surface (#174). The implementation walks down to the Control Bar
//! NSWindow's content view (raw `objc2_app_kit`), installs an event
//! tap for `NSEventTypeScrollWheel` + `mouseDown` / `mouseDragged` /
//! `mouseUp`, and dispatches GPUI actions when thresholds are crossed.
//!
//! Mappings (per the redesign plan):
//!
//! | Gesture                                                            | Action                |
//! |--------------------------------------------------------------------|-----------------------|
//! | two-finger horizontal swipe, \|dx\| > 60px, \|dy\| < 30px          | FocusPrev/NextAppWindow |
//! | mouse-down + drag, dy > 30px, t < 400ms                            | OpenCardSwitcher      |
//! | mouse-down, no movement > 6px, held > 400ms                        | OpenCardSwitcher      |
//!
//! None of this is wired yet. The actual event tap installation
//! depends on the Control Bar window being addressable via
//! `addChildWindow:` (which itself is deferred — see
//! `window::control_bar`). Once both land, gesture recognition is a
//! ~150 LoC bridge from raw AppKit events to the existing
//! `FocusPrevAppWindow` / `FocusNextAppWindow` / `OpenCardSwitcher`
//! actions.
//!
//! The fallback documented in the redesign plan — if GPUI's event
//! plumbing makes gesture recognition unreliable, ship with the
//! Window-list icon + keyboard shortcut only and log a follow-up —
//! is honoured by `crate::window::control_bar::ControlBarShellPlaceholder`
//! already (the icon is rendered disabled with a "card switcher
//! coming" tooltip, not faked).

/// Placeholder for the gesture recognizer. Future commits replace
/// this with a real `GestureRecognizer { state: Idle | Tracking { .. } }`
/// driven by AppKit event taps.
pub struct GestureRecognizerPlaceholder;

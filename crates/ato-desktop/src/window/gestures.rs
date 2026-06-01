//! Trackpad / mouse gesture recognition for the Control Bar (#174).
//!
//! Uses GPUI's native `on_scroll_wheel` / `on_mouse_down` /
//! `on_mouse_move` / `on_mouse_up` event handlers — no AppKit event
//! taps required. Gesture state is stored in `ControlBarShellPlaceholder`
//! and updated on each event; when a threshold is crossed the handler
//! dispatches the appropriate GPUI action.
//!
//! | Gesture                                              | Action                  |
//! |------------------------------------------------------|-------------------------|
//! | two-finger horizontal swipe, \|dx\| > 60px, \|dy\| < 30px | FocusPrev/NextAppWindow |
//! | mouse-down + drag, dy > 30px, t < 400ms              | OpenCardSwitcher        |
//! | mouse-down, no movement > 6px, held > 400ms          | OpenCardSwitcher        |

use std::time::Instant;

/// Pixels the horizontal scroll must accumulate before a swipe fires.
const SWIPE_DX_THRESHOLD: f32 = 60.0;
/// Max vertical scroll allowed while a horizontal swipe is in progress.
const SWIPE_DY_MAX: f32 = 30.0;
/// Reset accumulated scroll when no scroll event arrives for this many ms.
const SCROLL_RESET_MS: u128 = 300;

/// Pixels of downward drag required to open the Card Switcher.
const DRAG_DY_THRESHOLD: f32 = 30.0;
/// Max elapsed time (ms) for a drag gesture to count.
const DRAG_TIMEOUT_MS: u128 = 400;
/// Max displacement (px) before a hold is considered a drag.
const HOLD_MOVE_THRESHOLD: f32 = 6.0;
/// Minimum hold duration (ms) without movement to open the Card Switcher.
const HOLD_TIMEOUT_MS: u128 = 400;

/// Recognised gesture outcome. The Control Bar handler maps each
/// variant to the appropriate GPUI action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureAction {
    FocusPrev,
    FocusNext,
    OpenCardSwitcher,
}

/// Stateful gesture recognizer driven by GPUI mouse / scroll events.
///
/// Store one instance in `ControlBarShellPlaceholder` and call the
/// `on_*` methods from the corresponding GPUI event listeners. Each
/// method returns `Some(action)` when a gesture threshold is crossed,
/// after which the caller should dispatch the matching GPUI action and
/// the recognizer automatically resets.
#[derive(Default)]
pub struct GestureState {
    // --- scroll / swipe ---
    scroll_accum_dx: f32,
    scroll_accum_dy: f32,
    last_scroll_at: Option<Instant>,

    // --- mouse drag / hold ---
    mouse_down_at: Option<Instant>,
    mouse_down_x: f32,
    mouse_down_y: f32,
    mouse_moved: bool,
}

impl GestureState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Call from `on_scroll_wheel`. Returns `Some(FocusPrev)` or
    /// `Some(FocusNext)` when the horizontal swipe threshold is crossed.
    pub fn on_scroll_delta(&mut self, dx: f32, dy: f32) -> Option<GestureAction> {
        let now = Instant::now();
        if let Some(last) = self.last_scroll_at
            && last.elapsed().as_millis() > SCROLL_RESET_MS
        {
            self.scroll_accum_dx = 0.0;
            self.scroll_accum_dy = 0.0;
        }
        self.scroll_accum_dx += dx;
        self.scroll_accum_dy += dy;
        self.last_scroll_at = Some(now);

        if self.scroll_accum_dx.abs() > SWIPE_DX_THRESHOLD
            && self.scroll_accum_dy.abs() < SWIPE_DY_MAX
        {
            let action = if self.scroll_accum_dx > 0.0 {
                GestureAction::FocusNext
            } else {
                GestureAction::FocusPrev
            };
            self.scroll_accum_dx = 0.0;
            self.scroll_accum_dy = 0.0;
            return Some(action);
        }
        None
    }

    /// Call from `on_mouse_down`. Starts tracking a potential drag/hold.
    pub fn on_mouse_down(&mut self, x: f32, y: f32) {
        self.mouse_down_at = Some(Instant::now());
        self.mouse_down_x = x;
        self.mouse_down_y = y;
        self.mouse_moved = false;
    }

    /// Call from `on_mouse_move`. Returns `Some(OpenCardSwitcher)` if
    /// the downward-drag threshold is crossed within the time window.
    pub fn on_mouse_move(&mut self, x: f32, y: f32) -> Option<GestureAction> {
        let down_at = self.mouse_down_at?;
        let dx = x - self.mouse_down_x;
        let dy = y - self.mouse_down_y;
        if dx.abs() > HOLD_MOVE_THRESHOLD || dy.abs() > HOLD_MOVE_THRESHOLD {
            self.mouse_moved = true;
        }
        if dy > DRAG_DY_THRESHOLD && down_at.elapsed().as_millis() < DRAG_TIMEOUT_MS {
            self.mouse_down_at = None;
            return Some(GestureAction::OpenCardSwitcher);
        }
        None
    }

    /// Call from `on_mouse_up`. Returns `Some(OpenCardSwitcher)` if
    /// the mouse was held without significant movement for long enough.
    pub fn on_mouse_up(&mut self) -> Option<GestureAction> {
        let down_at = self.mouse_down_at.take()?;
        if !self.mouse_moved && down_at.elapsed().as_millis() >= HOLD_TIMEOUT_MS {
            return Some(GestureAction::OpenCardSwitcher);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swipe_right_fires_focus_next() {
        let mut g = GestureState::new();
        // Accumulate enough rightward scroll
        let result = g.on_scroll_delta(65.0, 0.0);
        assert_eq!(result, Some(GestureAction::FocusNext));
        // State resets after firing
        assert_eq!(g.scroll_accum_dx, 0.0);
    }

    #[test]
    fn swipe_left_fires_focus_prev() {
        let mut g = GestureState::new();
        let result = g.on_scroll_delta(-65.0, 5.0);
        assert_eq!(result, Some(GestureAction::FocusPrev));
    }

    #[test]
    fn swipe_with_too_much_vertical_is_ignored() {
        let mut g = GestureState::new();
        let result = g.on_scroll_delta(70.0, 35.0);
        assert_eq!(result, None);
    }

    #[test]
    fn drag_down_fires_card_switcher() {
        let mut g = GestureState::new();
        g.on_mouse_down(0.0, 0.0);
        let result = g.on_mouse_move(0.0, 35.0);
        assert_eq!(result, Some(GestureAction::OpenCardSwitcher));
    }

    #[test]
    fn short_hold_does_not_fire() {
        let mut g = GestureState::new();
        g.on_mouse_down(0.0, 0.0);
        // Small movement, no enough hold time
        let up = g.on_mouse_up();
        assert_eq!(up, None);
    }
}

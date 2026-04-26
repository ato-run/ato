use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self as crossterm_event, KeyCode, KeyEvent};

pub fn can_launch_tui(stdin_is_tty: bool, stdout_is_tty: bool) -> bool {
    stdin_is_tty && stdout_is_tty
}

pub fn drain_startup_input_events(window: Duration) -> Result<()> {
    let started_at = Instant::now();
    while started_at.elapsed() < window {
        if !crossterm_event::poll(Duration::from_millis(1))? {
            break;
        }
        let _ = crossterm_event::read()?;
    }
    Ok(())
}

pub fn should_ignore_startup_enter(
    key: &KeyEvent,
    started_at: Instant,
    guard_window: Duration,
) -> bool {
    key.code == KeyCode::Enter && started_at.elapsed() <= guard_window
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn tui_gate_requires_both_streams() {
        assert!(can_launch_tui(true, true));
        assert!(!can_launch_tui(true, false));
        assert!(!can_launch_tui(false, true));
        assert!(!can_launch_tui(false, false));
    }

    #[test]
    fn startup_enter_guard_only_applies_to_enter_inside_window() {
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let fresh = Instant::now();
        let old = Instant::now() - Duration::from_millis(400);

        assert!(should_ignore_startup_enter(
            &enter,
            fresh,
            Duration::from_millis(200)
        ));
        assert!(!should_ignore_startup_enter(
            &enter,
            old,
            Duration::from_millis(200)
        ));
        assert!(!should_ignore_startup_enter(
            &down,
            fresh,
            Duration::from_millis(200)
        ));
    }
}

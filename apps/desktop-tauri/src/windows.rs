//! Window label helpers and the caller-label authorization check.
//!
//! The capability file grants native permissions only to the `main` window,
//! but that is defense in depth, not the sole check: every command verifies its
//! caller label before acting. Remote Home and guest `app-*` windows must be
//! unable to invoke native commands regardless of capability configuration.

use crate::navigation::MAIN_WINDOW_LABEL;

/// Whether `label` is the bundled launcher's main window.
pub fn is_main_label(label: &str) -> bool {
    label == MAIN_WINDOW_LABEL
}

/// Refuse a native command unless the caller is the `main` window.
pub fn verify_main_caller(label: &str) -> Result<(), String> {
    if is_main_label(label) {
        Ok(())
    } else {
        Err("native commands are only available from the main window".to_owned())
    }
}

/// Derive a stable, safe window label for a guest surface id. Only ASCII
/// alphanumerics and `-` survive; everything else collapses to `-`.
pub fn app_window_label(surface_id: &str) -> String {
    let safe = surface_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("app-{safe}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_main_label_is_a_privileged_caller() {
        assert!(is_main_label("main"));
        assert!(verify_main_caller("main").is_ok());
        assert!(!is_main_label("home"));
        assert!(verify_main_caller("home").is_err());
        assert!(!is_main_label("app-abc"));
        assert!(verify_main_caller("app-abc").is_err());
    }

    #[test]
    fn guest_labels_are_sanitized() {
        assert_eq!(app_window_label("abc-123"), "app-abc-123");
        assert_eq!(app_window_label("a/b:c"), "app-a-b-c");
        assert_eq!(app_window_label("../../etc"), "app-------etc");
    }
}

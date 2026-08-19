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

/// Derive a collision-resistant window label for a guest surface. The label is
/// a truncated BLAKE3 digest over the canonical project path and the surface
/// URL, so distinct projects, and distinct surfaces of one project, can never
/// collide into the same window the way a path sanitizer would.
pub fn app_window_label(project: &str, surface_url: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(project.as_bytes());
    hasher.update(&[0]);
    hasher.update(surface_url.as_bytes());
    let digest = hasher.finalize().to_string();
    format!("app-{}", &digest[..16])
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
    fn guest_labels_are_collision_resistant() {
        let label = app_window_label("/projects/a-b/c", "http://127.0.0.1:8000");
        assert!(label.starts_with("app-"));
        assert!(
            label[4..]
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        // Same project + surface → same label (stable focus/reuse).
        assert_eq!(
            label,
            app_window_label("/projects/a-b/c", "http://127.0.0.1:8000")
        );
        // Different surfaces of one project → different labels.
        assert_ne!(
            app_window_label("/projects/a-b/c", "http://127.0.0.1:8000"),
            app_window_label("/projects/a-b/c", "http://127.0.0.1:8001")
        );
        // Path sanitizer collisions stay distinct.
        assert_ne!(
            app_window_label("/projects/a-b/c", "http://127.0.0.1:8000"),
            app_window_label("/projects/a/b-c", "http://127.0.0.1:8000")
        );
    }
}

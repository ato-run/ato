//! Stable event names emitted by the Tauri adapter after runner operations.

/// Installed library content or metadata changed.
pub const LIBRARY_CHANGED: &str = "desktop://library-changed";
/// A runtime session started, stopped, or changed status.
pub const SESSION_CHANGED: &str = "desktop://session-changed";
/// A long operation produced a progress update.
pub const OPERATION_PROGRESS: &str = "desktop://operation-progress";
/// An operation failed and the launcher should surface its diagnostic.
pub const OPERATION_FAILED: &str = "desktop://operation-failed";
/// Trusted remote Home requested that the local Launcher present an install.
/// The event is advisory: only the bundled Launcher can invoke the install
/// command, so the handoff never grants remote content a native capability.
pub const INSTALL_REQUESTED: &str = "desktop://install-requested";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_match_the_desktop_protocol() {
        assert_eq!(LIBRARY_CHANGED, "desktop://library-changed");
        assert_eq!(SESSION_CHANGED, "desktop://session-changed");
        assert_eq!(OPERATION_PROGRESS, "desktop://operation-progress");
        assert_eq!(OPERATION_FAILED, "desktop://operation-failed");
        assert_eq!(INSTALL_REQUESTED, "desktop://install-requested");
    }
}

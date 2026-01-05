//! macOS-specific sandbox implementation
//!
//! macOS does not have bubblewrap. For Phase 1, we return an error directing
//! users to use OCI fallback. Future phases may implement sandbox-exec or
//! Apple's App Sandbox framework.

use tracing::warn;

use crate::runtime::{LaunchRequest, LaunchResult, RuntimeError, SourceTarget};

use super::SourceRuntime;

/// Attempt to launch with native macOS sandbox (not yet implemented)
pub async fn launch_native_macos(
    _runtime: &SourceRuntime,
    _request: &LaunchRequest<'_>,
    _target: &SourceTarget,
) -> Result<LaunchResult, RuntimeError> {
    warn!("Native macOS sandbox not yet implemented");

    Err(RuntimeError::SandboxSetupFailed(
        "Native source execution not available on macOS. \
         Use OCI fallback or set ATO_DEV_MODE=0 to force container execution."
            .to_string(),
    ))
}

/// Check if native sandbox is available on macOS
///
/// Currently always returns false. Future implementation may check for:
/// - sandbox-exec availability
/// - Appropriate entitlements
/// - Code signing requirements
pub fn is_native_available() -> bool {
    // Phase 1: Always use OCI fallback on macOS
    false
}

/// Future: sandbox-exec profile for source execution
#[allow(dead_code)]
const SANDBOX_PROFILE: &str = r#"
(version 1)
(deny default)

; Allow reading standard system files
(allow file-read*
    (subpath "/usr/lib")
    (subpath "/usr/share")
    (subpath "/System/Library/Frameworks")
    (literal "/dev/urandom")
    (literal "/dev/random"))

; Allow reading the source directory
; (allow file-read* (subpath "<SOURCE_DIR>"))

; Allow network (for dev mode)
(allow network*)

; Allow process execution
(allow process-exec*)

; Deny writes outside tmp
(allow file-write*
    (subpath "/private/tmp")
    (subpath "/tmp"))
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_native_not_available() {
        assert!(!is_native_available());
    }
}

//! Runtime binding for a materialised, host-window-attached system capsule.
//!
//! `SystemCapsuleBinding` is the runtime counterpart to the static
//! `SystemCapsuleDescriptor` in `manifest.rs`.  It is created by
//! window-open code once capsule assets are available on disk and the
//! GPUI window exists.

use std::path::PathBuf;

use gpui::AnyWindowHandle;

use super::broker::SystemCapsuleId;

/// Runtime state produced when a system capsule is materialised and its
/// host window is opened.
///
/// **Separation rationale**: `SystemCapsuleDescriptor` is static and
/// lives in read-only memory; `SystemCapsuleBinding` is ephemeral and
/// tied to the lifetime of a running desktop process.  Never put paths,
/// hashes, or window handles into the descriptor.
#[derive(Debug, Clone)]
pub struct SystemCapsuleBinding {
    pub id: SystemCapsuleId,
    /// Short canonical slug (e.g. `"store"`, `"onboarding"`).
    pub canonical_slug: String,
    /// Absolute path to the materialised capsule root (asset tree).
    pub materialized_root: PathBuf,
    /// Absolute path to the directory actually served by the custom
    /// protocol handler (may be a `dist/` sub-directory).
    pub serving_root: PathBuf,
    /// Content hash of the seed that was materialised, used to detect
    /// stale or tampered assets.
    pub version_hash: String,
    /// The GPUI window that hosts this capsule's WebView.
    pub host_window: AnyWindowHandle,
}

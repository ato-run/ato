//! Inline manifest table for the four Phase-1 system capsules.
//!
//! Phase 1 keeps the manifest as a static Rust table to avoid
//! committing prematurely to a TOML schema. Phase 2 will lift this
//! out into `assets/system/<name>/manifest.toml` with semver +
//! signed updates. The field names here are chosen so that move is
//! a straightforward serde derive job.

use super::broker::{Capability, SystemCapsuleId};

/// Phase-1 manifest. Mirrors the fields a Phase-2 TOML capsule
/// manifest would carry (`schema_version`, `name`, `version`,
/// `[capabilities]`).
#[derive(Debug)]
pub struct SystemCapsuleManifest {
    pub id: SystemCapsuleId,
    /// Path segment in the handle `capsule://desktop.ato.run/<slug>`.
    pub slug: &'static str,
    /// Human label rendered in places that show "what's running"
    /// (Card Switcher cards, Control Bar). Kept short.
    pub display_name: &'static str,
    /// Capabilities the capsule is permitted to invoke through the
    /// broker. Anything else fails closed with `BrokerError::Forbidden`.
    pub allowed_capabilities: &'static [Capability],
}

/// Static, exhaustive table. The broker looks up by `SystemCapsuleId`
/// and rejects unknown IDs at compile time (the enum is closed).
const TABLE: &[SystemCapsuleManifest] = &[
    SystemCapsuleManifest {
        id: SystemCapsuleId::AtoWindows,
        slug: "windows",
        display_name: "Windows",
        allowed_capabilities: &[
            Capability::WindowsList,
            Capability::WindowsActivate,
            Capability::WindowsClose,
            Capability::WindowsCloseTarget,
            Capability::WebviewCreate,
            Capability::LaunchSystemCapsule,
        ],
    },
    SystemCapsuleManifest {
        id: SystemCapsuleId::AtoStore,
        slug: "store",
        display_name: "Store",
        allowed_capabilities: &[Capability::WebviewCreate, Capability::LaunchSystemCapsule],
    },
    SystemCapsuleManifest {
        id: SystemCapsuleId::AtoSettings,
        slug: "settings",
        display_name: "Settings",
        allowed_capabilities: &[
            Capability::SettingsRead,
            Capability::SettingsWrite,
            Capability::WindowsClose,
        ],
    },
    SystemCapsuleManifest {
        id: SystemCapsuleId::AtoWebViewer,
        slug: "web-viewer",
        display_name: "Web Viewer",
        allowed_capabilities: &[
            Capability::TabsCreate,
            Capability::WebviewCreate,
            Capability::LaunchSystemCapsule,
        ],
    },
    SystemCapsuleManifest {
        id: SystemCapsuleId::AtoLaunch,
        slug: "launch",
        display_name: "Launch",
        // ato-launch needs WebviewCreate so its `Approve` command
        // can spawn the target AppWindow on the user's behalf. It
        // also closes its own window after the approve/cancel
        // decision (WindowsClose). LaunchSystemCapsule lets it
        // hand off to other system capsules if needed (e.g.
        // showing settings before approving).
        allowed_capabilities: &[
            Capability::WebviewCreate,
            Capability::WindowsClose,
            Capability::LaunchSystemCapsule,
        ],
    },
    SystemCapsuleManifest {
        id: SystemCapsuleId::AtoIdentity,
        slug: "identity",
        display_name: "Identity",
        // Account / Identity popover invoked from the Control Bar
        // avatar button. Phase 1 menu items either close their own
        // window (WindowsClose) or hand off to ato-store /
        // ato-settings (LaunchSystemCapsule).
        allowed_capabilities: &[Capability::WindowsClose, Capability::LaunchSystemCapsule],
    },
    SystemCapsuleManifest {
        id: SystemCapsuleId::AtoStart,
        slug: "start",
        display_name: "Start",
        // Start page: can list open windows, open capsules via consent
        // (WebviewCreate), open system capsules like the Store
        // (LaunchSystemCapsule), and close its own window (WindowsClose).
        allowed_capabilities: &[
            Capability::WindowsList,
            Capability::WindowsClose,
            Capability::WebviewCreate,
            Capability::LaunchSystemCapsule,
        ],
    },
    SystemCapsuleManifest {
        id: SystemCapsuleId::AtoDock,
        slug: "dock",
        display_name: "Dock",
        // Dock: developer hub that can trigger ato login (WebviewCreate covers
        // spawning the re-opened window after login completes).
        allowed_capabilities: &[
            Capability::WebviewCreate,
            Capability::WindowsClose,
            Capability::LaunchSystemCapsule,
        ],
    },
];

/// Canonical handle URL for a system capsule slug. Mirrors the value
/// that AppWindow registry / content_windows record for system capsule
/// content windows (`capsule://desktop.ato.run/<slug>`).
pub fn system_capsule_url(slug: &str) -> String {
    format!("capsule://desktop.ato.run/{slug}")
}

pub fn lookup(id: SystemCapsuleId) -> &'static SystemCapsuleManifest {
    TABLE
        .iter()
        .find(|m| m.id == id)
        // Exhaustiveness is enforced by the closed enum + this table
        // being the only manifest source; a missing entry is a
        // build-time bug we want to surface loudly.
        .expect("system capsule manifest table missing an entry")
}

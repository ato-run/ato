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
    /// Stable URL slug. `capsule://system/<slug>/...` resolves here.
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
        slug: "ato-windows",
        display_name: "Windows",
        allowed_capabilities: &[
            Capability::WindowsList,
            Capability::WindowsActivate,
            Capability::WindowsClose,
            Capability::WebviewCreate,
            Capability::LaunchSystemCapsule,
        ],
    },
    SystemCapsuleManifest {
        id: SystemCapsuleId::AtoStore,
        slug: "ato-store",
        display_name: "Store",
        allowed_capabilities: &[
            Capability::WebviewCreate,
            Capability::LaunchSystemCapsule,
        ],
    },
    SystemCapsuleManifest {
        id: SystemCapsuleId::AtoSettings,
        slug: "ato-settings",
        display_name: "Settings",
        // SettingsWrite is intentionally NOT granted in Phase 1.
        // Phase 2 will gate writes through a consent prompt before
        // adding it here.
        allowed_capabilities: &[Capability::SettingsRead],
    },
    SystemCapsuleManifest {
        id: SystemCapsuleId::AtoWebViewer,
        slug: "ato-web-viewer",
        display_name: "Web Viewer",
        allowed_capabilities: &[
            Capability::TabsCreate,
            Capability::WebviewCreate,
            Capability::LaunchSystemCapsule,
        ],
    },
];

pub fn lookup(id: SystemCapsuleId) -> &'static SystemCapsuleManifest {
    TABLE
        .iter()
        .find(|m| m.id == id)
        // Exhaustiveness is enforced by the closed enum + this table
        // being the only manifest source; a missing entry is a
        // build-time bug we want to surface loudly.
        .expect("system capsule manifest table missing an entry")
}

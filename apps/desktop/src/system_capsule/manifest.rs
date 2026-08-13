//! Static descriptor table for all built-in system capsules.
//!
//! # Type separation
//!
//! `SystemCapsuleDescriptor` — compile-time static data: identity,
//! origin, capability allowlist.  Never contains runtime paths or
//! hashes.
//!
//! `SystemCapsuleBinding` — runtime state produced after a capsule is
//! materialised and a host window is opened for it: file paths, version
//! hash, the GPUI window handle.  Lives in `window/` code, not here.
//!
//! # Canonical slug set (closed)
//!
//! The ten canonical slugs below are the only values recognised by the
//! IPC layer and the `capsule://desktop.ato.run/<slug>` URL scheme.
//! Legacy `ato-*` aliases are accepted at the IPC boundary and
//! immediately normalised; the canonical form is always used downstream.
//!
//! # Stable-origin audit (Phase 7)
//!
//! `stable_origin_proxy.rs` derives stable-origin host labels from
//! `GuestRoute::Capsule { session }` or `GuestRoute::CapsuleHandle { handle }`,
//! **never** from the capsule slug.  Slug-to-`SystemCapsuleId` unification
//! therefore cannot affect origin isolation: each capsule instance keeps
//! its own `capsule_instance_key` (handle or session id) regardless of
//! which slug form was used to address it.

use super::broker::{Capability, SystemCapsuleId};

/// Closed set of canonical slugs.  Each entry matches the `slug` field
/// in the descriptor table below.
pub const CANONICAL_SLUGS: &[&str] = &[
    "store",
    "web-viewer",
    "settings",
    "windows",
    "launch",
    "identity",
    "start",
    "dock",
    "onboarding",
    "import",
];

/// Static descriptor for a built-in system capsule.
///
/// All fields are `'static` so the table can live in read-only memory.
/// Never put runtime-derived data (paths, hashes, window handles) here.
#[derive(Debug)]
pub struct SystemCapsuleDescriptor {
    pub id: SystemCapsuleId,
    /// Canonical slug — the single short name used in
    /// `capsule://desktop.ato.run/<slug>` URLs and as the IPC principal
    /// identifier.  Must be one of `CANONICAL_SLUGS`.
    pub canonical_slug: &'static str,
    /// Older names accepted at the IPC boundary and immediately
    /// normalised to `canonical_slug`.  Typically the `ato-*` prefix
    /// forms that shipped before the slug unification.
    pub legacy_aliases: &'static [&'static str],
    /// Human label for card-switcher cards and the Control Bar.
    pub display_name: &'static str,
    /// The origin that any WebView serving this capsule must present.
    /// Used by Phase 3 principal resolution to verify the binding.
    pub expected_origin: &'static str,
    /// Capabilities the capsule may invoke through the broker.
    pub allowed_capabilities: &'static [Capability],
}

/// Static, exhaustive descriptor table.
///
/// Keyed by `SystemCapsuleId`; the enum is closed so exhaustiveness is
/// enforced at compile-time via the `lookup`/`lookup_by_slug` panics.
const TABLE: &[SystemCapsuleDescriptor] = &[
    SystemCapsuleDescriptor {
        id: SystemCapsuleId::AtoWindows,
        canonical_slug: "windows",
        legacy_aliases: &["ato-windows"],
        display_name: "Windows",
        expected_origin: "capsule://desktop.ato.run/windows",
        allowed_capabilities: &[
            Capability::WindowsList,
            Capability::WindowsActivate,
            Capability::WindowsClose,
            Capability::WindowsCloseTarget,
            Capability::WebviewCreate,
            Capability::LaunchSystemCapsule,
        ],
    },
    SystemCapsuleDescriptor {
        id: SystemCapsuleId::AtoStore,
        canonical_slug: "store",
        legacy_aliases: &["ato-store"],
        display_name: "Store",
        expected_origin: "capsule://desktop.ato.run/store",
        allowed_capabilities: &[Capability::WebviewCreate, Capability::LaunchSystemCapsule],
    },
    SystemCapsuleDescriptor {
        id: SystemCapsuleId::AtoSettings,
        canonical_slug: "settings",
        legacy_aliases: &["ato-settings"],
        display_name: "Settings",
        expected_origin: "capsule://desktop.ato.run/settings",
        allowed_capabilities: &[
            Capability::SettingsRead,
            Capability::SettingsWrite,
            Capability::WindowsClose,
            // Settings is the post-onboarding re-setup surface: it can read
            // Runtime Setup status, install managed tools, prepare host
            // runtimes (Podman), and open logs.
            Capability::RuntimeSetupRead,
            Capability::RuntimeSetupInstall,
            Capability::RuntimeSetupPrepare,
            Capability::RuntimeSetupOpenLogs,
        ],
    },
    SystemCapsuleDescriptor {
        id: SystemCapsuleId::AtoWebViewer,
        canonical_slug: "web-viewer",
        legacy_aliases: &["ato-web-viewer"],
        display_name: "Web Viewer",
        expected_origin: "capsule://desktop.ato.run/web-viewer",
        allowed_capabilities: &[
            Capability::TabsCreate,
            Capability::WebviewCreate,
            Capability::LaunchSystemCapsule,
        ],
    },
    SystemCapsuleDescriptor {
        id: SystemCapsuleId::AtoLaunch,
        canonical_slug: "launch",
        legacy_aliases: &["ato-launch"],
        display_name: "Launch",
        expected_origin: "capsule://desktop.ato.run/launch",
        allowed_capabilities: &[
            Capability::WebviewCreate,
            Capability::WindowsClose,
            Capability::LaunchSystemCapsule,
        ],
    },
    SystemCapsuleDescriptor {
        id: SystemCapsuleId::AtoIdentity,
        canonical_slug: "identity",
        legacy_aliases: &["ato-identity"],
        display_name: "Identity",
        expected_origin: "capsule://desktop.ato.run/identity",
        allowed_capabilities: &[Capability::WindowsClose, Capability::LaunchSystemCapsule],
    },
    SystemCapsuleDescriptor {
        id: SystemCapsuleId::AtoStart,
        canonical_slug: "start",
        legacy_aliases: &["ato-start"],
        display_name: "Start",
        expected_origin: "capsule://desktop.ato.run/start",
        allowed_capabilities: &[
            Capability::WindowsList,
            Capability::WindowsClose,
            Capability::WebviewCreate,
            Capability::LaunchSystemCapsule,
            Capability::AppQuit,
            Capability::RuntimeControl,
        ],
    },
    SystemCapsuleDescriptor {
        id: SystemCapsuleId::AtoDock,
        canonical_slug: "dock",
        legacy_aliases: &["ato-dock"],
        display_name: "Dock",
        expected_origin: "capsule://desktop.ato.run/dock",
        allowed_capabilities: &[Capability::WindowsClose, Capability::LaunchSystemCapsule],
    },
    SystemCapsuleDescriptor {
        id: SystemCapsuleId::AtoOnboarding,
        canonical_slug: "onboarding",
        legacy_aliases: &["ato-onboarding"],
        display_name: "Onboarding",
        expected_origin: "capsule://desktop.ato.run/onboarding",
        // Onboarding completes the flow and runs the first-run Runtime Setup
        // panel (read + install managed tools + prepare Podman), but cannot
        // open logs — that re-setup affordance lives in Settings.
        allowed_capabilities: &[
            Capability::OnboardingComplete,
            Capability::RuntimeSetupRead,
            Capability::RuntimeSetupInstall,
            Capability::RuntimeSetupPrepare,
        ],
    },
    SystemCapsuleDescriptor {
        id: SystemCapsuleId::AtoImport,
        canonical_slug: "import",
        legacy_aliases: &["ato-import"],
        display_name: "Import",
        expected_origin: "capsule://desktop.ato.run/import",
        allowed_capabilities: &[Capability::WebviewCreate, Capability::WindowsClose],
    },
];

/// Canonical handle URL for a system capsule slug.
pub fn system_capsule_url(slug: &str) -> String {
    format!("capsule://desktop.ato.run/{slug}")
}

/// Look up a descriptor by `SystemCapsuleId`.  Panics if the table is
/// missing an entry (compile-time bug — the enum is closed).
pub fn lookup(id: SystemCapsuleId) -> &'static SystemCapsuleDescriptor {
    TABLE
        .iter()
        .find(|d| d.id == id)
        .expect("system capsule descriptor table missing an entry")
}

/// Resolve a slug string (canonical or legacy alias) to a
/// `SystemCapsuleId`.  Returns `None` for unrecognised slugs.
///
/// Callers at the IPC boundary **must** use this to normalise the slug
/// received from JS before any further processing.  Never trust the
/// raw JS-supplied string as a principal identifier.
pub fn lookup_by_slug(slug: &str) -> Option<SystemCapsuleId> {
    TABLE
        .iter()
        .find(|d| d.canonical_slug == slug || d.legacy_aliases.contains(&slug))
        .map(|d| d.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboarding_descriptor_grants_onboarding_complete() {
        let d = lookup(SystemCapsuleId::AtoOnboarding);
        assert_eq!(d.canonical_slug, "onboarding");
        assert!(
            d.allowed_capabilities
                .contains(&Capability::OnboardingComplete)
        );
    }

    #[test]
    fn all_canonical_slugs_in_table() {
        for &slug in CANONICAL_SLUGS {
            assert!(
                TABLE.iter().any(|d| d.canonical_slug == slug),
                "CANONICAL_SLUGS entry '{slug}' has no descriptor in TABLE"
            );
        }
    }

    #[test]
    fn legacy_alias_resolves_to_canonical_id() {
        assert_eq!(lookup_by_slug("ato-store"), Some(SystemCapsuleId::AtoStore));
        assert_eq!(
            lookup_by_slug("ato-onboarding"),
            Some(SystemCapsuleId::AtoOnboarding)
        );
        assert_eq!(lookup_by_slug("store"), Some(SystemCapsuleId::AtoStore));
    }

    #[test]
    fn unknown_slug_returns_none() {
        assert_eq!(lookup_by_slug("totally-unknown-capsule"), None);
    }

    #[test]
    fn ato_start_grants_runtime_control() {
        let d = lookup(SystemCapsuleId::AtoStart);
        assert!(
            d.allowed_capabilities.contains(&Capability::RuntimeControl),
            "AtoStart must have RuntimeControl to launch/stop sessions"
        );
    }

    #[test]
    fn runtime_control_not_granted_to_store() {
        let d = lookup(SystemCapsuleId::AtoStore);
        assert!(
            !d.allowed_capabilities.contains(&Capability::RuntimeControl),
            "AtoStore must not have RuntimeControl"
        );
    }

    #[test]
    fn expected_origin_matches_canonical_slug() {
        for d in TABLE {
            let expected = format!("capsule://desktop.ato.run/{}", d.canonical_slug);
            assert_eq!(
                d.expected_origin, expected,
                "descriptor for '{}' has wrong expected_origin",
                d.canonical_slug
            );
        }
    }
}

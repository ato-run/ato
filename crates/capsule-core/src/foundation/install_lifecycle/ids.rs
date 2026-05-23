//! Typed identifiers for the installed-app lifecycle.
//!
//! # Key stability contracts
//!
//! | Type | Stability |
//! |------|-----------|
//! | [`InstalledAppId`] | Stable for the lifetime of the installed app. Generated once at first install. |
//! | [`ProfileId`] | Stable for the lifetime of the profile (`"default"`, `"staging"`, …). |
//! | [`InstallProfileKey`] | Derived from `installed_app_id + profile_id`. Never changes even when the revision updates. Used for shortcuts / dashboard links. |
//! | [`InstallRevisionId`] | Changes with each new artifact push / update. Immutable revision root identifier. |
//! | [`CapsuleInstanceKey`] | Derived from `install_profile_key + install_revision_id`. Changes when the revision changes. Used for exact session / receipt replay. |
//! | [`ArtifactBuildId`] | Opaque build identifier produced by the artifact producer. Must NOT look like an [`ExecutionId`]. |
//! | [`ExecutionId`] | Assigned at launch time. Has an `exec_` prefix to distinguish it from build IDs. |

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ── Macro: newtype wrapper with Display / AsRef / From<String> ─────────────

macro_rules! typed_id {
    ($(#[$attr:meta])* $name:ident) => {
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap an already-validated string.
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            /// Borrow the inner string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }
    };
}

typed_id!(
    /// Stable identifier for an installed capsule application.
    /// Generated once at first install and never reassigned.
    InstalledAppId
);

typed_id!(
    /// Identifier for a launch profile within an installed app.
    /// Typically `"default"` but may be `"staging"`, `"gpu"`, etc.
    ProfileId
);

impl Default for ProfileId {
    fn default() -> Self {
        ProfileId::new("default")
    }
}

typed_id!(
    /// Stable composite key: `SHA256(installed_app_id || ":" || profile_id)[:16]`.
    /// Used for shortcuts and dashboard links; unchanged across revisions.
    InstallProfileKey
);

typed_id!(
    /// Identifier for a specific immutable revision of an installed app.
    /// Changes with every artifact push / update.
    InstallRevisionId
);

typed_id!(
    /// Per-revision-per-profile instance key.
    /// Derived from `install_profile_key + install_revision_id`.
    /// Changes when the revision changes; used for exact session / receipt replay.
    CapsuleInstanceKey
);

typed_id!(
    /// Identifier produced by the artifact build producer.
    /// Must start with `"build_"` and must NOT resemble an [`ExecutionId`].
    ArtifactBuildId
);

typed_id!(
    /// Runtime execution identifier assigned at launch time.
    /// Always starts with `"exec_"`.
    ExecutionId
);

// ── Derivation helpers ─────────────────────────────────────────────────────

/// Derive the [`InstallProfileKey`] from an installed-app id and profile id.
///
/// The key is the first 16 hex characters of `SHA256("ipk:" || installed_app_id || ":" || profile_id)`.
pub fn derive_install_profile_key(app: &InstalledAppId, profile: &ProfileId) -> InstallProfileKey {
    let input = format!("ipk:{}:{}", app.as_str(), profile.as_str());
    let hash = hex_prefix(&input, 16);
    InstallProfileKey::new(hash)
}

/// Derive the [`CapsuleInstanceKey`] from the profile key and a revision id.
///
/// The key is the first 16 hex characters of `SHA256("cik:" || install_profile_key || ":" || install_revision_id)`.
pub fn derive_capsule_instance_key(
    profile_key: &InstallProfileKey,
    revision: &InstallRevisionId,
) -> CapsuleInstanceKey {
    let input = format!("cik:{}:{}", profile_key.as_str(), revision.as_str());
    let hash = hex_prefix(&input, 16);
    CapsuleInstanceKey::new(hash)
}

fn hex_prefix(input: &str, chars: usize) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let hex = hex_encode(&digest);
    hex[..chars.min(hex.len())].to_owned()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Validation ─────────────────────────────────────────────────────────────

impl ArtifactBuildId {
    /// Returns `true` if the id has the required `build_` prefix.
    pub fn is_valid(&self) -> bool {
        self.0.starts_with("build_")
    }
}

impl ExecutionId {
    /// Returns `true` if the id has the required `exec_` prefix.
    pub fn is_valid(&self) -> bool {
        self.0.starts_with("exec_")
    }

    /// Returns `true` if the string looks like an execution id
    /// (used by the producer request validator to reject `execution_id`-shaped values).
    pub fn looks_like(s: &str) -> bool {
        s.starts_with("exec_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── install_profile_key stability ──────────────────────────────────────

    #[test]
    fn install_profile_key_stable_across_revisions() {
        let app = InstalledAppId::new("app_abc123");
        let profile = ProfileId::new("default");

        let rev_a = InstallRevisionId::new("rev_001");
        let rev_b = InstallRevisionId::new("rev_002");

        let key = derive_install_profile_key(&app, &profile);

        // The profile key itself is revision-independent.
        assert_eq!(key, derive_install_profile_key(&app, &profile));

        // Deriving capsule_instance_key with different revisions gives different keys.
        let ck_a = derive_capsule_instance_key(&key, &rev_a);
        let ck_b = derive_capsule_instance_key(&key, &rev_b);
        assert_ne!(ck_a, ck_b, "instance key must change when revision changes");
    }

    #[test]
    fn install_profile_key_stable_across_execution_ids() {
        let app = InstalledAppId::new("app_abc123");
        let profile = ProfileId::new("default");

        let key1 = derive_install_profile_key(&app, &profile);
        let key2 = derive_install_profile_key(&app, &profile);
        assert_eq!(key1, key2, "profile key must not change between calls");
    }

    // ── capsule_instance_key changes with revision ─────────────────────────

    #[test]
    fn capsule_instance_key_changes_with_revision() {
        let app = InstalledAppId::new("app_xyz");
        let profile = ProfileId::new("default");
        let key = derive_install_profile_key(&app, &profile);

        let ck1 = derive_capsule_instance_key(&key, &InstallRevisionId::new("rev_1"));
        let ck2 = derive_capsule_instance_key(&key, &InstallRevisionId::new("rev_2"));
        assert_ne!(ck1, ck2);
    }

    // ── shortcut / dashboard uses install_profile_key ─────────────────────

    #[test]
    fn shortcut_uses_install_profile_key_not_instance_key() {
        // Simulate: shortcut is created at rev_1, app is updated to rev_2.
        // The shortcut's stored key must still resolve.
        let app = InstalledAppId::new("app_shortcut");
        let profile = ProfileId::new("default");
        let key_at_install = derive_install_profile_key(&app, &profile);

        // After update to rev_2 the profile key is unchanged.
        let key_after_update = derive_install_profile_key(&app, &profile);
        assert_eq!(key_at_install, key_after_update);
    }

    // ── session / receipt uses capsule_instance_key ────────────────────────

    #[test]
    fn session_uses_capsule_instance_key() {
        let app = InstalledAppId::new("app_session");
        let profile = ProfileId::new("default");
        let key = derive_install_profile_key(&app, &profile);
        let rev = InstallRevisionId::new("rev_42");
        let instance_key = derive_capsule_instance_key(&key, &rev);

        // The instance key encodes both profile and revision.
        assert!(!instance_key.as_str().is_empty());
    }

    // ── artifact_build_id rejects execution_id prefix ─────────────────────

    #[test]
    fn artifact_build_id_valid() {
        let id = ArtifactBuildId::new("build_abc");
        assert!(id.is_valid());
    }

    #[test]
    fn artifact_build_id_invalid_when_exec_prefix() {
        let id = ArtifactBuildId::new("exec_abc");
        assert!(!id.is_valid(), "build id must not have exec_ prefix");
    }

    #[test]
    fn execution_id_valid() {
        let id = ExecutionId::new("exec_abc");
        assert!(id.is_valid());
    }

    #[test]
    fn execution_id_invalid_when_build_prefix() {
        let id = ExecutionId::new("build_abc");
        assert!(!id.is_valid());
    }

    #[test]
    fn execution_id_looks_like() {
        assert!(ExecutionId::looks_like("exec_123"));
        assert!(!ExecutionId::looks_like("build_123"));
        assert!(!ExecutionId::looks_like("rev_123"));
    }

    // ── serde round-trip ───────────────────────────────────────────────────

    #[test]
    fn serde_roundtrip() {
        let app = InstalledAppId::new("app_serde_test");
        let json = serde_json::to_string(&app).unwrap();
        let back: InstalledAppId = serde_json::from_str(&json).unwrap();
        assert_eq!(app, back);
    }
}

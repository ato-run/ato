//! Typed identifiers for the installed-app lifecycle.
//!
//! # Key stability contracts
//!
//! | Type | Shape | Stability |
//! |------|-------|-----------|
//! | [`InstalledAppId`] | any string | Stable for the lifetime of the installed app. Generated once at first install. |
//! | [`ProfileId`] | any string | Stable for the lifetime of the profile (`"default"`, `"staging"`, …). |
//! | [`InstallProfileKey`] | `ipk_<32 hex>` | Derived from `installed_app_id + profile_id`. Never changes. Used for shortcuts / dashboard links. |
//! | [`InstallRevisionId`] | `rev_<32 hex>` | Content-addressed from `artifact_build_id`. Immutable revision root identifier. |
//! | [`CapsuleInstanceKey`] | `cik_<32 hex>` | Derived from `install_profile_key + install_revision_id + execution_id`. Unique per launch. Used for session / receipt / exact replay. |
//! | [`ArtifactBuildId`] | `build_<64 hex>` | Content-addressed build identifier. Must NOT look like an [`ExecutionId`]. |
//! | [`ExecutionId`] | `exec_<32+ hex>` | Assigned at launch time. |
//!
//! # Separation of concerns
//!
//! - [`InstallProfileKey`] — stable shortcut / dashboard link; never changes across revisions.
//! - [`InstallRevisionId`] — identifies an immutable frozen artifact revision.
//! - [`CapsuleInstanceKey`] — identifies one specific launch invocation for receipt / replay.
//!   Only minted at launch time when [`ExecutionId`] is known.
//!   The [`crate::foundation::install_lifecycle::finalizer::InstallRevisionFinalizer`]
//!   does **not** produce a [`CapsuleInstanceKey`].

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
    /// Stable composite key: `SHA256("ipk:<installed_app_id>:<profile_id>")[:32]`.
    /// Used for shortcuts and dashboard links; unchanged across revisions.
    InstallProfileKey
);

typed_id!(
    /// Identifier for a specific immutable revision of an installed app.
    /// Changes with every artifact push / update.
    InstallRevisionId
);

typed_id!(
    /// Per-revision-per-profile-per-execution instance key.
    /// Derived from `install_profile_key + install_revision_id + execution_id`.
    /// Changes when the revision *or* execution_id changes; used for session / receipt / exact replay.
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
/// Shape: `ipk_<32 hex>` = first 32 hex chars of `SHA256("ipk:<app>:<profile>")`.
pub fn derive_install_profile_key(app: &InstalledAppId, profile: &ProfileId) -> InstallProfileKey {
    let input = format!("ipk:{}:{}", app.as_str(), profile.as_str());
    InstallProfileKey::new(format!("ipk_{}", hex_prefix(&input, 32)))
}

/// Derive the stable, user-facing **app URL** for an installed profile.
///
/// Shape: `ato://app/<install_profile_key>`.
///
/// This is the durable open-identity an Ato install assigns to a profile: it is
/// what Desktop windows, the Start page, Dashboard entries, and OS shortcuts
/// should target. Unlike a runtime session's `local_url` (an ephemeral loopback
/// port behind the router) or a `.capsule` / revision-output path, this URL is
/// independent of which revision is current and which port a given session
/// happens to bind — it depends *only* on the `install_profile_key`. A rollback,
/// update, or relaunch therefore never changes it.
///
/// The router/proxy that resolves `ato://app/<ipk>` to the current revision's
/// running session is a separate concern (not all callers can navigate this
/// scheme yet); until that binding exists, callers may open the session
/// `local_url` as a temporary adapter while still persisting this URL as the
/// stable identity.
pub fn derive_app_url(profile_key: &InstallProfileKey) -> String {
    format!("ato://app/{}", profile_key.as_str())
}

/// Derive a path-safe [`InstalledAppId`] from an arbitrary scoped capsule id (e.g. `publisher/slug`).
///
/// The raw scoped id may contain `/` which would be interpreted as a path separator
/// by the store's directory layout. This function hashes it to a single-component id:
/// `app_<32 hex>` = first 32 hex chars of `SHA256("app:<scoped_id>")`.
///
/// The `publisher`, `slug`, and `scoped_id` should be preserved in [`crate::foundation::install_lifecycle::store::AppRecord`].
pub fn path_safe_app_id(scoped_id: &str) -> InstalledAppId {
    let input = format!("app:{scoped_id}");
    InstalledAppId::new(format!("app_{}", hex_prefix(&input, 32)))
}

/// Derive the [`CapsuleInstanceKey`] from the profile key, revision, and execution id.
///
/// Shape: `cik_<32 hex>` = first 32 hex chars of `SHA256("cik:<ipk>:<rev>:<exec>")`.
///
/// All three components are required: the same revision can be launched multiple times
/// with different env closures / argv / policies, each producing a different `execution_id`.
/// The `CapsuleInstanceKey` must capture that distinction for receipt / replay isolation.
pub fn derive_capsule_instance_key(
    profile_key: &InstallProfileKey,
    revision: &InstallRevisionId,
    execution_id: &ExecutionId,
) -> CapsuleInstanceKey {
    let input = format!(
        "cik:{}:{}:{}",
        profile_key.as_str(),
        revision.as_str(),
        execution_id.as_str()
    );
    CapsuleInstanceKey::new(format!("cik_{}", hex_prefix(&input, 32)))
}

/// Mint a deterministic [`InstallRevisionId`] from an artifact build id.
///
/// Shape: `rev_<32 hex>` = first 32 hex chars of `SHA256("rev:<artifact_build_id>")`.
///
/// Deterministic: the same `artifact_build_id` always produces the same revision id,
/// so re-finalizing the same build is idempotent.
pub fn revision_id_for_build(build_id: &ArtifactBuildId) -> InstallRevisionId {
    let input = format!("rev:{}", build_id.as_str());
    InstallRevisionId::new(format!("rev_{}", hex_prefix(&input, 32)))
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
    /// Returns `Ok(())` if the id has the required shape: `build_` + exactly 64 lowercase hex chars.
    pub fn validate(&self) -> Result<(), String> {
        validate_prefixed_hex(&self.0, "build_", 64)
    }

    /// Returns `true` if the id is well-formed.
    pub fn is_valid(&self) -> bool {
        self.validate().is_ok()
    }
}

impl ExecutionId {
    /// Returns `Ok(())` if the id has the required shape: `exec_` + at least 32 lowercase hex chars.
    pub fn validate(&self) -> Result<(), String> {
        if !self.0.starts_with("exec_") {
            return Err(format!(
                "ExecutionId must start with 'exec_', got: {}",
                self.0
            ));
        }
        let hex_part = &self.0["exec_".len()..];
        if hex_part.len() < 32 {
            return Err(format!(
                "ExecutionId hex part must be ≥32 chars, got {} chars",
                hex_part.len()
            ));
        }
        if !hex_part.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
            return Err(format!(
                "ExecutionId hex part must be lowercase hex, got: {}",
                hex_part
            ));
        }
        Ok(())
    }

    /// Returns `true` if the id is well-formed.
    pub fn is_valid(&self) -> bool {
        self.validate().is_ok()
    }

    /// Returns `true` if the string looks like an execution id
    /// (used by the producer request validator to reject `execution_id`-shaped values).
    pub fn looks_like(s: &str) -> bool {
        s.starts_with("exec_")
    }

    /// Generate a fresh, cryptographically random `ExecutionId`.
    ///
    /// Shape: `exec_<32 lowercase hex>` (128-bit random).
    pub fn generate() -> Self {
        use rand::RngCore as _;
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Self::new(format!("exec_{hex}"))
    }
}

/// Validate shape: `<prefix>` + exactly `hex_len` lowercase hex chars.
fn validate_prefixed_hex(s: &str, prefix: &str, hex_len: usize) -> Result<(), String> {
    let hex_part = s
        .strip_prefix(prefix)
        .ok_or_else(|| format!("must start with '{}', got: {}", prefix, s))?;
    if hex_part.len() != hex_len {
        return Err(format!(
            "hex part must be exactly {} chars, got {} chars in: {}",
            hex_len,
            hex_part.len(),
            s
        ));
    }
    if !hex_part.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
        return Err(format!("hex part must be lowercase hex, got: {}", s));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_exec_id() -> ExecutionId {
        ExecutionId::new(format!("exec_{}", "a".repeat(32)))
    }

    // ── install_profile_key stability ──────────────────────────────────────

    #[test]
    fn install_profile_key_stable_across_revisions() {
        let app = InstalledAppId::new("app_abc123");
        let profile = ProfileId::new("default");
        let exec_a = ExecutionId::new(format!("exec_{}", "a".repeat(32)));
        let exec_b = ExecutionId::new(format!("exec_{}", "b".repeat(32)));

        let rev_a = InstallRevisionId::new("rev_001");
        let rev_b = InstallRevisionId::new("rev_002");

        let key = derive_install_profile_key(&app, &profile);
        assert!(
            key.as_str().starts_with("ipk_"),
            "profile key must have ipk_ prefix"
        );
        assert_eq!(key.as_str().len(), 4 + 32, "ipk_ + 32 hex chars expected");

        // The profile key itself is revision-independent.
        assert_eq!(key, derive_install_profile_key(&app, &profile));

        // Deriving capsule_instance_key with different revisions gives different keys.
        let ck_a = derive_capsule_instance_key(&key, &rev_a, &exec_a);
        let ck_b = derive_capsule_instance_key(&key, &rev_b, &exec_b);
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

    // ── app_url stable identity ────────────────────────────────────────────

    #[test]
    fn app_url_derives_from_profile_key_only() {
        let app = InstalledAppId::new("app_abc123");
        let profile = ProfileId::new("default");
        let key = derive_install_profile_key(&app, &profile);

        let url = derive_app_url(&key);
        assert_eq!(url, format!("ato://app/{}", key.as_str()));
        assert!(
            url.starts_with("ato://app/ipk_"),
            "app url must be ato://app/<ipk>, got: {url}"
        );
    }

    #[test]
    fn app_url_is_revision_and_port_independent() {
        let app = InstalledAppId::new("app_xyz");
        let profile = ProfileId::new("default");
        let key = derive_install_profile_key(&app, &profile);

        // The same profile key always yields the same app URL — it cannot
        // encode a revision id, an execution id, or a runtime port.
        let url1 = derive_app_url(&key);
        let url2 = derive_app_url(&derive_install_profile_key(&app, &profile));
        assert_eq!(url1, url2);
        assert!(!url1.contains("rev_"), "app url must not embed a revision");
        assert!(
            !url1.contains("exec_"),
            "app url must not embed an execution"
        );
        assert!(
            !url1.contains("127.0.0.1") && !url1.contains("localhost"),
            "app url must not embed a runtime loopback port: {url1}"
        );
    }

    // ── capsule_instance_key requires all 3 components ─────────────────────

    #[test]
    fn capsule_instance_key_changes_with_revision() {
        let app = InstalledAppId::new("app_xyz");
        let profile = ProfileId::new("default");
        let key = derive_install_profile_key(&app, &profile);
        let exec = make_exec_id();

        let ck1 = derive_capsule_instance_key(&key, &InstallRevisionId::new("rev_1"), &exec);
        let ck2 = derive_capsule_instance_key(&key, &InstallRevisionId::new("rev_2"), &exec);
        assert_ne!(
            ck1, ck2,
            "different revisions must produce different instance keys"
        );
    }

    #[test]
    fn capsule_instance_key_changes_with_execution_id() {
        let app = InstalledAppId::new("app_xyz");
        let profile = ProfileId::new("default");
        let key = derive_install_profile_key(&app, &profile);
        let rev = InstallRevisionId::new("rev_42");

        let exec1 = ExecutionId::new(format!("exec_{}", "1".repeat(32)));
        let exec2 = ExecutionId::new(format!("exec_{}", "2".repeat(32)));

        let ck1 = derive_capsule_instance_key(&key, &rev, &exec1);
        let ck2 = derive_capsule_instance_key(&key, &rev, &exec2);
        assert_ne!(
            ck1, ck2,
            "same revision but different execution ids must produce different instance keys"
        );
    }

    #[test]
    fn capsule_instance_key_stable_for_same_triple() {
        let app = InstalledAppId::new("app_stable");
        let profile = ProfileId::new("default");
        let key = derive_install_profile_key(&app, &profile);
        let rev = InstallRevisionId::new("rev_42");
        let exec = make_exec_id();

        let ck1 = derive_capsule_instance_key(&key, &rev, &exec);
        let ck2 = derive_capsule_instance_key(&key, &rev, &exec);
        assert_eq!(ck1, ck2, "same triple must produce stable instance key");
    }

    #[test]
    fn capsule_instance_key_has_cik_prefix() {
        let app = InstalledAppId::new("app_prefix");
        let profile = ProfileId::new("default");
        let key = derive_install_profile_key(&app, &profile);
        let rev = InstallRevisionId::new("rev_1");
        let exec = make_exec_id();
        let ck = derive_capsule_instance_key(&key, &rev, &exec);
        assert!(ck.as_str().starts_with("cik_"), "must have cik_ prefix");
        assert_eq!(ck.as_str().len(), 4 + 32, "cik_ + 32 hex expected");
    }

    // ── shortcut / dashboard uses install_profile_key ─────────────────────

    #[test]
    fn shortcut_uses_install_profile_key_not_instance_key() {
        // Simulate: shortcut is created at rev_1, app is updated to rev_2.
        // The shortcut's stored key must still resolve.
        let app = InstalledAppId::new("app_shortcut");
        let profile = ProfileId::new("default");
        let key_at_install = derive_install_profile_key(&app, &profile);
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
        let exec = make_exec_id();
        let instance_key = derive_capsule_instance_key(&key, &rev, &exec);
        assert!(!instance_key.as_str().is_empty());
    }

    // ── revision_id_for_build ──────────────────────────────────────────────

    #[test]
    fn revision_id_deterministic() {
        let build_id = ArtifactBuildId::new(format!("build_{}", "a".repeat(64)));
        let r1 = revision_id_for_build(&build_id);
        let r2 = revision_id_for_build(&build_id);
        assert_eq!(r1, r2, "same build id must always produce same revision id");
        assert!(r1.as_str().starts_with("rev_"), "must have rev_ prefix");
        assert_eq!(r1.as_str().len(), 4 + 32, "rev_ + 32 hex expected");
    }

    #[test]
    fn revision_id_different_for_different_builds() {
        let b1 = ArtifactBuildId::new(format!("build_{}", "a".repeat(64)));
        let b2 = ArtifactBuildId::new(format!("build_{}", "b".repeat(64)));
        assert_ne!(revision_id_for_build(&b1), revision_id_for_build(&b2));
    }

    // ── artifact_build_id validation ───────────────────────────────────────

    #[test]
    fn artifact_build_id_valid() {
        let id = ArtifactBuildId::new(format!("build_{}", "a".repeat(64)));
        assert!(id.is_valid());
    }

    #[test]
    fn artifact_build_id_invalid_when_exec_prefix() {
        let id = ArtifactBuildId::new(format!("exec_{}", "a".repeat(64)));
        assert!(!id.is_valid(), "build id must not have exec_ prefix");
    }

    #[test]
    fn artifact_build_id_invalid_when_hex_too_short() {
        let id = ArtifactBuildId::new("build_abc");
        assert!(!id.is_valid(), "build id hex part must be 64 chars");
    }

    #[test]
    fn artifact_build_id_invalid_when_uppercase_hex() {
        let id = ArtifactBuildId::new(format!("build_{}", "A".repeat(64)));
        assert!(!id.is_valid(), "build id hex must be lowercase");
    }

    // ── execution_id validation ────────────────────────────────────────────

    #[test]
    fn execution_id_valid() {
        let id = ExecutionId::new(format!("exec_{}", "a".repeat(32)));
        assert!(id.is_valid());
    }

    #[test]
    fn execution_id_invalid_when_build_prefix() {
        let id = ExecutionId::new(format!("build_{}", "a".repeat(32)));
        assert!(!id.is_valid());
    }

    #[test]
    fn execution_id_invalid_when_hex_too_short() {
        let id = ExecutionId::new("exec_abc");
        assert!(!id.is_valid());
    }

    #[test]
    fn execution_id_looks_like() {
        assert!(ExecutionId::looks_like("exec_123"));
        assert!(!ExecutionId::looks_like("build_123"));
        assert!(!ExecutionId::looks_like("rev_123"));
    }

    #[test]
    fn execution_id_invalid_when_uppercase_hex() {
        let id = ExecutionId::new(format!("exec_{}", "A".repeat(32)));
        assert!(!id.is_valid(), "execution id hex must be lowercase");
    }

    // ── path_safe_app_id ───────────────────────────────────────────────────

    #[test]
    fn path_safe_app_id_has_app_prefix() {
        let id = path_safe_app_id("koh0920/my-app");
        assert!(id.as_str().starts_with("app_"), "must have app_ prefix");
        assert_eq!(id.as_str().len(), 4 + 32, "app_ + 32 hex expected");
    }

    #[test]
    fn path_safe_app_id_is_deterministic() {
        let a = path_safe_app_id("koh0920/my-app");
        let b = path_safe_app_id("koh0920/my-app");
        assert_eq!(a, b);
    }

    #[test]
    fn path_safe_app_id_differs_for_different_scopes() {
        let a = path_safe_app_id("koh0920/foo");
        let b = path_safe_app_id("koh0920/bar");
        assert_ne!(a, b);
    }

    #[test]
    fn path_safe_app_id_no_slash() {
        let id = path_safe_app_id("publisher/slug");
        assert!(
            !id.as_str().contains('/'),
            "path-safe id must not contain '/'"
        );
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

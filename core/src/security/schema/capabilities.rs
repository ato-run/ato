//! Canonical capability descriptors for ato capsules.
//!
//! These four fields are the single source of truth used by:
//! - `capsule.toml [requirements.capabilities]` (this crate)
//! - `api.ato.run/v1/search` capability filters (web-api, vendored copy)
//! - `SKILL.md` enum vocabulary (agent-facing, vendored copy)
//! - future `ato encap` / `ato validate` lints
//!
//! The JSON Schema equivalent lives at
//! `apps/ato-cli/core/schema/capabilities.schema.json` (regenerated from this
//! module by `cargo run -p capsule-core --bin export_capabilities_schema`).
//!
//! Reconciliation with the older `health.toml` `network_mode` field is
//! documented in `apps/ato-cli/core/src/schema/README.md`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Schema version for the capabilities block.
///
/// Bump on breaking changes to any enum variant. Clients (SKILL.md, web-api
/// prompt) pin a specific version so they can refuse to translate requests
/// against an unknown schema.
pub const SCHEMA_VERSION: &str = "1";

/// Newtype for the schema version (so it shows up in the generated JSON
/// Schema as a dedicated definition rather than a raw string).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(transparent)]
pub struct SchemaVersion(pub String);

impl Default for SchemaVersion {
    fn default() -> Self {
        SchemaVersion(SCHEMA_VERSION.to_string())
    }
}

/// Network capability of a capsule.
///
/// Determines what kind of network access the sandbox grants. This is a
/// lossy projection of the finer-grained `health.toml` `network_mode` field
/// (see README.md for the mapping).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Network {
    /// No network access at all.
    None,
    /// Outbound only (allowlisted domains or unrestricted egress).
    Egress,
    /// Inbound server (capsule listens on a port but does not call out).
    Ingress,
    /// Both inbound and outbound.
    Bidirectional,
}

/// Filesystem write capability of a capsule.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum FsWrites {
    /// Purely read-only.
    None,
    /// Writes only to ephemeral scratch / temp storage scoped to the run.
    Scratch,
    /// Writes to persistent user data (documents, project files, app state).
    UserData,
    /// Writes to system-owned locations (root, config dirs, global caches).
    System,
}

/// Coarse-grained side-effect classification used for search-time filtering
/// and UI warnings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum SideEffects {
    /// No observable side effects outside the sandbox.
    Readonly,
    /// Side effects are safe to repeat (same inputs → same final state).
    Idempotent,
    /// Modifies external state, not safe to repeat without thought.
    Mutating,
    /// Can delete, overwrite, or otherwise destroy data.
    Destructive,
}

/// Full capability descriptor attached to a capsule.
///
/// This is an additive, optional block on `CapsuleRequirements`; absence
/// means "not declared" and must not be treated as any particular level.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct Capabilities {
    /// Network access required by the capsule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,

    /// Filesystem write surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs_writes: Option<FsWrites>,

    /// Side-effect classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_effects: Option<SideEffects>,

    /// Whether the capsule requires any secrets (API keys, tokens) at run
    /// time. Used by search to filter to capsules that run without setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets_required: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_roundtrip() {
        for v in [
            Network::None,
            Network::Egress,
            Network::Ingress,
            Network::Bidirectional,
        ] {
            let s = serde_json::to_string(&v).unwrap();
            let back: Network = serde_json::from_str(&s).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn network_kebab_case_wire() {
        assert_eq!(
            serde_json::to_string(&Network::Bidirectional).unwrap(),
            "\"bidirectional\""
        );
        let v: Network = serde_json::from_str("\"egress\"").unwrap();
        assert_eq!(v, Network::Egress);
    }

    #[test]
    fn fs_writes_roundtrip() {
        for v in [
            FsWrites::None,
            FsWrites::Scratch,
            FsWrites::UserData,
            FsWrites::System,
        ] {
            let s = serde_json::to_string(&v).unwrap();
            let back: FsWrites = serde_json::from_str(&s).unwrap();
            assert_eq!(v, back);
        }
        assert_eq!(
            serde_json::to_string(&FsWrites::UserData).unwrap(),
            "\"user-data\""
        );
    }

    #[test]
    fn side_effects_roundtrip() {
        for v in [
            SideEffects::Readonly,
            SideEffects::Idempotent,
            SideEffects::Mutating,
            SideEffects::Destructive,
        ] {
            let s = serde_json::to_string(&v).unwrap();
            let back: SideEffects = serde_json::from_str(&s).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn capabilities_partial_deserialize() {
        let json = r#"{"network":"none","secrets_required":false}"#;
        let caps: Capabilities = serde_json::from_str(json).unwrap();
        assert_eq!(caps.network, Some(Network::None));
        assert_eq!(caps.fs_writes, None);
        assert_eq!(caps.side_effects, None);
        assert_eq!(caps.secrets_required, Some(false));
    }

    #[test]
    fn capabilities_empty_deserialize() {
        let caps: Capabilities = serde_json::from_str("{}").unwrap();
        assert_eq!(caps, Capabilities::default());
    }

    #[test]
    fn capabilities_serialize_skips_none() {
        let caps = Capabilities {
            network: Some(Network::None),
            ..Default::default()
        };
        let s = serde_json::to_string(&caps).unwrap();
        assert_eq!(s, r#"{"network":"none"}"#);
    }

    #[test]
    fn capabilities_toml_roundtrip() {
        // Golden case: capsule.toml fragment with [capabilities] block.
        let toml_src = r#"
network = "egress"
fs_writes = "user-data"
side_effects = "mutating"
secrets_required = true
"#;
        let caps: Capabilities = toml::from_str(toml_src).unwrap();
        assert_eq!(caps.network, Some(Network::Egress));
        assert_eq!(caps.fs_writes, Some(FsWrites::UserData));
        assert_eq!(caps.side_effects, Some(SideEffects::Mutating));
        assert_eq!(caps.secrets_required, Some(true));
    }

    #[test]
    fn unknown_enum_variant_rejected() {
        let err = serde_json::from_str::<Network>("\"lan-only\"");
        assert!(err.is_err(), "unknown variant must fail to parse");
    }

    #[test]
    fn schema_generation_covers_all_fields() {
        let schema = schemars::schema_for!(Capabilities);
        let s = serde_json::to_string(&schema).unwrap();
        // Sanity: schema contains every top-level field name.
        for field in ["network", "fs_writes", "side_effects", "secrets_required"] {
            assert!(
                s.contains(field),
                "generated schema missing field `{}`: {}",
                field,
                s
            );
        }
        // Sanity: at least one enum variant string appears.
        assert!(s.contains("bidirectional"));
        assert!(s.contains("destructive"));
    }
}

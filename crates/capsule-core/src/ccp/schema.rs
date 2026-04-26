//! Capsule Control Protocol (CCP) envelope wire-shape helpers.
//!
//! Today this module exposes [`CcpHeader`], a payload-agnostic
//! deserialization helper for extracting `schema_version` from any CCP
//! envelope. Full envelope structs (`ResolveEnvelope`,
//! `SessionStartEnvelope`, `SessionStopEnvelope`,
//! `StatusEnvelope`/`BootstrapEnvelope`/`RepairEnvelope`) are still
//! defined in their producer/consumer crates; they will consolidate here
//! in a later monorepo phase once the manifest types also live in
//! `capsule-core` (see `docs/monorepo-consolidation-plan.md` §M5).

use serde::Deserialize;

/// Generic helper for deserializing the top-level `schema_version` from
/// any CCP envelope without committing to a specific payload shape — used
/// by tests and tolerance checks that want to verify the wire field is
/// plumbed through.
#[derive(Debug, Deserialize)]
pub struct CcpHeader {
    #[serde(default)]
    pub schema_version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ccp_header_deserializes_optional_field() {
        let with: CcpHeader =
            serde_json::from_str(r#"{"schema_version":"ccp/v1","other":42}"#).unwrap();
        assert_eq!(with.schema_version.as_deref(), Some("ccp/v1"));

        let without: CcpHeader = serde_json::from_str(r#"{"other":42}"#).unwrap();
        assert!(without.schema_version.is_none());
    }
}

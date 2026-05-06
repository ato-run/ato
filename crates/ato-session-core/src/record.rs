//! On-disk session record schema. Mirrors what `ato-cli` writes after
//! a successful `ato app session start`, and what `ato-desktop` reads
//! during the Phase 1 fast-path validation (RFC v0.3 §3.2).
//!
//! Schema is **forward-compatible**: older record files (lacking
//! `schema_version` / `launch_digest` / `process_start_time_unix_ms`)
//! deserialize cleanly via `#[serde(default)]` and are treated as
//! `schema_version=1` (reuse-incompatible) by the App Session
//! Materialization layer. Fast-path callers MUST gate reuse on
//! `schema_version >= SCHEMA_VERSION_V2`.

use capsule_wire::handle::{
    CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, ResolvedSnapshot, TrustState,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Minimum schema version that supports session reuse. Records below
/// this version are display-only and MUST NOT be reused (§3.2).
pub const SCHEMA_VERSION_V2: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSessionInfo {
    pub session_id: String,
    pub handle: String,
    pub normalized_handle: String,
    pub canonical_handle: Option<String>,
    pub trust_state: TrustState,
    pub source: Option<String>,
    pub restricted: bool,
    pub snapshot: Option<ResolvedSnapshot>,
    pub runtime: CapsuleRuntimeDescriptor,
    pub display_strategy: CapsuleDisplayStrategy,
    pub pid: i32,
    pub log_path: String,
    pub manifest_path: String,
    pub target_label: String,
    pub notes: Vec<String>,
    pub guest: Option<GuestSessionDisplay>,
    pub web: Option<WebSessionDisplay>,
    pub terminal: Option<TerminalSessionDisplay>,
    pub service: Option<ServiceBackgroundDisplay>,
    /// Embedded dependency-contract snapshot for crash recovery. This lets
    /// `ato app session stop <id>` tear down providers even when the
    /// `~/.ato/run-sessions/<id>/graph.json` sidecar is missing or unreadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_contracts: Option<StoredDependencyContracts>,

    // App Session Materialization v0 (RFC: APP_SESSION_MATERIALIZATION).
    // All three are `Option` for forward compatibility with schema=1
    // records; a record missing any of them is treated as
    // reuse-ineligible.
    /// Schema version. Records written by v0 set this to `Some(2)`.
    /// Older records (or hand-written ones) leave it as `None` and are
    /// treated as schema=1 — they MAY be displayed but MUST NOT be
    /// reused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    /// blake3 launch digest of the LaunchSpec used to start this session.
    /// Reuse requires this to match the current LaunchSpec digest exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_digest: Option<String>,
    /// Process creation time (unix ms). Used in conjunction with PID
    /// alive to defeat OS PID reuse. macOS / Linux only at v0; unknown
    /// on Windows and serializes as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_start_time_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestSessionDisplay {
    pub adapter: String,
    pub frontend_entry: String,
    pub transport: String,
    pub healthcheck_url: String,
    pub invoke_url: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSessionDisplay {
    pub local_url: String,
    pub healthcheck_url: String,
    pub served_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSessionDisplay {
    pub log_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceBackgroundDisplay {
    pub log_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDependencyContracts {
    pub consumer_pid: i32,
    #[serde(default)]
    pub providers: Vec<StoredDependencyProvider>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDependencyProvider {
    pub alias: String,
    pub pid: i32,
    pub state_dir: PathBuf,
    pub resolved: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_export_keys: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A schema=1 record (no schema_version / launch_digest / start_time)
    /// must round-trip through serde without losing any of the legacy
    /// fields, and must come back with the three new fields as `None`.
    fn make_runtime() -> CapsuleRuntimeDescriptor {
        CapsuleRuntimeDescriptor {
            target_label: "main".to_string(),
            runtime: Some("node".to_string()),
            driver: None,
            language: None,
            port: None,
        }
    }

    /// A schema=1 record (no schema_version / launch_digest / start_time)
    /// must round-trip through serde without losing any of the legacy
    /// fields, and must come back with the three new fields as `None`.
    /// `trust_state` / `display_strategy` use snake_case wire form.
    #[test]
    fn schema_v1_round_trips_with_none_materialization_fields() {
        let json = r#"{
            "session_id": "ato-desktop-session-1",
            "handle": "publisher/slug",
            "normalized_handle": "publisher/slug",
            "canonical_handle": null,
            "trust_state": "untrusted",
            "source": null,
            "restricted": false,
            "snapshot": null,
            "runtime": {
                "target_label": "main",
                "runtime": "node",
                "driver": null,
                "language": null,
                "port": null
            },
            "display_strategy": "guest_webview",
            "pid": 1234,
            "log_path": "/tmp/x.log",
            "manifest_path": "/tmp/manifest.toml",
            "target_label": "main",
            "notes": [],
            "guest": null,
            "web": null,
            "terminal": null,
            "service": null
        }"#;
        let parsed: StoredSessionInfo = serde_json::from_str(json).expect("parse v1");
        assert_eq!(parsed.session_id, "ato-desktop-session-1");
        assert!(matches!(parsed.trust_state, TrustState::Untrusted));
        assert!(matches!(
            parsed.display_strategy,
            CapsuleDisplayStrategy::GuestWebview
        ));
        assert!(parsed.schema_version.is_none());
        assert!(parsed.launch_digest.is_none());
        assert!(parsed.process_start_time_unix_ms.is_none());
        assert!(parsed.dependency_contracts.is_none());

        let reserialized = serde_json::to_string(&parsed).expect("reserialize");
        // None fields are skipped on serialize (skip_serializing_if), so a
        // v1 record stays a v1 record after round-trip.
        assert!(!reserialized.contains("schema_version"));
        assert!(!reserialized.contains("launch_digest"));
        assert!(!reserialized.contains("process_start_time_unix_ms"));
        assert!(!reserialized.contains("dependency_contracts"));
    }

    #[test]
    fn schema_v2_round_trips_with_all_fields_present() {
        let original = StoredSessionInfo {
            session_id: "ato-desktop-session-99".to_string(),
            handle: "publisher/slug".to_string(),
            normalized_handle: "publisher/slug".to_string(),
            canonical_handle: Some("publisher/slug".to_string()),
            trust_state: TrustState::Trusted,
            source: Some("registry".to_string()),
            restricted: false,
            snapshot: None,
            runtime: make_runtime(),
            display_strategy: CapsuleDisplayStrategy::GuestWebview,
            pid: 4242,
            log_path: "/tmp/x.log".to_string(),
            manifest_path: "/tmp/manifest.toml".to_string(),
            target_label: "main".to_string(),
            notes: vec!["test".to_string()],
            guest: Some(GuestSessionDisplay {
                adapter: "node".to_string(),
                frontend_entry: "index.html".to_string(),
                transport: "http".to_string(),
                healthcheck_url: "http://127.0.0.1:5000/health".to_string(),
                invoke_url: "http://127.0.0.1:5000/invoke".to_string(),
                capabilities: vec!["fs:read".to_string()],
            }),
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: Some(StoredDependencyContracts {
                consumer_pid: 4242,
                providers: vec![StoredDependencyProvider {
                    alias: "db".to_string(),
                    pid: 5252,
                    state_dir: PathBuf::from("/tmp/db"),
                    resolved: "capsule://github.com/Koh0920/ato-postgres@main".to_string(),
                    allocated_port: Some(5432),
                    log_path: Some(PathBuf::from("/tmp/db.log")),
                    runtime_export_keys: vec!["DATABASE_URL".to_string()],
                }],
            }),
            schema_version: Some(SCHEMA_VERSION_V2),
            launch_digest: Some("a".repeat(64)),
            process_start_time_unix_ms: Some(1_700_000_000_000),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: StoredSessionInfo = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.session_id, original.session_id);
        assert_eq!(parsed.schema_version, Some(SCHEMA_VERSION_V2));
        assert_eq!(parsed.launch_digest.as_deref(), Some(&"a".repeat(64)[..]));
        assert_eq!(parsed.process_start_time_unix_ms, Some(1_700_000_000_000));
        assert_eq!(
            parsed
                .dependency_contracts
                .as_ref()
                .and_then(|snapshot| snapshot.providers.first())
                .map(|provider| provider.alias.as_str()),
            Some("db")
        );
    }
}

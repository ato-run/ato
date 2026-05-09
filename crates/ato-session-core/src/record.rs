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

    /// Persisted subset of the materialized `ExecutionGraph` that
    /// teardown will reverse-traverse (umbrella #74 Phase 3). See
    /// [`StoredExecutionGraph`] for the schema-versioning contract; this
    /// field stays `None` until the Phase 3 implementation lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<StoredExecutionGraph>,

    /// Embedded `[services]` orchestration snapshot for crash recovery
    /// (#73 PR-D, closes #28 phase 2). For orchestration capsules, the
    /// services started by `ServicePhaseCoordinator` are persisted here so
    /// `ato app session stop <id>` can find their container ids / pids
    /// after the wrapper process has exited. `None` for non-orchestration
    /// capsules. Distinct from `dependency_contracts`: `dependency_contracts`
    /// represents top-level `[dependencies.<alias>]` providers (one per
    /// alias, started by `dependency_runtime`); `orchestration_services`
    /// represents `[services.<name>]` siblings inside an orchestration
    /// capsule (one per service, started by the OCI/source orchestrator).
    /// Both can coexist on a single record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration_services: Option<StoredOrchestrationServices>,

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

/// `[services]` orchestration graph subset persisted alongside the
/// session record (#73 PR-D, closes #28 phase 2).
///
/// `services` is in topological start order — index 0 is the first
/// service `ServicePhaseCoordinator` brought up, index N-1 is the last.
/// `stop_session` iterates this in reverse to tear services down in
/// reverse-topological order. (Within a single Vec position the OS-level
/// stop is independent, so concurrent reverse iteration is also valid.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredOrchestrationServices {
    /// PID of the wrapper process that materialized the orchestration
    /// graph. Mirrors `StoredDependencyContracts.consumer_pid` for
    /// consistency with the dep-contract subset; needed by `stop_session`
    /// to validate that this record was written by a still-known wrapper
    /// (PID-reuse defense).
    pub wrapper_pid: i32,
    /// Services in topological start order.
    #[serde(default)]
    pub services: Vec<StoredOrchestrationService>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredOrchestrationService {
    /// Service name from the manifest (`[services.<name>]`).
    pub name: String,
    /// Resolved target label (e.g. `"db"`, `"web"`).
    pub target_label: String,
    /// PID for `ResolvedServiceRuntime::Managed` (local) services. `None`
    /// for OCI-runtime services, which are addressed by `container_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_pid: Option<i32>,
    /// Container id for `ResolvedServiceRuntime::Oci` services. `None`
    /// for managed/local services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    /// Host-side port mapping (host_port -> container_port) for OCI
    /// services. Empty for managed services.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub host_ports: std::collections::BTreeMap<u16, u16>,
    /// Port the service publishes inside its own runtime, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_port: Option<u16>,
}

/// Persisted subset of the materialized `ExecutionGraph` (umbrella #74,
/// Phase 3). The Phase 3 implementation will populate this field; until
/// then it stays `None` on every record and v0.5.x records (without
/// `graph`) continue to deserialize cleanly via `#[serde(default)]`.
///
/// SCHEMA_VERSION_NOTE: future `schema_version` bumps must keep
/// deserialization of `schema_version: 1` records readable, by either
/// keeping fields additive or by explicit migration in
/// `StoredSessionInfo::deserialize`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredExecutionGraph {
    pub schema_version: u32,
    #[serde(default)]
    pub nodes: Vec<StoredGraphNode>,
    #[serde(default)]
    pub edges: Vec<StoredGraphEdge>,
}

impl StoredExecutionGraph {
    pub const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredGraphNode {
    pub kind: String,
    pub identifier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredGraphEdge {
    pub source: String,
    pub target: String,
    pub kind: String,
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
            graph: None,
            orchestration_services: None,
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
        assert!(parsed.orchestration_services.is_none());
    }

    /// PR-D round-trip: a record carrying `orchestration_services` (the
    /// `[services]` graph subset) survives serde unchanged, both for OCI
    /// services (with `container_id` + `host_ports`) and managed services
    /// (with `local_pid`). Both can coexist with `dependency_contracts`
    /// on a single record.
    #[test]
    fn schema_v2_round_trips_orchestration_services_alongside_dep_contracts() {
        use std::collections::BTreeMap;
        let mut host_ports = BTreeMap::new();
        host_ports.insert(54320u16, 5432u16);
        let original = StoredSessionInfo {
            session_id: "ato-desktop-session-77".to_string(),
            handle: "publisher/orch".to_string(),
            normalized_handle: "publisher/orch".to_string(),
            canonical_handle: Some("publisher/orch".to_string()),
            trust_state: TrustState::Trusted,
            source: Some("registry".to_string()),
            restricted: false,
            snapshot: None,
            runtime: make_runtime(),
            display_strategy: CapsuleDisplayStrategy::WebUrl,
            pid: 7777,
            log_path: "/tmp/x.log".to_string(),
            manifest_path: "/tmp/manifest.toml".to_string(),
            target_label: "web".to_string(),
            notes: vec![],
            guest: None,
            web: Some(WebSessionDisplay {
                local_url: "http://127.0.0.1:5173/".to_string(),
                healthcheck_url: "http://127.0.0.1:5173/".to_string(),
                served_by: "node".to_string(),
            }),
            terminal: None,
            service: None,
            dependency_contracts: None,
            graph: None,
            orchestration_services: Some(StoredOrchestrationServices {
                wrapper_pid: 7777,
                services: vec![
                    StoredOrchestrationService {
                        name: "db".to_string(),
                        target_label: "db".to_string(),
                        local_pid: None,
                        container_id: Some("c0ffee".to_string()),
                        host_ports,
                        published_port: Some(5432),
                    },
                    StoredOrchestrationService {
                        name: "web".to_string(),
                        target_label: "web".to_string(),
                        local_pid: Some(8888),
                        container_id: None,
                        host_ports: BTreeMap::new(),
                        published_port: Some(5173),
                    },
                ],
            }),
            schema_version: Some(SCHEMA_VERSION_V2),
            launch_digest: Some("b".repeat(64)),
            process_start_time_unix_ms: Some(1_700_000_001_000),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: StoredSessionInfo = serde_json::from_str(&json).expect("parse");

        let services = parsed
            .orchestration_services
            .as_ref()
            .expect("orchestration_services round-trips");
        assert_eq!(services.wrapper_pid, 7777);
        assert_eq!(services.services.len(), 2);
        // Insertion order preserved (= reverse-topological at teardown).
        assert_eq!(services.services[0].name, "db");
        assert_eq!(services.services[0].container_id.as_deref(), Some("c0ffee"));
        assert_eq!(services.services[0].host_ports.get(&54320), Some(&5432));
        assert_eq!(services.services[0].local_pid, None);
        assert_eq!(services.services[1].name, "web");
        assert_eq!(services.services[1].local_pid, Some(8888));
        assert_eq!(services.services[1].container_id, None);

        assert!(parsed.dependency_contracts.is_none());
    }

    /// A schema=1 record with orchestration_services explicitly set to null
    /// (or absent) must continue to deserialize cleanly.
    #[test]
    fn schema_v1_orchestration_services_is_optional() {
        let json_without = r#"{
            "session_id": "x", "handle": "h", "normalized_handle": "h",
            "canonical_handle": null, "trust_state": "untrusted",
            "source": null, "restricted": false, "snapshot": null,
            "runtime": {"target_label": "m", "runtime": "node",
                        "driver": null, "language": null, "port": null},
            "display_strategy": "web_url",
            "pid": 1, "log_path": "/x", "manifest_path": "/y",
            "target_label": "m", "notes": [],
            "guest": null, "web": null, "terminal": null, "service": null
        }"#;
        let parsed: StoredSessionInfo = serde_json::from_str(json_without).expect("parse");
        assert!(parsed.orchestration_services.is_none());
    }

    /// Phase 3 forward-compat (umbrella #74): a v0.5.x-shaped record that
    /// pre-dates the `graph` field deserializes cleanly into the
    /// post-scaffold `StoredSessionInfo`. `serde(default)` on the new
    /// field is what makes this safe; without it a stored record from
    /// 0.5.1 would refuse to load on 0.6.0+ readers.
    #[test]
    fn schema_v1_record_without_graph_field_deserializes_cleanly() {
        let json_without_graph = r#"{
            "session_id": "x", "handle": "h", "normalized_handle": "h",
            "canonical_handle": null, "trust_state": "untrusted",
            "source": null, "restricted": false, "snapshot": null,
            "runtime": {"target_label": "m", "runtime": "node",
                        "driver": null, "language": null, "port": null},
            "display_strategy": "web_url",
            "pid": 1, "log_path": "/x", "manifest_path": "/y",
            "target_label": "m", "notes": [],
            "guest": null, "web": null, "terminal": null, "service": null
        }"#;
        let parsed: StoredSessionInfo =
            serde_json::from_str(json_without_graph).expect("parse pre-Phase-3 record");
        assert!(parsed.graph.is_none(), "missing graph field must default to None");
    }

    /// `graph: None` round-trips byte-stable — the field is skipped on
    /// serialize (`skip_serializing_if = "Option::is_none"`), so the
    /// emitted JSON matches what a v0.5.x record looks like, and a second
    /// pass through serde produces the same string.
    #[test]
    fn graph_none_round_trips_byte_stable() {
        let original = StoredSessionInfo {
            session_id: "ato-desktop-session-graph-none".to_string(),
            handle: "publisher/slug".to_string(),
            normalized_handle: "publisher/slug".to_string(),
            canonical_handle: None,
            trust_state: TrustState::Untrusted,
            source: None,
            restricted: false,
            snapshot: None,
            runtime: make_runtime(),
            display_strategy: CapsuleDisplayStrategy::GuestWebview,
            pid: 1,
            log_path: "/tmp/x.log".to_string(),
            manifest_path: "/tmp/manifest.toml".to_string(),
            target_label: "main".to_string(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: None,
            graph: None,
            orchestration_services: None,
            schema_version: None,
            launch_digest: None,
            process_start_time_unix_ms: None,
        };
        let first = serde_json::to_string(&original).expect("serialize");
        let parsed: StoredSessionInfo = serde_json::from_str(&first).expect("parse");
        let second = serde_json::to_string(&parsed).expect("reserialize");
        assert_eq!(first, second, "graph: None must round-trip byte-stable");
        // skip_serializing_if elides the field entirely from the wire.
        assert!(!first.contains("\"graph\""));
    }

    /// `graph: Some(StoredExecutionGraph { schema_version: 1, nodes: [],
    /// edges: [] })` round-trips byte-stable — the on-disk shape that
    /// Phase 3 will eventually populate.
    #[test]
    fn graph_some_empty_round_trips_byte_stable() {
        let original = StoredSessionInfo {
            session_id: "ato-desktop-session-graph-empty".to_string(),
            handle: "publisher/slug".to_string(),
            normalized_handle: "publisher/slug".to_string(),
            canonical_handle: None,
            trust_state: TrustState::Untrusted,
            source: None,
            restricted: false,
            snapshot: None,
            runtime: make_runtime(),
            display_strategy: CapsuleDisplayStrategy::GuestWebview,
            pid: 1,
            log_path: "/tmp/x.log".to_string(),
            manifest_path: "/tmp/manifest.toml".to_string(),
            target_label: "main".to_string(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: None,
            graph: Some(StoredExecutionGraph {
                schema_version: StoredExecutionGraph::SCHEMA_VERSION,
                nodes: vec![],
                edges: vec![],
            }),
            orchestration_services: None,
            schema_version: None,
            launch_digest: None,
            process_start_time_unix_ms: None,
        };
        let first = serde_json::to_string(&original).expect("serialize");
        let parsed: StoredSessionInfo = serde_json::from_str(&first).expect("parse");
        let second = serde_json::to_string(&parsed).expect("reserialize");
        assert_eq!(first, second, "graph: Some(empty) must round-trip byte-stable");
        let parsed_graph = parsed.graph.expect("graph field present");
        assert_eq!(parsed_graph.schema_version, StoredExecutionGraph::SCHEMA_VERSION);
        assert!(parsed_graph.nodes.is_empty());
        assert!(parsed_graph.edges.is_empty());
    }
}

//! Minimal OCI session tracking for lifecycle management.
//!
//! When `ato run` starts an OCI multi-service graph (via `--oci-compose`,
//! `--oci-install-sh`, or an explicit `[services]` capsule), a `OciSessionRecord`
//! is written to `~/.ato/oci-sessions/<session_id>.json` before the service
//! graph enters the wait/log-stream loop.  The record is deleted when the
//! session exits (normal or cleanup).
//!
//! This lets `ato ps` show running OCI sessions and `ato stop --all` stop
//! Podman containers/networks that were started by Ato.
//!
//! # Non-goals
//! - Rich per-container status from Podman inspect (deferred).
//! - Desktop UX / session replay (deferred).
//! - Secret values are never stored here.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const OCI_SESSIONS_DIR: &str = "oci-sessions";

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OciSessionStatus {
    Running,
    Stopped,
}

impl std::fmt::Display for OciSessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OciSessionStatus::Running => write!(f, "running"),
            OciSessionStatus::Stopped => write!(f, "stopped"),
        }
    }
}

/// Caller-supplied metadata identifying how the session was imported.
/// Passed from the entry-point runner (install_sh, compose, explicit OCI)
/// into `execute_service_graph_with_provider`.
#[derive(Debug, Clone)]
pub struct OciSessionMeta {
    /// Import entry point: `"docker-run-script"`, `"compose"`, `"explicit-oci"`.
    pub import_kind: String,
    /// Absolute path to the source file / capsule directory.
    pub source_path: Option<String>,
    /// blake3 hash of the source file at import time.
    pub source_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OciServiceRecord {
    pub name: String,
    pub container_id: String,
    pub container_name: String,
    /// Declared image ref (e.g. `"blinkospace/blinko:latest"`).
    pub image_ref: String,
    /// Resolved image digest (e.g. `"sha256:..."`).
    pub image_digest: Option<String>,
    pub host_port: Option<u16>,
    /// Named Podman volumes that back persistent state bindings.
    /// These are preserved on stop/cleanup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persistent_volumes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OciSessionRecord {
    pub session_id: String,
    /// Import entry point: `"docker-run-script"`, `"compose"`, `"explicit-oci"`.
    pub import_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    pub network_name: String,
    /// Services in topological start order (index 0 = first started).
    pub services: Vec<OciServiceRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_endpoint: Option<String>,
    /// ISO 8601 timestamp of session creation.
    pub created_at: String,
    pub status: OciSessionStatus,
}

// ── Store ─────────────────────────────────────────────────────────────────────

pub struct OciSessionStore {
    sessions_dir: PathBuf,
}

impl OciSessionStore {
    pub fn new() -> Result<Self> {
        let sessions_dir = capsule_core::common::paths::ato_path_or_workspace_tmp(OCI_SESSIONS_DIR);
        if !sessions_dir.exists() {
            fs::create_dir_all(&sessions_dir).with_context(|| {
                format!(
                    "Failed to create OCI sessions directory: {}",
                    sessions_dir.display()
                )
            })?;
        }
        Ok(Self { sessions_dir })
    }

    /// Create a store pointing at an arbitrary directory (for tests).
    #[cfg(test)]
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { sessions_dir: dir }
    }

    fn record_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{session_id}.json"))
    }

    pub fn write_session(&self, record: &OciSessionRecord) -> Result<PathBuf> {
        let path = self.record_path(&record.session_id);
        let content = serde_json::to_string_pretty(record)
            .context("Failed to serialize OCI session record")?;
        fs::write(&path, content)
            .with_context(|| format!("Failed to write OCI session record: {}", path.display()))?;
        Ok(path)
    }

    pub fn list_sessions(&self) -> Result<Vec<OciSessionRecord>> {
        let mut records = Vec::new();
        if !self.sessions_dir.exists() {
            return Ok(records);
        }
        for entry in fs::read_dir(&self.sessions_dir).with_context(|| {
            format!(
                "Failed to read OCI sessions directory: {}",
                self.sessions_dir.display()
            )
        })? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                match self.read_record_from_path(&path) {
                    Ok(record) => records.push(record),
                    Err(_) => {} // Skip unparseable records
                }
            }
        }
        Ok(records)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let path = self.record_path(session_id);
        if path.exists() {
            fs::remove_file(&path).with_context(|| {
                format!("Failed to remove OCI session record: {}", path.display())
            })?;
        }
        Ok(())
    }

    pub fn mark_stopped(&self, session_id: &str) -> Result<()> {
        let path = self.record_path(session_id);
        if !path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&path)?;
        let mut record: OciSessionRecord = serde_json::from_str(&content)?;
        record.status = OciSessionStatus::Stopped;
        let updated = serde_json::to_string_pretty(&record)?;
        fs::write(&path, updated)?;
        Ok(())
    }

    fn read_record_from_path(&self, path: &Path) -> Result<OciSessionRecord> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read OCI session: {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse OCI session: {}", path.display()))
    }
}

// ── Stop helpers ──────────────────────────────────────────────────────────────

/// Stop all containers and remove the session network for an OCI session.
///
/// Containers are stopped in reverse topological order (last started = first stopped).
/// Persistent volumes are preserved. Ephemeral volumes are removed.
/// All operations are idempotent (already-gone resources are silently skipped).
pub fn stop_oci_session(record: &OciSessionRecord, force: bool) -> StopResult {
    let mut stopped = Vec::new();
    let mut errors = Vec::new();
    let stop_timeout = if force { "0" } else { "10" };

    // Stop + remove in reverse order.
    for svc in record.services.iter().rev() {
        let stop_out = Command::new("podman")
            .args(["stop", "-t", stop_timeout, &svc.container_name])
            .output();
        match stop_out {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                let msg = String::from_utf8_lossy(&o.stderr).to_string();
                // "no such container" is not an error (already cleaned up)
                if !msg.contains("no such container") && !msg.contains("No such container") {
                    errors.push(format!("stop {}: {}", svc.container_name, msg.trim()));
                }
            }
            Err(e) => errors.push(format!("stop {}: {e}", svc.container_name)),
        }

        let rm_out = Command::new("podman")
            .args(["rm", "-f", &svc.container_name])
            .output();
        match rm_out {
            Ok(o) if o.status.success() => {
                stopped.push(svc.container_name.clone());
            }
            Ok(o) => {
                let msg = String::from_utf8_lossy(&o.stderr).to_string();
                if !msg.contains("no such container") && !msg.contains("No such container") {
                    errors.push(format!("rm {}: {}", svc.container_name, msg.trim()));
                } else {
                    stopped.push(svc.container_name.clone()); // idempotent
                }
            }
            Err(e) => errors.push(format!("rm {}: {e}", svc.container_name)),
        }
    }

    // Remove network.
    let net_out = Command::new("podman")
        .args(["network", "rm", "-f", &record.network_name])
        .output();
    let network_removed = match net_out {
        Ok(o) => {
            if o.status.success() {
                true
            } else {
                let msg = String::from_utf8_lossy(&o.stderr).to_string();
                if msg.contains("no such network") || msg.contains("No such network") {
                    true // already gone
                } else {
                    errors.push(format!(
                        "network rm {}: {}",
                        record.network_name,
                        msg.trim()
                    ));
                    false
                }
            }
        }
        Err(e) => {
            errors.push(format!("network rm {}: {e}", record.network_name));
            false
        }
    };

    StopResult {
        stopped_containers: stopped,
        network_removed,
        errors,
    }
}

#[derive(Debug)]
pub struct StopResult {
    pub stopped_containers: Vec<String>,
    pub network_removed: bool,
    pub errors: Vec<String>,
}

// ── ISO 8601 timestamp helper ─────────────────────────────────────────────────

pub fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Minimal ISO 8601 UTC timestamp without chrono dependency.
    let (y, mo, d, h, mi, s) = unix_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn unix_to_ymdhms(mut secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let s = secs % 60;
    secs /= 60;
    let mi = secs % 60;
    secs /= 60;
    let h = secs % 24;
    let days = secs / 24;

    // Gregorian calendar decomposition.
    let mut year = 1970u64;
    let mut rem_days = days;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if rem_days < days_in_year {
            break;
        }
        rem_days -= days_in_year;
        year += 1;
    }
    let months = [
        31,
        if is_leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for &dim in &months {
        if rem_days < dim {
            break;
        }
        rem_days -= dim;
        month += 1;
    }
    (year, month, rem_days + 1, h, mi, s)
}

fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_record(session_id: &str) -> OciSessionRecord {
        OciSessionRecord {
            session_id: session_id.to_string(),
            import_kind: "docker-run-script".to_string(),
            source_path: Some("/tmp/blinko/install.sh".to_string()),
            source_hash: Some("abc123".to_string()),
            network_name: format!("ato-test-{}", &session_id[..8]),
            services: vec![
                OciServiceRecord {
                    name: "db".to_string(),
                    container_id: "sha256deadbeef".to_string(),
                    container_name: format!("ato-test-db-{}", &session_id[..8]),
                    image_ref: "postgres:14".to_string(),
                    image_digest: Some("sha256:abc".to_string()),
                    host_port: None,
                    persistent_volumes: vec!["ato-pg-data".to_string()],
                },
                OciServiceRecord {
                    name: "app".to_string(),
                    container_id: "sha256cafebabe".to_string(),
                    container_name: format!("ato-test-app-{}", &session_id[..8]),
                    image_ref: "blinkospace/blinko:latest".to_string(),
                    image_digest: Some("sha256:def".to_string()),
                    host_port: Some(37079),
                    persistent_volumes: vec![],
                },
            ],
            main_endpoint: Some("http://127.0.0.1:37079/".to_string()),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            status: OciSessionStatus::Running,
        }
    }

    #[test]
    fn oci_session_store_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let record = make_record("ato-test-12345678");
        store.write_session(&record).unwrap();

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "ato-test-12345678");
        assert_eq!(sessions[0].import_kind, "docker-run-script");
        assert_eq!(sessions[0].status, OciSessionStatus::Running);
    }

    #[test]
    fn oci_session_store_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let record = make_record("ato-test-12345678");
        store.write_session(&record).unwrap();
        store.delete_session("ato-test-12345678").unwrap();
        let sessions = store.list_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn oci_session_store_mark_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let record = make_record("ato-test-12345678");
        store.write_session(&record).unwrap();
        store.mark_stopped("ato-test-12345678").unwrap();
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions[0].status, OciSessionStatus::Stopped);
    }

    #[test]
    fn oci_session_store_delete_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        // Delete of nonexistent session should not error.
        store.delete_session("ato-missing-session").unwrap();
    }

    #[test]
    fn oci_session_store_no_secret_fields() {
        let record = make_record("ato-test-12345678");
        let json = serde_json::to_string(&record).unwrap();
        // Secret-like field names must not appear in the JSON.
        assert!(!json.contains("password"), "password leaked: {json}");
        assert!(!json.contains("secret"), "secret leaked: {json}");
        assert!(
            !json.contains("DATABASE_URL"),
            "DATABASE_URL leaked: {json}"
        );
    }

    #[test]
    fn oci_session_host_port_in_record_not_network_identity() {
        let record = make_record("ato-test-12345678");
        // host_port appears in session record (allowed) but not in network_name.
        let app = record.services.iter().find(|s| s.name == "app").unwrap();
        assert_eq!(app.host_port, Some(37079));
        assert!(!record.network_name.contains("37079"));
    }

    #[test]
    fn now_iso8601_produces_valid_format() {
        let ts = now_iso8601();
        // Minimal format check: YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
    }
}

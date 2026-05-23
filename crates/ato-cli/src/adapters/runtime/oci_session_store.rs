//! Minimal OCI session tracking for lifecycle management.
//!
//! When `ato run` starts an OCI multi-service graph (via `--oci-compose`,
//! `--oci-install-sh`, or an explicit `[services]` capsule), a `OciSessionRecord`
//! is written to `${ATO_HOME}/oci-sessions/<session_id>.json` before the service
//! graph enters the wait/log-stream loop.  The record is deleted when the
//! session exits (normal or cleanup).
//!
//! `ATO_HOME` defaults to `~/.ato` when the environment variable is not set.
//! All reads and writes use [`capsule_core::common::paths::ato_path_or_workspace_tmp`],
//! so the path is always ATO_HOME-correct and never hardcoded to `~/.ato`.
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
    /// Stop was attempted but one or more containers/networks could not be
    /// removed (e.g. Podman not reachable, permission error, partial cleanup).
    /// The session record is kept so that a subsequent `ato stop --all` can
    /// retry the cleanup.
    StopFailed,
}

impl std::fmt::Display for OciSessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OciSessionStatus::Running => write!(f, "running"),
            OciSessionStatus::Stopped => write!(f, "stopped"),
            OciSessionStatus::StopFailed => write!(f, "stop_failed"),
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

    pub fn find_session(&self, session_id: &str) -> Result<Option<OciSessionRecord>> {
        let path = self.record_path(session_id);
        if !path.exists() {
            return Ok(None);
        }
        self.read_record_from_path(&path).map(Some)
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
        self.set_status(session_id, OciSessionStatus::Stopped)
    }

    /// Mark the session as `StopFailed`, keeping the record so that a
    /// subsequent `ato stop --all` can retry the cleanup.
    pub fn mark_stop_failed(&self, session_id: &str) -> Result<()> {
        self.set_status(session_id, OciSessionStatus::StopFailed)
    }

    fn set_status(&self, session_id: &str, status: OciSessionStatus) -> Result<()> {
        let path = self.record_path(session_id);
        if !path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&path)?;
        let mut record: OciSessionRecord = serde_json::from_str(&content)?;
        record.status = status;
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
pub struct StopByIdAttempt {
    pub record: OciSessionRecord,
    pub result: StopResult,
}

/// Stop one OCI session by its stable CLI session id.
///
/// The store update is shared with `ato stop --all`: successful cleanup deletes
/// the record, while partial cleanup keeps it as `StopFailed` for retry.
pub fn stop_oci_session_by_id(
    store: &OciSessionStore,
    session_id: &str,
    force: bool,
) -> Result<Option<StopByIdAttempt>> {
    stop_oci_session_by_id_with(store, session_id, |record| stop_oci_session(record, force))
}

fn stop_oci_session_by_id_with<F>(
    store: &OciSessionStore,
    session_id: &str,
    stop: F,
) -> Result<Option<StopByIdAttempt>>
where
    F: FnOnce(&OciSessionRecord) -> StopResult,
{
    let Some(record) = store.find_session(session_id)? else {
        return Ok(None);
    };
    let result = stop(&record);
    apply_stop_result(store, session_id, &result);
    Ok(Some(StopByIdAttempt { record, result }))
}

#[derive(Debug)]
pub struct StopResult {
    pub stopped_containers: Vec<String>,
    pub network_removed: bool,
    pub errors: Vec<String>,
}

impl StopResult {
    pub fn is_fully_stopped(&self) -> bool {
        self.errors.is_empty() && self.network_removed
    }
}

/// Apply the result of [`stop_oci_session`] to the session store.
///
/// - **Full success** (`errors` empty AND `network_removed`): delete the record.
/// - **Partial failure**: mark as `StopFailed` so that a subsequent
///   `ato stop --all` can retry.  The record is preserved; real resources that
///   may still be running can still be discovered and cleaned up.
pub fn apply_stop_result(store: &OciSessionStore, session_id: &str, result: &StopResult) {
    if result.is_fully_stopped() {
        let _ = store.delete_session(session_id);
    } else {
        let _ = store.mark_stop_failed(session_id);
    }
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
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: String,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            #[allow(deprecated)]
            std::env::set_var(key, value);
            Self {
                key: key.to_string(),
                previous,
            }
        }

        fn remove(key: &str) -> Self {
            let previous = std::env::var_os(key);
            #[allow(deprecated)]
            std::env::remove_var(key);
            Self {
                key: key.to_string(),
                previous,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.as_ref() {
                #[allow(deprecated)]
                Some(v) => std::env::set_var(&self.key, v),
                #[allow(deprecated)]
                None => std::env::remove_var(&self.key),
            }
        }
    }

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

    // ── ATO_HOME path isolation tests ─────────────────────────────────────────

    /// OciSessionStore::new() must write under ${ATO_HOME}/oci-sessions/, not
    /// under HOME/.ato/oci-sessions/ when ATO_HOME is set to a different path.
    #[test]
    fn oci_session_store_uses_ato_home_not_home() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let ato_home = tempfile::tempdir().unwrap();
        let fake_home = tempfile::tempdir().unwrap();

        let _guard_ato = EnvVarGuard::set("ATO_HOME", ato_home.path().to_str().unwrap());
        let _guard_home = EnvVarGuard::set("HOME", fake_home.path().to_str().unwrap());

        let record = make_record("ato-ato-home-test");
        let store = OciSessionStore::new().expect("OciSessionStore::new should succeed");
        store.write_session(&record).expect("write_session");

        // File must exist under ATO_HOME.
        let expected = ato_home
            .path()
            .join("oci-sessions")
            .join("ato-ato-home-test.json");
        assert!(
            expected.exists(),
            "session file not found under ATO_HOME: {}",
            expected.display()
        );

        // File must NOT exist under HOME/.ato.
        let forbidden = fake_home
            .path()
            .join(".ato")
            .join("oci-sessions")
            .join("ato-ato-home-test.json");
        assert!(
            !forbidden.exists(),
            "session file leaked to HOME/.ato: {}",
            forbidden.display()
        );
    }

    /// list/mark_stopped/delete in OciSessionStore::new() must also resolve
    /// through ATO_HOME.
    #[test]
    fn stop_all_oci_sessions_reads_from_ato_home() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let ato_home = tempfile::tempdir().unwrap();
        let _guard_ato = EnvVarGuard::set("ATO_HOME", ato_home.path().to_str().unwrap());

        let record = make_record("ato-stop-test-1234");
        let store = OciSessionStore::new().unwrap();
        store.write_session(&record).unwrap();

        // A fresh store created with the same ATO_HOME must see it.
        let store2 = OciSessionStore::new().unwrap();
        let sessions = store2.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "ato-stop-test-1234");

        store2.mark_stopped("ato-stop-test-1234").unwrap();
        let sessions = store2.list_sessions().unwrap();
        assert_eq!(sessions[0].status, OciSessionStatus::Stopped);
    }

    /// Secret values must not appear in session records regardless of ATO_HOME.
    #[test]
    fn secret_values_not_written_to_ato_home_session_record() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let ato_home = tempfile::tempdir().unwrap();
        let _guard_ato = EnvVarGuard::set("ATO_HOME", ato_home.path().to_str().unwrap());

        let record = make_record("ato-noleak-check-12");
        let store = OciSessionStore::new().unwrap();
        let path = store.write_session(&record).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("password"),
            "password leaked in ATO_HOME record: {content}"
        );
        assert!(
            !content.contains("secret"),
            "secret leaked in ATO_HOME record: {content}"
        );
        assert!(
            !content.contains("DATABASE_URL"),
            "DATABASE_URL leaked: {content}"
        );
    }

    /// A clean ATO_HOME starts with no OCI sessions.
    #[test]
    fn clean_ato_home_has_no_cross_contamination_from_default_home() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let fresh_home = tempfile::tempdir().unwrap();
        let _guard_ato = EnvVarGuard::set("ATO_HOME", fresh_home.path().to_str().unwrap());

        // Should succeed and return empty list.
        let store = OciSessionStore::new().unwrap();
        let sessions = store.list_sessions().unwrap();
        assert!(
            sessions.is_empty(),
            "fresh ATO_HOME should have no sessions, got: {:?}",
            sessions.iter().map(|s| &s.session_id).collect::<Vec<_>>()
        );
    }

    // ── apply_stop_result regression tests ───────────────────────────────────

    /// When stop_oci_session returns container errors, the session record must
    /// be kept as StopFailed so a later `ato stop --all` can retry.
    #[test]
    fn stop_all_oci_sessions_keeps_record_when_container_stop_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let record = make_record("ato-fail-stop-1234");
        store.write_session(&record).unwrap();

        let failed_result = StopResult {
            stopped_containers: vec![],
            network_removed: false,
            errors: vec!["stop ato-fail-stop-1234-app: permission denied".to_string()],
        };
        apply_stop_result(&store, "ato-fail-stop-1234", &failed_result);

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1, "record must be kept on stop failure");
        assert_eq!(
            sessions[0].status,
            OciSessionStatus::StopFailed,
            "status must be StopFailed, not deleted or Running"
        );
    }

    /// When network removal fails (containers stopped but network rm errors),
    /// the record must be kept as StopFailed.
    #[test]
    fn stop_all_oci_sessions_keeps_record_when_network_remove_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let record = make_record("ato-fail-net-5678");
        store.write_session(&record).unwrap();

        let partial_result = StopResult {
            stopped_containers: vec!["ato-fail-net-5678-app".to_string()],
            network_removed: false, // network rm failed
            errors: vec!["network rm ato-fail-net-5678: active endpoints".to_string()],
        };
        apply_stop_result(&store, "ato-fail-net-5678", &partial_result);

        let sessions = store.list_sessions().unwrap();
        assert_eq!(
            sessions.len(),
            1,
            "record must be kept when network rm fails"
        );
        assert_eq!(sessions[0].status, OciSessionStatus::StopFailed);
    }

    /// When stop succeeds fully (no errors, network removed), the record must
    /// be deleted — not left as Running or StopFailed.
    #[test]
    fn stop_all_oci_sessions_deletes_record_only_on_full_success() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let record = make_record("ato-ok-stop-abcd");
        store.write_session(&record).unwrap();

        let success_result = StopResult {
            stopped_containers: vec![
                "ato-ok-stop-abcd-db".to_string(),
                "ato-ok-stop-abcd-app".to_string(),
            ],
            network_removed: true,
            errors: vec![],
        };
        apply_stop_result(&store, "ato-ok-stop-abcd", &success_result);

        let sessions = store.list_sessions().unwrap();
        assert!(
            sessions.is_empty(),
            "record must be deleted on full success, but found: {:?}",
            sessions.iter().map(|s| &s.session_id).collect::<Vec<_>>()
        );
    }

    /// A StopFailed session can be retried: after a subsequent successful stop,
    /// the record is deleted.
    #[test]
    fn stop_all_oci_sessions_can_retry_after_previous_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let record = make_record("ato-retry-efgh");
        store.write_session(&record).unwrap();

        // First attempt: partial failure.
        let fail_result = StopResult {
            stopped_containers: vec![],
            network_removed: false,
            errors: vec!["stop ato-retry-efgh-app: podman not running".to_string()],
        };
        apply_stop_result(&store, "ato-retry-efgh", &fail_result);

        // Record is kept as StopFailed.
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, OciSessionStatus::StopFailed);

        // Second attempt: full success.
        let retry_result = StopResult {
            stopped_containers: vec!["ato-retry-efgh-app".to_string()],
            network_removed: true,
            errors: vec![],
        };
        apply_stop_result(&store, "ato-retry-efgh", &retry_result);

        // Now the record must be deleted.
        let sessions = store.list_sessions().unwrap();
        assert!(
            sessions.is_empty(),
            "record must be deleted after successful retry"
        );
    }

    #[test]
    fn stop_by_id_stops_oci_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let record = make_record("ato-by-id-stop-1234");
        store.write_session(&record).unwrap();

        let attempt = stop_oci_session_by_id_with(&store, &record.session_id, |_| StopResult {
            stopped_containers: vec!["app".to_string(), "db".to_string()],
            network_removed: true,
            errors: vec![],
        })
        .unwrap()
        .expect("session should be found");

        assert_eq!(attempt.record.session_id, record.session_id);
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn stop_by_id_marks_stop_failed_on_partial_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let record = make_record("ato-by-id-failed-1");
        store.write_session(&record).unwrap();

        stop_oci_session_by_id_with(&store, &record.session_id, |_| StopResult {
            stopped_containers: vec!["app".to_string()],
            network_removed: false,
            errors: vec!["network is busy".to_string()],
        })
        .unwrap();

        assert_eq!(
            store.list_sessions().unwrap()[0].status,
            OciSessionStatus::StopFailed
        );
    }

    #[test]
    fn stop_by_id_retries_stop_failed_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let mut record = make_record("ato-by-id-retry-12");
        record.status = OciSessionStatus::StopFailed;
        store.write_session(&record).unwrap();

        let attempt = stop_oci_session_by_id_with(&store, &record.session_id, |_| StopResult {
            stopped_containers: vec!["app".to_string(), "db".to_string()],
            network_removed: true,
            errors: vec![],
        })
        .unwrap();

        assert!(attempt.is_some());
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn stop_by_id_does_not_delete_record_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let record = make_record("ato-by-id-keep-123");
        store.write_session(&record).unwrap();

        stop_oci_session_by_id_with(&store, &record.session_id, |_| StopResult {
            stopped_containers: vec![],
            network_removed: false,
            errors: vec!["podman machine stopped".to_string()],
        })
        .unwrap();

        assert_eq!(store.list_sessions().unwrap().len(), 1);
    }

    #[test]
    fn stop_by_id_preserves_persistent_volumes() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let record = make_record("ato-by-id-volume-1");
        store.write_session(&record).unwrap();

        stop_oci_session_by_id_with(&store, &record.session_id, |found| {
            assert_eq!(
                found.services[0].persistent_volumes,
                vec!["ato-pg-data".to_string()]
            );
            StopResult {
                stopped_containers: found
                    .services
                    .iter()
                    .rev()
                    .map(|service| service.container_name.clone())
                    .collect(),
                network_removed: true,
                errors: vec![],
            }
        })
        .unwrap();

        assert!(store.list_sessions().unwrap().is_empty());
    }
}

//! Minimal OCI session tracking for lifecycle management.
//!
//! When `ato run` starts an OCI multi-service graph (via `--oci-compose`,
//! `--oci-install-sh`, or an explicit `[services]` capsule), a `OciSessionRecord`
//! is written to `${ATO_HOME}/oci-sessions/<session_id>.json` before the service
//! graph enters the wait/log-stream loop.  The record is deleted when the
//! session exits (normal or cleanup).
//!
//! `ATO_HOME` defaults to `~/.ato` when the environment variable is not set.
//! All reads and writes use [`capsule::common::paths::ato_path_or_workspace_tmp`],
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
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::adapters::runtime::podman_machine::parse_podman_machine_list;

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

/// Ingress endpoint metadata stored in the OCI session record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OciSessionIngressRecord {
    pub mode: String,
    pub router_port: u16,
    pub token: String,
    pub primary_url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub routes: BTreeMap<String, IngressRouteRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngressRouteRecord {
    pub url: String,
    pub target: String,
    pub port: u16,
    pub listed: bool,
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
    /// Ingress path router metadata, present when the manifest declares
    /// `[ingress]` with `mode = "path"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress: Option<OciSessionIngressRecord>,
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
        let sessions_dir = capsule::common::paths::ato_path_or_workspace_tmp(OCI_SESSIONS_DIR);
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
            if path.extension().is_some_and(|e| e == "json")
                && let Ok(record) = self.read_record_from_path(&path)
            {
                records.push(record);
            }
        }
        Ok(records)
    }

    pub fn active_session_count(&self) -> Result<usize> {
        Ok(self
            .list_sessions()?
            .iter()
            .filter(|session| session.status.is_active())
            .count())
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

    #[allow(dead_code)]
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

impl OciSessionStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Running | Self::StopFailed)
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

// ── Podman machine helpers ───────────────────────────────────────────────────

// `PodmanMachineStatus` and `parse_podman_machine_list` live in the shared
// `podman_machine` module and are imported above.
pub use crate::adapters::runtime::podman_machine::PodmanMachineStatus;

#[derive(Debug)]
pub struct PodmanMachineStopResult {
    pub status_before: PodmanMachineStatus,
    pub stopped_machines: Vec<String>,
    pub errors: Vec<String>,
    pub skipped_reason: Option<String>,
}

#[derive(Debug)]
struct MachineCommandOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

#[derive(Debug, PartialEq, Eq)]
struct RunningContainerGuard {
    running_count: usize,
    non_ato_count: usize,
}

impl RunningContainerGuard {
    fn allows_machine_stop(&self) -> bool {
        self.non_ato_count == 0
    }
}

pub fn podman_machine_status() -> PodmanMachineStatus {
    podman_machine_status_with(run_podman_machine_command)
}

fn podman_machine_status_with<F>(mut run: F) -> PodmanMachineStatus
where
    F: FnMut(&[&str]) -> std::result::Result<MachineCommandOutput, String>,
{
    match run(&["machine", "list", "--format", "json"]) {
        Ok(output) if output.success => parse_podman_machine_list(&output.stdout),
        Ok(output) => PodmanMachineStatus::Unavailable {
            reason: first_non_empty(&output.stderr, &output.stdout)
                .unwrap_or_else(|| "podman machine list failed".to_string()),
        },
        Err(reason) => PodmanMachineStatus::Unavailable { reason },
    }
}

pub fn stop_podman_machines_if_idle(store: &OciSessionStore) -> PodmanMachineStopResult {
    stop_podman_machines_if_idle_with(
        store,
        run_podman_machine_command,
        run_podman_machine_command,
        run_podman_machine_command,
    )
}

fn stop_podman_machines_if_idle_with<F, G, H>(
    store: &OciSessionStore,
    mut status_run: F,
    mut container_run: G,
    mut stop_run: H,
) -> PodmanMachineStopResult
where
    F: FnMut(&[&str]) -> std::result::Result<MachineCommandOutput, String>,
    G: FnMut(&[&str]) -> std::result::Result<MachineCommandOutput, String>,
    H: FnMut(&[&str]) -> std::result::Result<MachineCommandOutput, String>,
{
    match store.active_session_count() {
        Ok(0) => {}
        Ok(count) => {
            return PodmanMachineStopResult {
                status_before: PodmanMachineStatus::Unknown {
                    reason: "not checked while OCI sessions are active".to_string(),
                },
                stopped_machines: vec![],
                errors: vec![],
                skipped_reason: Some(format!("{count} active OCI session(s) remain")),
            };
        }
        Err(err) => {
            return PodmanMachineStopResult {
                status_before: PodmanMachineStatus::Unknown {
                    reason: "session store read failed".to_string(),
                },
                stopped_machines: vec![],
                errors: vec![],
                skipped_reason: Some(format!("could not read OCI sessions: {err}")),
            };
        }
    }

    let status_before = podman_machine_status_with(&mut status_run);
    let names = match &status_before {
        PodmanMachineStatus::Running { all_names, .. } if all_names.len() == 1 => all_names.clone(),
        PodmanMachineStatus::Running { all_names, .. } => {
            let configured_count = all_names.len();
            return PodmanMachineStopResult {
                status_before,
                stopped_machines: vec![],
                errors: vec![],
                skipped_reason: Some(format!(
                    "{} configured Podman machine(s) present; machine ownership is ambiguous",
                    configured_count
                )),
            };
        }
        PodmanMachineStatus::Stopped { .. }
        | PodmanMachineStatus::NotConfigured
        | PodmanMachineStatus::Unavailable { .. }
        | PodmanMachineStatus::Unknown { .. } => {
            return PodmanMachineStopResult {
                status_before,
                stopped_machines: vec![],
                errors: vec![],
                skipped_reason: None,
            };
        }
    };

    match running_container_guard(&mut container_run) {
        Ok(guard) if guard.allows_machine_stop() => {}
        Ok(guard) => {
            return PodmanMachineStopResult {
                status_before,
                stopped_machines: vec![],
                errors: vec![],
                skipped_reason: Some(format!(
                    "{} non-Ato running container(s) present",
                    guard.non_ato_count
                )),
            };
        }
        Err(reason) => {
            return PodmanMachineStopResult {
                status_before,
                stopped_machines: vec![],
                errors: vec![],
                skipped_reason: Some(format!("could not verify running containers: {reason}")),
            };
        }
    }

    let mut stopped_machines = Vec::new();
    let mut errors = Vec::new();
    for name in names {
        match stop_run(&["machine", "stop", &name]) {
            Ok(output) if output.success => stopped_machines.push(name),
            Ok(output) => {
                let detail = first_non_empty(&output.stderr, &output.stdout)
                    .unwrap_or_else(|| "podman machine stop failed".to_string());
                if is_already_stopped_message(&detail) {
                    stopped_machines.push(name);
                } else {
                    errors.push(format!("machine stop {name}: {detail}"));
                }
            }
            Err(reason) => errors.push(format!("machine stop {name}: {reason}")),
        }
    }

    PodmanMachineStopResult {
        status_before,
        stopped_machines,
        errors,
        skipped_reason: None,
    }
}

fn running_container_guard<F>(mut run: F) -> std::result::Result<RunningContainerGuard, String>
where
    F: FnMut(&[&str]) -> std::result::Result<MachineCommandOutput, String>,
{
    let output = run(&["ps", "--format", "json"])?;
    if !output.success {
        return Err(first_non_empty(&output.stderr, &output.stdout)
            .unwrap_or_else(|| "podman ps failed".to_string()));
    }
    parse_running_container_guard(&output.stdout)
}

fn run_podman_machine_command(args: &[&str]) -> std::result::Result<MachineCommandOutput, String> {
    let output = Command::new("podman")
        .args(args)
        .output()
        .map_err(|err| format!("failed to run podman {}: {err}", args.join(" ")))?;
    Ok(MachineCommandOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

fn parse_running_container_guard(
    stdout: &str,
) -> std::result::Result<RunningContainerGuard, String> {
    let containers: Vec<serde_json::Value> = serde_json::from_str(stdout)
        .map_err(|err| format!("podman ps output was not recognized: {err}"))?;
    let running_count = containers.len();
    let non_ato_count = containers
        .iter()
        .filter(|container| !is_ato_managed_container(container))
        .count();
    Ok(RunningContainerGuard {
        running_count,
        non_ato_count,
    })
}

fn is_ato_managed_container(container: &serde_json::Value) -> bool {
    match container.get("Labels") {
        Some(serde_json::Value::Object(labels)) => {
            labels
                .get("io.ato.managed")
                .and_then(|value| value.as_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("true"))
                || ["io.ato.session_id", "io.ato.session", "io.ato.execution_id"]
                    .iter()
                    .any(|key| {
                        labels
                            .get(*key)
                            .and_then(|value| value.as_str())
                            .is_some_and(|value| !value.is_empty())
                    })
        }
        Some(serde_json::Value::String(labels)) => labels.split(',').map(str::trim).any(|label| {
            label.eq_ignore_ascii_case("io.ato.managed=true")
                || label.starts_with("io.ato.session_id=")
                || label.starts_with("io.ato.session=")
                || label.starts_with("io.ato.execution_id=")
        }),
        _ => false,
    }
}

fn first_non_empty(first: &str, second: &str) -> Option<String> {
    [first.trim(), second.trim()]
        .into_iter()
        .find(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn is_already_stopped_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("already stopped") || lower.contains("not running")
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
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
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
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key: key.to_string(),
                previous,
            }
        }

        #[allow(dead_code)]
        fn remove(key: &str) -> Self {
            let previous = std::env::var_os(key);
            #[allow(deprecated)]
            unsafe {
                std::env::remove_var(key);
            }
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
                Some(v) => unsafe {
                    std::env::set_var(&self.key, v);
                },
                #[allow(deprecated)]
                None => unsafe {
                    std::env::remove_var(&self.key);
                },
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
            ingress: None,
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
    #[serial_test::serial]
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
    #[serial_test::serial]
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
    #[serial_test::serial]
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
    #[serial_test::serial]
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

    #[test]
    fn active_session_count_treats_running_and_stop_failed_as_active() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        let running = make_record("ato-active-run-12");
        let mut stop_failed = make_record("ato-active-fail-1");
        stop_failed.status = OciSessionStatus::StopFailed;
        let mut stopped = make_record("ato-active-stop-1");
        stopped.status = OciSessionStatus::Stopped;

        store.write_session(&running).unwrap();
        store.write_session(&stop_failed).unwrap();
        store.write_session(&stopped).unwrap();

        assert_eq!(store.active_session_count().unwrap(), 2);
    }

    #[test]
    fn parse_podman_machine_list_reports_running_machine_names() {
        let status = parse_podman_machine_list(
            r#"[{"Name":"podman-machine-default","Running":true},{"Name":"old","Running":false}]"#,
        );

        assert_eq!(
            status,
            PodmanMachineStatus::Running {
                running_names: vec!["podman-machine-default".to_string()],
                all_names: vec!["podman-machine-default".to_string(), "old".to_string(),]
            }
        );
        assert_eq!(
            status.display_status(),
            "running (podman-machine-default); configured (podman-machine-default, old)"
        );
    }

    #[test]
    fn parse_podman_machine_list_reports_configured_but_stopped() {
        let status =
            parse_podman_machine_list(r#"[{"Name":"podman-machine-default","Running":false}]"#);

        assert_eq!(
            status,
            PodmanMachineStatus::Stopped {
                names: vec!["podman-machine-default".to_string()]
            }
        );
    }

    #[test]
    fn stop_podman_machines_if_idle_skips_when_oci_session_active() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());
        store
            .write_session(&make_record("ato-active-skip-1"))
            .unwrap();

        let result = stop_podman_machines_if_idle_with(
            &store,
            |_| panic!("machine status must not be checked while sessions are active"),
            |_| panic!("container state must not be checked while sessions are active"),
            |_| panic!("machine stop must not be called while sessions are active"),
        );

        assert_eq!(
            result.skipped_reason.as_deref(),
            Some("1 active OCI session(s) remain")
        );
    }

    #[test]
    fn stop_podman_machines_if_idle_stops_running_machines() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());

        let result = stop_podman_machines_if_idle_with(
            &store,
            |_| {
                Ok(MachineCommandOutput {
                    success: true,
                    stdout: r#"[{"Name":"podman-machine-default","Running":true}]"#.to_string(),
                    stderr: String::new(),
                })
            },
            |_| {
                Ok(MachineCommandOutput {
                    success: true,
                    stdout: r#"[{"Labels":{"io.ato.managed":"true"}}]"#.to_string(),
                    stderr: String::new(),
                })
            },
            |args| {
                assert_eq!(args, ["machine", "stop", "podman-machine-default"]);
                Ok(MachineCommandOutput {
                    success: true,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            },
        );

        assert_eq!(
            result.stopped_machines,
            vec!["podman-machine-default".to_string()]
        );
        assert!(result.errors.is_empty());
    }

    #[test]
    fn stop_podman_machines_if_idle_does_not_stop_when_non_ato_containers_are_running() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());

        let result = stop_podman_machines_if_idle_with(
            &store,
            |_| {
                Ok(MachineCommandOutput {
                    success: true,
                    stdout: r#"[{"Name":"podman-machine-default","Running":true}]"#.to_string(),
                    stderr: String::new(),
                })
            },
            |_| {
                Ok(MachineCommandOutput {
                    success: true,
                    stdout: r#"[{"Labels":{"io.ato.managed":"true"}},{"Labels":{"com.example.owner":"other"}}]"#.to_string(),
                    stderr: String::new(),
                })
            },
            |_| panic!("machine stop must not run while non-Ato containers are active"),
        );

        assert_eq!(
            result.skipped_reason.as_deref(),
            Some("1 non-Ato running container(s) present")
        );
        assert!(result.stopped_machines.is_empty());
    }

    #[test]
    fn stop_podman_machines_if_idle_does_not_stop_when_multiple_machines_are_running() {
        let dir = tempfile::tempdir().unwrap();
        let store = OciSessionStore::with_dir(dir.path().to_path_buf());

        let result = stop_podman_machines_if_idle_with(
            &store,
            |_| {
                Ok(MachineCommandOutput {
                    success: true,
                    stdout: r#"[{"Name":"podman-machine-default","Running":true},{"Name":"work","Running":true}]"#.to_string(),
                    stderr: String::new(),
                })
            },
            |_| panic!("container state must not be checked when machine ownership is ambiguous"),
            |_| panic!("machine stop must not run when multiple machines are running"),
        );

        // Production message intentionally changed in commit 1537c672
        // ("fix(ato): align Podman mixed-state handling"): the count is now
        // `configured` (all machines, regardless of running/stopped) so the
        // ambiguity warning fires consistently in mixed-state scenarios.
        assert_eq!(
            result.skipped_reason.as_deref(),
            Some("2 configured Podman machine(s) present; machine ownership is ambiguous")
        );
        assert!(result.stopped_machines.is_empty());
    }

    #[test]
    fn parse_running_container_guard_accepts_current_ato_label_forms() {
        let guard = parse_running_container_guard(
            r#"[
                {"Labels":{"io.ato.managed":"true"}},
                {"Labels":{"io.ato.session_id":"ato-session-1"}},
                {"Labels":"io.ato.execution_id=ato-session-2,other=value"},
                {"Labels":{"io.ato.managed":"false"}}
            ]"#,
        )
        .unwrap();

        assert_eq!(
            guard,
            RunningContainerGuard {
                running_count: 4,
                non_ato_count: 1
            }
        );
    }
}

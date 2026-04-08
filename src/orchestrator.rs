use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

const ATO_BIN_ENV: &str = "ATO_DESKTOP_ATO_BIN";

#[derive(Clone, Debug)]
pub struct GuestLaunchSession {
    pub handle: String,
    pub normalized_handle: String,
    pub canonical_handle: Option<String>,
    pub source: Option<String>,
    pub trust_state: String,
    pub restricted: bool,
    pub snapshot_label: Option<String>,
    pub session_id: String,
    pub adapter: String,
    pub frontend_entry: String,
    pub invoke_url: String,
    pub healthcheck_url: String,
    pub capabilities: Vec<String>,
    pub manifest_path: PathBuf,
    pub app_root: PathBuf,
    pub target_label: String,
    pub notes: Vec<String>,
}

impl GuestLaunchSession {
    pub fn frontend_url_path(&self) -> String {
        format!("/{}", self.frontend_entry.trim_start_matches('/'))
    }

    pub fn session_payload(&self) -> serde_json::Value {
        serde_json::json!({
            "sessionId": self.session_id,
            "adapter": self.adapter,
            "invokeUrl": self.invoke_url,
            "healthcheckUrl": self.healthcheck_url,
            "manifestPath": self.manifest_path.display().to_string(),
            "targetLabel": self.target_label,
            "handle": self.handle,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ResolveEnvelope {
    resolution: ResolvePayload,
}

#[derive(Debug, Deserialize)]
struct ResolvePayload {
    render_strategy: String,
    canonical_handle: Option<String>,
    source: Option<String>,
    trust_state: Option<String>,
    restricted: Option<bool>,
    snapshot: Option<serde_json::Value>,
    guest: Option<ResolveGuest>,
    target: Option<ResolveTarget>,
    notes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ResolveGuest {
    adapter: String,
    frontend_entry: String,
    capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ResolveTarget {
    manifest_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionStartEnvelope {
    session: SessionStartInfo,
}

#[derive(Debug, Deserialize)]
struct SessionStartInfo {
    session_id: String,
    handle: String,
    normalized_handle: String,
    canonical_handle: Option<String>,
    trust_state: String,
    source: Option<String>,
    restricted: bool,
    snapshot: Option<serde_json::Value>,
    adapter: String,
    frontend_entry: String,
    healthcheck_url: String,
    invoke_url: String,
    capabilities: Vec<String>,
    manifest_path: String,
    target_label: String,
    notes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SessionStopEnvelope {
    stopped: bool,
}

#[derive(Debug, Deserialize)]
struct StoredSessionRecord {
    session_id: String,
    pid: i32,
    log_path: String,
}

pub fn resolve_and_start_guest(handle: &str) -> Result<GuestLaunchSession> {
    // Resolve first so we can validate the guest contract before a session is started.
    let resolved = resolve_guest(handle)?;
    let started = start_guest(handle)?;

    let manifest_path = PathBuf::from(&started.manifest_path);
    let app_root = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            anyhow::anyhow!("manifest path has no parent: {}", manifest_path.display())
        })?;

    let guest = resolved
        .guest
        .ok_or_else(|| anyhow::anyhow!("resolve did not return guest metadata"))?;
    // Guest frontend entries are served relative to the capsule root, so normalize them here.
    let frontend_entry =
        normalize_frontend_entry(&app_root, &started.frontend_entry, &guest.frontend_entry)?;

    Ok(GuestLaunchSession {
        handle: started.handle,
        normalized_handle: started.normalized_handle,
        canonical_handle: resolved.canonical_handle,
        source: resolved.source,
        trust_state: started.trust_state,
        restricted: started.restricted,
        snapshot_label: started.snapshot.as_ref().map(snapshot_label),
        session_id: started.session_id,
        adapter: started.adapter,
        frontend_entry,
        invoke_url: started.invoke_url,
        healthcheck_url: started.healthcheck_url,
        capabilities: if started.capabilities.is_empty() {
            guest.capabilities
        } else {
            started.capabilities
        },
        manifest_path,
        app_root,
        target_label: started.target_label,
        notes: combine_notes(resolved.notes, started.notes, &guest.adapter),
    })
}

pub fn stop_guest_session(session_id: &str) -> Result<bool> {
    let stopped: SessionStopEnvelope =
        run_ato_json(&["app", "session", "stop", session_id, "--json"])?;
    Ok(stopped.stopped)
}

pub fn cleanup_stale_guest_sessions() -> Result<Vec<String>> {
    let root = session_root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut notes = Vec::new();
    // Session files are process-bound; remove dead ones so restarts do not leak old state.
    for entry in fs::read_dir(&root)
        .with_context(|| format!("failed to read session root {}", root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.starts_with("desky-session-")
            || path.extension().and_then(|ext| ext.to_str()) != Some("json")
        {
            continue;
        }

        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let record: StoredSessionRecord = match serde_json::from_str(&raw) {
            Ok(record) => record,
            Err(_) => continue,
        };
        if process_is_alive(record.pid) {
            continue;
        }

        fs::remove_file(&path)
            .with_context(|| format!("failed to remove stale session file {}", path.display()))?;
        let log_path = PathBuf::from(&record.log_path);
        if log_path.exists() {
            let _ = fs::remove_file(&log_path);
        }
        notes.push(format!("Removed stale guest session {}", record.session_id));
    }

    Ok(notes)
}

fn resolve_guest(handle: &str) -> Result<ResolvePayload> {
    let envelope: ResolveEnvelope = run_ato_json(&["app", "resolve", handle, "--json"])?;
    // The desktop shell only knows how to mount guest webviews, not other render strategies.
    if envelope.resolution.render_strategy != "guest-webview" {
        bail!(
            "ato app resolve returned unsupported render strategy '{}' for {}",
            envelope.resolution.render_strategy,
            handle
        );
    }
    if envelope.resolution.guest.is_none() {
        bail!(
            "ato app resolve did not return guest metadata for {}",
            handle
        );
    }
    Ok(envelope.resolution)
}

fn start_guest(handle: &str) -> Result<SessionStartInfo> {
    let envelope: SessionStartEnvelope =
        run_ato_json(&["app", "session", "start", handle, "--json"])?;
    Ok(envelope.session)
}

fn run_ato_json<T>(args: &[&str]) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let ato_bin = resolve_ato_binary()?;
    let output = Command::new(&ato_bin)
        .args(args)
        .output()
        .with_context(|| {
            format!(
                "failed to run ato helper '{}' with args {}",
                ato_bin.display(),
                args.join(" ")
            )
        })?;

    if !output.status.success() {
        bail!(
            "ato helper command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "failed to parse ato-cli json output for args {}",
            args.join(" ")
        )
    })
}

fn resolve_ato_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(ATO_BIN_ENV) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!(
            "{} points to a missing ato helper binary: {}",
            ATO_BIN_ENV,
            path.display()
        );
    }

    if let Some(path) = bundled_ato_binary()? {
        return Ok(path);
    }

    if let Some(path) = which_in_path("ato") {
        return Ok(path);
    }

    bail!(
        "ato helper binary was not found. Bundle Helpers/ato, set {}, or install 'ato' on PATH.",
        ATO_BIN_ENV
    )
}

fn bundled_ato_binary() -> Result<Option<PathBuf>> {
    let exe = std::env::current_exe().context("failed to resolve ato-desktop executable path")?;
    let Some(macos_dir) = exe.parent() else {
        return Ok(None);
    };

    let bundled = macos_dir
        .parent()
        .map(|contents| contents.join("Helpers").join("ato"));
    if let Some(path) = bundled {
        if path.is_file() {
            return Ok(Some(path));
        }
    }

    Ok(None)
}

fn which_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|entry| entry.join(binary))
        .find(|candidate| candidate.is_file())
}

fn combine_notes(
    mut resolve_notes: Vec<String>,
    start_notes: Vec<String>,
    adapter: &str,
) -> Vec<String> {
    // Preserve both resolve-time and launch-time notes, but avoid repeating the same line twice.
    for note in start_notes {
        if !resolve_notes.contains(&note) {
            resolve_notes.push(note);
        }
    }
    resolve_notes.push(format!("Resolved guest adapter {adapter} through ato-cli"));
    resolve_notes
}

fn snapshot_label(snapshot: &serde_json::Value) -> String {
    if let Some(commit_sha) = snapshot.get("commit_sha").and_then(serde_json::Value::as_str) {
        return format!("commit {}", short_id(commit_sha));
    }
    if let Some(version) = snapshot.get("version").and_then(serde_json::Value::as_str) {
        return format!("version {version}");
    }
    "resolved".to_string()
}

fn short_id(value: &str) -> String {
    value.chars().take(12).collect()
}

fn normalize_frontend_entry(app_root: &Path, primary: &str, fallback: &str) -> Result<String> {
    let raw = if primary.is_empty() {
        fallback
    } else {
        primary
    };
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        let canonical_root = app_root
            .canonicalize()
            .with_context(|| format!("failed to resolve app root {}", app_root.display()))?;
        let canonical_entry = candidate
            .canonicalize()
            .with_context(|| format!("failed to resolve frontend entry {}", candidate.display()))?;
        let relative = canonical_entry
            .strip_prefix(&canonical_root)
            .with_context(|| {
                format!(
                    "frontend entry {} is outside app root {}",
                    canonical_entry.display(),
                    canonical_root.display()
                )
            })?;
        return Ok(relative.display().to_string());
    }

    Ok(raw.trim_start_matches("./").to_string())
}

fn session_root() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("DESKY_SESSION_ROOT") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| anyhow::anyhow!("failed to resolve home directory from HOME"))?;
    Ok(home
        .join(".ato")
        .join("apps")
        .join("desky")
        .join("sessions"))
}

fn process_is_alive(pid: i32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::which_in_path;

    #[test]
    fn which_in_path_resolves_existing_binary() {
        let sh = which_in_path("sh").expect("sh should exist on PATH in tests");
        assert!(sh.is_file());
    }
}

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use capsule_core::handle::{
    normalize_capsule_handle, CanonicalHandle, CapsuleDisplayStrategy, CapsuleRuntimeDescriptor,
};
use serde::Deserialize;

const ATO_BIN_ENV: &str = "ATO_DESKTOP_ATO_BIN";

#[derive(Clone, Debug)]
pub struct CapsuleLaunchSession {
    pub handle: String,
    pub normalized_handle: String,
    pub canonical_handle: Option<String>,
    pub source: Option<String>,
    pub trust_state: String,
    pub restricted: bool,
    pub snapshot_label: Option<String>,
    pub session_id: String,
    pub runtime: CapsuleRuntimeDescriptor,
    pub display_strategy: CapsuleDisplayStrategy,
    pub manifest_path: PathBuf,
    pub app_root: PathBuf,
    pub target_label: String,
    pub adapter: Option<String>,
    pub frontend_entry: Option<String>,
    pub invoke_url: Option<String>,
    pub healthcheck_url: Option<String>,
    pub capabilities: Vec<String>,
    pub local_url: Option<String>,
    pub served_by: Option<String>,
    pub log_path: Option<PathBuf>,
    pub notes: Vec<String>,
}

impl CapsuleLaunchSession {
    pub fn frontend_url_path(&self) -> Option<String> {
        self.frontend_entry
            .as_ref()
            .map(|entry| format!("/{}", entry.trim_start_matches('/')))
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

pub type GuestLaunchSession = CapsuleLaunchSession;

pub fn resolve_and_start_guest(handle: &str) -> Result<GuestLaunchSession> {
    resolve_and_start_capsule(handle)
}

pub fn stop_guest_session(session_id: &str) -> Result<bool> {
    stop_capsule_session(session_id)
}

#[derive(Clone, Debug, Deserialize)]
struct ResolveEnvelope {
    resolution: ResolvePayload,
}

#[derive(Clone, Debug, Deserialize)]
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

#[derive(Clone, Debug, Deserialize)]
struct ResolveGuest {
    adapter: String,
    frontend_entry: String,
    capabilities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct ResolveTarget {
    manifest_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct SessionStartEnvelope {
    session: SessionStartInfo,
}

#[derive(Clone, Debug, Deserialize)]
struct SessionStartInfo {
    session_id: String,
    handle: String,
    normalized_handle: String,
    canonical_handle: Option<String>,
    trust_state: String,
    source: Option<String>,
    restricted: bool,
    snapshot: Option<serde_json::Value>,
    runtime: CapsuleRuntimeDescriptor,
    display_strategy: CapsuleDisplayStrategy,
    manifest_path: String,
    target_label: String,
    log_path: String,
    notes: Vec<String>,
    guest: Option<GuestSessionDisplay>,
    web: Option<WebSessionDisplay>,
    terminal: Option<TerminalSessionDisplay>,
    service: Option<ServiceBackgroundDisplay>,
}

#[derive(Clone, Debug, Deserialize)]
struct GuestSessionDisplay {
    adapter: String,
    frontend_entry: String,
    healthcheck_url: String,
    invoke_url: String,
    capabilities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct WebSessionDisplay {
    local_url: String,
    healthcheck_url: String,
    served_by: String,
}

#[derive(Clone, Debug, Deserialize)]
struct TerminalSessionDisplay {
    log_path: String,
}

#[derive(Clone, Debug, Deserialize)]
struct ServiceBackgroundDisplay {
    log_path: String,
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

pub fn resolve_and_start_capsule(handle: &str) -> Result<CapsuleLaunchSession> {
    let resolved = resolve_capsule(handle)?;
    let started = start_capsule(handle)?;
    build_launch_session(handle, resolved, started)
}

pub fn stop_capsule_session(session_id: &str) -> Result<bool> {
    let stopped: SessionStopEnvelope =
        run_ato_json(&["app", "session", "stop", session_id, "--json"])?;
    Ok(stopped.stopped)
}

pub fn cleanup_stale_capsule_sessions() -> Result<Vec<String>> {
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
        notes.push(format!("Removed stale capsule session {}", record.session_id));
    }

    Ok(notes)
}

fn resolve_capsule(handle: &str) -> Result<ResolvePayload> {
    let envelope: ResolveEnvelope = run_ato_json(&["app", "resolve", handle, "--json"])?;
    Ok(envelope.resolution)
}

fn start_capsule(handle: &str) -> Result<SessionStartInfo> {
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

fn combine_notes(mut resolve_notes: Vec<String>, start_notes: Vec<String>) -> Vec<String> {
    // Preserve both resolve-time and launch-time notes, but avoid repeating the same line twice.
    for note in start_notes {
        if !resolve_notes.contains(&note) {
            resolve_notes.push(note);
        }
    }
    resolve_notes
}

fn allows_registry_guest_recovery(handle: &str, resolved: &ResolvePayload) -> bool {
    let source_is_registry = resolved.source.as_deref() == Some("registry");
    let canonical_is_registry = resolved
        .canonical_handle
        .as_deref()
        .is_some_and(is_registry_capsule_handle);

    (source_is_registry || canonical_is_registry) && is_registry_capsule_handle(handle)
}

fn is_registry_capsule_handle(handle: &str) -> bool {
    matches!(
        normalize_capsule_handle(handle),
        Ok(CanonicalHandle::RegistryCapsule { .. })
    )
}

fn build_launch_session(
    handle: &str,
    resolved: ResolvePayload,
    started: SessionStartInfo,
) -> Result<CapsuleLaunchSession> {
    let manifest_path = PathBuf::from(&started.manifest_path);
    let app_root = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("manifest path has no parent: {}", manifest_path.display()))?;

    let recover_from_materialized_manifest =
        resolved.guest.is_none() && allows_registry_guest_recovery(handle, &resolved);
    let mut notes = combine_notes(resolved.notes, started.notes);
    let display_strategy = started.display_strategy.clone();
    let guest = started.guest.clone();
    let web = started.web.clone();
    let terminal = started.terminal.clone();
    let service = started.service.clone();
    let guest_metadata_missing = matches!(display_strategy, CapsuleDisplayStrategy::GuestWebview)
        && guest.is_none();

    if recover_from_materialized_manifest && !guest_metadata_missing {
        notes.push(
            "Remote resolve was metadata-only; guest contract was recovered from the materialized session manifest."
                .to_string(),
        );
    }
    if let Some(manifest_path) = resolved
        .target
        .as_ref()
        .and_then(|target| target.manifest_path.as_deref())
    {
        notes.push(format!(
            "Resolve target advertised manifest path {manifest_path} before local materialization."
        ));
    }

    let trust_state = if started.trust_state.is_empty() {
        resolved
            .trust_state
            .clone()
            .unwrap_or_else(|| "untrusted".to_string())
    } else {
        started.trust_state.clone()
    };

    let frontend_entry = match guest.as_ref() {
        Some(guest) => Some(normalize_frontend_entry(
            &app_root,
            &guest.frontend_entry,
            &guest.frontend_entry,
        )?),
        None => None,
    };

    if guest_metadata_missing {
        bail!(
            "ato app session start returned guest_webview for {handle} without guest payload"
        );
    }

    Ok(CapsuleLaunchSession {
        handle: started.handle,
        normalized_handle: started.normalized_handle,
        canonical_handle: started
            .canonical_handle
            .clone()
            .or_else(|| resolved.canonical_handle.clone()),
        source: started.source.clone().or_else(|| resolved.source.clone()),
        trust_state,
        restricted: started.restricted || resolved.restricted.unwrap_or(false),
        snapshot_label: started
            .snapshot
            .as_ref()
            .or(resolved.snapshot.as_ref())
            .map(snapshot_label),
        session_id: started.session_id,
        runtime: started.runtime,
        display_strategy: display_strategy.clone(),
        manifest_path,
        app_root,
        target_label: started.target_label,
        adapter: guest.as_ref().map(|item| item.adapter.clone()),
        frontend_entry,
        invoke_url: guest.as_ref().map(|item| item.invoke_url.clone()),
        healthcheck_url: guest
            .as_ref()
            .map(|item| item.healthcheck_url.clone())
            .or_else(|| web.as_ref().map(|item| item.healthcheck_url.clone())),
        capabilities: guest
            .as_ref()
            .map(|item| item.capabilities.clone())
            .unwrap_or_default(),
        local_url: web.as_ref().map(|item| item.local_url.clone()),
        served_by: web.as_ref().map(|item| item.served_by.clone()),
        log_path: terminal
            .as_ref()
            .map(|item| PathBuf::from(&item.log_path))
            .or_else(|| service.as_ref().map(|item| PathBuf::from(&item.log_path)))
            .or_else(|| Some(PathBuf::from(&started.log_path))),
        notes,
    })
}

fn snapshot_label(snapshot: &serde_json::Value) -> String {
    if let Some(commit_sha) = snapshot
        .get("commit_sha")
        .and_then(serde_json::Value::as_str)
    {
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
    use capsule_core::handle::{CapsuleDisplayStrategy, CapsuleRuntimeDescriptor};

    use super::{allows_registry_guest_recovery, build_launch_session, which_in_path, ResolvePayload, SessionStartInfo};

    fn resolved_payload(
        render_strategy: &str,
        source: Option<&str>,
        guest: bool,
    ) -> ResolvePayload {
        ResolvePayload {
            render_strategy: render_strategy.to_string(),
            canonical_handle: Some("capsule://ato.run/koh0920/ato-onboarding".to_string()),
            source: source.map(ToOwned::to_owned),
            trust_state: Some("untrusted".to_string()),
            restricted: Some(true),
            snapshot: Some(serde_json::json!({ "version": "0.1.0" })),
            guest: guest.then(|| super::ResolveGuest {
                adapter: "tauri".to_string(),
                frontend_entry: "dist/index.html".to_string(),
                capabilities: vec!["read-file".to_string()],
            }),
            target: None,
            notes: vec!["resolved".to_string()],
        }
    }

    fn session_start() -> SessionStartInfo {
        SessionStartInfo {
            session_id: "desky-session-1".to_string(),
            handle: "capsule://ato.run/koh0920/ato-onboarding".to_string(),
            normalized_handle: "capsule://ato.run/koh0920/ato-onboarding".to_string(),
            canonical_handle: Some("capsule://ato.run/koh0920/ato-onboarding".to_string()),
            trust_state: "untrusted".to_string(),
            source: Some("registry".to_string()),
            restricted: true,
            snapshot: Some(serde_json::json!({ "version": "0.1.0" })),
            runtime: CapsuleRuntimeDescriptor {
                target_label: "web".to_string(),
                runtime: Some("source".to_string()),
                driver: Some("tauri".to_string()),
                language: Some("tauri".to_string()),
                port: Some(9000),
            },
            display_strategy: CapsuleDisplayStrategy::GuestWebview,
            manifest_path: "/tmp/example/capsule.toml".to_string(),
            target_label: "web".to_string(),
            log_path: "/tmp/example/session.log".to_string(),
            notes: vec!["started".to_string()],
            guest: Some(super::GuestSessionDisplay {
                adapter: "tauri".to_string(),
                frontend_entry: "dist/index.html".to_string(),
                healthcheck_url: "http://127.0.0.1:9000/health".to_string(),
                invoke_url: "http://127.0.0.1:9000/rpc".to_string(),
                capabilities: vec!["read-file".to_string()],
            }),
            web: None,
            terminal: None,
            service: None,
        }
    }

    #[test]
    fn which_in_path_resolves_existing_binary() {
        let sh = which_in_path("sh").expect("sh should exist on PATH in tests");
        assert!(sh.is_file());
    }

    #[test]
    fn registry_capsule_can_recover_guest_from_materialized_session() {
        let resolved = resolved_payload("terminal", Some("registry"), false);
        assert!(allows_registry_guest_recovery(
            "capsule://ato.run/koh0920/ato-onboarding",
            &resolved
        ));

        let session = build_launch_session(
            "capsule://ato.run/koh0920/ato-onboarding",
            resolved,
            session_start(),
        )
        .expect("session");

        assert_eq!(session.adapter.as_deref(), Some("tauri"));
        assert_eq!(session.snapshot_label.as_deref(), Some("version 0.1.0"));
        assert!(session
            .notes
            .iter()
            .any(|note| note.contains("metadata-only")));
    }

    #[test]
    fn loopback_registry_capsule_can_recover_guest_from_materialized_session() {
        let resolved = resolved_payload("terminal", Some("registry"), false);
        assert!(allows_registry_guest_recovery(
            "capsule://localhost:8787/acme/chat",
            &resolved
        ));
    }

    #[test]
    fn web_url_sessions_keep_runtime_and_attach_url() {
        let mut started = session_start();
        started.display_strategy = CapsuleDisplayStrategy::WebUrl;
        started.runtime = CapsuleRuntimeDescriptor {
            target_label: "default".to_string(),
            runtime: Some("web".to_string()),
            driver: Some("deno".to_string()),
            language: Some("deno".to_string()),
            port: Some(4173),
        };
        started.guest = None;
        started.web = Some(super::WebSessionDisplay {
            local_url: "http://127.0.0.1:4173/".to_string(),
            healthcheck_url: "http://127.0.0.1:4173/".to_string(),
            served_by: "deno".to_string(),
        });

        let session = build_launch_session(
            "capsule://localhost:8787/acme/chat",
            resolved_payload("web", Some("registry"), false),
            started,
        )
        .expect("session");

        assert_eq!(session.display_strategy, CapsuleDisplayStrategy::WebUrl);
        assert_eq!(session.local_url.as_deref(), Some("http://127.0.0.1:4173/"));
        assert_eq!(session.runtime.runtime.as_deref(), Some("web"));
    }

    #[test]
    fn registry_recovery_is_not_available_for_non_registry_handles() {
        let resolved = resolved_payload("terminal", Some("github"), false);
        assert!(!allows_registry_guest_recovery(
            "capsule://github.com/acme/chat",
            &resolved
        ));
    }
}

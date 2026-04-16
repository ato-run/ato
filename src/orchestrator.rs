use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use capsule_core::handle::{
    normalize_capsule_handle, CanonicalHandle, CapsuleDisplayStrategy, CapsuleRuntimeDescriptor,
};
use serde::Deserialize;
use tracing::{debug, error, info, warn};

/// Pending terminal processes spawned by share URL executor.
/// `webview.rs` drains these when creating Terminal panes.
static PENDING_SHARE_TERMINALS: std::sync::OnceLock<Mutex<HashMap<String, TerminalProcess>>> =
    std::sync::OnceLock::new();

fn pending_share_terminals() -> &'static Mutex<HashMap<String, TerminalProcess>> {
    PENDING_SHARE_TERMINALS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Take a pending share terminal process by session_id.
/// Called by `webview.rs` when spawning a Terminal pane.
pub fn take_pending_share_terminal(session_id: &str) -> Option<TerminalProcess> {
    pending_share_terminals()
        .lock()
        .ok()
        .and_then(|mut map| map.remove(session_id))
}

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

/// Returns true when `s` looks like an ato.run share URL (`https://ato.run/s/...`).
/// Used by both the orchestrator and the state layer to intercept share URLs before
/// `classify_surface_input` routes them as plain external web pages.
pub fn is_share_url(s: &str) -> bool {
    // Only match known ato.run domains and localhost dev server, followed by /s/<token>
    let known_host = s.starts_with("https://ato.run/s/")
        || s.starts_with("https://staging.ato.run/s/")
        || s.starts_with("http://localhost:");
    if !known_host {
        return false;
    }
    // For localhost, still require /s/ path segment
    if s.starts_with("http://localhost:") {
        // e.g. http://localhost:8787/s/token
        return s.contains("/s/");
    }
    true
}

pub fn resolve_and_start_capsule(handle: &str) -> Result<CapsuleLaunchSession> {
    info!(handle, "resolving capsule");
    if is_share_url(handle) {
        return resolve_and_start_from_share(handle);
    }
    let resolved = resolve_capsule(handle)?;
    let started = start_capsule(handle)?;
    let session = build_launch_session(handle, resolved, started)?;
    info!(
        session_id = %session.session_id,
        handle,
        "capsule session started"
    );
    Ok(session)
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
    debug!(bin = %ato_bin.display(), args = %args.join(" "), "spawning ato helper");
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
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        error!(args = %args.join(" "), stderr = %stderr, "ato helper command failed");
        bail!("ato helper command failed: {stderr}");
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

/// Parsed `.ato/share/state.json` written by `ato decap` on success.
#[derive(Deserialize)]
struct ShareStateJson {
    sources: Vec<ShareStateSource>,
    #[serde(default)]
    verification: Option<ShareStateVerification>,
}

#[derive(Deserialize)]
struct ShareStateVerification {
    result: String, // "ok" | "warning" | "error"
    #[serde(default)]
    issues: Vec<String>,
}

#[derive(Deserialize)]
struct ShareStateSource {
    id: String,
    #[serde(default)]
    status: Option<String>, // "ok" | "error"
    #[serde(default)]
    last_error: Option<String>,
}

/// Walk the workspace to find a directory that contains `capsule.toml`.
///
/// Checks the workspace root first, then each `<workspace>/<source_id>/` subdirectory
/// listed in `.ato/share/state.json`.  Returns `None` for web-only workspaces.
fn find_capsule_root(workspace: &Path) -> Option<PathBuf> {
    if workspace.join("capsule.toml").exists() {
        return Some(workspace.to_path_buf());
    }
    let state_path = workspace.join(".ato").join("share").join("state.json");
    let state_str = fs::read_to_string(&state_path).ok()?;
    let state: ShareStateJson = serde_json::from_str(&state_str).ok()?;
    for source in &state.sources {
        let candidate = workspace.join(&source.id);
        if candidate.join("capsule.toml").exists() {
            return Some(candidate);
        }
    }
    None
}

// Directory names that are skipped during recursive dev-script discovery.
const DEV_SCAN_SKIP: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "build",
    ".next",
    "target",
    ".turbo",
    ".cache",
    "coverage",
    "out",
    ".output",
];

// Directory names that indicate a frontend app — used to rank candidates.
const FRONTEND_NAMES: &[&str] = &[
    "dashboard",
    "frontend",
    "web",
    "ui",
    "client",
    "app",
    "portal",
    "studio",
];

/// Search `source_roots` recursively (up to depth 4) for directories that contain a
/// `package.json` with a `"dev"` npm script.  Returns the best candidate:
///   1. Directories whose path contains a common frontend name (dashboard, frontend, …)
///   2. Shallower directories are preferred within the same priority tier
fn find_dev_script_dir(source_roots: &[PathBuf]) -> Option<PathBuf> {
    let mut candidates: Vec<(PathBuf, usize)> = Vec::new();
    for root in source_roots {
        collect_dev_script_dirs(root, 0, 4, &mut candidates);
    }
    if candidates.is_empty() {
        return None;
    }
    // Score: frontend-named path components win; smaller depth wins on ties.
    candidates.sort_by(|(a, a_depth), (b, b_depth)| {
        let a_score = frontend_score(a);
        let b_score = frontend_score(b);
        b_score.cmp(&a_score).then(a_depth.cmp(b_depth))
    });
    Some(candidates.into_iter().next().map(|(p, _)| p).unwrap())
}

fn collect_dev_script_dirs(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<(PathBuf, usize)>,
) {
    if let Ok(content) = fs::read_to_string(dir.join("package.json")) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            if json["scripts"]["dev"].is_string() {
                out.push((dir.to_path_buf(), depth));
            }
        }
    }
    if depth < max_depth {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if !DEV_SCAN_SKIP
                        .iter()
                        .any(|skip| name_str.as_ref() == *skip)
                    {
                        collect_dev_script_dirs(&path, depth + 1, max_depth, out);
                    }
                }
            }
        }
    }
}

fn frontend_score(path: &Path) -> usize {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .filter(|part| {
            let lower = part.to_lowercase();
            FRONTEND_NAMES.iter().any(|n| lower.contains(n))
        })
        .count()
}



/// Detect the package manager to use for a given source directory.
///
/// Searches the directory and up to 3 ancestor levels for lock files and workspace
/// config files.  `pnpm-workspace.yaml` is a reliable signal even when `pnpm-lock.yaml`
/// is not committed.  Returns `(pm_name, install_root)` where `install_root` is the
/// highest ancestor that owns the lock / workspace file (monorepo root), or `source_dir`
/// itself if no such ancestor is found.
fn detect_package_manager(source_dir: &Path) -> (&'static str, PathBuf) {
    // Check source_dir itself first, then ancestors (skip(1)) up to 3 levels.
    let candidates = std::iter::once(source_dir)
        .chain(source_dir.ancestors().skip(1).take(3));

    let mut pnpm_root: Option<PathBuf> = None;
    let mut yarn_root: Option<PathBuf> = None;

    for dir in candidates {
        if pnpm_root.is_none()
            && (dir.join("pnpm-lock.yaml").exists() || dir.join("pnpm-workspace.yaml").exists())
        {
            pnpm_root = Some(dir.to_path_buf());
        }
        if yarn_root.is_none() && dir.join("yarn.lock").exists() {
            yarn_root = Some(dir.to_path_buf());
        }
    }

    if let Some(root) = pnpm_root {
        return ("pnpm", root);
    }
    if let Some(root) = yarn_root {
        return ("yarn", root);
    }
    ("npm", source_dir.to_path_buf())
}

/// Spawn a web dev server inside a share workspace that has no `capsule.toml`.
///
/// Reads `state.json` to locate the source directory, then **recursively** searches for a
/// `package.json` that has a `"dev"` script (supporting monorepos where the frontend lives
/// in a subdirectory such as `apps/dashboard`).  Frontend-named directories are preferred.
/// Detects the package manager, runs `<pm> run dev`, and waits for a localhost URL.
/// Returns a synthetic `CapsuleLaunchSession` with `display_strategy = WebUrl`.
fn start_web_service_from_workspace(
    share_url: &str,
    workspace: &Path,
) -> Result<CapsuleLaunchSession> {
    let state_path = workspace.join(".ato").join("share").join("state.json");
    let state_str = fs::read_to_string(&state_path)
        .with_context(|| format!("failed to read state.json at {}", state_path.display()))?;
    let state: ShareStateJson = serde_json::from_str(&state_str)
        .with_context(|| format!("failed to parse state.json at {}", state_path.display()))?;

    // Guard: bail early if any source failed to materialize.
    // A broken workspace (e.g. unpushed git commit) will have no node_modules,
    // no lock files, and nothing useful to run.  Trying anyway leads to the dev
    // server exiting immediately, which causes the fallback port scan to pick up
    // whatever happens to be running on a common port (e.g. code-server on 8080).
    let failed_sources: Vec<String> = state
        .sources
        .iter()
        .filter(|s| s.status.as_deref() == Some("error"))
        .map(|s| {
            let err = s.last_error.as_deref().unwrap_or("unknown error");
            format!("source '{}' failed: {}", s.id, err)
        })
        .collect();
    if !failed_sources.is_empty() {
        bail!(
            "workspace materialization failed — cannot start web server:\n{}",
            failed_sources.join("\n")
        );
    }
    if let Some(ref v) = state.verification {
        if v.result == "error" {
            bail!(
                "workspace verification failed — cannot start web server:\n{}",
                v.issues.join("\n")
            );
        }
    }

    // Collect all source root directories.
    let source_roots: Vec<PathBuf> = state
        .sources
        .iter()
        .map(|s| workspace.join(&s.id))
        .filter(|d| d.is_dir())
        .collect();

    // If the spec had no sources at all, the share was created with an older ato-cli
    // that couldn't capture non-git directories. Give an actionable error instead of
    // the generic "no dev script" message.
    if state.sources.is_empty() {
        bail!(
            "share {} has no sources — it was captured without any source content.\n\
             Re-share the project with the current ato-cli: ato encap <dir> --share",
            share_url
        );
    }

    // Recursively find directories with a "dev" npm script inside those roots.
    let source_dir = find_dev_script_dir(&source_roots).ok_or_else(|| {
        anyhow::anyhow!(
            "no runnable web service found in workspace {}: \
             no capsule.toml and no 'dev' npm script in any source subdirectory",
            workspace.display()
        )
    })?;

    // Detect package manager by searching for lock files or workspace config files
    // in the source dir and its ancestors (up to 3 levels — handles monorepos where
    // pnpm-lock.yaml / pnpm-workspace.yaml sit at the monorepo root, not the app dir).
    //
    // pnpm-workspace.yaml is checked explicitly because it is often committed while
    // pnpm-lock.yaml may not be (e.g. when it is .gitignored or not yet generated).
    let (pm, install_root) = detect_package_manager(&source_dir);
    info!(share_url, pm, dir = %source_dir.display(), "spawning web dev server");

    // If the install_root has no node_modules, the ato decap install step either
    // ran the wrong runtime (e.g. Python) or was skipped.  Run install now so
    // that dev-script binaries (next, vite, …) are available.
    if !install_root.join("node_modules").exists() {
        info!(
            share_url,
            pm,
            dir = %install_root.display(),
            "node_modules missing — running install before dev"
        );
        let install_status = Command::new(pm)
            .arg("install")
            .current_dir(&install_root)
            .status()
            .with_context(|| format!("failed to run `{pm} install` in {}", install_root.display()))?;
        if !install_status.success() {
            bail!("`{pm} install` failed in {} (exit {:?})", install_root.display(), install_status.code());
        }
        info!(share_url, pm, "install completed");
    }

    let mut child = Command::new(pm)
        .args(["run", "dev"])
        .current_dir(&source_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to spawn `{pm} run dev` in {}",
                source_dir.display()
            )
        })?;

    let pid = child.id();
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    // Collect stderr in a background thread so it's available if the process exits early.
    let stderr_handle = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        let _ = std::io::BufReader::new(stderr).read_to_string(&mut buf);
        buf
    });

    let url_result = detect_dev_server_url(stdout, std::time::Duration::from_secs(30));

    // If URL detection failed, check whether the child process already exited with an error.
    let local_url = match url_result {
        Ok(url) => url,
        Err(detect_err) => {
            let stderr_output = stderr_handle
                .join()
                .unwrap_or_default()
                .trim()
                .to_string();
            if !stderr_output.is_empty() {
                error!(
                    share_url,
                    pm,
                    pid,
                    stderr = %stderr_output,
                    "dev server process failed"
                );
                bail!(
                    "`{pm} run dev` failed: {detect_err}\nstderr:\n{stderr_output}"
                );
            }
            return Err(detect_err);
        }
    };

    info!(share_url, local_url, pid, "web dev server ready");

    let session_id = format!("share-web-{pid}");
    let manifest_path = state_path;
    let app_root = workspace.to_path_buf();

    Ok(CapsuleLaunchSession {
        handle: share_url.to_string(),
        normalized_handle: share_url.to_string(),
        canonical_handle: None,
        source: Some("web".to_string()),
        trust_state: "untrusted".to_string(),
        restricted: false,
        snapshot_label: None,
        session_id,
        runtime: CapsuleRuntimeDescriptor {
            target_label: "web".to_string(),
            runtime: Some("web".to_string()),
            driver: Some(pm.to_string()),
            language: Some("node".to_string()),
            port: url_port(&local_url),
        },
        display_strategy: CapsuleDisplayStrategy::WebUrl,
        manifest_path,
        app_root,
        target_label: "web".to_string(),
        adapter: None,
        frontend_entry: None,
        invoke_url: None,
        healthcheck_url: Some(local_url.clone()),
        capabilities: Vec::new(),
        local_url: Some(local_url),
        served_by: Some(pm.to_string()),
        log_path: None,
        notes: vec![format!(
            "Web dev server started via `{pm} run dev` (pid {pid})."
        )],
    })
}

/// Read lines from a dev-server's stdout until a localhost URL is found or the timeout expires.
///
/// Understands Vite (`➜  Local:   http://localhost:5173/`), Next.js, Parcel, and any other
/// framework that prints `http://localhost:PORT` or `http://127.0.0.1:PORT` to stdout.
///
/// After detecting a candidate URL, verifies it actually serves HTML (not a stale backend
/// service that happens to occupy the same port) before accepting it.
fn detect_dev_server_url(
    stdout: std::process::ChildStdout,
    timeout: std::time::Duration,
) -> Result<String> {
    use std::io::{BufRead, BufReader};
    let deadline = std::time::Instant::now() + timeout;
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        if std::time::Instant::now() > deadline {
            break;
        }
        let Ok(line) = line else { break };
        debug!("dev server: {line}");
        if let Some(url) = extract_localhost_url(&line) {
            // Trust any URL that the process itself prints to stdout — it IS the server
            // we just spawned.  verify_html_url is NOT called here because some frameworks
            // (e.g. Next.js) print the URL before finishing initial compilation; a HEAD
            // request at that moment would time out and incorrectly discard the correct URL.
            // verify_html_url is only used in the fallback port scan below, where we need
            // to distinguish our server from pre-existing unrelated services.
            return Ok(url);
        }
    }
    // Timeout fallback: check common Vite / Next / CRA ports for an HTML server.
    // NOTE: 8080 is intentionally excluded — it is commonly used by code-server,
    // Jupyter, and other unrelated services; picking it up would open the wrong app.
    for port in [5173u16, 5174, 5175, 3000, 3001, 4173] {
        for host in ["localhost", "[::1]", "127.0.0.1"] {
            let url = format!("http://{host}:{port}/");
            if verify_html_url(&url) {
                warn!(url, "dev server URL not found in stdout, using first HTML-serving port");
                return Ok(url);
            }
        }
    }
    bail!("web dev server did not emit a URL within {timeout:?} and no HTML server found on common ports")
}

/// Return true when a quick HTTP HEAD request to `url` receives a `text/html` response.
///
/// A 2 s timeout is used so startup detection is not delayed by a slow or absent server.
/// If the server returns no `Content-Type` header (unusual but possible) we assume HTML.
fn verify_html_url(url: &str) -> bool {
    match ureq::head(url).timeout(std::time::Duration::from_secs(2)).call() {
        Ok(resp) => resp
            .header("content-type")
            .map(|ct| ct.contains("text/html"))
            .unwrap_or(true),
        Err(_) => false,
    }
}

/// Extract the first `http://localhost:PORT` or `http://127.0.0.1:PORT` substring from a line,
/// preserving the original host so the caller can resolve it correctly (e.g. via IPv6 when
/// `127.0.0.1` is occupied by a different service).
fn extract_localhost_url(line: &str) -> Option<String> {
    for prefix in ["http://localhost:", "http://127.0.0.1:"] {
        if let Some(start) = line.find(prefix) {
            let url_start = &line[start..];
            // Take until the first whitespace character.
            let end = url_start
                .find(|c: char| c.is_ascii_whitespace())
                .unwrap_or(url_start.len());
            let url = &url_start[..end];
            // Verify there is a valid port number after the prefix.
            let after_prefix = &url[prefix.len()..];
            let port_end = after_prefix
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after_prefix.len());
            if after_prefix[..port_end].parse::<u16>().is_ok() {
                let normalized = if url.ends_with('/') {
                    url.to_string()
                } else {
                    format!("{url}/")
                };
                return Some(normalized);
            }
        }
    }
    None
}

fn url_port(url: &str) -> Option<u16> {
    url.rsplit_once(':')
        .and_then(|(_, rest)| rest.trim_end_matches('/').parse().ok())
}

/// Returns a stable temporary directory path derived from the share URL.
/// Stored under ~/.ato/apps/desky/shared-runs/<hash> so the same share URL
/// always materializes to the same location, enabling session resume.
fn share_tmp_dir(share_url: &str) -> Result<PathBuf> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    share_url.hash(&mut hasher);
    let hash = hasher.finish();
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| anyhow::anyhow!("HOME not set — cannot create share tmp dir"))?;
    Ok(home
        .join(".ato")
        .join("apps")
        .join("desky")
        .join("shared-runs")
        .join(format!("{hash:016x}")))
}

/// Materializes a share URL into a local directory by calling `ato decap`.
fn decap_share(share_url: &str, into: &Path) -> Result<()> {
    fs::create_dir_all(into)
        .with_context(|| format!("failed to create share tmp dir {}", into.display()))?;
    let ato_bin = resolve_ato_binary()?;
    info!(share_url, dest = %into.display(), "running ato decap");
    let output = Command::new(&ato_bin)
        .args(["decap", share_url, "--into"])
        .arg(into)
        .output()
        .with_context(|| format!("failed to spawn ato decap for {share_url}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        error!(share_url, stderr = %stderr, "ato decap failed");
        bail!("ato decap failed for {share_url}: {stderr}");
    }
    info!(share_url, dest = %into.display(), "ato decap completed");
    Ok(())
}

/// Resolve and start a capsule from a share URL by materializing it locally first.
fn resolve_and_start_from_share(share_url: &str) -> Result<CapsuleLaunchSession> {
    info!(share_url, "starting share URL execution via nacelle sandbox");

    let result = capsule_core::share::execute_share(capsule_core::share::ShareRunRequest {
        input: share_url.to_string(),
        entry: None,
        extra_args: vec![],
        env_overlay: std::collections::BTreeMap::new(),
        mode: capsule_core::share::ShareExecutionMode::Piped { cols: 120, rows: 40 },
        nacelle_path: std::env::var("NACELLE_PATH").ok().map(PathBuf::from),
        ato_path: std::env::var("ATO_DESKTOP_ATO_BIN")
            .ok()
            .map(PathBuf::from)
            .or_else(|| resolve_ato_binary().ok()),
    })?;

    match result {
        capsule_core::share::ShareExecutionResult::Spawned(piped) => {
            let session_id = piped.session_id.clone();
            info!(share_url, %session_id, "share terminal session spawned via nacelle");

            // Convert SharePipedSession to TerminalProcess and stash for webview.rs
            let terminal_process = TerminalProcess {
                session_id: session_id.clone(),
                input_tx: piped.input_tx,
                resize_tx: piped.resize_tx,
                output_rx: piped.output_rx,
            };
            if let Ok(mut map) = pending_share_terminals().lock() {
                map.insert(session_id.clone(), terminal_process);
            }

            // Build a synthetic CapsuleLaunchSession with TerminalStream strategy
            let workspace = share_tmp_dir(share_url).unwrap_or_else(|_| PathBuf::from("/tmp"));
            Ok(CapsuleLaunchSession {
                handle: share_url.to_string(),
                normalized_handle: share_url.to_string(),
                canonical_handle: None,
                source: Some("share".to_string()),
                trust_state: "untrusted".to_string(),
                restricted: false,
                snapshot_label: None,
                session_id,
                runtime: CapsuleRuntimeDescriptor {
                    target_label: "share".to_string(),
                    runtime: Some("shell".to_string()),
                    driver: Some("nacelle".to_string()),
                    language: None,
                    port: None,
                },
                display_strategy: CapsuleDisplayStrategy::TerminalStream,
                manifest_path: workspace.join(".ato/share/state.json"),
                app_root: workspace,
                target_label: "share".to_string(),
                adapter: None,
                frontend_entry: None,
                invoke_url: None,
                healthcheck_url: None,
                capabilities: vec!["terminal".to_string()],
                local_url: None,
                served_by: Some("nacelle".to_string()),
                log_path: None,
                notes: vec!["Share URL executed via nacelle sandbox.".to_string()],
            })
        }
        capsule_core::share::ShareExecutionResult::Completed { exit_code } => {
            bail!("share execution completed unexpectedly (code {exit_code}) in piped mode")
        }
    }
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

    use super::{
        allows_registry_guest_recovery, build_launch_session, collect_dev_script_dirs,
        detect_package_manager, extract_localhost_url, find_capsule_root, find_dev_script_dir,
        url_port, which_in_path, ResolvePayload, SessionStartInfo,
    };

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

    #[test]
    fn is_share_url_detects_share_paths() {
        assert!(super::is_share_url("https://ato.run/s/abc123"));
        assert!(super::is_share_url("https://ato.run/s/abc123?extra=1"));
        assert!(super::is_share_url("http://localhost:8787/s/test-run"));
        assert!(super::is_share_url("https://staging.ato.run/s/xyz"));
    }

    #[test]
    fn is_share_url_rejects_non_share_urls() {
        // publisher/slug registry handles must NOT be treated as share URLs
        assert!(!super::is_share_url("https://ato.run/koh0920/ato-onboarding"));
        assert!(!super::is_share_url("capsule://ato.run/acme/chat"));
        assert!(!super::is_share_url("https://ato.run/dock"));
        assert!(!super::is_share_url("acme/chat"));
        assert!(!super::is_share_url(""));
        // These should NOT match even though they contain /s/
        assert!(!super::is_share_url("https://example.com/s/something"));
        assert!(!super::is_share_url("https://evil.ato.run/s/inject"));
        assert!(!super::is_share_url("https://ato.run.evil.com/s/inject"));
    }

    // ────────────────────────────────────────────────────────────────────────
    // Unit tests for web-project helpers
    // ────────────────────────────────────────────────────────────────────────

    #[test]
    fn extract_localhost_url_parses_vite_output() {
        let line = "  ➜  Local:   http://localhost:5173/";
        // Preserves the original host (localhost) instead of forcing 127.0.0.1
        assert_eq!(
            extract_localhost_url(line),
            Some("http://localhost:5173/".to_string())
        );
    }

    #[test]
    fn extract_localhost_url_parses_127_address() {
        let line = "  - Local:        http://127.0.0.1:3000";
        assert_eq!(
            extract_localhost_url(line),
            Some("http://127.0.0.1:3000/".to_string())
        );
    }

    #[test]
    fn extract_localhost_url_appends_trailing_slash() {
        let line = "Server running at http://localhost:1234";
        assert_eq!(
            extract_localhost_url(line),
            Some("http://localhost:1234/".to_string())
        );
    }

    #[test]
    fn extract_localhost_url_returns_none_for_plain_lines() {
        assert_eq!(extract_localhost_url("Starting compilation..."), None);
        assert_eq!(extract_localhost_url(""), None);
    }

    #[test]
    fn url_port_extracts_port_number() {
        assert_eq!(url_port("http://127.0.0.1:5173/"), Some(5173));
        assert_eq!(url_port("http://127.0.0.1:3000/"), Some(3000));
        assert_eq!(url_port("http://127.0.0.1/"), None);
    }

    #[test]
    fn find_capsule_root_returns_none_for_empty_dir() {
        let dir = std::env::temp_dir().join("ato-desktop-test-empty");
        std::fs::create_dir_all(&dir).ok();
        assert!(find_capsule_root(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_capsule_root_finds_root_level_capsule_toml() {
        let dir = std::env::temp_dir().join("ato-desktop-test-root-capsule");
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("capsule.toml"), "[package]\nname = \"test\"").ok();
        assert_eq!(find_capsule_root(&dir), Some(dir.clone()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_capsule_root_finds_capsule_in_source_subdir_via_state_json() {
        let dir = std::env::temp_dir().join("ato-desktop-test-subdir-capsule");
        let sub = dir.join("my-app");
        std::fs::create_dir_all(&sub).ok();
        let state_dir = dir.join(".ato").join("share");
        std::fs::create_dir_all(&state_dir).ok();
        std::fs::write(
            state_dir.join("state.json"),
            r#"{"sources":[{"id":"my-app"}]}"#,
        )
        .ok();
        std::fs::write(sub.join("capsule.toml"), "[package]\nname = \"sub\"").ok();
        assert_eq!(find_capsule_root(&dir), Some(sub));
        std::fs::remove_dir_all(&dir).ok();
    }

    // ────────────────────────────────────────────────────────────────────────
    // Unit tests for monorepo dev-script discovery
    // ────────────────────────────────────────────────────────────────────────

    fn write_pkg(dir: &std::path::Path, scripts: &str) {
        std::fs::create_dir_all(dir).ok();
        std::fs::write(
            dir.join("package.json"),
            format!(r#"{{"scripts":{{{scripts}}}}}"#),
        )
        .ok();
    }

    #[test]
    fn find_dev_script_dir_finds_direct_match() {
        let root = std::env::temp_dir().join("ato-dev-direct");
        write_pkg(&root, r#""dev":"vite""#);
        let result = find_dev_script_dir(&[root.clone()]);
        assert_eq!(result, Some(root.clone()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn find_dev_script_dir_finds_nested_dev_script() {
        let root = std::env::temp_dir().join("ato-dev-nested");
        let sub = root.join("apps").join("frontend");
        write_pkg(&sub, r#""dev":"next dev""#);
        // root has no dev script
        write_pkg(&root, r#""build":"tsc""#);
        let result = find_dev_script_dir(&[root.clone()]);
        assert_eq!(result, Some(sub));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn find_dev_script_dir_prefers_frontend_named_subdir() {
        let root = std::env::temp_dir().join("ato-dev-prefer-frontend");
        let backend = root.join("apps").join("server");
        let frontend = root.join("apps").join("dashboard");
        write_pkg(&backend, r#""dev":"node index.js""#);
        write_pkg(&frontend, r#""dev":"next dev""#);
        // Both have "dev" but "dashboard" should win
        let result = find_dev_script_dir(&[root.clone()]);
        assert_eq!(result, Some(frontend));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn collect_dev_script_dirs_skips_node_modules() {
        let root = std::env::temp_dir().join("ato-dev-skip-nm");
        let nm = root.join("node_modules").join("some-pkg");
        write_pkg(&nm, r#""dev":"vite""#);
        let mut out = Vec::new();
        collect_dev_script_dirs(&root, 0, 4, &mut out);
        assert!(out.is_empty(), "node_modules should be skipped");
        std::fs::remove_dir_all(&root).ok();
    }

    // ────────────────────────────────────────────────────────────────────────
    // Unit test: start_web_service_from_workspace rejects broken workspaces
    // ────────────────────────────────────────────────────────────────────────

    #[test]
    fn start_web_service_rejects_source_error_in_state_json() {
        let dir = std::env::temp_dir().join("ato-ws-source-error");
        let state_dir = dir.join(".ato").join("share");
        std::fs::create_dir_all(&state_dir).ok();
        std::fs::write(
            state_dir.join("state.json"),
            r#"{
              "sources": [{"id": "myapp", "status": "error", "last_error": "commit not pushed"}],
              "verification": {"result": "warning", "issues": ["source myapp failed: commit not pushed"]}
            }"#,
        )
        .ok();
        let result = super::start_web_service_from_workspace("https://ato.run/s/fake", &dir);
        assert!(result.is_err(), "should fail for a source-error workspace");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("materialization failed") || msg.contains("source 'myapp' failed"),
            "error should mention materialization failure, got: {msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ────────────────────────────────────────────────────────────────────────
    // Unit tests for detect_package_manager
    // ────────────────────────────────────────────────────────────────────────

    fn touch(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).ok();
        std::fs::write(path, "").ok();
    }

    #[test]
    fn detect_pm_finds_pnpm_from_lock_file_at_root() {
        let root = std::env::temp_dir().join("ato-pm-pnpm-lock");
        touch(&root.join("pnpm-lock.yaml"));
        let (pm, install_root) = detect_package_manager(&root);
        assert_eq!(pm, "pnpm");
        assert_eq!(install_root, root.clone());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_pm_finds_pnpm_from_workspace_yaml_ancestor() {
        // Simulates: file2api/pnpm-workspace.yaml exists but pnpm-lock.yaml doesn't
        let root = std::env::temp_dir().join("ato-pm-pnpm-ws");
        let source_dir = root.join("apps").join("dashboard");
        std::fs::create_dir_all(&source_dir).ok();
        touch(&root.join("pnpm-workspace.yaml"));
        let (pm, install_root) = detect_package_manager(&source_dir);
        assert_eq!(pm, "pnpm");
        assert_eq!(install_root, root, "install root should be monorepo root");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_pm_falls_back_to_npm_when_no_lock_file() {
        let root = std::env::temp_dir().join("ato-pm-npm-fallback");
        std::fs::create_dir_all(&root).ok();
        let (pm, install_root) = detect_package_manager(&root);
        assert_eq!(pm, "npm");
        assert_eq!(install_root, root.clone());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_pm_finds_yarn_from_ancestor() {
        let root = std::env::temp_dir().join("ato-pm-yarn");
        let sub = root.join("packages").join("app");
        std::fs::create_dir_all(&sub).ok();
        touch(&root.join("yarn.lock"));
        let (pm, install_root) = detect_package_manager(&sub);
        assert_eq!(pm, "yarn");
        assert_eq!(install_root, root);
        std::fs::remove_dir_all(&root).ok();
    }

    // ────────────────────────────────────────────────────────────────────────
    // E2E: full decap + session start for the real share URL
    //
    // This test calls the real `ato` binary and requires:
    //   1. `ato` to be on PATH (or ATO_DESKTOP_ATO_BIN to be set)
    //   2. network access to ato.run
    //
    // Skipped by default.  Set env var ATO_E2E_TEST=1 to run:
    //   ATO_E2E_TEST=1 cargo test -p ato-desktop e2e_share_url_decap_and_start -- --nocapture
    // ────────────────────────────────────────────────────────────────────────
    #[test]
    fn e2e_share_url_decap_and_start() {
        const SHARE_URL: &str = "https://ato.run/s/01KP5WDF81SQQTVZRF88RNY8MR";

        if std::env::var("ATO_E2E_TEST").as_deref() != Ok("1") {
            eprintln!("[e2e] skipped — set ATO_E2E_TEST=1 to run");
            return;
        }

        // Materialise the share URL and start a session.
        let session = super::resolve_and_start_capsule(SHARE_URL)
            .expect("resolve_and_start_capsule should succeed for the share URL");

        eprintln!("[e2e] session_id  = {}", session.session_id);
        eprintln!("[e2e] handle      = {}", session.handle);
        eprintln!("[e2e] target      = {}", session.target_label);
        eprintln!("[e2e] local_url   = {:?}", session.local_url);
        eprintln!("[e2e] invoke_url  = {:?}", session.invoke_url);
        eprintln!("[e2e] notes       = {:?}", session.notes);

        // Session must have an ID.
        assert!(
            !session.session_id.is_empty(),
            "session_id must not be empty"
        );

        // The handle stored on the session must reference the share URL.
        assert_eq!(
            session.handle, SHARE_URL,
            "session.handle must equal the original share URL"
        );

        // For web-only share URLs (no capsule.toml) the session carries a local_url
        // and uses the WebUrl display strategy.
        if session.display_strategy == CapsuleDisplayStrategy::WebUrl {
            assert!(
                session.local_url.is_some(),
                "WebUrl session must have a local_url"
            );
            eprintln!("[e2e] display     = WebUrl (web dev server)");
            // Kill the spawned dev-server process via the pid embedded in session_id.
            if let Some(pid_str) = session.session_id.strip_prefix("share-web-") {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    std::process::Command::new("kill")
                        .args(["-TERM", &pid.to_string()])
                        .status()
                        .ok();
                    eprintln!("[e2e] killed dev server pid={pid}");
                }
            }
        } else {
            eprintln!("[e2e] display     = {:?}", session.display_strategy);
            // Clean up: stop the ato-managed session.
            let stopped = super::stop_capsule_session(&session.session_id)
                .expect("stop_capsule_session should succeed");
            eprintln!("[e2e] stopped     = {stopped}");
        }
    }
}

// ── Terminal PTY session management ──────────────────────────────────────────

use std::sync::mpsc::{channel, Receiver, Sender};

/// A live terminal session routed through nacelle, owned by `WebViewManager`.
pub struct TerminalProcess {
    pub session_id: String,
    /// Send base64-encoded bytes to the PTY stdin.
    pub input_tx: Sender<Vec<u8>>,
    /// Send a resize request (cols, rows) to the PTY.
    pub resize_tx: Sender<(u16, u16)>,
    /// Receive base64-encoded output chunks from the PTY.
    pub output_rx: Receiver<String>,
}


/// Spawn a terminal session routed through nacelle for a given `session_id`.
///
/// Routes: ato-desktop → nacelle (interactive:true, type:"shell") → PTY → /bin/zsh
/// This ensures nacelle's security stack applies: shell allowlist, env filter,
/// output sanitizer, Seatbelt/bwrap/Landlock.
///
/// The ExecEnvelope is written to a temp file so nacelle's stdin remains free
/// for TerminalCommand JSON messages (nacelle --input reads the whole file,
/// then stdin is used for the command stream).
pub fn spawn_terminal_session(
    session_id: String,
    shell: &str,
    cols: u16,
    rows: u16,
) -> Result<TerminalProcess> {
    // Locate nacelle binary
    let nacelle_bin = std::env::var("NACELLE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("nacelle"));

    // Write ExecEnvelope to a temp file so stdin stays free for TerminalCommands
    let tmp_dir = PathBuf::from(".tmp");
    std::fs::create_dir_all(&tmp_dir).ok();
    let envelope_path = tmp_dir.join(format!("terminal-{session_id}.json"));
    let envelope_json = serde_json::json!({
        "spec_version": "1.0",
        "workload": { "type": "shell" },
        "interactive": true,
        "terminal": {
            "cols": cols,
            "rows": rows,
            "shell": shell,
            "env_filter": "safe"
        }
    });
    std::fs::write(&envelope_path, envelope_json.to_string())
        .with_context(|| format!("failed to write nacelle envelope to {}", envelope_path.display()))?;

    // Spawn nacelle subprocess with stdin/stdout piped
    let mut child = std::process::Command::new(&nacelle_bin)
        .args(["internal", "--input", &envelope_path.to_string_lossy(), "exec"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to spawn nacelle at {}", nacelle_bin.display()))?;

    let mut nacelle_stdin = child.stdin.take().context("nacelle stdin unavailable")?;
    let nacelle_stdout = child.stdout.take().context("nacelle stdout unavailable")?;

    // Channels
    let (input_tx, input_rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = channel();
    let (resize_tx, resize_rx): (Sender<(u16, u16)>, Receiver<(u16, u16)>) = channel();
    let (output_tx, output_rx): (Sender<String>, Receiver<String>) = channel();

    let sid_a = session_id.clone();
    let envelope_path_cleanup = envelope_path.clone();

    // Thread A: nacelle stdout NDJSON → output_tx (extract terminal_data.data_b64)
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let reader = BufReader::new(nacelle_stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match value.get("event").and_then(|e| e.as_str()) {
                Some("terminal_data") => {
                    if let Some(b64) = value.get("data_b64").and_then(|d| d.as_str()) {
                        if output_tx.send(b64.to_string()).is_err() {
                            break;
                        }
                    }
                }
                Some("terminal_exited") => {
                    let code = value.get("exit_code").and_then(|c| c.as_i64());
                    info!(session_id = %sid_a, exit_code = ?code, "nacelle terminal session exited");
                    break;
                }
                _ => {}
            }
        }
        // Clean up envelope temp file
        std::fs::remove_file(&envelope_path_cleanup).ok();
    });

    let sid_b = session_id.clone();

    // Thread B: input_rx + resize_rx → nacelle stdin (TerminalCommand JSON lines)
    std::thread::spawn(move || {
        use std::io::Write;
        loop {
            // Service input bytes
            while let Ok(data) = input_rx.try_recv() {
                let cmd = serde_json::json!({
                    "type": "terminal_input",
                    "session_id": sid_b,
                    "data_b64": base64::engine::general_purpose::STANDARD.encode(&data)
                });
                if writeln!(nacelle_stdin, "{}", cmd).is_err() {
                    return;
                }
                let _ = nacelle_stdin.flush();
            }
            // Service resize requests
            while let Ok((c, r)) = resize_rx.try_recv() {
                let cmd = serde_json::json!({
                    "type": "terminal_resize",
                    "session_id": sid_b,
                    "cols": c,
                    "rows": r
                });
                if writeln!(nacelle_stdin, "{}", cmd).is_err() {
                    return;
                }
                let _ = nacelle_stdin.flush();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });

    info!(session_id = %session_id, shell, cols, rows, "Terminal session spawned via nacelle");

    Ok(TerminalProcess {
        session_id,
        input_tx,
        resize_tx,
        output_rx,
    })
}

/// Spawn a log-tail session for `terminal_stream` capsule sessions.
///
/// The capsule process writes its stdout/stderr to `log_path`. This function
/// opens that file, streams its bytes to xterm.js via `output_rx`, and keeps
/// reading until the receiver is dropped (pane closed). Bare `\n` bytes are
/// normalised to `\r\n` so xterm.js renders line breaks correctly.
pub fn spawn_log_tail_session(session_id: String, log_path: PathBuf) -> Result<TerminalProcess> {
    let (input_tx, _): (Sender<Vec<u8>>, _) = channel();
    let (resize_tx, _): (Sender<(u16, u16)>, _) = channel();
    let (output_tx, output_rx): (Sender<String>, Receiver<String>) = channel();

    let sid = session_id.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        use base64::engine::general_purpose::STANDARD;

        // Wait for the log file to appear (capsule process may still be starting).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if log_path.exists() {
                break;
            }
            if std::time::Instant::now() > deadline {
                let _ = output_tx.send(STANDARD.encode(b"\x1b[31m[Log file not found]\x1b[0m\r\n"));
                info!(session_id = %sid, "log-tail: log file never appeared");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        let mut file = match std::fs::File::open(&log_path) {
            Ok(f) => f,
            Err(e) => {
                let msg = format!("\x1b[31m[Cannot open log: {}]\x1b[0m\r\n", e);
                let _ = output_tx.send(STANDARD.encode(msg.as_bytes()));
                return;
            }
        };

        let mut buf = [0u8; 4096];
        let mut prev_cr = false;
        info!(session_id = %sid, path = %log_path.display(), "log-tail: started");

        loop {
            match file.read(&mut buf) {
                Ok(0) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Ok(n) => {
                    let normalised = normalize_log_newlines(&buf[..n], &mut prev_cr);
                    if output_tx.send(STANDARD.encode(&normalised)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        info!(session_id = %sid, "log-tail: ended");
    });

    Ok(TerminalProcess {
        session_id,
        input_tx,
        resize_tx,
        output_rx,
    })
}

/// Normalise bare `\n` → `\r\n` for xterm.js, preserving existing `\r\n` sequences.
fn normalize_log_newlines(data: &[u8], prev_cr: &mut bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    for &b in data {
        if b == b'\n' && !*prev_cr {
            out.push(b'\r');
        }
        out.push(b);
        *prev_cr = b == b'\r';
    }
    out
}

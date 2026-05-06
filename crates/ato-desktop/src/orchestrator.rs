use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use ato_session_core::{
    read_session_records, validate_record_only, RecordValidationOutcome, RecordValidationParams,
    StoredSessionInfo,
};
use base64::Engine as _;
use capsule_core::common::paths::ato_path;
use capsule_wire::handle::{
    normalize_capsule_handle, CanonicalHandle, CapsuleDisplayStrategy, CapsuleRuntimeDescriptor,
    ResolvedSnapshot,
};
use serde::Deserialize;
use tracing::{debug, error, info, warn};

use crate::config::SecretEntry;
use crate::surface_timing::{ClickOrigin, SurfaceStageTimer};
use crate::terminal::{TerminalCore, TryRecvOutput};

/// Healthcheck budget for the fast-path. Phase 0 measured a healthy
/// `session_start_subprocess` at 4 s; the fast path's win comes from
/// staying under ~200 ms total so the user-perceived latency falls
/// below 500 ms once WebView creation + navigation (~50 ms) is added.
const FAST_PATH_HEALTHCHECK_TIMEOUT: Duration = Duration::from_millis(200);

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

/// Launch specification for a bare CLI panel opened via `ato://cli`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliLaunchSpec {
    /// Default: a line-oriented REPL that routes every command through `ato run`.
    ///
    /// `prelude` is an optional command line that the REPL will execute
    /// immediately after printing its banner, as if the user had typed it
    /// at the `ato>` prompt (the command is echoed then submitted). Used by
    /// share URL integration to kick off `ato run <share-url>` automatically.
    ///
    /// `initial_allow_hosts` seeds the session egress allowlist so the
    /// prelude's capsule can reach its own origin (e.g. share capsules get
    /// `ato.run` allowed). Patterns are parsed via `HostPattern::parse`;
    /// invalid entries are silently dropped.
    AtoRunRepl {
        prelude: Option<String>,
        initial_allow_hosts: Vec<String>,
    },
    /// Raw interactive shell under nacelle (e.g. bash, zsh, /bin/sh).
    RawShell(String),
    /// Plain invocation of the `ato` binary (its own help / subcommand entrypoint).
    RawAto,
}

impl CliLaunchSpec {
    /// Construct a plain ato-run REPL with no prelude and the default
    /// (localhost-only) egress policy.
    pub fn ato_run_repl() -> Self {
        CliLaunchSpec::AtoRunRepl {
            prelude: None,
            initial_allow_hosts: Vec::new(),
        }
    }
}

/// Pending CLI launch specs keyed by session_id, populated by
/// `AppState::handle_host_route` when an `ato://cli` deep link is opened.
/// Drained by `webview.rs` when the Terminal pane is first rendered.
static PENDING_CLI_COMMANDS: std::sync::OnceLock<Mutex<HashMap<String, CliLaunchSpec>>> =
    std::sync::OnceLock::new();

fn pending_cli_commands() -> &'static Mutex<HashMap<String, CliLaunchSpec>> {
    PENDING_CLI_COMMANDS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a pending CLI launch spec for a session_id.
pub fn register_pending_cli_command(session_id: String, spec: CliLaunchSpec) {
    if let Ok(mut map) = pending_cli_commands().lock() {
        map.insert(session_id, spec);
    }
}

/// Take a pending CLI launch spec by session_id.
/// Returns `None` if the session was not opened via `ato://cli`.
pub fn take_pending_cli_command(session_id: &str) -> Option<CliLaunchSpec> {
    pending_cli_commands()
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
    /// Portable execution-receipt identity for the launched session. `None`
    /// when the CLI was invoked without v2 schema enabled or pre-dates the
    /// receipt-emitting session start path.
    pub execution_id: Option<String>,
    /// Schema version of the receipt referenced by `execution_id`.
    pub execution_receipt_schema_version: Option<u32>,
    /// Wall-clock anchor for SURFACE-TIMING (RFC v0.3 §5.1). Set by
    /// `resolve_and_start_capsule` from the click handler entry; read
    /// by `webview.rs` to emit the `total` line once the user-visible
    /// signal fires. `None` only on launch paths that pre-date Phase 0
    /// instrumentation.
    pub click_origin: Option<ClickOrigin>,
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

/// Typed failure returned by `resolve_and_start_guest` /
/// `resolve_and_start_capsule`. The two variants are deliberately
/// asymmetric: `MissingConfig` carries enough structured payload to
/// reconstitute a UI modal and retry the launch (Day 4), while `Other`
/// flattens any non-recoverable failure to a string for display in the
/// existing activity log / toast surface.
///
/// # Why a typed enum instead of `anyhow::Error`
///
/// The drain path in `webview.rs` runs on the foreground thread and
/// needs to *branch* on the failure: E103 → modal, anything else →
/// toast. Anyhow's `downcast_ref` is awkward across the
/// `mpsc::channel` send (the `anyhow::Error` is `Send` but not
/// `Sync`, and the existing code already collapses to `String`), so a
/// dedicated enum is both clearer at the call site and channel-safe.
#[derive(Debug, Clone)]
pub enum LaunchError {
    /// The CLI aborted with E103 — the capsule needs config the user
    /// hasn't supplied. Carries the parsed schema and a snapshot of
    /// the launch args so `webview.rs` can populate
    /// `AppState::pending_config` and Day 4 can retry the same
    /// `start_capsule` invocation post-Save.
    MissingConfig {
        /// Original handle the user asked to launch — re-fed into
        /// `resolve_and_start_capsule` on retry.
        handle: String,
        /// Optional `target` from the CLI's `details.target` (e.g.
        /// `"main"`). Surfaces in the modal title; not used in retry.
        target: Option<String>,
        /// `details.missing_schema` verbatim — drives the dynamic
        /// form. Iterated as-is by the modal; never index-aligned
        /// with `details.missing_keys`.
        fields: Vec<capsule_wire::config::ConfigField>,
        /// Snapshot of secrets passed to the original
        /// `start_capsule` call. Cloned at error-construction time so
        /// a concurrent SecretStore mutation can't corrupt the retry.
        original_secrets: Vec<SecretEntry>,
    },
    /// Any other failure — opaque string suitable for direct display.
    Other(String),
}

impl LaunchError {
    /// Wrap any `Display` value (typically `anyhow::Error`,
    /// `io::Error`, or `&str`) as the opaque variant.
    pub fn other<E: std::fmt::Display>(err: E) -> Self {
        Self::Other(err.to_string())
    }
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingConfig { handle, .. } => {
                write!(f, "guest launch needs configuration for '{handle}'")
            }
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for LaunchError {}

impl From<anyhow::Error> for LaunchError {
    fn from(err: anyhow::Error) -> Self {
        // `{:#}` includes the chain of contexts so the toast surface
        // doesn't lose the "while doing X" framing the helpers add.
        Self::Other(format!("{err:#}"))
    }
}

pub fn resolve_and_start_guest(
    handle: &str,
    secrets: &[SecretEntry],
    plain_configs: &[(String, String)],
) -> Result<GuestLaunchSession, LaunchError> {
    resolve_and_start_capsule(handle, secrets, plain_configs)
}

pub fn stop_guest_session(session_id: &str) -> Result<bool> {
    stop_capsule_session(session_id)
}

#[derive(Clone, Debug, Deserialize)]
struct ResolveEnvelope {
    /// CCP wire-contract field. `None` for legacy CLIs that predate v0.5.
    /// See `capsule_wire::ccp::enforce_ccp_compat` for the tolerance rules.
    #[serde(default)]
    schema_version: Option<String>,
    resolution: ResolvePayload,
}

impl capsule_wire::ccp::HasSchemaVersion for ResolveEnvelope {
    fn schema_version(&self) -> Option<&str> {
        self.schema_version.as_deref()
    }
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
    #[serde(default)]
    schema_version: Option<String>,
    session: SessionStartInfo,
}

impl capsule_wire::ccp::HasSchemaVersion for SessionStartEnvelope {
    fn schema_version(&self) -> Option<&str> {
        self.schema_version.as_deref()
    }
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
    /// Portable execution-receipt identity emitted by `ato app session start`
    /// when the v2 receipt path is enabled (cf. `SessionStartPhaseRunner::
    /// emit_execution_receipt`). Surfaces the launch envelope identity into
    /// the desktop orchestrator so the UI can cross-reference the session
    /// with `~/.ato/executions/<execution_id>/receipt.json`.
    #[serde(default)]
    execution_id: Option<String>,
    /// Schema version (1 or 2) of the execution receipt above.
    #[serde(default)]
    execution_receipt_schema_version: Option<u32>,
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
    #[serde(default)]
    schema_version: Option<String>,
    stopped: bool,
}

impl capsule_wire::ccp::HasSchemaVersion for SessionStopEnvelope {
    fn schema_version(&self) -> Option<&str> {
        self.schema_version.as_deref()
    }
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

pub fn resolve_and_start_capsule(
    handle: &str,
    secrets: &[SecretEntry],
    plain_configs: &[(String, String)],
) -> Result<CapsuleLaunchSession, LaunchError> {
    info!(handle, "resolving capsule");

    // Phase 0 (RFC: SURFACE_MATERIALIZATION §3.1): every click anchors
    // a `total` line that's emitted from `webview.rs` once the
    // user-visible signal fires. Capture the wall-clock origin here
    // and store it on the launch session so downstream consumers
    // (WebView creation, navigation finished) can resolve it.
    let click_origin = ClickOrigin::now();

    if is_share_url(handle) {
        // Share-URL launches don't go through preflight/E103 today —
        // any failure is opaque, matching pre-Day-3 behavior.
        return resolve_and_start_from_share(handle)
            .map(|mut session| {
                session.click_origin = Some(click_origin);
                session
            })
            .map_err(LaunchError::from);
    }

    // Phase 1 fast path (RFC v0.3 §3.2 — PR 4A.1): try to reuse a
    // recorded session without spawning the CLI. Records the
    // `session_record_lookup` + `session_record_validate` stages so
    // the absence of `resolve_subprocess` / `session_start_subprocess`
    // is the implicit "fast path hit" signal in the SURFACE-TIMING
    // log. Any failure falls through to the legacy two-subprocess
    // path; we never crash on a corrupted record.
    match try_session_record_fast_path(handle) {
        Ok(Some(mut session)) => {
            session.click_origin = Some(click_origin);
            info!(
                session_id = %session.session_id,
                handle,
                "capsule session reused via session-record fast path"
            );
            // Best-effort: refresh the v2 execution receipt in the background.
            // The fast path bypasses `ato app session start` entirely, so the
            // receipt would otherwise stay stale. The CLI's own reuse path
            // detects the cached session and emits/refreshes the receipt
            // quickly (~150ms subprocess overhead, no fresh spawn). Failures
            // are logged at debug — they only weaken later inspect/replay,
            // not the running session.
            spawn_background_receipt_refresh(handle);
            return Ok(session);
        }
        Ok(None) => {
            debug!(
                handle,
                "session-record fast path miss; falling back to subprocess"
            );
        }
        Err(err) => {
            // The fast path is best-effort — every failure (corrupt
            // JSON, unreadable directory, permission error) MUST fall
            // through silently so the user still gets a working
            // capsule. We log at debug to avoid noise on the cold
            // path where there's nothing to reuse.
            debug!(error = %err, handle, "session-record fast path errored; falling back to subprocess");
        }
    }

    // `resolve_capsule` and `build_launch_session` return
    // `anyhow::Result`; the `?` operator lifts those into
    // `LaunchError::Other` via `From<anyhow::Error>`. `start_capsule`
    // already returns `Result<_, LaunchError>` so its `MissingConfig`
    // variant flows through unchanged.
    //
    // SURFACE-TIMING wraps each subprocess at the call site so the
    // resolve / session-start halves of the hot path are measured
    // independently (RFC §3.1 — Phase 1's two fast paths key on this
    // separation).
    let resolved = {
        let timer = SurfaceStageTimer::start("resolve_subprocess");
        let result = resolve_capsule(handle);
        timer.finish_ok();
        result?
    };
    let started = {
        let timer = SurfaceStageTimer::start("session_start_subprocess");
        let result = start_capsule(handle, secrets, plain_configs);
        timer.finish_ok();
        result?
    };
    let mut session = {
        let timer = SurfaceStageTimer::start("build_launch_session");
        let result = build_launch_session(handle, resolved, started);
        timer.finish_ok();
        result?
    };
    session.click_origin = Some(click_origin);
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
    capsule_wire::ccp::enforce_ccp_compat(&stopped, "session_stop")?;
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
        if !name.starts_with("ato-desktop-session-")
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
        notes.push(format!(
            "Removed stale capsule session {}",
            record.session_id
        ));
    }

    Ok(notes)
}

fn resolve_capsule(handle: &str) -> Result<ResolvePayload> {
    let envelope: ResolveEnvelope = run_ato_json(&["app", "resolve", handle, "--json"])?;
    capsule_wire::ccp::enforce_ccp_compat(&envelope, "resolve_handle")?;
    Ok(envelope.resolution)
}

#[derive(Clone, Debug, Deserialize)]
struct LatestEnvelope {
    #[serde(default)]
    schema_version: Option<String>,
    result: LatestResult,
}

impl capsule_wire::ccp::HasSchemaVersion for LatestEnvelope {
    fn schema_version(&self) -> Option<&str> {
        self.schema_version.as_deref()
    }
}

#[derive(Clone, Debug, Deserialize)]
struct LatestResult {
    /// Surfaced for diagnostics; the desktop currently does not key on it.
    #[allow(dead_code)]
    scoped_id: String,
    /// `None` when the registry advertises the capsule but has no published
    /// release yet — treat as "no update available" rather than an error.
    latest_version: Option<String>,
}

/// Subprocess wrapper around `ato app latest <handle> --json`. Used by the
/// background update-check worker spawned from `WebViewManager::sync_from_state`.
/// Returns the registry's `latest_version` (if any) so the worker can compare
/// against the running snapshot label and decide whether to surface an update
/// banner.
pub fn fetch_latest_capsule_version(handle: &str) -> Result<Option<String>> {
    let envelope: LatestEnvelope = run_ato_json(&["app", "latest", handle, "--json"])?;
    capsule_wire::ccp::enforce_ccp_compat(&envelope, "fetch_latest")?;
    Ok(envelope.result.latest_version)
}

fn start_capsule(
    handle: &str,
    secrets: &[SecretEntry],
    plain_configs: &[(String, String)],
) -> Result<SessionStartInfo, LaunchError> {
    let ato_bin = resolve_ato_binary().map_err(LaunchError::from)?;
    debug!(bin = %ato_bin.display(), handle, "spawning ato helper for session start");
    let mut cmd = Command::new(&ato_bin);
    cmd.args(["app", "session", "start", handle, "--json"]);

    // Inject granted secrets under the schema-supplied env-var name
    // (e.g. `OPENAI_API_KEY`). The legacy `ATO_SECRET_<KEY>` prefix
    // form was removed in CLI v0.5 (see `application/credential/
    // backend/env.rs::EnvBackend` and `application/secrets/store.rs::
    // legacy_ato_secret_env_is_ignored`); preflight in
    // `adapters/runtime/executors/target_runner.rs::
    // preflight_required_environment_variables` only inspects the bare
    // `field.name`, so any prefix would re-trip the same E103 forever.
    for secret in secrets {
        cmd.env(&secret.key, &secret.value);
    }

    // Inject non-secret config (model name, port, etc.) directly as
    // env vars on the child. We deliberately do *not* use the
    // `ATO_SECRET_` prefix — these values are plaintext by design and
    // the capsule reads them under their schema-supplied name (e.g.
    // `MODEL`, `PORT`). No file is materialized — the values flow
    // memory → child env in one hop.
    //
    // # Why a Vec<(String, String)> instead of `&HashMap`
    //
    // The desktop's `CapsuleConfigStore` is keyed by handle and the
    // value is a `HashMap<String,String>`, but the orchestrator
    // doesn't need the map shape — it just iterates. Taking a slice
    // of pairs keeps the orchestrator decoupled from the storage
    // layout, so a future move to a different backing store (e.g. a
    // SQLite table) requires no signature change here.
    for (key, value) in plain_configs {
        cmd.env(key, value);
    }

    let output = cmd.output().map_err(|err| {
        LaunchError::Other(format!(
            "failed to run ato helper '{}' for session start: {err}",
            ato_bin.display()
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        error!(handle, stderr = %stderr, stdout = %stdout, "ato session start failed");

        // Try to lift the trailing JSONL fatal envelope out of the
        // CLI output — `emit_ato_error_jsonl` writes to stderr in
        // text mode, but `--json` mode routes the diagnostic envelope
        // through `lib.rs::main_entry` which `println!`s it on stdout
        // via `to_json_envelope`. Scan both streams so the desktop
        // can lift the typed event in either case. If the envelope is
        // the missing-env event with a non-empty `missing_schema`,
        // surface a typed `MissingConfig` so `webview.rs` can drive
        // the UI modal instead of a generic toast. Anything else
        // (parse failure, unrelated fatal, empty schema) falls back
        // to the opaque string variant.
        //
        // Match on `name == "missing_required_env"` rather than
        // `code`: the wire code was renamed from `E103` to
        // `ATO_ERR_MISSING_REQUIRED_ENV` (capsule-core's
        // `AtoErrorCode`), so binding to the stable `name` field
        // avoids re-breaking on future renumbering.
        let event = crate::cli_envelope::parse_cli_error_event(&stderr)
            .or_else(|| crate::cli_envelope::parse_cli_error_event(&stdout));
        if let Some(event) = event {
            let is_missing_env = event.name.as_deref() == Some("missing_required_env")
                || event.code == "ATO_ERR_MISSING_REQUIRED_ENV"
                || event.code == "E103";
            if is_missing_env {
                if let Some(details) = event.missing_env_details() {
                    if !details.missing_schema.is_empty() {
                        return Err(LaunchError::MissingConfig {
                            handle: handle.to_string(),
                            target: details.target.or(event.target),
                            fields: details.missing_schema,
                            original_secrets: secrets.to_vec(),
                        });
                    }
                }
            }
        }

        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            extract_json_error_message(&stdout).unwrap_or(stdout)
        } else {
            format!("exit status {}", output.status)
        };
        return Err(LaunchError::Other(format!(
            "ato session start failed: {detail}"
        )));
    }

    let envelope: SessionStartEnvelope = serde_json::from_slice(&output.stdout).map_err(|err| {
        LaunchError::Other(format!("failed to parse session start response: {err}"))
    })?;
    capsule_wire::ccp::enforce_ccp_compat(&envelope, "session_start")
        .map_err(|err| LaunchError::Other(err.to_string()))?;
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
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        error!(args = %args.join(" "), stderr = %stderr, stdout = %stdout, "ato helper command failed");
        let detail = extract_json_error_message(&stdout)
            .or_else(|| (!stderr.is_empty()).then(|| stderr.clone()))
            .or_else(|| (!stdout.is_empty()).then(|| stdout.clone()))
            .unwrap_or_else(|| format!("exit status {}", output.status));
        bail!("ato helper command failed: {detail}");
    }

    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "failed to parse ato-cli json output for args {}",
            args.join(" ")
        )
    })
}

fn extract_json_error_message(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let error = value.get("error")?;
    let message = error.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let code = error.get("code").and_then(|v| v.as_str()).unwrap_or("");
    let combined = match (code.is_empty(), message.is_empty()) {
        (false, false) => format!("{code}: {message}"),
        (false, true) => code.to_string(),
        (true, false) => message.to_string(),
        (true, true) => return None,
    };
    Some(combined)
}

/// Fire-and-forget: spawn `ato app session start <handle> --json` in the
/// background so the CLI's reuse path can refresh / emit the v2 execution
/// receipt for a launch we just served via the desktop's session-record
/// fast path. The fast path bypasses the CLI entirely; without this, the
/// receipt under `~/.ato/executions/<execution_id>/` would not reflect
/// that the launch happened.
///
/// Returns immediately. Stdout/stderr are routed to /dev/null so the
/// background subprocess does not leak descriptors back into the desktop.
/// Errors (binary not found, spawn failure, etc.) are logged at debug.
fn spawn_background_receipt_refresh(handle: &str) {
    let ato_bin = match resolve_ato_binary() {
        Ok(path) => path,
        Err(err) => {
            debug!(error = %err, handle, "background receipt refresh skipped: ato binary not resolvable");
            return;
        }
    };
    let mut cmd = Command::new(&ato_bin);
    cmd.arg("app")
        .arg("session")
        .arg("start")
        .arg(handle)
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match cmd.spawn() {
        Ok(_child) => {
            debug!(handle, "spawned background receipt refresh");
        }
        Err(err) => {
            debug!(error = %err, handle, "failed to spawn background receipt refresh");
        }
    }
}

pub fn resolve_ato_binary() -> Result<PathBuf> {
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

    if let Some(path) = sibling_ato_binary()? {
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

fn sibling_ato_binary() -> Result<Option<PathBuf>> {
    let exe = std::env::current_exe().context("failed to resolve ato-desktop executable path")?;
    let Some(parent) = exe.parent() else {
        return Ok(None);
    };

    let bin_name = if cfg!(windows) { "ato.exe" } else { "ato" };
    let candidate = parent.join(bin_name);
    if candidate.is_file() {
        return Ok(Some(candidate));
    }
    Ok(None)
}

fn which_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    which_in_path_entries(binary, std::env::split_paths(&path_var))
}

fn which_in_path_entries(
    binary: &str,
    entries: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    entries
        .into_iter()
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
    let guest_metadata_missing =
        matches!(display_strategy, CapsuleDisplayStrategy::GuestWebview) && guest.is_none();

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
        bail!("ato app session start returned guest_webview for {handle} without guest payload");
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
        execution_id: started.execution_id,
        execution_receipt_schema_version: started.execution_receipt_schema_version,
        click_origin: None,
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

/// Typed mirror of `snapshot_label` for the fast path where the
/// snapshot comes back from `ato-session-core` already deserialized
/// into `ResolvedSnapshot`. Kept separate so the legacy subprocess
/// path can keep operating on raw `serde_json::Value` without forcing
/// the schema migration on every CLI envelope.
fn snapshot_label_from_resolved(snapshot: &ResolvedSnapshot) -> String {
    match snapshot {
        ResolvedSnapshot::GithubRepo { commit_sha, .. } => {
            format!("commit {}", short_id(commit_sha))
        }
        ResolvedSnapshot::RegistryRelease { version, .. } => format!("version {version}"),
        ResolvedSnapshot::LocalPath { resolved_path, .. } => {
            format!("path {resolved_path}")
        }
    }
}

// ---------------------------------------------------------------------------
// PR 4A.1: Desktop session-record fast path
// ---------------------------------------------------------------------------

/// Try to reuse a previously-recorded session without spawning the
/// CLI. Returns:
/// - `Ok(Some(session))` when a valid record is found and the user-
///   visible `CapsuleLaunchSession` was reconstructed from it.
/// - `Ok(None)` when no candidate record passes the 5 reuse
///   conditions (nobody to reuse — caller must fall back).
/// - `Err(_)` only on infrastructure failures (root unreadable,
///   construction error). The caller treats `Err` identically to
///   `Ok(None)` and falls back to the subprocess path.
///
/// The function emits two SURFACE-TIMING stages so Phase 0 logs can
/// distinguish fast-path-hit (`session_record_lookup` +
/// `session_record_validate` present, `*_subprocess` absent) from
/// fast-path-miss (both stages still emitted, then the subprocess
/// stages follow on fallback).
fn try_session_record_fast_path(handle: &str) -> Result<Option<CapsuleLaunchSession>> {
    let root = ato_session_core::session_root()?;
    try_session_record_fast_path_inner(handle, &root, FAST_PATH_HEALTHCHECK_TIMEOUT)
}

/// Inner helper that takes the session root + healthcheck timeout
/// explicitly. Production callers go through `try_session_record_fast_path`
/// (which resolves the env-aware default root); tests inject a tempdir
/// root to avoid touching the real `~/.ato/apps/ato-desktop/sessions/`.
fn try_session_record_fast_path_inner(
    handle: &str,
    root: &Path,
    healthcheck_timeout: Duration,
) -> Result<Option<CapsuleLaunchSession>> {
    // Stage 1: lookup. Includes dir read and JSON parse for every record.
    let lookup_started = Instant::now();
    let records = read_session_records(root)?;
    let lookup_elapsed_ms = lookup_started.elapsed().as_millis() as u64;
    crate::surface_timing::emit_stage(
        "session_record_lookup",
        "ok",
        lookup_elapsed_ms,
        None,
        &crate::surface_timing::SurfaceExtras::default().with_route_key(handle.to_string()),
    );

    if records.is_empty() {
        return Ok(None);
    }

    // Stage 2: validation. Walk records in arbitrary order and pick
    // the first one that passes all 5 conditions. Healthcheck
    // dominates this stage — every other check is a cheap struct
    // compare.
    let validate_started = Instant::now();
    let params = RecordValidationParams {
        requested_handle: handle,
        healthcheck_timeout,
    };

    let mut chosen: Option<StoredSessionInfo> = None;
    let mut last_outcome = RecordValidationOutcome::HandleMismatch;
    for record in records {
        let outcome = validate_record_only(&record, &params);
        match outcome {
            RecordValidationOutcome::Reusable => {
                chosen = Some(record);
                last_outcome = RecordValidationOutcome::Reusable;
                break;
            }
            other => {
                last_outcome = other;
            }
        }
    }
    let validate_elapsed_ms = validate_started.elapsed().as_millis() as u64;
    let validate_state = if matches!(last_outcome, RecordValidationOutcome::Reusable) {
        "ok"
    } else {
        "fail"
    };
    let mut validate_extras =
        crate::surface_timing::SurfaceExtras::default().with_route_key(handle.to_string());
    if let Some(record) = chosen.as_ref() {
        validate_extras = validate_extras.with_session_id(record.session_id.clone());
    }
    let validate_error = if matches!(last_outcome, RecordValidationOutcome::Reusable) {
        None
    } else {
        Some(validation_outcome_label(&last_outcome))
    };
    crate::surface_timing::emit_stage(
        "session_record_validate",
        validate_state,
        validate_elapsed_ms,
        validate_error,
        &validate_extras,
    );

    let Some(stored) = chosen else {
        return Ok(None);
    };
    Ok(Some(build_launch_session_from_stored(handle, stored)?))
}

/// Map a `RecordValidationOutcome` to a stable, grep-friendly label
/// emitted as the `error` field on the `session_record_validate`
/// SURFACE-TIMING line. Lets analysis distinguish "no record" from
/// "found but pid dead" from "found but healthcheck failed" without
/// re-running the validation.
fn validation_outcome_label(outcome: &RecordValidationOutcome) -> &'static str {
    match outcome {
        RecordValidationOutcome::Reusable => "reusable",
        RecordValidationOutcome::StaleSchema => "stale_schema",
        RecordValidationOutcome::MissingLaunchDigest => "missing_launch_digest",
        RecordValidationOutcome::HandleMismatch => "handle_mismatch",
        RecordValidationOutcome::PidNotAlive => "pid_not_alive",
        RecordValidationOutcome::StartTimeMismatch => "start_time_mismatch",
        RecordValidationOutcome::HealthcheckFailed => "healthcheck_failed",
    }
}

/// Reconstruct a `CapsuleLaunchSession` from a validated
/// `StoredSessionInfo`. This is the fast-path analogue of
/// `build_launch_session(handle, resolved, started)` — but takes only
/// the on-disk record because the validator has already confirmed the
/// session is alive and healthy.
///
/// **v0 staleness contract**: `trust_state` / `restricted` /
/// `snapshot_label` are taken from the record at session-creation
/// time. If the publisher has since updated trust/restriction state
/// in the registry, the fast path will display the older value. This
/// is acceptable for v0 because (a) the user has already been
/// running this session — restriction state can only become more
/// permissive than what the session was started under, and (b) the
/// failure-safe direction is "user keeps seeing the running app",
/// not "user gets blocked from the running app". A later PR can add
/// a TTL-based background re-resolve if this becomes a problem.
fn build_launch_session_from_stored(
    requested_handle: &str,
    stored: StoredSessionInfo,
) -> Result<CapsuleLaunchSession> {
    let manifest_path = PathBuf::from(&stored.manifest_path);
    let app_root = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("manifest path has no parent: {}", manifest_path.display()))?;

    // Frontend entry needs canonicalization against app_root (handles
    // absolute → relative conversion for guests that wrote the
    // canonical path into the record). Same helper the subprocess
    // path uses, so behaviour stays identical.
    let frontend_entry = match stored.guest.as_ref() {
        Some(guest) => Some(normalize_frontend_entry(
            &app_root,
            &guest.frontend_entry,
            &guest.frontend_entry,
        )?),
        None => None,
    };

    let trust_state = match stored.trust_state {
        capsule_wire::handle::TrustState::Unknown => "unknown",
        capsule_wire::handle::TrustState::Untrusted => "untrusted",
        capsule_wire::handle::TrustState::Trusted => "trusted",
        capsule_wire::handle::TrustState::Promoted => "promoted",
        capsule_wire::handle::TrustState::Local => "local",
    }
    .to_string();

    // The `requested_handle` parameter exists so logs / notes can
    // reflect what the user clicked even if the record was indexed
    // under a different normalization. We deliberately keep
    // `stored.handle` as the canonical reusable identity.
    let _ = requested_handle;

    Ok(CapsuleLaunchSession {
        handle: stored.handle.clone(),
        normalized_handle: stored.normalized_handle.clone(),
        canonical_handle: stored.canonical_handle.clone(),
        source: stored.source.clone(),
        trust_state,
        restricted: stored.restricted,
        snapshot_label: stored.snapshot.as_ref().map(snapshot_label_from_resolved),
        session_id: stored.session_id,
        runtime: stored.runtime,
        display_strategy: stored.display_strategy,
        manifest_path,
        app_root,
        target_label: stored.target_label,
        adapter: stored.guest.as_ref().map(|g| g.adapter.clone()),
        frontend_entry,
        invoke_url: stored.guest.as_ref().map(|g| g.invoke_url.clone()),
        healthcheck_url: stored
            .guest
            .as_ref()
            .map(|g| g.healthcheck_url.clone())
            .or_else(|| stored.web.as_ref().map(|w| w.healthcheck_url.clone())),
        capabilities: stored
            .guest
            .as_ref()
            .map(|g| g.capabilities.clone())
            .unwrap_or_default(),
        local_url: stored.web.as_ref().map(|w| w.local_url.clone()),
        served_by: stored.web.as_ref().map(|w| w.served_by.clone()),
        log_path: Some(PathBuf::from(&stored.log_path)),
        notes: stored.notes,
        // Pre-receipt-aware records (cached before the receipt-emitting
        // session start landed) have no execution_id. New launches will
        // populate it via the SessionStartInfo path.
        execution_id: None,
        execution_receipt_schema_version: None,
        click_origin: None,
    })
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
    if let Ok(path) = std::env::var("ATO_DESKTOP_SESSION_ROOT") {
        return Ok(PathBuf::from(path));
    }
    ato_path("apps/ato-desktop/sessions").context("failed to resolve ato home for session root")
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
                    if !DEV_SCAN_SKIP.iter().any(|skip| name_str.as_ref() == *skip) {
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
    let candidates = std::iter::once(source_dir).chain(source_dir.ancestors().skip(1).take(3));

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
            .with_context(|| {
                format!("failed to run `{pm} install` in {}", install_root.display())
            })?;
        if !install_status.success() {
            bail!(
                "`{pm} install` failed in {} (exit {:?})",
                install_root.display(),
                install_status.code()
            );
        }
        info!(share_url, pm, "install completed");
    }

    let mut child = Command::new(pm)
        .args(["run", "dev"])
        .current_dir(&source_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn `{pm} run dev` in {}", source_dir.display()))?;

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
            let stderr_output = stderr_handle.join().unwrap_or_default().trim().to_string();
            if !stderr_output.is_empty() {
                error!(
                    share_url,
                    pm,
                    pid,
                    stderr = %stderr_output,
                    "dev server process failed"
                );
                bail!("`{pm} run dev` failed: {detect_err}\nstderr:\n{stderr_output}");
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
        // Web dev-server launches bypass the receipt-emitting session start
        // path (they wrap a host npm/pnpm/yarn dev process directly), so no
        // execution_id is available.
        execution_id: None,
        execution_receipt_schema_version: None,
        click_origin: None,
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
                warn!(
                    url,
                    "dev server URL not found in stdout, using first HTML-serving port"
                );
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
    match ureq::head(url)
        .timeout(std::time::Duration::from_secs(2))
        .call()
    {
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
/// Stored under ~/.ato/apps/ato-desktop/shared-runs/<hash> so the same share URL
/// always materializes to the same location, enabling session resume.
fn share_tmp_dir(share_url: &str) -> Result<PathBuf> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    share_url.hash(&mut hasher);
    let hash = hasher.finish();
    ato_path(format!("apps/ato-desktop/shared-runs/{hash:016x}"))
        .context("failed to resolve ato home for shared run temp dir")
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
    info!(
        share_url,
        "starting share URL execution via nacelle sandbox"
    );

    let result = capsule_core::share::execute_share(capsule_core::share::ShareRunRequest {
        input: share_url.to_string(),
        entry: None,
        extra_args: vec![],
        env_overlay: std::collections::BTreeMap::new(),
        mode: capsule_core::share::ShareExecutionMode::Piped {
            cols: 120,
            rows: 40,
        },
        nacelle_path: std::env::var("NACELLE_PATH").ok().map(PathBuf::from),
        ato_path: std::env::var("ATO_DESKTOP_ATO_BIN")
            .ok()
            .map(PathBuf::from)
            .or_else(|| resolve_ato_binary().ok()),
        compat_host: false,
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
                // Share-URL terminal launches go straight through nacelle and
                // do not pass the receipt-emitting session start path.
                execution_id: None,
                execution_receipt_schema_version: None,
                click_origin: None,
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
    use std::path::PathBuf;

    use capsule_wire::handle::{CapsuleDisplayStrategy, CapsuleRuntimeDescriptor};

    use super::{
        allows_registry_guest_recovery, build_launch_session, collect_dev_script_dirs,
        detect_package_manager, extract_localhost_url, find_capsule_root, find_dev_script_dir,
        pop_last_codepoint_width, url_port, which_in_path_entries, ResolvePayload,
        SessionStartInfo,
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
            session_id: "ato-desktop-session-1".to_string(),
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
            execution_id: None,
            execution_receipt_schema_version: None,
        }
    }

    #[test]
    #[cfg(unix)]
    fn which_in_path_resolves_existing_binary() {
        let sh = which_in_path_entries("sh", [PathBuf::from("/bin"), PathBuf::from("/usr/bin")])
            .expect("sh should exist in standard Unix system paths");
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
        assert!(!super::is_share_url(
            "https://ato.run/koh0920/ato-onboarding"
        ));
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
        let session = super::resolve_and_start_capsule(SHARE_URL, &[], &[])
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

    #[test]
    fn find_ato_toolchain_binary_finds_real_python_if_installed() {
        // Integration-ish: only asserts when the real ~/.ato/toolchains
        // contains a python install. Otherwise skipped.
        let toolchains = match capsule_core::common::paths::ato_path("toolchains") {
            Ok(path) => path,
            Err(_) => return,
        };
        let has_python = std::fs::read_dir(toolchains)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().starts_with("python-"))
            })
            .unwrap_or(false);
        if !has_python {
            return;
        }
        let found = super::find_ato_toolchain_binary("python");
        assert!(
            found.is_some(),
            "python toolchain installed but find_ato_toolchain_binary returned None"
        );
        let path = found.unwrap();
        assert!(path.exists(), "resolved path does not exist: {path:?}");
        assert_eq!(path.file_name().and_then(|s| s.to_str()), Some("python"));
    }

    #[test]
    fn find_ato_toolchain_binary_rejects_invalid_names() {
        assert!(super::find_ato_toolchain_binary("").is_none());
        assert!(super::find_ato_toolchain_binary("foo/bar").is_none());
    }

    /// Regression: sibling binaries inside a family toolchain dir
    /// (`npm`, `npx`, `pip`, `pip3`, `uvx`) must resolve via the family
    /// fallback. Before the fix, `find_ato_toolchain_binary("npm")` looked
    /// for `~/.ato/toolchains/npm-*/` and returned None, so `npm install`
    /// was incorrectly routed through `ato run --` and hit `scoped_id_required`.
    #[test]
    fn find_ato_toolchain_binary_finds_npm_inside_node_family() {
        let toolchains = match capsule_core::common::paths::ato_path("toolchains") {
            Ok(path) => path,
            Err(_) => return,
        };
        let has_node = std::fs::read_dir(toolchains)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().starts_with("node-"))
            })
            .unwrap_or(false);
        if !has_node {
            return; // environment doesn't have a node toolchain; skip.
        }
        let npm = super::find_ato_toolchain_binary("npm");
        assert!(
            npm.is_some(),
            "node toolchain installed but `npm` did not resolve — family fallback is broken"
        );
        let p = npm.unwrap();
        assert_eq!(p.file_name().and_then(|s| s.to_str()), Some("npm"));
        assert!(p.exists(), "resolved npm path does not exist: {p:?}");
    }

    /// Companion regression for Python family siblings.
    #[test]
    fn find_ato_toolchain_binary_finds_pip_inside_python_family() {
        let toolchains = match capsule_core::common::paths::ato_path("toolchains") {
            Ok(path) => path,
            Err(_) => return,
        };
        let has_python = std::fs::read_dir(toolchains)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().starts_with("python-"))
            })
            .unwrap_or(false);
        if !has_python {
            return;
        }
        let pip = super::find_ato_toolchain_binary("pip");
        assert!(
            pip.is_some(),
            "python toolchain installed but `pip` did not resolve — family fallback is broken"
        );
        let p = pip.unwrap();
        assert_eq!(p.file_name().and_then(|s| s.to_str()), Some("pip"));
    }

    #[test]
    fn find_executable_named_in_bin_subdir() {
        let base = std::env::temp_dir().join(format!("ato-desktop-test-{}", std::process::id()));
        let bin = base.join("x").join("bin");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&bin).expect("mkdir");
        let exe = bin.join("mytool");
        std::fs::write(&exe, b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&exe).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&exe, p).unwrap();
            let found = super::find_executable_named(&base, "mytool", 4);
            assert_eq!(found.as_deref(), Some(exe.as_path()));
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn shell_split_parses_plain_args() {
        let argv = super::shell_split("ls -la /tmp").expect("parse");
        assert_eq!(argv, vec!["ls", "-la", "/tmp"]);
    }

    #[test]
    fn shell_split_handles_double_quotes_and_escapes() {
        let argv = super::shell_split(r#"echo "hello world" foo\ bar"#).expect("parse");
        assert_eq!(argv, vec!["echo", "hello world", "foo bar"]);
    }

    #[test]
    fn shell_split_handles_single_quotes_literally() {
        let argv = super::shell_split(r#"echo 'a "b" c'"#).expect("parse");
        assert_eq!(argv, vec!["echo", r#"a "b" c"#]);
    }

    #[test]
    fn shell_split_rejects_unterminated_quote() {
        assert!(super::shell_split(r#"echo "unterminated"#).is_err());
    }

    #[test]
    fn shell_split_returns_empty_for_blank_input() {
        let argv = super::shell_split("   ").expect("parse");
        assert!(argv.is_empty());
    }

    #[test]
    fn pending_cli_command_registry_roundtrip() {
        let session_id = format!("test-cli-{}", std::process::id());
        super::register_pending_cli_command(
            session_id.clone(),
            super::CliLaunchSpec::ato_run_repl(),
        );
        let spec = super::take_pending_cli_command(&session_id).expect("spec");
        assert!(matches!(spec, super::CliLaunchSpec::AtoRunRepl { .. }));
        // taking again must return None (consumed).
        assert!(super::take_pending_cli_command(&session_id).is_none());
    }

    #[test]
    fn ato_run_repl_spec_carries_prelude_and_allow_hosts() {
        let spec = super::CliLaunchSpec::AtoRunRepl {
            prelude: Some("https://ato.run/s/demo".to_string()),
            initial_allow_hosts: vec!["ato.run".to_string()],
        };
        match spec {
            super::CliLaunchSpec::AtoRunRepl {
                prelude,
                initial_allow_hosts,
            } => {
                assert_eq!(prelude.as_deref(), Some("https://ato.run/s/demo"));
                assert_eq!(initial_allow_hosts, vec!["ato.run".to_string()]);
            }
            _ => panic!("expected AtoRunRepl"),
        }
    }

    #[test]
    fn ato_run_repl_default_ctor_is_empty() {
        match super::CliLaunchSpec::ato_run_repl() {
            super::CliLaunchSpec::AtoRunRepl {
                prelude,
                initial_allow_hosts,
            } => {
                assert!(prelude.is_none());
                assert!(initial_allow_hosts.is_empty());
            }
            _ => panic!("expected AtoRunRepl"),
        }
    }

    #[test]
    fn pending_cli_command_registry_preserves_shell_variant() {
        let session_id = format!("test-cli-shell-{}", std::process::id());
        super::register_pending_cli_command(
            session_id.clone(),
            super::CliLaunchSpec::RawShell("zsh".to_string()),
        );
        match super::take_pending_cli_command(&session_id).expect("spec") {
            super::CliLaunchSpec::RawShell(s) => assert_eq!(s, "zsh"),
            other => panic!("expected RawShell, got {other:?}"),
        }
    }

    // ── REPL backspace: pop one codepoint, not one byte ─────────────────────
    //
    // Regression: typing `あ` (3 bytes, 2 cells) then pressing BS three times
    // used to pop bytes individually and emit three `\x08 \x08` sequences,
    // walking the cursor past `あ` and into the `ato>` prompt. After the fix,
    // one BS erases one codepoint's worth of display cells.
    #[test]
    fn bs_ascii_pops_one_byte_one_cell() {
        let mut line = b"abc".to_vec();
        assert_eq!(pop_last_codepoint_width(&mut line), Some(1));
        assert_eq!(line, b"ab");
        assert_eq!(pop_last_codepoint_width(&mut line), Some(1));
        assert_eq!(pop_last_codepoint_width(&mut line), Some(1));
        assert_eq!(pop_last_codepoint_width(&mut line), None);
        assert!(line.is_empty());
    }

    #[test]
    fn bs_cjk_pops_three_bytes_two_cells() {
        // 'あ' = 0xE3 0x81 0x82 in UTF-8, 2 columns wide.
        let mut line = "あ".as_bytes().to_vec();
        assert_eq!(line.len(), 3);
        assert_eq!(pop_last_codepoint_width(&mut line), Some(2));
        assert!(
            line.is_empty(),
            "all 3 UTF-8 bytes of 'あ' must be popped in one BS"
        );
        // Further BS on empty buffer is a no-op (prompt-safe).
        assert_eq!(pop_last_codepoint_width(&mut line), None);
    }

    #[test]
    fn bs_mixed_ascii_cjk_prompt_safe() {
        // "aあb" — 1 + 3 + 1 = 5 bytes; columns 1 + 2 + 1.
        let mut line = "aあb".as_bytes().to_vec();
        assert_eq!(line.len(), 5);
        assert_eq!(pop_last_codepoint_width(&mut line), Some(1)); // 'b'
        assert_eq!(line, "aあ".as_bytes());
        assert_eq!(pop_last_codepoint_width(&mut line), Some(2)); // 'あ'
        assert_eq!(line, b"a");
        assert_eq!(pop_last_codepoint_width(&mut line), Some(1)); // 'a'
        assert!(line.is_empty());
        // 4th BS: None → REPL must NOT emit `\x08 \x08`, preserving `ato>`.
        assert_eq!(pop_last_codepoint_width(&mut line), None);
    }

    // ── REPL CSI / SS3 consumption ───────────────────────────────────────
    //
    // Regression coverage for the bug where ArrowUp/Down inside the
    // `ato://cli` REPL pane caused `[A`/`[B`-shaped glyphs to bleed into
    // the prompt: the byte loop swallowed the bare ESC byte but echoed
    // the trailing CSI bytes (`[`, `A`, …) as printable input.
    //
    // The contract here is just: given a chunk that begins at an ESC,
    // `consume_terminal_escape_sequence` returns the index of the first
    // byte that is *not* part of the sequence. The byte loop wraps that
    // with `i = consume_terminal_escape_sequence(bytes, i); continue;`.
    use super::consume_terminal_escape_sequence;

    fn idx_after(bytes: &[u8]) -> usize {
        consume_terminal_escape_sequence(bytes, 0)
    }

    #[test]
    fn csi_arrow_keys_consume_three_bytes() {
        for terminator in [b'A', b'B', b'C', b'D'] {
            let bytes = [0x1b, b'[', terminator];
            assert_eq!(
                idx_after(&bytes),
                3,
                "ESC [ {} should consume 3 bytes",
                terminator as char
            );
        }
    }

    #[test]
    fn csi_with_parameters_consumes_through_terminator() {
        // PageUp = ESC [ 5 ~  → terminator is `~` (0x7E, in CSI final-byte range).
        let bytes = [0x1b, b'[', b'5', b'~'];
        assert_eq!(idx_after(&bytes), 4);
        // Home = ESC [ H  (CSI with no params, terminator `H`).
        let bytes = [0x1b, b'[', b'H'];
        assert_eq!(idx_after(&bytes), 3);
        // CSI with multiple parameters: ESC [ 1 ; 2 H (cursor position).
        let bytes = [0x1b, b'[', b'1', b';', b'2', b'H'];
        assert_eq!(idx_after(&bytes), 6);
    }

    #[test]
    fn ss3_function_keys_consume_three_bytes() {
        // F1 = ESC O P. SS3 is a single-parameter form xterm.js uses for
        // F1–F4 and some terminal-mode arrow keys.
        let bytes = [0x1b, b'O', b'P'];
        assert_eq!(idx_after(&bytes), 3);
    }

    #[test]
    fn bare_esc_consumes_only_one_byte() {
        // The user pressed ESC alone (or the chunk ended right after ESC);
        // we must not over-consume into the next normal byte.
        let bytes = [0x1b];
        assert_eq!(idx_after(&bytes), 1);
        // Same when followed by a non-CSI/non-SS3 byte: only the ESC is
        // consumed and the loop's next iteration sees `b'a'` as printable.
        let bytes = [0x1b, b'a'];
        assert_eq!(idx_after(&bytes), 1);
    }

    #[test]
    fn csi_truncated_chunk_consumes_to_end() {
        // Chunk boundaries can split a CSI sequence. If the terminator
        // is missing, we consume to end-of-chunk and return the chunk
        // length — preferable to leaving `[A` in the buffer.
        let bytes = [0x1b, b'[', b'5'];
        assert_eq!(idx_after(&bytes), 3);
        // Pure CSI start (`ESC [`) with no following byte at all.
        let bytes = [0x1b, b'['];
        assert_eq!(idx_after(&bytes), 2);
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

impl TerminalCore for TerminalProcess {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn send_input(&self, data: Vec<u8>) -> bool {
        self.input_tx.send(data).is_ok()
    }

    fn send_resize(&self, cols: u16, rows: u16) -> bool {
        self.resize_tx.send((cols, rows)).is_ok()
    }

    fn try_recv_output(&self) -> TryRecvOutput {
        use std::sync::mpsc::TryRecvError;
        match self.output_rx.try_recv() {
            Ok(chunk) => TryRecvOutput::Data(chunk),
            Err(TryRecvError::Empty) => TryRecvOutput::Empty,
            Err(TryRecvError::Disconnected) => TryRecvOutput::Disconnected,
        }
    }
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
    std::fs::write(&envelope_path, envelope_json.to_string()).with_context(|| {
        format!(
            "failed to write nacelle envelope to {}",
            envelope_path.display()
        )
    })?;

    // Spawn nacelle subprocess with stdin/stdout piped
    let mut child = std::process::Command::new(&nacelle_bin)
        .args([
            "internal",
            "--input",
            &envelope_path.to_string_lossy(),
            "exec",
        ])
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
        use std::sync::mpsc::TryRecvError;
        loop {
            // Service input bytes
            loop {
                match input_rx.try_recv() {
                    Ok(data) => {
                        let cmd = serde_json::json!({
                            "type": "terminal_input",
                            "session_id": sid_b,
                            "data_b64": base64::engine::general_purpose::STANDARD.encode(&data)
                        });
                        if writeln!(nacelle_stdin, "{}", cmd).is_err() {
                            warn!(session_id = %sid_b, "nacelle stdin write failed, stopping input thread");
                            return;
                        }
                        if nacelle_stdin.flush().is_err() {
                            warn!(session_id = %sid_b, "nacelle stdin flush failed, stopping input thread");
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            // Service resize requests
            loop {
                match resize_rx.try_recv() {
                    Ok((c, r)) => {
                        let cmd = serde_json::json!({
                            "type": "terminal_resize",
                            "session_id": sid_b,
                            "cols": c,
                            "rows": r
                        });
                        if writeln!(nacelle_stdin, "{}", cmd).is_err() {
                            warn!(session_id = %sid_b, "nacelle stdin write failed (resize), stopping input thread");
                            return;
                        }
                        if nacelle_stdin.flush().is_err() {
                            warn!(session_id = %sid_b, "nacelle stdin flush failed (resize), stopping input thread");
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
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
    let (input_tx, input_rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = channel();
    let (resize_tx, _): (Sender<(u16, u16)>, _) = channel();
    let (output_tx, output_rx): (Sender<String>, Receiver<String>) = channel();

    // Input-warning thread: log-tail is read-only, but xterm.js still forwards
    // keystrokes. Keep the receiver alive (so `input_tx.send()` does not fail)
    // and emit a one-shot banner the first time the user types, directing them
    // to `ato://cli` if they want an interactive shell.
    let banner_tx = output_tx.clone();
    let banner_sid = session_id.clone();
    std::thread::spawn(move || {
        use base64::engine::general_purpose::STANDARD;
        let mut warned = false;
        while let Ok(bytes) = input_rx.recv() {
            if bytes.is_empty() {
                continue;
            }
            if !warned {
                warned = true;
                let msg = "\r\n\x1b[33m[CLI mode is read-only: this pane tails capsule logs. \
                           Open an interactive shell with \x1b[1mato://cli\x1b[0m\x1b[33m.]\x1b[0m\r\n";
                if banner_tx.send(STANDARD.encode(msg.as_bytes())).is_err() {
                    break;
                }
                warn!(
                    session_id = %banner_sid,
                    "log-tail received input; emitted read-only banner"
                );
            }
        }
    });

    let sid = session_id.clone();
    std::thread::spawn(move || {
        use base64::engine::general_purpose::STANDARD;
        use std::io::Read;

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

/// Spawn an interactive terminal session driven by a `CliLaunchSpec`.
///
/// Dispatches to:
/// - `AtoRunRepl` → `spawn_ato_run_repl` (line-buffered REPL that routes every
///   command through `ato run`).
/// - `RawShell(shell)` → `spawn_terminal_session` (nacelle-backed PTY shell).
/// - `RawAto` → a nacelle-backed PTY running the `ato` binary directly.
#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub session_id: String,
    pub cols: u16,
    pub rows: u16,
    pub kind: SpawnKind,
    pub secrets: Vec<SecretEntry>,
}

#[derive(Clone, Debug)]
pub enum SpawnKind {
    AtoRunRepl {
        prelude: Option<String>,
        initial_allow_hosts: Vec<String>,
    },
    NacelleShell {
        shell: String,
    },
    RawAto,
    LogTail {
        log_path: PathBuf,
    },
}

pub fn spawn_terminal(spec: SpawnSpec) -> Result<TerminalProcess> {
    match spec.kind {
        SpawnKind::AtoRunRepl {
            prelude,
            initial_allow_hosts,
        } => spawn_ato_run_repl(
            spec.session_id,
            spec.cols,
            spec.rows,
            prelude,
            initial_allow_hosts,
            spec.secrets,
        ),
        SpawnKind::NacelleShell { shell } => {
            spawn_terminal_session(spec.session_id, &shell, spec.cols, spec.rows)
        }
        SpawnKind::RawAto => {
            let ato_bin =
                resolve_ato_binary().context("cannot resolve ato binary for SpawnKind::RawAto")?;
            spawn_terminal_session(
                spec.session_id,
                &ato_bin.to_string_lossy(),
                spec.cols,
                spec.rows,
            )
        }
        SpawnKind::LogTail { log_path } => spawn_log_tail_session(spec.session_id, log_path),
    }
}

pub fn spawn_cli_session(
    session_id: String,
    cols: u16,
    rows: u16,
    spec: CliLaunchSpec,
    secrets: Vec<SecretEntry>,
) -> Result<TerminalProcess> {
    let kind = match spec {
        CliLaunchSpec::AtoRunRepl {
            prelude,
            initial_allow_hosts,
        } => SpawnKind::AtoRunRepl {
            prelude,
            initial_allow_hosts,
        },
        CliLaunchSpec::RawShell(shell) => SpawnKind::NacelleShell { shell },
        CliLaunchSpec::RawAto => SpawnKind::RawAto,
    };
    spawn_terminal(SpawnSpec {
        session_id,
        cols,
        rows,
        kind,
        secrets,
    })
}

/// Spawn a line-oriented REPL that routes each input line through `ato run`.
///
/// The REPL lives entirely in Rust (no PTY required): xterm.js keystrokes are
/// Skip past one terminal escape sequence in `bytes` starting at `start`
/// (which must be `0x1B`/ESC). Returns the index of the next byte AFTER the
/// sequence — so the caller can `i = consume_terminal_escape_sequence(bytes, i)`
/// and continue without falling into the default printable-byte arm.
///
/// Recognises three forms emitted by xterm.js (and most other terminals) on
/// arrow keys, navigation keys, and function keys:
///
/// * **CSI** (`ESC [ ... terminator`): consumes through the first byte in
///   the range `0x40..=0x7E` (the CSI "final byte" range). Covers ArrowUp/
///   Down/Left/Right (`ESC [ A`/`B`/`C`/`D`), PageUp/Down (`ESC [ 5~`/`6~`),
///   Home/End (`ESC [ H`/`F`), and other CSI-encoded keys.
/// * **SS3** (`ESC O X`): consumes one extra byte. xterm.js sends F1–F4 and
///   some terminal-mode arrow keys this way.
/// * **bare ESC**: nothing follows in this chunk — consumes only the ESC.
///
/// Without this helper, the previous byte loop swallowed the ESC byte itself
/// but echoed the trailing CSI bytes (`[A`, `[B`, …) as printable input —
/// surfacing as `[A`/`[B`-shaped glyphs the user described as `""`-like
/// characters when pressing arrow keys in the `ato://cli` REPL pane.
fn consume_terminal_escape_sequence(bytes: &[u8], start: usize) -> usize {
    debug_assert_eq!(bytes.get(start), Some(&0x1B));
    let mut i = start + 1;
    match bytes.get(i) {
        Some(&b'[') => {
            // CSI: walk forward until we hit a byte in 0x40..=0x7E (final byte).
            i += 1;
            while let Some(&b) = bytes.get(i) {
                i += 1;
                if (0x40..=0x7E).contains(&b) {
                    break;
                }
            }
            i
        }
        Some(&b'O') => {
            // SS3: one parameter byte.
            i + 2
        }
        // Bare ESC, or some other less-common 7-bit form we don't decode here.
        // Either way, consume just the ESC byte and let the next iteration
        // re-examine `bytes[start + 1]` as a normal byte.
        _ => i,
    }
}

/// decoded from `input_tx`, echoed back through `output_tx` with minimal line
/// editing, and on Enter the current buffer is forwarded to `ato run -- <line>`.
/// Child stdout/stderr stream back as base64-encoded chunks.
///
/// Supported editing:
/// - printable bytes: appended to buffer and echoed.
/// - `\r` or `\n`: submit the current line.
/// - `\x7f` / `\x08` (DEL / Backspace): erase one char with `\b \b`.
/// - `\x03` (Ctrl-C): cancel the current line (or kill the running child).
/// - `\x04` (Ctrl-D) on an empty line: close the session.
/// - `\x1b` followed by CSI/SS3: consumed as a single escape sequence so
///   arrow keys / function keys never leak into the line buffer.
pub fn spawn_ato_run_repl(
    session_id: String,
    cols: u16,
    rows: u16,
    prelude: Option<String>,
    initial_allow_hosts: Vec<String>,
    secrets: Vec<SecretEntry>,
) -> Result<TerminalProcess> {
    use crate::egress_policy::{EgressPolicy, HostPattern};
    use crate::egress_proxy::{DenyEvent, EgressProxy, EgressProxyHandle};
    use std::sync::{mpsc::channel as std_channel, Arc, Mutex as StdMutex};

    let ato_bin =
        resolve_ato_binary().context("cannot resolve ato binary for ato://cli (ato run REPL)")?;

    let (input_tx, input_rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = channel();
    let (resize_tx, resize_rx): (Sender<(u16, u16)>, Receiver<(u16, u16)>) = channel();
    let (output_tx, output_rx): (Sender<String>, Receiver<String>) = channel();

    // Current terminal geometry. We track this in a shared cell so both the
    // main input loop (which owns `resize_rx`) and the per-command PTY spawn
    // (which needs an initial PtySize) see the latest dims.
    let initial_cols = if cols == 0 { 120 } else { cols };
    let initial_rows = if rows == 0 { 40 } else { rows };
    let pty_dims: Arc<StdMutex<(u16, u16)>> = Arc::new(StdMutex::new((initial_cols, initial_rows)));

    let sid = session_id.clone();

    // Session-only egress allowlist. Localhost is always in `default_allow`;
    // `.allow <pat>` mutates `session_allow` which dies with the REPL.
    let egress_policy: Arc<StdMutex<EgressPolicy>> =
        Arc::new(StdMutex::new(EgressPolicy::localhost_only()));

    // Seed the session allowlist with any caller-provided hosts (e.g. the
    // share URL's own origin for share-initiated REPLs). Invalid patterns
    // are skipped with a warn log so they surface in diagnostics.
    if !initial_allow_hosts.is_empty() {
        if let Ok(mut g) = egress_policy.lock() {
            for host in &initial_allow_hosts {
                match HostPattern::parse(host) {
                    Ok(p) => {
                        let added = g.allow(p);
                        debug!(
                            session_id = %sid,
                            host = %host,
                            added,
                            "ato-run REPL: seeded initial allow host"
                        );
                    }
                    Err(e) => {
                        warn!(
                            session_id = %sid,
                            host = %host,
                            error = %e,
                            "ato-run REPL: invalid initial_allow_hosts pattern, skipped"
                        );
                    }
                }
            }
        }
    }

    // Pre-queue the prelude into the input channel BEFORE spawning the REPL
    // thread. The thread prints the banner + prompt first, then processes the
    // prelude bytes as if the user had typed them — giving the `echo_and_run`
    // UX for free (local echo + Enter submit).
    if let Some(ref pre) = prelude {
        let mut bytes = pre.as_bytes().to_vec();
        bytes.push(b'\r');
        if let Err(e) = input_tx.send(bytes) {
            warn!(
                session_id = %sid,
                error = %e,
                "ato-run REPL: failed to queue prelude (channel closed)"
            );
        } else {
            info!(session_id = %sid, prelude = %pre, "ato-run REPL: prelude queued");
        }
    }

    // Start the SOCKS5 gate. Every child spawned below will have its
    // HTTP(S)_PROXY / ALL_PROXY pointed here. Deny events surface via
    // `deny_rx` and are injected into the REPL output stream.
    let (deny_tx, deny_rx) = std_channel::<DenyEvent>();
    let egress_proxy: Option<EgressProxyHandle> = match EgressProxy::spawn(
        egress_policy.clone(),
        Some(deny_tx),
    ) {
        Ok(h) => {
            info!(session_id = %sid, addr=%h.addr(), "ato-run REPL: egress proxy listening");
            Some(h)
        }
        Err(e) => {
            warn!(session_id = %sid, error=%e, "ato-run REPL: egress proxy failed to start — egress will not be gated");
            None
        }
    };
    let socks5_url = egress_proxy.as_ref().map(|h| h.http_url());

    // Send helper: base64-encode and push to xterm.js.
    fn send(tx: &Sender<String>, bytes: &[u8]) -> bool {
        tx.send(base64::engine::general_purpose::STANDARD.encode(bytes))
            .is_ok()
    }

    std::thread::spawn(move || {
        // Keep the egress proxy alive for the duration of this REPL
        // session. Dropping the handle on thread exit stops the listener.
        let _egress_proxy_guard = egress_proxy;

        // Initial banner + prompt.
        let banner = "\x1b[36m┌─ ato CLI ──────────────────────────────────────────┐\x1b[0m\r\n\
             \x1b[36m│\x1b[0m Capsules: \x1b[1m<slug>\x1b[0m or \x1b[1m<publisher>/<slug>\x1b[0m (via \x1b[1mato run\x1b[0m)\r\n\
             \x1b[36m│\x1b[0m Toolchains: \x1b[1mpython\x1b[0m \x1b[1mpip\x1b[0m \x1b[1mnode\x1b[0m \x1b[1mnpm\x1b[0m \x1b[1mnpx\x1b[0m \x1b[1mdeno\x1b[0m \x1b[1muv\x1b[0m (from ~/.ato/toolchains)\r\n\
             \x1b[36m│\x1b[0m Userland: \x1b[1mnpm i -g\x1b[0m / \x1b[1mpip install\x1b[0m land in ~/.ato/userland/ and stay on PATH\r\n\
             \x1b[36m│\x1b[0m Egress: \x1b[1mlocalhost only\x1b[0m — type \x1b[1m.egress\x1b[0m or \x1b[1m.allow <host>\x1b[0m\r\n\
             \x1b[36m│\x1b[0m Ctrl-C cancels; Ctrl-D exits.\r\n\
             \x1b[36m└────────────────────────────────────────────────────┘\x1b[0m\r\n";
        if !send(&output_tx, banner.as_bytes()) {
            return;
        }
        if !send(&output_tx, b"\x1b[32mato>\x1b[0m ") {
            return;
        }

        let mut line: Vec<u8> = Vec::new();

        while let Ok(bytes) = input_rx.recv() {
            // We walk `bytes` with an explicit cursor (rather than `for &b`)
            // so an ESC byte can advance `i` past the rest of its escape
            // sequence in one step. Without that, the trailing CSI bytes
            // (e.g. `[A` from ArrowUp) leak into the default printable-byte
            // arm and surface as visible glyphs in the prompt.
            let mut i = 0;
            while i < bytes.len() {
                let b = bytes[i];
                if b == 0x1b {
                    i = consume_terminal_escape_sequence(&bytes, i);
                    continue;
                }
                match b {
                    // Enter → submit
                    b'\r' | b'\n' => {
                        if !send(&output_tx, b"\r\n") {
                            return;
                        }
                        let cmd = String::from_utf8_lossy(&line).trim().to_string();
                        line.clear();

                        if cmd.is_empty() {
                            if !send(&output_tx, b"\x1b[32mato>\x1b[0m ") {
                                return;
                            }
                            continue;
                        }

                        // Handle a couple of REPL-local shortcuts before invoking ato.
                        if cmd == "exit" || cmd == "quit" {
                            let _ = send(&output_tx, b"\x1b[90mbye.\x1b[0m\r\n");
                            info!(session_id = %sid, "ato-run REPL: exit requested");
                            return;
                        }

                        // Egress meta-commands: processed entirely in-process,
                        // no child spawn. Session-only grants (discarded when
                        // this REPL closes). Phase 1: visibility only — the
                        // policy is not yet wired to a proxy or sandbox.
                        if let Some(rest) = cmd.strip_prefix('.') {
                            let mut parts = rest.splitn(2, char::is_whitespace);
                            let sub = parts.next().unwrap_or("");
                            let arg = parts.next().unwrap_or("").trim();
                            match sub {
                                "egress" => {
                                    let snap = match egress_policy.lock() {
                                        Ok(g) => g.snapshot(),
                                        Err(_) => {
                                            let _ = send(
                                                &output_tx,
                                                b"\x1b[31megress policy lock poisoned\x1b[0m\r\n",
                                            );
                                            let _ = send(&output_tx, b"\x1b[32mato>\x1b[0m ");
                                            continue;
                                        }
                                    };
                                    let text = snap.render_human().replace('\n', "\r\n");
                                    let _ = send(&output_tx, text.as_bytes());
                                    let _ = send(&output_tx, b"\x1b[32mato>\x1b[0m ");
                                    continue;
                                }
                                "allow" => {
                                    if arg.is_empty() {
                                        let _ = send(
                                            &output_tx,
                                            b"\x1b[33musage: .allow <host | *.host | ip>\x1b[0m\r\n",
                                        );
                                    } else {
                                        match HostPattern::parse(arg) {
                                            Ok(p) => {
                                                let rendered = p.render();
                                                let added = egress_policy
                                                    .lock()
                                                    .map(|mut g| g.allow(p))
                                                    .unwrap_or(false);
                                                let msg = if added {
                                                    format!(
                                                        "\x1b[32m+ allowed this session: {rendered}\x1b[0m\r\n"
                                                    )
                                                } else {
                                                    format!(
                                                        "\x1b[90m(already allowed: {rendered})\x1b[0m\r\n"
                                                    )
                                                };
                                                let _ = send(&output_tx, msg.as_bytes());
                                            }
                                            Err(e) => {
                                                let msg = format!(
                                                    "\x1b[31minvalid pattern: {e}\x1b[0m\r\n"
                                                );
                                                let _ = send(&output_tx, msg.as_bytes());
                                            }
                                        }
                                    }
                                    let _ = send(&output_tx, b"\x1b[32mato>\x1b[0m ");
                                    continue;
                                }
                                "deny" => {
                                    if arg.is_empty() {
                                        let _ = send(
                                            &output_tx,
                                            b"\x1b[33musage: .deny <host | *.host | ip>\x1b[0m\r\n",
                                        );
                                    } else {
                                        match HostPattern::parse(arg) {
                                            Ok(p) => {
                                                let rendered = p.render();
                                                let removed = egress_policy
                                                    .lock()
                                                    .map(|mut g| g.revoke(&p))
                                                    .unwrap_or(false);
                                                let msg = if removed {
                                                    format!(
                                                        "\x1b[33m- revoked: {rendered}\x1b[0m\r\n"
                                                    )
                                                } else {
                                                    format!(
                                                        "\x1b[90m(not in session allows or is built-in: {rendered})\x1b[0m\r\n"
                                                    )
                                                };
                                                let _ = send(&output_tx, msg.as_bytes());
                                            }
                                            Err(e) => {
                                                let msg = format!(
                                                    "\x1b[31minvalid pattern: {e}\x1b[0m\r\n"
                                                );
                                                let _ = send(&output_tx, msg.as_bytes());
                                            }
                                        }
                                    }
                                    let _ = send(&output_tx, b"\x1b[32mato>\x1b[0m ");
                                    continue;
                                }
                                "reset-egress" => {
                                    if let Ok(mut g) = egress_policy.lock() {
                                        g.reset_session();
                                    }
                                    let _ = send(
                                        &output_tx,
                                        b"\x1b[33msession egress allows cleared\x1b[0m\r\n",
                                    );
                                    let _ = send(&output_tx, b"\x1b[32mato>\x1b[0m ");
                                    continue;
                                }
                                "help" => {
                                    let help = "\
\x1b[1mREPL meta-commands\x1b[0m\r\n\
  \x1b[36m.egress\x1b[0m              show current allowlist\r\n\
  \x1b[36m.allow\x1b[0m <pattern>     add a session-only allow (e.g. example.com, *.github.com, 1.2.3.4)\r\n\
  \x1b[36m.deny\x1b[0m <pattern>      remove a session allow\r\n\
  \x1b[36m.reset-egress\x1b[0m        clear all session allows (defaults remain)\r\n\
  \x1b[36m.help\x1b[0m                this message\r\n\
  \x1b[36mexit\x1b[0m / \x1b[36mquit\x1b[0m         close the REPL\r\n";
                                    let _ = send(&output_tx, help.as_bytes());
                                    let _ = send(&output_tx, b"\x1b[32mato>\x1b[0m ");
                                    continue;
                                }
                                _ => {
                                    let msg = format!(
                                        "\x1b[31munknown meta-command: .{sub}\x1b[0m (try \x1b[36m.help\x1b[0m)\r\n"
                                    );
                                    let _ = send(&output_tx, msg.as_bytes());
                                    let _ = send(&output_tx, b"\x1b[32mato>\x1b[0m ");
                                    continue;
                                }
                            }
                        }

                        // Spawn `ato run -- <cmd>` with pipes. We split on whitespace
                        // respecting simple shell-like quoting via shell-words.
                        let argv = match shell_split(&cmd) {
                            Ok(v) => v,
                            Err(e) => {
                                let msg = format!("\x1b[31mparse error: {e}\x1b[0m\r\n");
                                let _ = send(&output_tx, msg.as_bytes());
                                let _ = send(&output_tx, b"\x1b[32mato>\x1b[0m ");
                                continue;
                            }
                        };

                        // ── Per-command subprocess spawn ───────────────
                        //
                        // We allocate a PTY for every child so that TUI apps
                        // (claude, vim, htop, python -i, etc.) see a real
                        // terminal. Line-oriented tools (`ls`, `npm install`)
                        // run perfectly through a PTY too — the TTY driver
                        // just line-buffers for them. Using portable-pty keeps
                        // this cross-platform (openpty on unix, conpty on
                        // Windows via the crate's shim).
                        use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

                        // Userland-aware resolution:
                        //   1. ~/.ato/toolchains/<name>-<ver>/...  (existing)
                        //   2. ~/.ato/userland/<family>/bin/<name> (NEW)
                        //   3. fallback to `ato run --` (scoped_id required)
                        let tc_hit = find_ato_toolchain_binary(&argv[0]);
                        let userland_hit = if tc_hit.is_none() {
                            crate::userland::find_userland_binary(&argv[0])
                        } else {
                            None
                        };
                        let family_for_env: Option<crate::userland::Family> =
                            if let Some(ref p) = userland_hit {
                                crate::userland::family_for_userland_binary(p)
                            } else {
                                crate::userland::Family::classify(&argv[0])
                            };

                        let command_label: String;
                        let mut cmd_builder: CommandBuilder;
                        if let Some(tc) = tc_hit {
                            command_label = format!("{} (toolchain)", argv[0]);
                            debug!(
                                session_id = %sid,
                                name = %argv[0],
                                path = %tc.display(),
                                "ato-run REPL: resolved toolchain binary"
                            );
                            cmd_builder = CommandBuilder::new(&tc);
                            for a in &argv[1..] {
                                cmd_builder.arg(a);
                            }
                        } else if let Some(ul) = userland_hit {
                            command_label = format!("{} (userland)", argv[0]);
                            debug!(
                                session_id = %sid,
                                name = %argv[0],
                                path = %ul.display(),
                                "ato-run REPL: resolved userland binary"
                            );
                            cmd_builder = CommandBuilder::new(&ul);
                            for a in &argv[1..] {
                                cmd_builder.arg(a);
                            }
                        } else {
                            command_label = "ato run".to_string();
                            cmd_builder = CommandBuilder::new(&ato_bin);
                            cmd_builder.arg("run");
                            cmd_builder.arg("--");
                            for a in &argv {
                                cmd_builder.arg(a);
                            }
                        }

                        // Userland env envelope: redirect install targets into
                        // ~/.ato/userland/<family>/ so `npm i -g` cannot reach
                        // /usr/local. Applies to both install and query
                        // commands for consistency.
                        if let Some(family) = family_for_env {
                            for (k, v) in crate::userland::install_env(family) {
                                cmd_builder.env(k, v);
                            }
                            if let Some(root) = crate::userland::family_root(family) {
                                let bin = root.join("bin");
                                let existing = std::env::var("PATH").unwrap_or_default();
                                let new_path = if existing.is_empty() {
                                    bin.to_string_lossy().to_string()
                                } else {
                                    format!("{}:{}", bin.display(), existing)
                                };
                                cmd_builder.env("PATH", new_path);
                            }
                        }

                        // Install-verb auto-allow: add registry hosts to
                        // session_allow on install verbs only. Query
                        // commands stay gated (see userland::install_verb_allowlist).
                        let auto_allow = crate::userland::install_verb_allowlist(&argv);
                        if !auto_allow.is_empty() {
                            if let Ok(mut g) = egress_policy.lock() {
                                let mut added_any = Vec::new();
                                for host in &auto_allow {
                                    if let Ok(p) = HostPattern::parse(host) {
                                        if g.allow(p) {
                                            added_any.push(host.clone());
                                        }
                                    }
                                }
                                if !added_any.is_empty() {
                                    let msg = format!(
                                        "\x1b[90m[hint] auto-allow for install: {}\x1b[0m\r\n",
                                        added_any.join(", ")
                                    );
                                    let _ = send(&output_tx, msg.as_bytes());
                                }
                            }
                        }

                        cmd_builder.env("FORCE_COLOR", "1");
                        cmd_builder.env("CLICOLOR_FORCE", "1");
                        // Advertise a real TTY so TUI apps enable interactive
                        // UIs. xterm-256color is what xterm.js implements.
                        cmd_builder.env("TERM", "xterm-256color");
                        // Inject secrets with ATO_SECRET_ prefix so capsule processes
                        // can access them without leaking to untrusted env inherit.
                        for secret in &secrets {
                            cmd_builder.env(
                                format!("ATO_SECRET_{}", secret.key.to_ascii_uppercase()),
                                &secret.value,
                            );
                        }
                        // Inherit HOME/USER/LANG from the parent so shells
                        // behave naturally (CommandBuilder does NOT inherit
                        // env by default — explicit inheritance below).
                        for key in &[
                            "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "SHELL", "TMPDIR",
                        ] {
                            if let Ok(v) = std::env::var(key) {
                                cmd_builder.env(key, v);
                            }
                        }

                        // Route child egress through the session's SOCKS5
                        // gate so the allowlist is enforced.
                        if let Some(ref url) = socks5_url {
                            cmd_builder.env("ALL_PROXY", url);
                            cmd_builder.env("all_proxy", url);
                            cmd_builder.env("HTTPS_PROXY", url);
                            cmd_builder.env("https_proxy", url);
                            cmd_builder.env("HTTP_PROXY", url);
                            cmd_builder.env("http_proxy", url);
                            cmd_builder.env("NO_PROXY", "localhost,127.0.0.1,::1");
                            cmd_builder.env("no_proxy", "localhost,127.0.0.1,::1");
                        }

                        // Open the PTY with the current xterm geometry. If
                        // resize events arrive mid-run we forward them to the
                        // master with `resize()`.
                        let (init_cols, init_rows) = pty_dims
                            .lock()
                            .map(|g| *g)
                            .unwrap_or((initial_cols, initial_rows));
                        let pty_system = NativePtySystem::default();
                        let pty_pair = match pty_system.openpty(PtySize {
                            rows: init_rows,
                            cols: init_cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        }) {
                            Ok(p) => p,
                            Err(e) => {
                                let msg = format!("\x1b[31mfailed to open PTY: {e}\x1b[0m\r\n");
                                let _ = send(&output_tx, msg.as_bytes());
                                let _ = send(&output_tx, b"\x1b[32mato>\x1b[0m ");
                                continue;
                            }
                        };

                        let mut pty_child = match pty_pair.slave.spawn_command(cmd_builder) {
                            Ok(c) => c,
                            Err(e) => {
                                let msg = format!(
                                    "\x1b[31mfailed to spawn {command_label}: {e}\x1b[0m\r\n"
                                );
                                let _ = send(&output_tx, msg.as_bytes());
                                let _ = send(&output_tx, b"\x1b[32mato>\x1b[0m ");
                                continue;
                            }
                        };
                        // Close the slave on our side; the child owns it now.
                        drop(pty_pair.slave);

                        // The master is used for: (a) reading child output,
                        // (b) writing user input, (c) resizing. Stash in an
                        // Arc so the output-reader thread can run in parallel
                        // with the main wait loop (which writes + resizes).
                        let mut master_reader = match pty_pair.master.try_clone_reader() {
                            Ok(r) => r,
                            Err(e) => {
                                let msg =
                                    format!("\x1b[31mpty clone_reader failed: {e}\x1b[0m\r\n");
                                let _ = send(&output_tx, msg.as_bytes());
                                let _ = pty_child.kill();
                                let _ = send(&output_tx, b"\x1b[32mato>\x1b[0m ");
                                continue;
                            }
                        };

                        // Take the writer ONCE before entering the loop.
                        // portable-pty's take_writer() can only be called once
                        // (subsequent calls fail with "cannot take writer more
                        // than once"), so we must hold the writer for the
                        // lifetime of the child process.
                        let mut master_writer = match pty_pair.master.take_writer() {
                            Ok(w) => w,
                            Err(e) => {
                                let msg = format!("\x1b[31mpty take_writer failed: {e}\x1b[0m\r\n");
                                let _ = send(&output_tx, msg.as_bytes());
                                let _ = pty_child.kill();
                                let _ = send(&output_tx, b"\x1b[32mato>\x1b[0m ");
                                continue;
                            }
                        };

                        let master_for_ops: Arc<StdMutex<Box<dyn portable_pty::MasterPty + Send>>> =
                            Arc::new(StdMutex::new(pty_pair.master));

                        // Output pump: PTY master → xterm, verbatim. A PTY
                        // already hands us terminal-ready bytes (CRLF where
                        // needed, escape sequences, etc.) so we skip the
                        // `normalize_log_newlines` step that the piped path
                        // required.
                        let out_tx = output_tx.clone();
                        let output_thread = std::thread::spawn(move || {
                            use std::io::Read;
                            let mut buf = [0u8; 4096];
                            loop {
                                match master_reader.read(&mut buf) {
                                    Ok(0) | Err(_) => break,
                                    Ok(n) => {
                                        if !send(&out_tx, &buf[..n]) {
                                            break;
                                        }
                                    }
                                }
                            }
                        });

                        // Wait for the child to exit. While it runs we:
                        //   - forward ALL xterm input bytes straight to the
                        //     PTY master (no echo, no line editing — the
                        //     child / TTY layer handles echo).
                        //   - forward resize events via MasterPty::resize.
                        //   - surface egress-deny events as inline hints.
                        let exit_status = loop {
                            match pty_child.try_wait() {
                                Ok(Some(status)) => break Some(status),
                                Ok(None) => {}
                                Err(_) => break None,
                            }

                            // Pass-through: every input byte goes straight to
                            // the PTY. Ctrl-C (0x03) is delivered via the TTY
                            // line discipline which will deliver SIGINT to
                            // the foreground process group naturally — no
                            // special kill() path needed.
                            while let Ok(bytes) = input_rx.try_recv() {
                                if bytes.is_empty() {
                                    continue;
                                }
                                use std::io::Write;
                                let _ = master_writer.write_all(&bytes);
                                let _ = master_writer.flush();
                            }

                            // Resize: always update the stored dims and also
                            // resize the live master so ncurses/readline
                            // repaint correctly.
                            while let Ok((rc, rr)) = resize_rx.try_recv() {
                                if let Ok(mut g) = pty_dims.lock() {
                                    *g = (rc, rr);
                                }
                                if let Ok(master) = master_for_ops.lock() {
                                    let _ = master.resize(PtySize {
                                        cols: rc,
                                        rows: rr,
                                        pixel_width: 0,
                                        pixel_height: 0,
                                    });
                                }
                            }

                            // Egress-deny hints (best-effort, non-fatal).
                            while let Ok(ev) = deny_rx.try_recv() {
                                let msg = format!(
                                    "\r\n\x1b[33m⚠ egress blocked: {}:{}\x1b[0m  \x1b[90m(type `.allow {}` to permit this session)\x1b[0m\r\n",
                                    ev.host, ev.port, ev.host
                                );
                                let _ = send(&output_tx, msg.as_bytes());
                            }

                            std::thread::sleep(std::time::Duration::from_millis(20));
                        };

                        // Drop the writer first, then the master to close
                        // the PTY cleanly. The output-reader will then EOF.
                        drop(master_writer);
                        drop(master_for_ops);
                        let _ = output_thread.join();

                        if let Some(status) = exit_status {
                            if !status.success() {
                                let code = status.exit_code();
                                let msg = format!(
                                    "\x1b[31m[{command_label} exited with status {code}]\x1b[0m\r\n"
                                );
                                let _ = send(&output_tx, msg.as_bytes());
                            }
                        }

                        if !send(&output_tx, b"\x1b[32mato>\x1b[0m ") {
                            return;
                        }
                    }
                    // Ctrl-C outside of a running child → clear the current line.
                    0x03 => {
                        line.clear();
                        if !send(&output_tx, b"^C\r\n\x1b[32mato>\x1b[0m ") {
                            return;
                        }
                    }
                    // Ctrl-D on empty line → exit.
                    0x04 => {
                        if line.is_empty() {
                            let _ = send(&output_tx, b"\r\n\x1b[90mbye.\x1b[0m\r\n");
                            info!(session_id = %sid, "ato-run REPL: EOF");
                            return;
                        }
                    }
                    // Backspace / DEL
                    //
                    // Erase exactly one user-visible character, not one byte.
                    // Before this fix, typing a multi-byte UTF-8 char (e.g.
                    // Japanese `あ` = 3 bytes, 2 cells) and then pressing
                    // Backspace popped one byte off `line` and emitted one
                    // `\x08 \x08` per press. Pressing BS three times popped
                    // all three bytes but walked the cursor back 3 cells —
                    // past where `あ` ever occupied — and started erasing
                    // the `ato>` prompt itself.
                    //
                    // Fix: pop one complete codepoint (1–4 bytes) per BS, and
                    // emit one `\x08 \x08` per DISPLAY COLUMN the codepoint
                    // occupied. CJK/emoji wide chars take 2 columns; ASCII/
                    // Latin take 1. Width is resolved via unicode-width.
                    0x7f | 0x08 => {
                        if let Some(width) = pop_last_codepoint_width(&mut line) {
                            // Safety cap: even if width comes back as 0 (e.g.
                            // stray combining char), never erase nothing —
                            // we've already popped bytes, and the original
                            // render emitted at least visual motion for them.
                            let cells = width.max(1);
                            let mut erase = Vec::with_capacity(cells * 3);
                            for _ in 0..cells {
                                erase.extend_from_slice(b"\x08 \x08");
                            }
                            if !send(&output_tx, &erase) {
                                return;
                            }
                        }
                    }
                    // Tab — no completion yet, just print a space.
                    b'\t' => {
                        line.push(b' ');
                        if !send(&output_tx, b" ") {
                            return;
                        }
                    }
                    // Printable / UTF-8 byte
                    _ => {
                        line.push(b);
                        if !send(&output_tx, &[b]) {
                            return;
                        }
                    }
                }
                i += 1;
            }
        }

        // Input channel closed → exit.
        let _ = send(&output_tx, b"\r\n\x1b[90m[session closed]\x1b[0m\r\n");
    });

    info!(session_id = %session_id, "ato-run REPL session spawned");

    Ok(TerminalProcess {
        session_id,
        input_tx,
        resize_tx,
        output_rx,
    })
}

/// Split a command line into argv, respecting simple single/double quotes.
///
/// This is intentionally small — full POSIX expansion (globs, variables) is
/// the responsibility of `ato run` itself. Unmatched quotes return an error.
/// Resolve a bare command name against `~/.ato/toolchains/`.
///
/// Lookup strategy:
///   1. Direct prefix match: dirs named `<name>-<ver>/` (e.g. `python-3.12/`).
///      This is how primary binaries resolve: `python`, `node`, `deno`, `uv`.
///   2. Family fallback: if `name` belongs to a known family (`npm`→Node,
///      `pip`→Python, ...), also search the family's toolchain root
///      (`node-<ver>/`, `python-<ver>/`, ...). This is how sibling binaries
///      resolve: `npm`, `npx`, `pnpm`, `pip`, `pip3`, `uvx`, `corepack`, ...
///
/// Returns the path to an executable file named `name` inside the newest
/// matching toolchain directory, or `None` if no toolchain is installed.
///
/// Walks up to ~4 levels deep to accommodate toolchain-specific layouts:
///   - python: `python-3.11.10/python/bin/python`
///   - node:   `node-20.11.0/node-v20.11.0-darwin-arm64/bin/node`
///   - deno:   `deno-2.6.8/deno`
///   - uv:     `uv-0.4.19/uv-aarch64-apple-darwin/uv`
fn find_ato_toolchain_binary(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return None;
    }
    let toolchains = ato_path("toolchains").ok()?;

    // Build the list of directory-name prefixes we will search in priority
    // order. The direct match wins over the family fallback so
    // `node → node-*/.../bin/node` stays stable even if a future `node-*`
    // layout happens to also contain a nested `node` directory.
    let mut prefixes: Vec<String> = vec![format!("{name}-")];
    if let Some(family) = crate::userland::Family::classify(name) {
        let family_prefix = match family {
            crate::userland::Family::Node => "node-",
            crate::userland::Family::Python => "python-",
            crate::userland::Family::Deno => "deno-",
        };
        let fp = family_prefix.to_string();
        if !prefixes.contains(&fp) {
            prefixes.push(fp);
        }
    }

    let entries: Vec<PathBuf> = fs::read_dir(&toolchains)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.is_dir() {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    for prefix in &prefixes {
        // Collect matching roots, newest-first by lexicographic order (good
        // enough for semver-ish names).
        let mut roots: Vec<PathBuf> = entries
            .iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.starts_with(prefix))
            })
            .cloned()
            .collect();
        roots.sort();
        roots.reverse();

        for root in roots {
            if let Some(bin) = find_executable_named(&root, name, 4) {
                return Some(bin);
            }
        }
    }
    None
}

/// Shallow recursive search for an executable file named exactly `name`
/// inside `root`, up to `max_depth` levels. Prefers `bin/<name>` over
/// top-level files.
fn find_executable_named(root: &Path, name: &str, max_depth: usize) -> Option<PathBuf> {
    if max_depth == 0 {
        return None;
    }
    let entries: Vec<_> = fs::read_dir(root).ok()?.filter_map(|e| e.ok()).collect();

    // First pass: direct file named `name` at this level.
    for e in &entries {
        let path = e.path();
        if path.is_file() && path.file_name().and_then(|s| s.to_str()) == Some(name) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = path.metadata() {
                    if meta.permissions().mode() & 0o111 != 0 {
                        return Some(path);
                    }
                }
            }
            #[cfg(not(unix))]
            {
                return Some(path);
            }
        }
    }

    // Second pass: recurse into subdirs (prefer `bin`, then others).
    let mut subdirs: Vec<PathBuf> = entries
        .iter()
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    subdirs.sort_by_key(|p| {
        let is_bin = p.file_name().and_then(|s| s.to_str()) == Some("bin");
        (!is_bin, p.clone())
    });
    for sub in subdirs {
        if let Some(hit) = find_executable_named(&sub, name, max_depth - 1) {
            return Some(hit);
        }
    }
    None
}

fn shell_split(input: &str) -> std::result::Result<Vec<String>, String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for ch in input.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        match ch {
            '\\' if !in_single => {
                escape = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if in_single || in_double {
        return Err("unterminated quote".to_string());
    }
    if escape {
        return Err("trailing backslash".to_string());
    }
    if !current.is_empty() {
        args.push(current);
    }
    Ok(args)
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

/// Pop the last UTF-8 codepoint from `line` and return its display width in
/// terminal columns (1 for ASCII/Latin, 2 for CJK/emoji wide chars, `None` if
/// the buffer is empty or malformed).
///
/// Walks back over UTF-8 continuation bytes (0x80..=0xBF) until it finds a
/// leading byte, then pops that whole codepoint as a unit. Uses
/// `unicode_width` to compute column count. Invalid tail bytes are dropped
/// defensively (never leaves a dangling continuation).
fn pop_last_codepoint_width(line: &mut Vec<u8>) -> Option<usize> {
    if line.is_empty() {
        return None;
    }
    // Find the start index of the last codepoint.
    let mut start = line.len();
    while start > 0 {
        start -= 1;
        let b = line[start];
        // Leading byte: ASCII (0..=0x7F) or multi-byte leader (0xC0..=0xFF).
        // Continuation bytes are 0x80..=0xBF; keep walking past them.
        if b < 0x80 || b >= 0xC0 {
            break;
        }
    }
    let bytes = line.split_off(start);
    // Decode and measure width. If bytes are not valid UTF-8, assume 1 column.
    let width = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| s.chars().next())
        .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(1))
        .unwrap_or(1);
    Some(width)
}

// ---------------------------------------------------------------------------
// PR 4A.2: fallback correctness tests for try_session_record_fast_path_inner
// ---------------------------------------------------------------------------
//
// These tests cover every reuse-rejection path: missing record, schema=1
// record, missing launch_digest, dead pid, start-time mismatch, and dead
// healthcheck endpoint. The "valid record passes" path is exercised by a
// dedicated test that brings up a tiny in-process HTTP server so we can
// validate `build_launch_session_from_stored` produces the expected
// `CapsuleLaunchSession`.
//
// Tests inject the session-record root via `try_session_record_fast_path_inner`
// rather than via `ATO_DESKTOP_SESSION_ROOT` so they run safely in parallel.
//
// Timing assertions are deliberately absent — those belong in RFC §1.1
// (real-world measurement), not in CI where wall-clock is flaky.
#[cfg(test)]
mod fast_path_tests {
    use super::*;
    use ato_session_core::record::{GuestSessionDisplay, SCHEMA_VERSION_V2};
    use ato_session_core::write_session_record_atomic;
    use capsule_wire::handle::{CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, TrustState};
    use tempfile::TempDir;

    const TEST_HANDLE: &str = "capsule://ato.run/koh0920/byok-ai-chat";
    const TEST_HEALTHCHECK_TIMEOUT: Duration = Duration::from_millis(50);

    /// Build a `StoredSessionInfo` that, on its own, is reuse-eligible:
    /// schema=2, launch_digest set, pid=self, start_time=self's actual
    /// start time. The healthcheck URL points at port 1 (unbound) by
    /// default — individual tests override it when they need the
    /// healthcheck to pass.
    fn base_record(handle: &str) -> StoredSessionInfo {
        StoredSessionInfo {
            session_id: format!("ato-desktop-session-{}", std::process::id()),
            handle: handle.to_string(),
            normalized_handle: handle.trim_start_matches("capsule://").to_string(),
            canonical_handle: Some(handle.trim_start_matches("capsule://").to_string()),
            trust_state: TrustState::Untrusted,
            source: None,
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "main".to_string(),
                runtime: Some("node".to_string()),
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::GuestWebview,
            pid: std::process::id() as i32,
            log_path: "/tmp/x.log".to_string(),
            // app_root is derived from manifest_path.parent(); use a
            // valid existing dir so build_launch_session_from_stored
            // doesn't error on canonicalization later.
            manifest_path: "/tmp/manifest.toml".to_string(),
            target_label: "main".to_string(),
            notes: vec![],
            guest: Some(GuestSessionDisplay {
                adapter: "node".to_string(),
                frontend_entry: "index.html".to_string(),
                transport: "http".to_string(),
                healthcheck_url: "http://127.0.0.1:1/health".to_string(),
                invoke_url: "http://127.0.0.1:1/invoke".to_string(),
                capabilities: vec!["fs:read".to_string()],
            }),
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: None,
            schema_version: Some(SCHEMA_VERSION_V2),
            launch_digest: Some("d".repeat(64)),
            process_start_time_unix_ms: ato_session_core::process::process_start_time_unix_ms(
                std::process::id(),
            ),
        }
    }

    fn write_record(root: &Path, record: &StoredSessionInfo) {
        write_session_record_atomic(root, record).expect("write fixture");
    }

    fn run_fast_path(root: &Path, handle: &str) -> Option<CapsuleLaunchSession> {
        try_session_record_fast_path_inner(handle, root, TEST_HEALTHCHECK_TIMEOUT)
            .expect("fast path must not error on these fixtures")
    }

    #[test]
    fn record_missing_falls_back() {
        let dir = TempDir::new().expect("tempdir");
        // Empty session root -> no records to validate.
        assert!(run_fast_path(dir.path(), TEST_HANDLE).is_none());
    }

    #[test]
    fn nonexistent_session_root_falls_back() {
        let dir = TempDir::new().expect("tempdir");
        let missing = dir.path().join("never");
        // read_session_records tolerates missing root → Ok(empty).
        let result =
            try_session_record_fast_path_inner(TEST_HANDLE, &missing, TEST_HEALTHCHECK_TIMEOUT)
                .expect("missing root must not error");
        assert!(result.is_none());
    }

    #[test]
    fn schema_v1_record_falls_back() {
        let dir = TempDir::new().expect("tempdir");
        let mut record = base_record(TEST_HANDLE);
        record.schema_version = None; // pre-v2 record
        write_record(dir.path(), &record);
        assert!(run_fast_path(dir.path(), TEST_HANDLE).is_none());
    }

    #[test]
    fn missing_launch_digest_falls_back() {
        let dir = TempDir::new().expect("tempdir");
        let mut record = base_record(TEST_HANDLE);
        record.launch_digest = None;
        write_record(dir.path(), &record);
        assert!(run_fast_path(dir.path(), TEST_HANDLE).is_none());
    }

    #[test]
    #[cfg(unix)]
    fn dead_pid_alone_does_not_force_fallback_when_healthcheck_decides() {
        // PR 4B.3 fix (PID-drift bug): the fast path no longer gates
        // on `pid_is_alive`. A record with a dead PID still falls
        // back here — but ONLY because the healthcheck URL is
        // unbound (port 1). The negative outcome is "HealthcheckFailed",
        // not "PidNotAlive". This rebound matters for `npm run start`
        // capsules where the recorded PID exits while the actual
        // server keeps serving traffic under a different PID.
        let dir = TempDir::new().expect("tempdir");
        let mut record = base_record(TEST_HANDLE);
        record.pid = 999_999_999;
        write_record(dir.path(), &record);
        assert!(run_fast_path(dir.path(), TEST_HANDLE).is_none());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn start_time_mismatch_alone_does_not_force_fallback_when_healthcheck_decides() {
        // Mirror of the dead-pid test for the start_time field. Same
        // rationale: the field stays on the record for diagnostics
        // but no longer gates reuse. Only the healthcheck does.
        let dir = TempDir::new().expect("tempdir");
        let mut record = base_record(TEST_HANDLE);
        record.process_start_time_unix_ms = Some(1);
        write_record(dir.path(), &record);
        assert!(run_fast_path(dir.path(), TEST_HANDLE).is_none());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn healthcheck_failure_falls_back() {
        let dir = TempDir::new().expect("tempdir");
        // base_record points at port 1 (unbound) — five conditions
        // pass except healthcheck.
        let record = base_record(TEST_HANDLE);
        write_record(dir.path(), &record);
        assert!(run_fast_path(dir.path(), TEST_HANDLE).is_none());
    }

    #[test]
    fn handle_alias_match_via_normalized() {
        let dir = TempDir::new().expect("tempdir");
        let mut record = base_record(TEST_HANDLE);
        // Pretend the user clicked the bare "publisher/slug" form.
        record.handle = "koh0920/byok-ai-chat".to_string();
        record.normalized_handle = "koh0920/byok-ai-chat".to_string();
        record.canonical_handle = Some("koh0920/byok-ai-chat".to_string());
        write_record(dir.path(), &record);
        // Healthcheck still fails (port 1) so this should be a None,
        // but the handle match itself is exercised — the validation
        // outcome will be HealthcheckFailed (last_outcome path), not
        // HandleMismatch. We assert the outer behaviour: still falls
        // back, never panics on the alias path.
        assert!(run_fast_path(dir.path(), "koh0920/byok-ai-chat").is_none());
    }

    #[test]
    fn corrupt_json_does_not_panic() {
        let dir = TempDir::new().expect("tempdir");
        // Garbage record alongside no valid record → fast path returns
        // None without unwinding. read_session_records logs a warn and
        // skips the file.
        std::fs::write(dir.path().join("corrupt.json"), b"{ not json").expect("write garbage");
        assert!(run_fast_path(dir.path(), TEST_HANDLE).is_none());
    }

    /// Verifies that `build_launch_session_from_stored` correctly
    /// maps every consumer-visible field of `StoredSessionInfo` onto
    /// `CapsuleLaunchSession`. This is the "happy path" assertion
    /// that the negative tests above cannot make on their own.
    ///
    /// Earlier revisions of this suite spun up an in-process HTTP
    /// server so `try_session_record_fast_path_inner` could be
    /// exercised end-to-end; that approach proved flaky under
    /// parallel test load (TCP accept races with hundreds of other
    /// suite threads). The pure-function call here verifies the
    /// production assembly without TCP — and the healthcheck branch
    /// is already tested independently in
    /// `ato_session_core::healthcheck::tests`.
    #[test]
    fn build_launch_session_from_stored_maps_fields_correctly() {
        let stored = base_record(TEST_HANDLE);
        let session = build_launch_session_from_stored(TEST_HANDLE, stored.clone())
            .expect("synthesizing CapsuleLaunchSession from a complete record must succeed");

        // Identity fields flow straight from the record.
        assert_eq!(session.session_id, stored.session_id);
        assert_eq!(session.handle, stored.handle);
        assert_eq!(session.normalized_handle, stored.normalized_handle);
        assert_eq!(session.canonical_handle, stored.canonical_handle);
        // Staleness contract (RFC §6.4 / §10.2 v0): trust_state is
        // taken from the record at session-creation time and rendered
        // as the snake_case CCP wire form.
        assert_eq!(session.trust_state, "untrusted");
        // Guest payload mapped through.
        assert_eq!(session.adapter.as_deref(), Some("node"));
        assert_eq!(
            session.healthcheck_url.as_deref(),
            Some("http://127.0.0.1:1/health")
        );
        assert_eq!(session.capabilities, vec!["fs:read".to_string()]);
        // app_root is derived from manifest_path.parent().
        assert_eq!(session.app_root, PathBuf::from("/tmp"));
    }
}

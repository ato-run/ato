//! Connected Runner agent — `ato runner login` / `ato runner serve`.
//!
//! Control-plane client for the Store API's runner device registry
//! (`/v1/runners`, ato-api#17). `login` authenticates the OPERATOR once via
//! the existing Store bridge device flow, registers this host as a runner
//! device, and persists ONLY the long-lived runner token; the user session
//! token is used for the single registration call and then discarded — a
//! runner host must never hold a long-lived user session. `serve` proves
//! liveness with token-authenticated heartbeats so the device shows as
//! online in the PWA's Connected Runners page.
//!
//! Out of scope here (later slices): run leases, ready_url reporting,
//! relay/tunnel.
//!
//! Secrecy invariant: the runner token is written once to the credentials
//! file (0600 on Unix) and is never printed or logged afterwards.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

use capsule_core::common::paths::ato_path_or_workspace_tmp;

const RUNNER_CREDENTIALS_RELATIVE: &str = "runner/credentials.json";
const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 30;
/// Floor so a misbehaving server value cannot turn the loop into a busy-spin.
const MIN_HEARTBEAT_INTERVAL_SECS: u64 = 5;

// ─────────────────────────────────────────────
// Credentials
// ─────────────────────────────────────────────

/// Persisted runner identity. Deliberately contains NO user session fields:
/// the only secret a runner host keeps is its own device token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerCredentials {
    pub api_base: String,
    pub runner_id: String,
    pub runner_token: String,
    pub display_name: String,
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_seconds: u64,
}

fn default_heartbeat_interval() -> u64 {
    DEFAULT_HEARTBEAT_INTERVAL_SECS
}

pub fn credentials_path() -> PathBuf {
    ato_path_or_workspace_tmp(RUNNER_CREDENTIALS_RELATIVE)
}

pub fn save_credentials(path: &std::path::Path, creds: &RunnerCredentials) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let json = serde_json::to_string_pretty(creds)?;
    std::fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set 0600 on {}", path.display()))?;
    }
    Ok(())
}

pub fn load_credentials(path: &std::path::Path) -> Result<RunnerCredentials> {
    let raw = std::fs::read_to_string(path).with_context(|| {
        format!(
            "no runner credentials at {} — run `ato runner login` first",
            path.display()
        )
    })?;
    serde_json::from_str(&raw).with_context(|| {
        format!(
            "invalid runner credentials at {} — re-run `ato runner login`",
            path.display()
        )
    })
}

// ─────────────────────────────────────────────
// Capabilities
// ─────────────────────────────────────────────

fn binary_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(name);
        candidate.is_file()
    })
}

/// Honest capability probe: only advertise what this host can actually see.
pub fn collect_capabilities() -> Vec<String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let mut caps = vec![os.to_string(), arch.to_string(), format!("{}/{}", os, arch)];
    if os == "linux" && binary_on_path("bwrap") {
        caps.push("sandbox=linux-bwrap".to_string());
    }
    if binary_on_path("python3") || binary_on_path("python") {
        caps.push("python".to_string());
    }
    caps.push("source-sandbox".to_string());
    caps
}

// ─────────────────────────────────────────────
// Wire types
// ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RegisteredRunner {
    id: String,
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct HeartbeatContract {
    #[serde(default = "default_heartbeat_interval")]
    interval_seconds: u64,
}

fn default_heartbeat_contract() -> HeartbeatContract {
    HeartbeatContract {
        interval_seconds: DEFAULT_HEARTBEAT_INTERVAL_SECS,
    }
}

#[derive(Debug, Deserialize)]
struct RegisterResponse {
    runner: RegisteredRunner,
    runner_token: String,
    #[serde(default = "default_heartbeat_contract")]
    heartbeat: HeartbeatContract,
}

#[derive(Debug, Deserialize)]
struct HeartbeatRunnerView {
    #[serde(default)]
    online: bool,
}

#[derive(Debug, Deserialize)]
struct HeartbeatResponse {
    #[serde(default)]
    next_heartbeat_seconds: Option<u64>,
    #[serde(default)]
    runner: Option<HeartbeatRunnerView>,
}

#[derive(Debug, Deserialize, Default)]
struct ApiErrorBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Heartbeat request body. `public_base_url` is serialized ONLY when
/// configured — sending null would clear the server-side value.
pub fn build_heartbeat_body(
    capabilities: &[String],
    public_base_url: Option<&str>,
    os: &str,
    arch: &str,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "capabilities": capabilities,
        "os": os,
        "arch": arch,
    });
    if let Some(url) = public_base_url {
        body["public_base_url"] = serde_json::Value::String(url.to_string());
    }
    body
}

/// One line per heartbeat for operator logs. MUST never include the runner
/// token (secrecy invariant; guarded by a test).
pub fn format_heartbeat_log(online: bool, next_seconds: u64) -> String {
    format!(
        "[{}] heartbeat ok — {}, next in {}s",
        if online { "✓" } else { "•" },
        if online { "online" } else { "recorded" },
        next_seconds
    )
}

// ─────────────────────────────────────────────
// Heartbeat outcome
// ─────────────────────────────────────────────

#[derive(Debug)]
pub enum HeartbeatOutcome {
    /// Accepted; carries the server-directed next interval (seconds).
    Ok { online: bool, next_seconds: u64 },
    /// 401 runner_revoked — terminal, fail closed.
    Revoked,
    /// 401 with any other code — token unknown/invalid. Terminal.
    InvalidToken,
    /// Transient transport/server problem; retry after backoff.
    Transient(String),
}

async fn send_heartbeat_once(
    client: &reqwest::Client,
    api_base: &str,
    runner_id: &str,
    runner_token: &str,
    body: &serde_json::Value,
) -> HeartbeatOutcome {
    let url = format!(
        "{}/v1/runners/{}/heartbeat",
        api_base.trim_end_matches('/'),
        runner_id
    );
    let response = match client
        .post(&url)
        .bearer_auth(runner_token)
        .json(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => return HeartbeatOutcome::Transient(format!("request failed: {err}")),
    };

    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        let parsed: ApiErrorBody = response.json().await.unwrap_or_default();
        return match parsed.error.as_deref() {
            Some("runner_revoked") => HeartbeatOutcome::Revoked,
            _ => HeartbeatOutcome::InvalidToken,
        };
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return HeartbeatOutcome::Transient(format!("HTTP {status}: {body}"));
    }
    let parsed: HeartbeatResponse = match response.json().await {
        Ok(parsed) => parsed,
        Err(err) => return HeartbeatOutcome::Transient(format!("invalid response: {err}")),
    };
    HeartbeatOutcome::Ok {
        online: parsed.runner.map(|r| r.online).unwrap_or(false),
        next_seconds: parsed
            .next_heartbeat_seconds
            .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_SECS)
            .max(MIN_HEARTBEAT_INTERVAL_SECS),
    }
}

// ─────────────────────────────────────────────
// Commands
// ─────────────────────────────────────────────

fn default_display_name() -> String {
    if let Ok(name) = std::env::var("HOSTNAME")
        && !name.trim().is_empty()
    {
        return name.trim().to_string();
    }
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: buf is a valid, writable buffer of the stated length.
        if unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            if let Ok(name) = std::str::from_utf8(&buf[..end])
                && !name.trim().is_empty()
            {
                return name.trim().to_string();
            }
        }
    }
    format!("runner-{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// `ato runner login`: operator device-flow sign-in → register this host →
/// persist runner token (only). The session token never leaves this function.
pub async fn run_login(
    api_base: Option<String>,
    site_base: Option<String>,
    display_name: Option<String>,
    public_base_url: Option<String>,
    headless: bool,
) -> Result<()> {
    let api_base = api_base
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(crate::application::auth::store_api_base_url);
    let site_base = site_base
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(crate::application::auth::store_site_base_url);
    let display_name = display_name.unwrap_or_else(default_display_name);

    println!("🛰  Registering this host as a Connected Runner");
    println!("   API:  {}", api_base);
    println!("   Name: {}", display_name);

    // Ephemeral operator sign-in: nothing is persisted by this call.
    let bridge =
        crate::application::auth::bridge_authenticate_ephemeral(&api_base, &site_base, headless)
            .await?;
    let session_token = bridge.access_token;

    let capabilities = collect_capabilities();
    let mut register_body = serde_json::json!({
        "display_name": display_name,
        "kind": "connected_runner",
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "capabilities": capabilities,
    });
    if let Some(url) = public_base_url.as_deref() {
        register_body["public_base_url"] = serde_json::Value::String(url.to_string());
    }

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/v1/runners", api_base))
        .bearer_auth(&session_token)
        .json(&register_body)
        .send()
        .await
        .context("failed to call POST /v1/runners")?;
    // The one-time session token is no longer needed past this point.
    drop(session_token);

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("runner registration failed (HTTP {status}): {body}");
    }
    let registered: RegisterResponse = response
        .json()
        .await
        .context("invalid /v1/runners registration response")?;

    let creds = RunnerCredentials {
        api_base: api_base.clone(),
        runner_id: registered.runner.id.clone(),
        runner_token: registered.runner_token,
        display_name: registered.runner.display_name.clone(),
        heartbeat_interval_seconds: registered
            .heartbeat
            .interval_seconds
            .max(MIN_HEARTBEAT_INTERVAL_SECS),
    };
    let path = credentials_path();
    save_credentials(&path, &creds)?;

    println!("✅ Runner registered");
    println!("   Runner ID: {}", creds.runner_id);
    println!("   Credentials: {} (0600)", path.display());
    println!("   The runner token was saved and will not be shown.");
    println!("   Start heartbeats with: ato runner serve");
    Ok(())
}

/// `ato runner serve`: heartbeat loop. Runner-token auth only; fails closed
/// on revocation.
pub async fn run_serve(
    api_base: Option<String>,
    display_name: Option<String>,
    public_base_url: Option<String>,
) -> Result<()> {
    let path = credentials_path();
    let creds = load_credentials(&path)?;
    let api_base = api_base
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or(creds.api_base.clone());

    if let Some(requested) = display_name.as_deref()
        && requested != creds.display_name
    {
        // Heartbeats cannot rename a device; say so instead of silently
        // ignoring the flag or pretending it took effect.
        eprintln!(
            "⚠️  Display name is set at registration ('{}'); '--display-name {}' has no effect. Re-run `ato runner login` to rename.",
            creds.display_name, requested
        );
    }

    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    println!("🛰  Connected Runner heartbeat");
    println!("   Runner: {} ({})", creds.display_name, creds.runner_id);
    println!("   API:    {}", api_base);
    if let Some(url) = public_base_url.as_deref() {
        println!("   Public base URL: {}", url);
    }

    let client = reqwest::Client::new();
    let mut interval = creds
        .heartbeat_interval_seconds
        .max(MIN_HEARTBEAT_INTERVAL_SECS);
    let mut consecutive_failures: u32 = 0;
    // One active run at a time (v0): while a dispatched child is alive the
    // runner does not claim further leases — GET leases/next CLAIMS, so a
    // busy runner must not even poll.
    let busy = Arc::new(AtomicBool::new(false));

    loop {
        let capabilities = collect_capabilities();
        let body = build_heartbeat_body(&capabilities, public_base_url.as_deref(), &os, &arch);
        match send_heartbeat_once(
            &client,
            &api_base,
            &creds.runner_id,
            &creds.runner_token,
            &body,
        )
        .await
        {
            HeartbeatOutcome::Ok {
                online,
                next_seconds,
            } => {
                consecutive_failures = 0;
                interval = next_seconds;
                println!("{}", format_heartbeat_log(online, next_seconds));
            }
            HeartbeatOutcome::Revoked => {
                bail!(
                    "this runner has been revoked by the account owner. Run `ato runner login` to enroll it again."
                );
            }
            HeartbeatOutcome::InvalidToken => {
                bail!(
                    "the stored runner token was rejected (unknown or invalid). Run `ato runner login` to enroll this host again."
                );
            }
            HeartbeatOutcome::Transient(reason) => {
                consecutive_failures += 1;
                let backoff = (interval * u64::from(consecutive_failures.min(4))).min(300);
                eprintln!(
                    "⚠️  heartbeat failed ({reason}); retrying in {backoff}s (attempt {consecutive_failures})"
                );
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => { println!("stopped"); return Ok(()); }
                    _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
                }
                continue;
            }
        }

        // Between heartbeats: poll for leases in short slices while idle.
        let mut remaining = interval;
        while remaining > 0 {
            let slice = remaining.min(LEASE_POLL_SECONDS);
            tokio::select! {
                _ = tokio::signal::ctrl_c() => { println!("stopped"); return Ok(()); }
                _ = tokio::time::sleep(Duration::from_secs(slice)) => {}
            }
            remaining = remaining.saturating_sub(slice);

            if busy.load(Ordering::SeqCst) {
                continue;
            }
            match fetch_next_lease(&client, &api_base, &creds.runner_id, &creds.runner_token).await
            {
                LeasePoll::None => {}
                LeasePoll::Claimed(lease) => {
                    handle_claimed_lease(
                        &client,
                        &api_base,
                        &creds.runner_token,
                        lease,
                        Arc::clone(&busy),
                    )
                    .await;
                }
                LeasePoll::Revoked => {
                    bail!(
                        "this runner has been revoked by the account owner. Run `ato runner login` to enroll it again."
                    );
                }
                LeasePoll::InvalidToken => {
                    bail!(
                        "the stored runner token was rejected (unknown or invalid). Run `ato runner login` to enroll this host again."
                    );
                }
                LeasePoll::Transient(reason) => {
                    eprintln!("⚠️  lease poll failed ({})", scrub_secrets(&reason));
                }
            }
        }
    }
}

// ─────────────────────────────────────────────
// Lease execution (PR C2)
//
// The runner claims run leases from the control plane and executes EXACTLY
// ONE supported command shape: { kind: "run_source_sandbox", source_url }.
// The API can never make this host run an arbitrary shell command — anything
// else is reported failed(unsupported_command) without executing.
//
// Status reports mirror the device's local honest-readiness chain
// (ato#608–#611): ready is reported only on the real ready signal AND must
// carry the observed execution_id (the control plane rejects it otherwise);
// a workload with no readiness signal is reported running, never ready.
// ─────────────────────────────────────────────

/** Interval between lease polls while idle (seconds). */
const LEASE_POLL_SECONDS: u64 = 5;
const DEFAULT_READY_TIMEOUT_SECS: u64 = 600;
/** Cap per-run log files so a chatty child cannot fill the disk. */
const MAX_RUN_LOG_BYTES: usize = 2 * 1024 * 1024;

pub const LEASE_COMMAND_KIND: &str = "run_source_sandbox";

#[derive(Debug, Deserialize)]
struct LeaseEnvelope {
    lease: Option<LeaseDto>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeaseDto {
    pub id: String,
    pub run_id: String,
    #[serde(default)]
    pub command: serde_json::Value,
}

#[derive(Debug)]
enum LeasePoll {
    None,
    Claimed(LeaseDto),
    Revoked,
    InvalidToken,
    Transient(String),
}

async fn fetch_next_lease(
    client: &reqwest::Client,
    api_base: &str,
    runner_id: &str,
    runner_token: &str,
) -> LeasePoll {
    let url = format!(
        "{}/v1/runners/{}/leases/next",
        api_base.trim_end_matches('/'),
        runner_id
    );
    let response = match client.get(&url).bearer_auth(runner_token).send().await {
        Ok(response) => response,
        Err(err) => return LeasePoll::Transient(format!("request failed: {err}")),
    };
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        let parsed: ApiErrorBody = response.json().await.unwrap_or_default();
        return match parsed.error.as_deref() {
            Some("runner_revoked") => LeasePoll::Revoked,
            _ => LeasePoll::InvalidToken,
        };
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return LeasePoll::Transient(format!("HTTP {status}: {body}"));
    }
    match response.json::<LeaseEnvelope>().await {
        Ok(envelope) => match envelope.lease {
            Some(lease) => LeasePoll::Claimed(lease),
            None => LeasePoll::None,
        },
        Err(err) => LeasePoll::Transient(format!("invalid response: {err}")),
    }
}

// ── Command validation ──

#[derive(Debug, Clone, PartialEq)]
pub struct ValidLeaseCommand {
    pub source_url: String,
    pub capsule_slug: Option<String>,
}

/// Validate the lease command. Only `run_source_sandbox` with an http(s)
/// `source_url` is executable; everything else is rejected WITHOUT executing.
pub fn parse_lease_command(
    command: &serde_json::Value,
) -> std::result::Result<ValidLeaseCommand, (String, String)> {
    let kind = command.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if kind != LEASE_COMMAND_KIND {
        return Err((
            "unsupported_command".to_string(),
            format!(
                "unsupported lease command kind {kind:?}; this runner only executes {LEASE_COMMAND_KIND}"
            ),
        ));
    }
    let source_url = command
        .get("source_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if source_url.is_empty() {
        return Err((
            "invalid_command".to_string(),
            "lease command is missing source_url".to_string(),
        ));
    }
    // The URL becomes an `ato run` positional argument: require an http(s)
    // URL so the API can never smuggle a flag or a host-local path.
    if !(source_url.starts_with("https://") || source_url.starts_with("http://")) {
        return Err((
            "invalid_command".to_string(),
            format!("source_url must be an http(s) URL, got {source_url:?}"),
        ));
    }
    let capsule_slug = command
        .get("capsule_slug")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    Ok(ValidLeaseCommand {
        source_url,
        capsule_slug,
    })
}

// ── Secret scrubbing ──

/// Redact runner tokens from any text that leaves this process (error
/// reports, log excerpts). The token never goes into child env, but scrub
/// defensively anyway.
pub fn scrub_secrets(text: &str) -> String {
    const PREFIX: &str = "ato_rnr_";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find(PREFIX) {
        out.push_str(&rest[..idx]);
        out.push_str("ato_rnr_[REDACTED]");
        let tail = &rest[idx + PREFIX.len()..];
        let end = tail
            .char_indices()
            .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-'))
            .map(|(i, _)| i)
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

// ── Child output signals ──

#[derive(Debug, Clone, PartialEq)]
pub enum ChildSignal {
    /// Stable machine-readable receipt pointer ("RECEIPT: <path>").
    Receipt(PathBuf),
    /// The honest ready signal (probe-confirmed; ato#608).
    Ready,
    /// Launched with NO readiness signal (StartedWithoutReadiness).
    StartedNoReadiness,
    /// The CLI announced the service exited before readiness.
    ExitedBeforeReady,
}

/// Map one line of `ato run` output to a lifecycle signal.
///
/// `RECEIPT:` is the CLI's stable machine-readable line; the readiness lines
/// are the human strings the CLI prints from its honest lifecycle events —
/// accepted as a documented, test-covered fallback until the CLI emits a
/// machine-readable lifecycle line (tracked follow-up).
pub fn parse_child_line(line: &str) -> Option<ChildSignal> {
    let trimmed = line.trim();
    if let Some(path) = trimmed.strip_prefix("RECEIPT: ") {
        let path = path.trim();
        if !path.is_empty() {
            return Some(ChildSignal::Receipt(PathBuf::from(path)));
        }
    }
    if trimmed.contains("(ready event received)") {
        return Some(ChildSignal::Ready);
    }
    if trimmed.contains("no readiness signal") {
        return Some(ChildSignal::StartedNoReadiness);
    }
    if trimmed.contains("exited before readiness")
        || trimmed.contains("exited before start confirmation")
    {
        return Some(ChildSignal::ExitedBeforeReady);
    }
    None
}

/// Read the execution_id from a receipt file the child pointed at.
pub fn execution_id_from_receipt(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("execution_id")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
}

// ── Reports ──

#[derive(Debug, Clone, PartialEq)]
pub enum LeaseReport {
    Preparing,
    Running,
    Ready { execution_id: String },
    Failed { code: String, message: String },
}

async fn report_lease_status(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    lease_id: &str,
    report: &LeaseReport,
) -> Result<()> {
    let url = format!(
        "{}/v1/runner-leases/{}/status",
        api_base.trim_end_matches('/'),
        lease_id
    );
    let body = match report {
        LeaseReport::Preparing => serde_json::json!({ "status": "preparing" }),
        LeaseReport::Running => serde_json::json!({ "status": "running" }),
        LeaseReport::Ready { execution_id } => serde_json::json!({
            "status": "ready",
            "execution_id": execution_id,
        }),
        LeaseReport::Failed { code, message } => serde_json::json!({
            "status": "failed",
            "error": { "code": code, "message": scrub_secrets(message) },
        }),
    };
    let response = client
        .post(&url)
        .bearer_auth(runner_token)
        .json(&body)
        .send()
        .await
        .context("lease status request failed")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("lease status report rejected (HTTP {status}): {body}");
    }
    Ok(())
}

// ── Child execution ──

fn run_log_path(lease_id: &str) -> PathBuf {
    let base = credentials_path();
    let dir = base
        .parent()
        .map(|parent| parent.join("runs"))
        .unwrap_or_else(|| PathBuf::from("runs"));
    dir.join(format!("{lease_id}.log"))
}

fn ready_timeout() -> Duration {
    let secs = std::env::var("ATO_RUNNER_READY_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_READY_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// `ato run` requires GitHub refs in `github.com/owner/repo` form and
/// rejects scheme-prefixed URLs. The lease payload carries the canonical
/// repository URL, so strip the scheme for the child invocation; non-GitHub
/// URLs pass through unchanged.
pub fn child_run_ref(source_url: &str) -> String {
    for prefix in ["https://github.com/", "http://github.com/"] {
        if let Some(rest) = source_url.strip_prefix(prefix) {
            return format!("github.com/{}", rest.trim_end_matches('/'));
        }
    }
    source_url.to_string()
}

fn spawn_run_child(source_url: &str) -> Result<tokio::process::Child> {
    let child_bin = match std::env::var("ATO_RUNNER_CHILD_BIN") {
        Ok(path) if !path.trim().is_empty() => PathBuf::from(path),
        _ => std::env::current_exe().context("failed to resolve the ato binary path")?,
    };
    let mut cmd = tokio::process::Command::new(child_bin);
    cmd.arg("run")
        .arg(child_run_ref(source_url))
        .arg("--sandbox")
        .arg("-y");
    // Operator-controlled extras (e.g. --nacelle <path> on dev hosts). Comes
    // from the runner host env, never from the lease payload.
    if let Ok(extra) = std::env::var("ATO_RUNNER_RUN_ARGS") {
        for arg in extra.split_whitespace() {
            cmd.arg(arg);
        }
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // The runner token lives only in this process's memory and credentials
    // file — it is never exported to the child environment.
    cmd.kill_on_drop(true);
    cmd.spawn().context("failed to spawn ato run child")
}

struct BoundedLog {
    file: Option<std::fs::File>,
    written: usize,
    truncated: bool,
}

impl BoundedLog {
    fn create(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self {
            file: std::fs::File::create(path).ok(),
            written: 0,
            truncated: false,
        }
    }

    fn line(&mut self, line: &str) {
        use std::io::Write as _;
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if self.written >= MAX_RUN_LOG_BYTES {
            if !self.truncated {
                let _ = writeln!(file, "...[log truncated at {MAX_RUN_LOG_BYTES} bytes]");
                self.truncated = true;
            }
            return;
        }
        let scrubbed = scrub_secrets(line);
        self.written += scrubbed.len() + 1;
        let _ = writeln!(file, "{scrubbed}");
    }
}

/// Drive one dispatched child to a settled outcome, emitting honest reports.
///
/// Settles on the FIRST of: honest ready signal (→ Ready, requires the
/// receipt-derived execution_id), no-readiness signal (→ Running), child
/// exit (→ Failed), or the ready timeout (→ Failed + kill). After Ready or
/// Running the child keeps serving and nothing is retroactively cleared;
/// the function returns when the child exits.
async fn run_lease_child(
    mut child: tokio::process::Child,
    log_path: PathBuf,
    timeout: Duration,
    reports: tokio::sync::mpsc::UnboundedSender<LeaseReport>,
) {
    let mut log = BoundedLog::create(&log_path);
    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    drop(line_tx);

    let mut execution_id: Option<String> = None;
    let mut settled = false;
    let mut saw_exited_before = false;
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        tokio::select! {
            line = line_rx.recv() => {
                let Some(line) = line else {
                    // Output streams closed; wait for the exit status.
                    let status = child.wait().await.ok();
                    if !settled {
                        let code = status.and_then(|s| s.code());
                        let message = match (code, saw_exited_before) {
                            (Some(code), true) => format!(
                                "service exited before readiness (exit code {code})"
                            ),
                            (Some(code), false) => format!("exit code {code}"),
                            (None, _) => "terminated by signal".to_string(),
                        };
                        let _ = reports.send(LeaseReport::Failed {
                            code: "exit_before_ready".to_string(),
                            message,
                        });
                    }
                    return;
                };
                log.line(&line);
                match parse_child_line(&line) {
                    Some(ChildSignal::Receipt(path)) => {
                        if execution_id.is_none() {
                            execution_id = execution_id_from_receipt(&path);
                        }
                    }
                    Some(ChildSignal::Ready) if !settled => {
                        match execution_id.clone() {
                            Some(execution_id) => {
                                settled = true;
                                let _ = reports.send(LeaseReport::Ready { execution_id });
                            }
                            None => {
                                // A ready we cannot tie to an execution receipt is
                                // unverifiable — fail closed rather than report it.
                                settled = true;
                                let _ = reports.send(LeaseReport::Failed {
                                    code: "execution_id_unavailable".to_string(),
                                    message: "child reported ready but no execution receipt was observed".to_string(),
                                });
                                let _ = child.start_kill();
                            }
                        }
                    }
                    Some(ChildSignal::StartedNoReadiness) if !settled => {
                        settled = true;
                        let _ = reports.send(LeaseReport::Running);
                    }
                    Some(ChildSignal::ExitedBeforeReady) => {
                        saw_exited_before = true;
                    }
                    _ => {}
                }
            }
            _ = tokio::time::sleep_until(deadline), if !settled => {
                settled = true;
                let _ = reports.send(LeaseReport::Failed {
                    code: "readiness_timeout".to_string(),
                    message: format!(
                        "no readiness signal within {}s",
                        timeout.as_secs()
                    ),
                });
                let _ = child.start_kill();
            }
        }
    }
}

async fn handle_claimed_lease(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    lease: LeaseDto,
    busy: Arc<AtomicBool>,
) {
    println!("📦 lease {} claimed (run {})", lease.id, lease.run_id);
    let command = match parse_lease_command(&lease.command) {
        Ok(command) => command,
        Err((code, message)) => {
            eprintln!("⚠️  lease {} rejected: {}", lease.id, message);
            let report = LeaseReport::Failed { code, message };
            if let Err(err) =
                report_lease_status(client, api_base, runner_token, &lease.id, &report).await
            {
                eprintln!(
                    "⚠️  failed to report lease failure: {}",
                    scrub_secrets(&format!("{err:#}"))
                );
            }
            return;
        }
    };

    if let Err(err) = report_lease_status(
        client,
        api_base,
        runner_token,
        &lease.id,
        &LeaseReport::Preparing,
    )
    .await
    {
        eprintln!(
            "⚠️  failed to report preparing: {}",
            scrub_secrets(&format!("{err:#}"))
        );
    }

    let child = match spawn_run_child(&command.source_url) {
        Ok(child) => child,
        Err(err) => {
            let report = LeaseReport::Failed {
                code: "spawn_failed".to_string(),
                message: format!("{err:#}"),
            };
            let _ = report_lease_status(client, api_base, runner_token, &lease.id, &report).await;
            return;
        }
    };

    let log_path = run_log_path(&lease.id);
    println!(
        "🚀 lease {}: ato run {} --sandbox (log: {})",
        lease.id,
        command.source_url,
        log_path.display()
    );

    busy.store(true, Ordering::SeqCst);
    let client = client.clone();
    let api_base = api_base.to_string();
    let runner_token = runner_token.to_string();
    let lease_id = lease.id.clone();
    tokio::spawn(async move {
        let (report_tx, mut report_rx) = tokio::sync::mpsc::unbounded_channel::<LeaseReport>();
        let monitor = tokio::spawn(run_lease_child(child, log_path, ready_timeout(), report_tx));
        while let Some(report) = report_rx.recv().await {
            let label = match &report {
                LeaseReport::Preparing => "preparing".to_string(),
                LeaseReport::Running => "running (launched, readiness not confirmed)".to_string(),
                LeaseReport::Ready { execution_id } => format!("ready ({execution_id})"),
                LeaseReport::Failed { code, .. } => format!("failed ({code})"),
            };
            println!("📨 lease {lease_id}: {label}");
            if let Err(err) =
                report_lease_status(&client, &api_base, &runner_token, &lease_id, &report).await
            {
                eprintln!(
                    "⚠️  lease {lease_id}: report failed: {}",
                    scrub_secrets(&format!("{err:#}"))
                );
            }
        }
        let _ = monitor.await;
        busy.store(false, Ordering::SeqCst);
        println!("📦 lease {lease_id}: child exited; runner is idle again");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn credentials_roundtrip_sets_0600_and_holds_no_session_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("runner").join("credentials.json");
        let creds = RunnerCredentials {
            api_base: "https://staging.example".to_string(),
            runner_id: "01TEST".to_string(),
            runner_token: "ato_rnr_secret-token-value".to_string(),
            display_name: "OCI A1".to_string(),
            heartbeat_interval_seconds: 30,
        };
        save_credentials(&path, &creds).expect("save");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "credentials file must be 0600");
        }

        let raw = std::fs::read_to_string(&path).expect("read");
        // The runner host must never persist a user session.
        assert!(
            !raw.contains("session"),
            "credentials file must not contain session fields: {raw}"
        );

        let loaded = load_credentials(&path).expect("load");
        assert_eq!(loaded.runner_id, "01TEST");
        assert_eq!(loaded.runner_token, creds.runner_token);
        assert_eq!(loaded.heartbeat_interval_seconds, 30);
    }

    #[test]
    fn load_credentials_missing_points_to_login() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = load_credentials(&dir.path().join("none.json")).unwrap_err();
        assert!(
            format!("{err:#}").contains("ato runner login"),
            "missing credentials must point at `ato runner login`, got: {err:#}"
        );
    }

    #[test]
    fn capabilities_include_os_arch_and_source_sandbox() {
        let caps = collect_capabilities();
        assert!(caps.contains(&std::env::consts::OS.to_string()));
        assert!(caps.contains(&std::env::consts::ARCH.to_string()));
        assert!(caps.contains(&format!(
            "{}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )));
        assert!(caps.contains(&"source-sandbox".to_string()));
    }

    #[test]
    fn heartbeat_body_includes_public_base_url_only_when_configured() {
        let caps = vec!["linux".to_string()];
        let without = build_heartbeat_body(&caps, None, "linux", "aarch64");
        assert!(
            without.get("public_base_url").is_none(),
            "absent public_base_url must not be sent (null would clear it server-side)"
        );
        let with = build_heartbeat_body(
            &caps,
            Some("https://oci-a1.example.com"),
            "linux",
            "aarch64",
        );
        assert_eq!(
            with["public_base_url"].as_str(),
            Some("https://oci-a1.example.com")
        );
    }

    #[test]
    fn heartbeat_log_lines_never_contain_a_token() {
        for line in [
            format_heartbeat_log(true, 30),
            format_heartbeat_log(false, 30),
        ] {
            assert!(
                !line.contains("ato_rnr_"),
                "heartbeat log must never include the runner token: {line}"
            );
        }
    }

    // ── Lease command validation ──

    #[test]
    fn lease_command_accepts_only_run_source_sandbox() {
        let ok = parse_lease_command(&serde_json::json!({
            "kind": "run_source_sandbox",
            "source_url": "https://github.com/Koh0920/hello-capsule",
            "capsule_slug": "hello-capsule",
        }))
        .expect("valid command");
        assert_eq!(ok.source_url, "https://github.com/Koh0920/hello-capsule");
        assert_eq!(ok.capsule_slug.as_deref(), Some("hello-capsule"));

        let (code, _) = parse_lease_command(&serde_json::json!({
            "kind": "shell",
            "command": "rm -rf /",
        }))
        .unwrap_err();
        assert_eq!(code, "unsupported_command");

        let (code, _) =
            parse_lease_command(&serde_json::json!({ "kind": "run_source_sandbox" })).unwrap_err();
        assert_eq!(code, "invalid_command");

        // A non-URL positional could be smuggled as a flag or local path.
        for bad in [
            "--help",
            "-rf",
            "/etc/passwd",
            "file:///x",
            "git@github.com:x/y",
        ] {
            let (code, _) = parse_lease_command(&serde_json::json!({
                "kind": "run_source_sandbox",
                "source_url": bad,
            }))
            .unwrap_err();
            assert_eq!(code, "invalid_command", "must reject {bad}");
        }
    }

    #[test]
    fn child_run_ref_strips_github_scheme_only() {
        assert_eq!(
            child_run_ref("https://github.com/Koh0920/hello-capsule"),
            "github.com/Koh0920/hello-capsule"
        );
        assert_eq!(
            child_run_ref("https://github.com/Koh0920/hello-capsule/"),
            "github.com/Koh0920/hello-capsule"
        );
        assert_eq!(
            child_run_ref("https://gitlab.com/x/y"),
            "https://gitlab.com/x/y",
            "non-GitHub URLs pass through unchanged"
        );
    }

    // ── Child output signal parsing (documented fallback, test-covered) ──

    #[test]
    fn child_lines_map_to_lifecycle_signals() {
        assert_eq!(
            parse_child_line("RECEIPT: /home/u/.ato/executions/x/receipt.json"),
            Some(ChildSignal::Receipt(PathBuf::from(
                "/home/u/.ato/executions/x/receipt.json"
            )))
        );
        assert_eq!(
            parse_child_line("[✓] Service 'hello-capsule' is ready (ready event received)"),
            Some(ChildSignal::Ready)
        );
        assert_eq!(
            parse_child_line("[✓] Command started (ready event received)"),
            Some(ChildSignal::Ready)
        );
        assert_eq!(
            parse_child_line("[•] Service launched — no readiness signal, not confirmed ready"),
            Some(ChildSignal::StartedNoReadiness)
        );
        assert_eq!(
            parse_child_line("❌ Service 'x' exited before readiness (exit code: 7)"),
            Some(ChildSignal::ExitedBeforeReady)
        );
        assert_eq!(parse_child_line("Streaming logs..."), None);
        // Heartbeat-style noise must never read as run readiness.
        assert_eq!(
            parse_child_line("[✓] heartbeat ok — online, next in 30s"),
            None
        );
    }

    #[test]
    fn execution_id_read_from_receipt_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("receipt.json");
        std::fs::write(
            &path,
            r#"{"schema_version":2,"execution_id":"blake3:abc123","graph_receipt":{"gate":"launch-passed"}}"#,
        )
        .expect("write receipt");
        assert_eq!(
            execution_id_from_receipt(&path).as_deref(),
            Some("blake3:abc123")
        );
        assert_eq!(
            execution_id_from_receipt(&dir.path().join("none.json")),
            None
        );
    }

    #[test]
    fn scrub_redacts_runner_tokens() {
        let scrubbed = scrub_secrets("auth failed for Bearer ato_rnr_AbC-123_xyz while polling");
        assert!(!scrubbed.contains("AbC-123_xyz"));
        assert!(scrubbed.contains("ato_rnr_[REDACTED]"));
        assert_eq!(scrub_secrets("no secrets here"), "no secrets here");
    }

    // ── Fake-child execution flows (no API, no network) ──

    #[cfg(unix)]
    fn fake_child(script: &str) -> tokio::process::Child {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        cmd.spawn().expect("spawn fake child")
    }

    #[cfg(unix)]
    async fn collect_reports(
        script: &str,
        timeout: Duration,
    ) -> (Vec<LeaseReport>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("run.log");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        run_lease_child(fake_child(script), log_path, timeout, tx).await;
        let mut reports = Vec::new();
        while let Ok(report) = rx.try_recv() {
            reports.push(report);
        }
        (reports, dir)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ready_flow_reports_ready_with_execution_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let receipt = dir.path().join("receipt.json");
        std::fs::write(&receipt, r#"{"execution_id":"blake3:ready-e2e"}"#).unwrap();
        let script = format!(
            "echo 'RECEIPT: {}'; echo \"[✓] Service 'x' is ready (ready event received)\"; exit 0",
            receipt.display()
        );
        let (reports, _logdir) = collect_reports(&script, Duration::from_secs(20)).await;
        assert_eq!(
            reports,
            vec![LeaseReport::Ready {
                execution_id: "blake3:ready-e2e".to_string()
            }],
            "ready must be reported exactly once, with the receipt's execution_id, and the post-ready exit must not retroactively change it"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ready_without_receipt_fails_closed() {
        let script = "echo \"[✓] Service 'x' is ready (ready event received)\"; exec sleep 30";
        let (reports, _logdir) = collect_reports(script, Duration::from_secs(20)).await;
        assert_eq!(reports.len(), 1);
        match &reports[0] {
            LeaseReport::Failed { code, .. } => {
                assert_eq!(code, "execution_id_unavailable");
            }
            other => panic!("unverifiable ready must fail closed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn no_readiness_signal_reports_running_never_ready() {
        let script =
            "echo '[•] Service launched — no readiness signal, not confirmed ready'; exit 0";
        let (reports, _logdir) = collect_reports(script, Duration::from_secs(20)).await;
        assert_eq!(
            reports,
            vec![LeaseReport::Running],
            "StartedWithoutReadiness maps to running — never ready, and the post-settle exit adds nothing"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn crash_before_ready_reports_failed_with_exit_code() {
        let script = "echo 'building...'; exit 7";
        let (reports, _logdir) = collect_reports(script, Duration::from_secs(20)).await;
        assert_eq!(reports.len(), 1);
        match &reports[0] {
            LeaseReport::Failed { code, message } => {
                assert_eq!(code, "exit_before_ready");
                assert!(
                    message.contains('7'),
                    "message must carry the exit code: {message}"
                );
            }
            other => panic!("crash must report failed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn silent_child_hits_readiness_timeout() {
        let script = "exec sleep 30";
        let (reports, _logdir) = collect_reports(script, Duration::from_secs(1)).await;
        assert_eq!(reports.len(), 1);
        match &reports[0] {
            LeaseReport::Failed { code, .. } => assert_eq!(code, "readiness_timeout"),
            other => panic!("silent child must time out, got {other:?}"),
        }
    }

    // ── Lease poll wire handling ──

    #[tokio::test]
    async fn lease_poll_parses_claimed_lease() {
        let (base, server) = one_shot_http(
            "HTTP/1.1 200 OK",
            "{\"lease\":{\"id\":\"01LEASE\",\"run_id\":\"01RUN\",\"command\":{\"kind\":\"run_source_sandbox\",\"source_url\":\"https://github.com/x/y\"}},\"next_poll_seconds\":5}",
        );
        let client = reqwest::Client::new();
        let outcome = fetch_next_lease(&client, &base, "01R", "ato_rnr_t").await;
        let request = server.join().expect("server");
        assert!(request.contains("GET /v1/runners/01R/leases/next"));
        match outcome {
            LeasePoll::Claimed(lease) => {
                assert_eq!(lease.id, "01LEASE");
                assert_eq!(lease.run_id, "01RUN");
                let parsed = parse_lease_command(&lease.command).expect("valid");
                assert_eq!(parsed.source_url, "https://github.com/x/y");
            }
            other => panic!("expected claimed lease, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lease_poll_revoked_is_terminal() {
        let (base, server) = one_shot_http(
            "HTTP/1.1 401 Unauthorized",
            "{\"error\":\"runner_revoked\",\"message\":\"revoked\"}",
        );
        let client = reqwest::Client::new();
        let outcome = fetch_next_lease(&client, &base, "01R", "ato_rnr_t").await;
        let _ = server.join();
        assert!(matches!(outcome, LeasePoll::Revoked));
    }

    /// Minimal one-shot HTTP server: accepts a single connection, captures the
    /// request head+body, answers with the given status line and JSON body
    /// (content-length computed, so the canned response is always well-formed).
    fn one_shot_http(
        status_line: &'static str,
        json_body: &'static str,
    ) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let response = format!(
                "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json_body}",
                json_body.len()
            );
            stream.write_all(response.as_bytes()).expect("write");
            request
        });
        (format!("http://{}", addr), handle)
    }

    #[tokio::test]
    async fn serve_sends_runner_token_bearer_heartbeat() {
        let (base, server) = one_shot_http(
            "HTTP/1.1 200 OK",
            "{\"ok\":true,\"next_heartbeat_seconds\":30,\"runner\":{\"online\":true}}",
        );
        let client = reqwest::Client::new();
        let body = build_heartbeat_body(
            &["linux".to_string()],
            Some("https://oci-a1.example.com"),
            "linux",
            "aarch64",
        );
        let outcome =
            send_heartbeat_once(&client, &base, "01TEST", "ato_rnr_test-token", &body).await;
        let request = server.join().expect("server thread");

        assert!(
            request.contains("POST /v1/runners/01TEST/heartbeat"),
            "wrong request line: {request}"
        );
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer ato_rnr_test-token"),
            "heartbeat must use the runner token bearer: {request}"
        );
        assert!(request.contains("public_base_url"));
        match outcome {
            HeartbeatOutcome::Ok {
                online,
                next_seconds,
            } => {
                assert!(online);
                assert_eq!(next_seconds, 30);
            }
            other => panic!("expected Ok outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn revoked_heartbeat_fails_closed_with_actionable_outcome() {
        let (base, server) = one_shot_http(
            "HTTP/1.1 401 Unauthorized",
            "{\"error\":\"runner_revoked\",\"message\":\"This runner was revoked\"}",
        );
        let client = reqwest::Client::new();
        let body = build_heartbeat_body(&[], None, "linux", "aarch64");
        let outcome =
            send_heartbeat_once(&client, &base, "01TEST", "ato_rnr_test-token", &body).await;
        let _ = server.join();
        assert!(
            matches!(outcome, HeartbeatOutcome::Revoked),
            "revoked must be terminal, got {outcome:?}"
        );
    }
}

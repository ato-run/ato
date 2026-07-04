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

use capsule::common::paths::ato_path_or_workspace_tmp;

const RUNNER_CREDENTIALS_RELATIVE: &str = "runner/credentials.json";
const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 30;
/// Floor so a misbehaving server value cannot turn the loop into a busy-spin.
const MIN_HEARTBEAT_INTERVAL_SECS: u64 = 5;
/// Ceiling so a pathological server value cannot park the runner for hours or
/// overflow the failure-backoff multiplication.
const MAX_HEARTBEAT_INTERVAL_SECS: u64 = 3600;
/// Cap on the failure backoff regardless of the negotiated interval.
const MAX_HEARTBEAT_BACKOFF_SECS: u64 = 300;

/// Clamp a server-controlled heartbeat interval to a sane range. Every ingest
/// of an interval (registration, persisted credentials, heartbeat response)
/// must pass through here.
fn clamp_heartbeat_interval(seconds: u64) -> u64 {
    seconds.clamp(MIN_HEARTBEAT_INTERVAL_SECS, MAX_HEARTBEAT_INTERVAL_SECS)
}

/// Backoff after `consecutive_failures` failed heartbeats. Saturating so an
/// out-of-range interval can never overflow (wrap would defeat the backoff).
fn heartbeat_backoff_secs(interval: u64, consecutive_failures: u32) -> u64 {
    interval
        .saturating_mul(u64::from(consecutive_failures.min(4)))
        .min(MAX_HEARTBEAT_BACKOFF_SECS)
}

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

/// Env var names the systemd EnvironmentFile (`/etc/ato/runner.env`, written by
/// `ato runner enroll`) can supply. Used only as a fallback when there is no
/// `credentials.json` — e.g. the operator ran `ato runner enroll` as themselves but
/// `ato-runner-agent.service` runs as root, so the two do not share a home.
pub const ENV_RUNNER_API_URL: &str = "ATO_API_URL";
pub const ENV_RUNNER_TOKEN: &str = "ATO_RUNNER_TOKEN";
pub const ENV_RUNNER_ID: &str = "ATO_RUNNER_ID";
pub const ENV_RUNNER_DISPLAY_NAME: &str = "ATO_RUNNER_DISPLAY_NAME";

/// Resolve runner credentials from `credentials.json` (authoritative), else
/// reconstruct them from the environment (systemd EnvironmentFile). Fail-closed with
/// the same guidance when neither is available.
pub fn load_runner_credentials() -> Result<RunnerCredentials> {
    let path = credentials_path();
    if path.exists() {
        return load_credentials(&path);
    }
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    if let (Some(api_base), Some(token), Some(id)) =
        (env(ENV_RUNNER_API_URL), env(ENV_RUNNER_TOKEN), env(ENV_RUNNER_ID))
    {
        return Ok(RunnerCredentials {
            api_base: api_base.trim_end_matches('/').to_string(),
            runner_id: id,
            runner_token: token,
            display_name: env(ENV_RUNNER_DISPLAY_NAME).unwrap_or_else(default_display_name),
            heartbeat_interval_seconds: default_heartbeat_interval(),
        });
    }
    // Neither source available — surface the credentials.json guidance.
    load_credentials(&path)
}

// ─────────────────────────────────────────────
// Capabilities
// ─────────────────────────────────────────────

pub(crate) fn binary_on_path(name: &str) -> bool {
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

/// Server-pushed self-update directive (present while this runner is below the
/// operator-configured minimum). `minimum_version` is the floor — the runner
/// self-updates to the LATEST release, which must satisfy it.
#[derive(Debug, Deserialize)]
struct UpdateDirective {
    minimum_version: String,
}

#[derive(Debug, Deserialize)]
struct HeartbeatResponse {
    #[serde(default)]
    next_heartbeat_seconds: Option<u64>,
    #[serde(default)]
    runner: Option<HeartbeatRunnerView>,
    #[serde(default)]
    update: Option<UpdateDirective>,
}

/// The slice of an API error body the agent acts on. The machine `error` code
/// drives behavior (revoked vs invalid token); the human-facing `message` is
/// surfaced only in operator-facing failures (e.g. enrollment) and never drives
/// control flow.
#[derive(Debug, Deserialize, Default)]
struct ApiErrorBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// This runner's `ato` agent version, reported to the control plane so the
/// operator can see who is behind and target a self-update.
pub fn agent_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Heartbeat request body. `public_base_url` is serialized ONLY when
/// configured — sending null would clear the server-side value.
pub fn build_heartbeat_body(
    capabilities: &[String],
    public_base_url: Option<&str>,
    os: &str,
    arch: &str,
    max_slots: usize,
    active_slots: usize,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "capabilities": capabilities,
        // Lease command kinds this runner can actually EXECUTE today. The control
        // plane gates dispatch on this so a runner is never sent a kind it would
        // reject on-device (e.g. `run_capsule` before that execution path ships).
        "supported_lease_kinds": advertised_lease_kinds(),
        "os": os,
        "arch": arch,
        "agent_version": agent_version(),
        // Advertise concurrency so the control plane can stop dispatching to a
        // full device instead of relying on the runner to silently decline
        // (server-side capacity is the #3 follow-up; these fields are additive).
        "max_slots": max_slots,
        "active_slots": active_slots,
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
    /// Accepted; carries the server-directed next interval (seconds) and an
    /// optional self-update minimum the operator requested for this runner.
    Ok {
        online: bool,
        next_seconds: u64,
        update_min: Option<String>,
    },
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
        next_seconds: clamp_heartbeat_interval(
            parsed
                .next_heartbeat_seconds
                .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_SECS),
        ),
        update_min: parsed.update.map(|u| u.minimum_version),
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
    enrollment_token: Option<String>,
) -> Result<()> {
    let api_base = api_base
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(crate::application::auth::store_api_base_url);

    // Headless hosted-runner enrollment (#699): a Managed Cloud VM exchanges a
    // single-use enrollment token for a runner token — no operator device flow.
    // Precedence is explicit: an `--enrollment-token` flag wins; otherwise fall
    // back to ATO_RUNNER_ENROLLMENT_TOKEN (convenient for cloud-init). The token
    // VALUE is never logged. With neither set, the normal device flow runs.
    let enrollment_token = enrollment_token.or_else(enrollment_token_from_env);
    if let Some(token) = enrollment_token {
        return run_login_with_enrollment_token(api_base, display_name, public_base_url, token)
            .await;
    }

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
        "agent_version": agent_version(),
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
        heartbeat_interval_seconds: clamp_heartbeat_interval(registered.heartbeat.interval_seconds),
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

/// Cooldown before retrying a self-update that failed transiently (network /
/// release fetch). A no-receipt failure is terminal and never retried; an
/// already-latest-but-below-minimum case waits a long cooldown (the operator's
/// minimum is likely newer than any release yet).
const SELF_UPDATE_TRANSIENT_COOLDOWN: Duration = Duration::from_secs(5 * 60);
const SELF_UPDATE_UNSATISFIABLE_COOLDOWN: Duration = Duration::from_secs(60 * 60);

/// What to do after an idle self-update attempt for the current minimum.
enum SelfUpdateNext {
    /// The binary was replaced — re-exec into it.
    ReExec,
    /// Stop trying this minimum (no install receipt — can't self-update here).
    GiveUp,
    /// Retry after this cooldown (transient failure, or latest still < minimum).
    RetryAfter(Duration),
}

/// Per-minimum self-update bookkeeping so a directive that cannot be satisfied
/// is not retried on every heartbeat, while a transient failure still retries.
struct UpdateAttempt {
    min: String,
    terminal: bool,
    retry_after: Option<std::time::Instant>,
}

/// Run a self-update for the requested minimum (updates to LATEST). Returns the
/// caller's next action. Best-effort: never panics; failures keep the runner on
/// its current version.
async fn maybe_self_update(min: &str) -> SelfUpdateNext {
    use crate::cli::commands::update::SelfUpdateOutcome;
    println!(
        "⬆️  update requested (minimum {min}); current {} → updating to latest…",
        agent_version()
    );
    match crate::cli::commands::update::run_self_update_async().await {
        Ok(SelfUpdateOutcome::Updated(new_version)) => {
            println!("✅ updated to v{new_version}; restarting runner…");
            SelfUpdateNext::ReExec
        }
        Ok(SelfUpdateOutcome::AlreadyLatest) => {
            eprintln!(
                "ℹ️  already on the latest release but below the requested minimum {min}; will recheck later"
            );
            SelfUpdateNext::RetryAfter(SELF_UPDATE_UNSATISFIABLE_COOLDOWN)
        }
        Ok(SelfUpdateOutcome::NoReceipt) => {
            eprintln!(
                "⚠️  no install receipt; this runner cannot self-update — update it manually"
            );
            SelfUpdateNext::GiveUp
        }
        Err(err) => {
            eprintln!(
                "⚠️  self-update failed (will retry): {}",
                scrub_secrets(&format!("{err:#}"))
            );
            SelfUpdateNext::RetryAfter(SELF_UPDATE_TRANSIENT_COOLDOWN)
        }
    }
}

/// Replace this process with a fresh `ato runner serve` (the just-updated
/// binary), preserving the exact original argv. The runner credentials persist
/// on disk, so the new process re-authenticates and resumes; startup reconcile
/// settles any leases orphaned by the swap. Never returns on success.
fn reexec_serve() -> ! {
    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("⚠️  cannot resolve current exe for re-exec: {err}");
            std::process::exit(1);
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&exe).args(&args).exec();
        // exec() only returns on failure.
        eprintln!("⚠️  re-exec after self-update failed: {err}");
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(&exe).args(&args).spawn() {
            Ok(_) => std::process::exit(0),
            Err(err) => {
                eprintln!("⚠️  re-exec after self-update failed: {err}");
                std::process::exit(1);
            }
        }
    }
}

/// Build the `POST /v1/runners/enroll` body. Pure (unit-tested). The enrollment
/// token is the body's only credential — there is NO bearer auth — and it is
/// never logged. os/arch/capabilities are the honest host probe.
fn build_enroll_body(
    enrollment_token: &str,
    display_name: &str,
    capabilities: &[String],
) -> serde_json::Value {
    serde_json::json!({
        "enrollment_token": enrollment_token,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "capabilities": capabilities,
        "display_name": display_name,
    })
}

/// Human-readable failure for a non-2xx enroll response. NEVER echoes the raw
/// response body — only the server's typed `{ error, message }` — so a single-
/// use enrollment token can never leak into logs through an error path.
fn enroll_failure_message(status_code: u16, body: &str) -> String {
    let parsed: ApiErrorBody = serde_json::from_str(body).unwrap_or_default();
    let detail = parsed
        .message
        .or(parsed.error)
        .unwrap_or_else(|| format!("HTTP {status_code}"));
    format!("runner enrollment failed (HTTP {status_code}): {detail}")
}

/// Resolve an enrollment token from `ATO_RUNNER_ENROLLMENT_TOKEN` when the
/// `--enrollment-token` flag was not passed (cloud-init convenience). Logs that
/// the env var is being used — NEVER its value. Empty/whitespace is treated as
/// absent.
fn enrollment_token_from_env() -> Option<String> {
    match std::env::var("ATO_RUNNER_ENROLLMENT_TOKEN") {
        Ok(token) if !token.trim().is_empty() => {
            eprintln!(
                "Using enrollment token from ATO_RUNNER_ENROLLMENT_TOKEN environment variable (value hidden)."
            );
            Some(token)
        }
        _ => None,
    }
}

/// Exchange a single-use enrollment token for the runner credential via
/// `POST /v1/runners/enroll`. The returned `RunnerCredentials` are the SAME
/// shape, store fields, and identifiers device-flow `run_login` produces — only
/// the acquisition path differs, so `serve`/`logout`/`status` all read it
/// unchanged. Does NOT persist (the caller saves), which keeps it unit-testable
/// against a mock server without touching the on-disk credential store. The
/// enrollment token is never logged; HTTP failures surface only the server's
/// typed `{ error, message }`, never the raw body.
async fn enroll_for_credentials(
    api_base: &str,
    display_name: &str,
    enrollment_token: String,
) -> Result<RunnerCredentials> {
    let capabilities = collect_capabilities();
    let body = build_enroll_body(&enrollment_token, display_name, &capabilities);

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/v1/runners/enroll", api_base))
        .json(&body)
        .send()
        .await
        .context("failed to call POST /v1/runners/enroll")?;
    // The single-use token is spent server-side now; drop it promptly.
    drop(enrollment_token);

    let status = response.status();
    if !status.is_success() {
        let raw = response.text().await.unwrap_or_default();
        bail!("{}", enroll_failure_message(status.as_u16(), &raw));
    }
    let registered: RegisterResponse = response
        .json()
        .await
        .context("invalid /v1/runners/enroll response")?;

    Ok(RunnerCredentials {
        api_base: api_base.to_string(),
        runner_id: registered.runner.id.clone(),
        runner_token: registered.runner_token,
        display_name: registered.runner.display_name.clone(),
        heartbeat_interval_seconds: registered
            .heartbeat
            .interval_seconds
            .max(MIN_HEARTBEAT_INTERVAL_SECS),
    })
}

/// `ato runner login --enrollment-token <TOKEN>`: headless hosted-runner
/// enrollment. Exchanges a single-use `ato_enr_…` token for a runner token via
/// `POST /v1/runners/enroll`, then persists credentials EXACTLY like device-flow
/// login. No browser, no operator session — used by a Managed Cloud VM whose
/// cloud-init injected the token. The runner knows nothing about the provider
/// (Fly/Hetzner/…): it sends the token, stores the returned `ato_rnr_` token,
/// and `ato runner serve` proceeds on the existing heartbeat/poll/claim loop
/// unchanged. The enrollment token is never printed and never written to disk.
async fn run_login_with_enrollment_token(
    api_base: String,
    display_name: Option<String>,
    public_base_url: Option<String>,
    enrollment_token: String,
) -> Result<()> {
    let display_name = display_name.unwrap_or_else(default_display_name);
    if public_base_url.is_some() {
        // A hosted runner's public origin is assigned by the control plane at
        // enrollment, not chosen by the runner. Say so rather than silently
        // dropping the flag.
        eprintln!(
            "⚠️  --public-base-url is ignored with --enrollment-token: a hosted runner's public URL is assigned by the control plane."
        );
    }

    println!("🛰  Enrolling this host as a hosted runner");
    println!("   API:  {}", api_base);
    println!("   Name: {}", display_name);

    let creds = enroll_for_credentials(&api_base, &display_name, enrollment_token).await?;
    let path = credentials_path();
    save_credentials(&path, &creds)?;

    println!("✅ Hosted runner enrolled");
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
    proxy_listen: Option<String>,
    max_slots: Option<usize>,
    public_url_template: Option<String>,
) -> Result<()> {
    let proxy_listen = proxy_listen.unwrap_or_else(|| DEFAULT_PROXY_LISTEN.to_string());
    let (proxy_host, proxy_base_port) = parse_proxy_listen(&proxy_listen)?;
    // Slot count: flag > env > default. Default 1 keeps single-slot behavior;
    // operators opt into concurrency explicitly.
    let capacity = clamp_max_slots(
        max_slots
            .or_else(|| {
                std::env::var("ATO_RUNNER_MAX_SLOTS")
                    .ok()
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or(DEFAULT_MAX_SLOTS),
    );
    let public_url_template = public_url_template
        .or_else(|| std::env::var("ATO_RUNNER_PUBLIC_URL_TEMPLATE").ok())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    // The systemd unit runs `ato runner serve` with no flags, reading config from
    // EnvironmentFile=/etc/ato/runner.env — so honor ATO_RUNNER_PUBLIC_BASE_URL (written
    // by `ato runner enroll`) as the public base URL when the flag is absent. Without
    // this the service would advertise no URL and never be dispatchable.
    let public_base_url = public_base_url
        .or_else(|| std::env::var("ATO_RUNNER_PUBLIC_BASE_URL").ok())
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty());
    // Fail fast at startup on configurations that would violate the
    // no-port-collision / no-fabricated-URL invariants, rather than discovering
    // them per-slot at ready time.
    validate_slot_port_range(proxy_base_port, capacity)?;
    validate_public_url_template(public_url_template.as_deref())?;
    let creds = load_runner_credentials()?;
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

    // Settle leases orphaned by a previous runner process BEFORE the first
    // heartbeat/claim, so stop-requested zombies clear immediately on boot.
    reconcile_open_leases(&client, &api_base, &creds.runner_id, &creds.runner_token).await;

    let mut interval = clamp_heartbeat_interval(creds.heartbeat_interval_seconds);
    let mut consecutive_failures: u32 = 0;
    // Self-update bookkeeping for the current requested minimum: terminal (no
    // receipt) is never retried; a transient failure or unsatisfiable minimum
    // retries only after a cooldown. Reset when the requested minimum changes.
    let mut update_attempt: Option<UpdateAttempt> = None;
    // N-slot executor (#632): the runner claims leases while a slot is free.
    // GET leases/next CLAIMS, so a full runner must not poll — the pool gates
    // the poll instead of a single `busy` boolean. Each slot owns its own proxy
    // port so concurrent workloads never collide.
    let pool = SlotPool::new(capacity, proxy_host, proxy_base_port);
    println!(
        "   Slots:  {} concurrent run(s); per-slot proxy from {}",
        pool.capacity(),
        proxy_listen
    );
    if capacity == DEFAULT_MAX_SLOTS {
        println!(
            "           (default; override with --max-slots or ATO_RUNNER_MAX_SLOTS, max {MAX_SLOTS_CEILING})"
        );
    }
    if let Some(template) = public_url_template.as_deref() {
        println!("   Public URL template: {template}");
    }

    loop {
        let capabilities = collect_capabilities();
        let body = build_heartbeat_body(
            &capabilities,
            public_base_url.as_deref(),
            &os,
            &arch,
            pool.capacity(),
            pool.active(),
        );
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
                update_min,
            } => {
                consecutive_failures = 0;
                interval = next_seconds;
                println!("{}", format_heartbeat_log(online, next_seconds));
                // The operator requested a self-update. Act only while idle (no
                // workload). A given minimum is retried only after a cooldown
                // (transient failure / unsatisfiable) and never once terminal
                // (no install receipt). On a successful update we re-exec into
                // the new binary; startup reconcile settles orphaned leases.
                if let Some(min) = update_min
                    && pool.active() == 0
                {
                    if update_attempt.as_ref().map(|a| a.min.as_str()) != Some(min.as_str()) {
                        update_attempt = Some(UpdateAttempt {
                            min: min.clone(),
                            terminal: false,
                            retry_after: None,
                        });
                    }
                    let attempt = update_attempt.as_mut().expect("just set");
                    let gated = attempt.terminal
                        || attempt
                            .retry_after
                            .is_some_and(|t| std::time::Instant::now() < t);
                    if !gated {
                        match maybe_self_update(&min).await {
                            SelfUpdateNext::ReExec => reexec_serve(),
                            SelfUpdateNext::GiveUp => attempt.terminal = true,
                            SelfUpdateNext::RetryAfter(cooldown) => {
                                attempt.retry_after = Some(std::time::Instant::now() + cooldown);
                            }
                        }
                    }
                }
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
                let backoff = heartbeat_backoff_secs(interval, consecutive_failures);
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
            let slice = remaining.min(lease_poll_seconds());
            tokio::select! {
                _ = tokio::signal::ctrl_c() => { println!("stopped"); return Ok(()); }
                _ = tokio::time::sleep(Duration::from_secs(slice)) => {}
            }
            remaining = remaining.saturating_sub(slice);

            // Don't poll (and therefore don't CLAIM) when every slot is taken.
            if !pool.has_free() {
                continue;
            }
            match fetch_next_lease(&client, &api_base, &creds.runner_id, &creds.runner_token).await
            {
                LeasePoll::None => {}
                LeasePoll::Claimed(lease) => match pool.acquire() {
                    Some(slot) => {
                        handle_claimed_lease(
                            &client,
                            &api_base,
                            &creds.runner_token,
                            lease,
                            slot,
                            public_base_url.clone(),
                            public_url_template.clone(),
                        )
                        .await;
                    }
                    None => {
                        // Defensive: `has_free()` was true immediately before the
                        // claim and the serve loop is the only acquirer, so this
                        // is unreachable in practice. If it ever happens, fail the
                        // lease rather than strand it pending forever.
                        eprintln!(
                            "⚠️  lease {} claimed but no slot free; reporting at-capacity",
                            lease.id
                        );
                        let report = LeaseReport::Failed {
                            code: "runner_at_capacity".to_string(),
                            message: "no free run slot on this runner".to_string(),
                        };
                        if let Err(err) = report_lease_status(
                            &client,
                            &api_base,
                            &creds.runner_token,
                            &lease.id,
                            &report,
                        )
                        .await
                        {
                            eprintln!(
                                "⚠️  failed to report at-capacity: {}",
                                scrub_secrets(&format!("{err:#}"))
                            );
                        }
                    }
                },
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

/** Default interval between lease polls while idle (seconds). Conservative for
 *  production; latency-sensitive deployments (e.g. the staging demo runner)
 *  override via `ATO_LEASE_POLL_SECONDS` — the 0–5s claim jitter was the
 *  largest single share of the measured Run→Open latency (ato#940). */
const LEASE_POLL_SECONDS: u64 = 5;

/// Effective idle lease-poll interval: `ATO_LEASE_POLL_SECONDS` (clamped to
/// 1..=60 — sub-second polling would hammer the control plane, and anything
/// over a minute starves lease pickup) or the conservative default.
fn lease_poll_seconds() -> u64 {
    lease_poll_seconds_from(std::env::var("ATO_LEASE_POLL_SECONDS").ok().as_deref())
}

fn lease_poll_seconds_from(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .map(|v| v.clamp(1, 60))
        .unwrap_or(LEASE_POLL_SECONDS)
}
const DEFAULT_READY_TIMEOUT_SECS: u64 = 600;
/// After a port-LESS readiness signal (the human "[✓] ready" echo), hold this
/// long for the canonical "LIFECYCLE: ready port=N" line — they race on separate
/// streams and the port line usually lands within a line or two. Without this,
/// whichever wins decides whether the proxy + ready_url come up.
const READY_PORT_GRACE: Duration = Duration::from_millis(2500);
/// After a ready signal with NO receipt observed yet, hold this long for the
/// `RECEIPT:` line — receipts go to stderr while lifecycle lines go to stdout,
/// and the two independent stream readers merge in arrival order, so a ready
/// can outrun its receipt. Only after this grace is the ready treated as
/// unverifiable (fail closed).
const READY_RECEIPT_GRACE: Duration = Duration::from_millis(2500);
/** Cap per-run log files so a chatty child cannot fill the disk. */
const MAX_RUN_LOG_BYTES: usize = 2 * 1024 * 1024;

pub const LEASE_COMMAND_KIND: &str = "run_source_sandbox";

/// Lease kind that carries a stable capsule identity (OCI / Store capsule) the
/// runner launches via `ato run <ref> --managed-state-root ...` (see
/// `resolve_lease_execution`). Executed and advertised.
pub const RUN_CAPSULE_LEASE_KIND: &str = "run_capsule";

/// Lease command kinds this runner can actually EXECUTE. Advertised in the
/// heartbeat so the control plane never dispatches a kind the runner would
/// reject on-device.
pub const SUPPORTED_LEASE_KINDS: &[&str] = &[LEASE_COMMAND_KIND, RUN_CAPSULE_LEASE_KIND];

/// The lease command `runtime` value that selects HOST dispatch (no `--sandbox`).
/// native-inference runs a managed engine (llama.cpp) as a host process and is
/// incompatible with the source sandbox (ato#762). It is ALSO advertised in
/// `supported_lease_kinds` (when this host can run it) so the control plane only
/// dispatches native-inference leases to capable runners. Only this exact value
/// selects host dispatch — there is no generic host-exec path.
pub const NATIVE_INFERENCE_RUNTIME: &str = "native-inference";

/// How the runner dispatches a claimed lease's `ato run` child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunnerDispatchMode {
    /// Default: run the dispatched ref under the nacelle sandbox (`--sandbox`).
    Sandboxed,
    /// native-inference: host execution (managed engine/model), no `--sandbox`.
    NativeInferenceHost,
}

impl RunnerDispatchMode {
    /// Decide the mode from the lease command's `runtime` field. Only the exact
    /// `native-inference` runtime selects host dispatch; absent or any other
    /// runtime keeps the default sandboxed dispatch (no generic host-exec).
    fn from_command(command: &serde_json::Value) -> Self {
        match command.get("runtime").and_then(|v| v.as_str()) {
            Some(NATIVE_INFERENCE_RUNTIME) => Self::NativeInferenceHost,
            _ => Self::Sandboxed,
        }
    }
}

/// Cached host native-inference capability (probed once per process). The probe
/// is cheap but the heartbeat path calls it frequently, and the host capability
/// does not change across a serve session.
fn native_inference_ready() -> bool {
    static READY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *READY.get_or_init(crate::application::native_inference_doctor::is_ready)
}

/// Lease "kinds"/runtimes this runner accepts dispatch for. The base command
/// kinds plus `native-inference` when this host can actually run it — the control
/// plane gates native-inference dispatch on this advertised value (ato#762).
/// Pure: the lease kinds/runtimes to advertise given the host's native-inference
/// readiness. Split from the cached host probe so the conditional `native-inference`
/// append is unit-testable.
fn advertised_lease_kinds_for(native_inference_ready: bool, restore_ready: bool) -> Vec<String> {
    let mut kinds: Vec<String> = SUPPORTED_LEASE_KINDS
        .iter()
        .map(|s| s.to_string())
        .collect();
    if native_inference_ready {
        kinds.push(NATIVE_INFERENCE_RUNTIME.to_string());
    }
    // Track E (#912): only advertise restore_snapshot where this host can actually
    // restore + SERVE a sealed microVM (KVM + a firecracker binary). The control plane
    // capability-gates dispatch on this, so a KVM-free host is never handed a restore.
    if restore_ready {
        kinds.push(crate::application::ready_state::restore_lease::RESTORE_SNAPSHOT_LEASE_KIND.to_string());
    }
    kinds
}

/// This host can restore + serve a sealed Ready-State snapshot iff a real VMM backend
/// probes available (KVM present + the backend binary). Mirrors `select_backend`'s
/// fail-closed probe without materializing a backend.
fn ready_state_restore_ready() -> bool {
    snapshot::FirecrackerBackend::kvm_present()
}

pub(crate) fn advertised_lease_kinds() -> Vec<String> {
    advertised_lease_kinds_for(native_inference_ready(), ready_state_restore_ready())
}

/// Fail-closed dispatch guard. The control plane already gates native-inference
/// dispatch on the advertised capability, but the runner re-checks before
/// spawning so a mis-dispatched native-inference lease is rejected with a typed
/// reason rather than forced into the sandbox (which native-inference cannot
/// use). `native_inference_ready` is this host's (cached) capability.
fn ensure_dispatch_supported(
    mode: RunnerDispatchMode,
    native_inference_ready: bool,
) -> std::result::Result<(), (String, String)> {
    if mode == RunnerDispatchMode::NativeInferenceHost && !native_inference_ready {
        return Err((
            "native_inference_unavailable".to_string(),
            "received a native-inference lease but this runner cannot run native-inference (host not ready)".to_string(),
        ));
    }
    Ok(())
}

pub const DEFAULT_PROXY_LISTEN: &str = "127.0.0.1:8420";

/// Default number of concurrent run slots. `1` preserves the historical
/// single-slot behavior exactly: slot 0 owns the base proxy port and the legacy
/// `public_base_url` mapping. Operators opt into more with `--max-slots` /
/// `ATO_RUNNER_MAX_SLOTS`; per-app public URLs then require a
/// `--public-url-template` (or an ingress that maps each slot's proxy port),
/// since one `public_base_url` can only reach one local port.
pub const DEFAULT_MAX_SLOTS: usize = 1;
/// Hard ceiling on slots so a fat-fingered value cannot exhaust ports/PIDs.
pub const MAX_SLOTS_CEILING: usize = 64;

/// Clamp an operator-requested slot count into `[1, MAX_SLOTS_CEILING]`.
pub fn clamp_max_slots(requested: usize) -> usize {
    requested.clamp(1, MAX_SLOTS_CEILING)
}

/// Split a `host:port` proxy-listen address into its host and base port. The
/// port is the LAST `:`-separated field (so IPv4 hosts and bare hostnames work);
/// slot `i` then listens on `base_port + i`. Errors rather than guessing if the
/// port is missing or unparseable.
pub fn parse_proxy_listen(listen: &str) -> Result<(String, u16)> {
    let (host, port) = listen
        .rsplit_once(':')
        .with_context(|| format!("proxy listen address '{listen}' must be host:port"))?;
    let port: u16 = port
        .parse()
        .with_context(|| format!("proxy listen port '{port}' is not a valid port"))?;
    if host.is_empty() {
        bail!("proxy listen address '{listen}' is missing a host");
    }
    Ok((host.to_string(), port))
}

/// Reject a proxy base port + slot count whose range would run past
/// `u16::MAX`. Slot `i` listens on `base + i`, so the highest slot needs
/// `base + capacity - 1` to fit. Without this check, saturating math would
/// collapse high slots onto port 65535 and silently break the no-collision
/// invariant — so we fail loudly at startup instead.
pub fn validate_slot_port_range(proxy_base_port: u16, capacity: usize) -> Result<()> {
    let capacity = capacity.max(1);
    let highest = proxy_base_port as usize + capacity - 1;
    if highest > u16::MAX as usize {
        bail!(
            "proxy listen base port {proxy_base_port} with max_slots={capacity} exceeds the valid port range (slot {} would need port {highest} > {})",
            capacity - 1,
            u16::MAX
        );
    }
    Ok(())
}

/// A configured public URL template must distinguish slots: without a `{port}`
/// or `{slot}` placeholder every slot would render the SAME URL, which is a
/// collision/fabrication for a multi-slot runner. The template is a new flag
/// (no back-compat user), so require a placeholder whenever it is set.
pub fn validate_public_url_template(template: Option<&str>) -> Result<()> {
    if let Some(t) = template
        && !(t.contains("{port}") || t.contains("{slot}"))
    {
        bail!(
            "public URL template must include {{port}} or {{slot}} so each slot renders a distinct URL"
        );
    }
    Ok(())
}

/// A fixed pool of concurrent run slots. Slot `i` owns proxy port
/// `proxy_base_port + i`, so concurrent workloads never collide on a listen port
/// and an operator can map a stable external URL to each. The serve loop is the
/// SOLE acquirer (single-threaded), so acquisition takes no lock; detached lease
/// tasks only ever release their own slot.
struct SlotPool {
    occupied: Vec<Arc<AtomicBool>>,
    proxy_host: String,
    proxy_base_port: u16,
}

impl SlotPool {
    fn new(capacity: usize, proxy_host: String, proxy_base_port: u16) -> Self {
        let occupied = (0..capacity.max(1))
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect();
        SlotPool {
            occupied,
            proxy_host,
            proxy_base_port,
        }
    }

    fn capacity(&self) -> usize {
        self.occupied.len()
    }

    fn active(&self) -> usize {
        self.occupied
            .iter()
            .filter(|o| o.load(Ordering::SeqCst))
            .count()
    }

    fn has_free(&self) -> bool {
        self.occupied.iter().any(|o| !o.load(Ordering::SeqCst))
    }

    /// Claim the lowest free slot, if any. Returns `None` when at capacity.
    fn acquire(&self) -> Option<SlotLease> {
        for (index, occ) in self.occupied.iter().enumerate() {
            if occ
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                // The base+capacity range is validated at startup
                // (validate_slot_port_range), so this never overflows; checked
                // arithmetic makes that invariant explicit rather than wrapping
                // or saturating two slots onto the same port.
                let proxy_port = self
                    .proxy_base_port
                    .checked_add(index as u16)
                    .expect("slot port range validated at startup");
                return Some(SlotLease {
                    index,
                    proxy_listen: format!("{}:{}", self.proxy_host, proxy_port),
                    proxy_port,
                    occupied: Arc::clone(occ),
                    released: Arc::new(AtomicBool::new(false)),
                });
            }
        }
        None
    }
}

/// A claimed concurrency slot, handed to the lease task. `release()` frees it
/// for reuse and is idempotent (clones share the flag), so the many lease exit
/// paths can each release without risk of double-freeing — and a slot whose
/// workload could not be confirmed gone is simply never released (fail closed,
/// same invariant the single-slot `busy` flag held).
#[derive(Clone)]
struct SlotLease {
    index: usize,
    proxy_listen: String,
    proxy_port: u16,
    occupied: Arc<AtomicBool>,
    released: Arc<AtomicBool>,
}

impl SlotLease {
    fn release(&self) {
        if !self.released.swap(true, Ordering::SeqCst) {
            self.occupied.store(false, Ordering::SeqCst);
        }
    }
}

/// The public URL the runner will claim for a slot — honestly.
///
/// * With a template (`{port}` / `{slot}` placeholders) the operator asserts
///   their ingress maps each slot's proxy port to that URL, so it is filled in
///   for every slot.
/// * Without a template only the legacy single mapping exists: `public_base_url`
///   reaches the base proxy port, which is slot 0. Any other slot gets `None`
///   rather than a fabricated URL.
fn public_ready_url(
    public_base_url: Option<&str>,
    public_url_template: Option<&str>,
    slot: &SlotLease,
) -> Option<String> {
    if let Some(template) = public_url_template {
        return Some(
            template
                .replace("{port}", &slot.proxy_port.to_string())
                .replace("{slot}", &slot.index.to_string()),
        );
    }
    match public_base_url {
        Some(base) if slot.index == 0 => Some(format!("{}/", base.trim_end_matches('/'))),
        _ => None,
    }
}

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

/// A validated `run_capsule` lease. The runner executes `run_ref` AND keys
/// persistent state on it, so the executed artifact and the state namespace can
/// never diverge. `owner_id` (server-confirmed) scopes the state namespace;
/// `capsule_slug` is display-only and never drives execution or state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCapsuleCommand {
    /// The single capsule reference the runner runs via `ato run <run_ref>` and
    /// uses as the immutable state-namespace identity. CONTRACT: the control
    /// plane MUST resolve any mutable slug to an **immutable, point-in-time** ref
    /// (revision/digest-pinned) before dispatch, so re-runs and the state
    /// namespace are reproducible and the executed artifact matches the state.
    /// Using one field for both makes execution/state divergence impossible.
    pub run_ref: String,
    /// Owner/account id, confirmed server-side. Namespaces persistent state.
    /// Required: a runner must never key state off client-supplied input.
    pub owner_id: String,
    /// Optional inline recipe `capsule.toml` content. Present for capsules that
    /// have no installable release/manifest (community Store recipes are
    /// published as a recipe TOML, not a built version) — `ato run <run_ref>`
    /// would fail "no installable version". When set, the runner materializes
    /// this TOML to a per-lease dir and runs THAT dir, while still keying
    /// persistent state on `run_ref` (the immutable identity). When absent, the
    /// runner installs `run_ref` directly (developer-published, versioned apps).
    pub recipe_toml: Option<String>,
    /// Optional run id, for audit/logging only.
    pub run_id: Option<String>,
    /// Display-only. MUST NOT drive execution or state isolation.
    pub capsule_slug: Option<String>,
}

/// Parse a `run_capsule` lease. Rejects any other kind and any payload missing
/// the required `run_ref` / `owner_id`. `capsule_slug` is a display hint only.
/// The validated lease is turned into an executable plan by
/// [`resolve_lease_execution`].
pub fn parse_run_capsule_command(
    command: &serde_json::Value,
) -> std::result::Result<RunCapsuleCommand, (String, String)> {
    let kind = command.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if kind != RUN_CAPSULE_LEASE_KIND {
        return Err((
            "unsupported_command".to_string(),
            format!("expected lease command kind {RUN_CAPSULE_LEASE_KIND:?}, got {kind:?}"),
        ));
    }

    let required = |key: &str| -> std::result::Result<String, (String, String)> {
        let value = command
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if value.is_empty() {
            return Err((
                "invalid_command".to_string(),
                format!("run_capsule lease is missing {key}"),
            ));
        }
        Ok(value)
    };
    let optional = |key: &str| -> Option<String> {
        command
            .get(key)
            .and_then(|v| v.as_str())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    Ok(RunCapsuleCommand {
        run_ref: required("run_ref")?,
        owner_id: required("owner_id")?,
        // Recipe TOML is file CONTENT, not an identifier — preserve it verbatim
        // (do not trim, unlike the id/ref fields).
        recipe_toml: command
            .get("recipe_toml")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string()),
        run_id: optional("run_id"),
        capsule_slug: optional("capsule_slug"),
    })
}

// ── Secret scrubbing ──

/// The single redaction placeholder used for every scrubbed secret value.
const SCRUB_PLACEHOLDER: &str = "[REDACTED]";

/// One redaction pass: a compiled pattern plus the replacement template
/// applied to each match. `$1`/`$2`/… in the template refer to capture groups,
/// so a pass can keep the structural prefix (`KEY=`, `Bearer `) and redact only
/// the secret value.
struct ScrubPass {
    re: regex::Regex,
    replacement: &'static str,
}

/// Compiled, ordered redaction passes applied to any text that leaves this
/// process. Each pass keeps the surrounding structure (the line/traceback
/// shape) and replaces only the secret VALUE, so persisted failure reports
/// remain useful for debugging while never carrying a live credential.
///
/// Compiled once: [`scrub_secrets`] runs per child-log line, so per-call
/// compilation would be wasteful. None of these patterns can fail to compile;
/// the static asserts that at construction time.
///
/// Runner tokens are redacted FIRST (in [`scrub_runner_tokens`]) into the
/// `ato_rnr_[REDACTED]` form; these passes deliberately do not re-touch that
/// marker (the value classes exclude `[`), so the runner-token shape survives.
static SCRUB_PASSES: std::sync::LazyLock<Vec<ScrubPass>> = std::sync::LazyLock::new(|| {
    let patterns: &[(&str, &str)] = &[
        // URL credentials: `scheme://user:pass@host` → keep the shape, drop
        // the userinfo. Must run before the generic key=value pass.
        (r"://[^:@/\s]+:[^@\s]+@", "://[REDACTED]@"),
        // Known high-confidence token prefixes (case-insensitive on the
        // prefix label, value kept verbatim-length-agnostic). Covers GitHub
        // (ghp_/gho_/ghu_/ghs_/ghr_/github_pat_), OpenAI/Anthropic
        // (sk-, sk-ant-), and npm. `sk-ant-` is matched by the `sk-` arm
        // (the trailing run is consumed greedily).
        (
            r"(?i)\b(github_pat_|ghp_|gho_|ghu_|ghs_|ghr_|sk-|npm_)[A-Za-z0-9_-]+",
            SCRUB_PLACEHOLDER,
        ),
        // AWS access-key ids.
        (r"\bAKIA[A-Z0-9]{16}\b", SCRUB_PLACEHOLDER),
        // Bearer / Authorization headers. The value class excludes `[` so the
        // already-redacted runner-token marker (`ato_rnr_[REDACTED]`) is not
        // re-matched as a whole; the leftover `ato_rnr_` prefix is skipped in
        // [`scrub_secrets`] (see the per-match guard), preserving its shape.
        (
            r"(?i)(bearer\s+|authorization:\s*(?:bearer\s+)?)[A-Za-z0-9._~+/=-]{8,}",
            "$1[REDACTED]",
        ),
        // `.env`-style / generic secret assignments. Matches either
        // `KEY=value` or `KEY: value` when the KEY looks secret-bearing
        // (api key/apikey/token/secret/password/passwd/pwd/credential).
        (
            r#"(?i)((?:[A-Za-z0-9_.-]*(?:api[_-]?key|token|secret|password|passwd|pwd|credential)[A-Za-z0-9_.-]*)\s*[:=]\s*)("?)[^\s"']+("?)"#,
            "$1$2[REDACTED]$3",
        ),
        // Any UPPER_SNAKE assignment with a non-trivial value (catches
        // `OPENAI_API_KEY=…`, `DATABASE_URL=…`, `MY_SECRET=…`). The value
        // class excludes `[`, leaving an already-redacted `KEY=[REDACTED]`
        // untouched.
        (
            r#"(?m)\b([A-Z][A-Z0-9_]{2,})=("?)[^\s"'\[]{4,}("?)"#,
            "$1=$2[REDACTED]$3",
        ),
    ];
    patterns
        .iter()
        .map(|(re, replacement)| ScrubPass {
            re: regex::Regex::new(re).expect("scrub pattern must compile"),
            replacement,
        })
        .collect()
});

/// Redact secrets from any text that leaves this process — error reports, lease
/// failure messages, and the persisted run-log tail (a sandboxed child's raw
/// stdout/stderr echoed as `[child] …`). This is the single common boundary
/// every runner sink routes through before persistence (ato#702):
/// [`BoundedLog::line`] (saved `log_tail`), the [`LeaseReport::Failed`] message
/// (lease error / failure report), and the `scrub_secrets(&format!("{err:#}"))`
/// run-error reports throughout the lease loop.
///
/// Redacts: ato runner tokens (`ato_rnr_…`), GitHub tokens
/// (`ghp_`/`gho_`/`github_pat_`/…), OpenAI & Anthropic keys (`sk-…`,
/// `sk-ant-…`), npm tokens, AWS access-key ids (`AKIA…`), Bearer /
/// Authorization headers, URL userinfo credentials, and `.env`-style
/// `KEY=value` / `KEY: value` secret assignments. The surrounding traceback
/// shape is preserved — only the secret value is replaced with `[REDACTED]`.
pub fn scrub_secrets(text: &str) -> String {
    let mut out = scrub_runner_tokens(text);
    for pass in SCRUB_PASSES.iter() {
        // Cow::Owned only when a match was rewritten; the common
        // (no-secret) line stays a borrow and avoids an allocation. The
        // closure keeps the structural capture groups and, where a value
        // group exists, substitutes the placeholder — but leaves the leftover
        // `ato_rnr_` prefix of an already-redacted runner token untouched so
        // [`scrub_runner_tokens`]'s dedicated `ato_rnr_[REDACTED]` shape
        // survives.
        out = pass
            .re
            .replace_all(&out, |caps: &regex::Captures| {
                // Skip a match that already carries (or overlaps) a redaction
                // artifact — most importantly the runner token's
                // `ato_rnr_[REDACTED]` form, which the Bearer pass would
                // otherwise partially re-match into `[REDACTED][REDACTED]`.
                if caps[0].contains("ato_rnr_") || caps[0].contains(SCRUB_PLACEHOLDER) {
                    return caps[0].to_string();
                }
                let mut rendered = String::new();
                caps.expand(pass.replacement, &mut rendered);
                rendered
            })
            .into_owned();
    }
    out
}

/// Redact ato runner bearer tokens (`ato_rnr_…`). Split out from the regex
/// passes because it predates them and has dedicated test coverage; the token
/// never goes into child env, but scrub defensively anyway.
fn scrub_runner_tokens(text: &str) -> String {
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

/// Parsed payload of a `CONSENT-REQUIRED: <json>` line from `ato run` (P4-A).
///
/// This is the shared wire type [`protocol::consent::ConsentRequiredLine`]:
/// the full identity 5-tuple (under [`identity`](protocol::consent::ConsentRequiredLine::identity))
/// is the decision contract; `consent_ref` is its hash
/// (blake3(JCS(schema + 5-tuple))). The runner reports this as needs_consent
/// and, only after the owner approves this exact `consent_ref`, calls the
/// local `approve-execution-plan` primitive and retries.
///
/// All fields are required (no serde defaults) and the line is honored only
/// when [`is_valid`](protocol::consent::ConsentRequiredLine::is_valid)
/// holds — an incomplete or ill-formed signal must never reach the control
/// plane. Validation lives in `protocol` so the producer
/// (`consent_store`), this consumer, and the desktop stderr consumer can
/// never drift.
pub use protocol::consent::ConsentRequiredLine as ConsentRequest;

#[derive(Debug, Clone, PartialEq)]
pub enum ChildSignal {
    /// Stable machine-readable receipt pointer ("RECEIPT: <path>").
    Receipt(PathBuf),
    /// The honest ready signal (probe-confirmed; ato#608). Carries the
    /// observed workload port when the lifecycle line reports one.
    Ready { port: Option<u16> },
    /// Launched with NO readiness signal (StartedWithoutReadiness).
    StartedNoReadiness,
    /// The CLI announced the service exited before readiness.
    ExitedBeforeReady,
    /// `ato run` requires consent for this ExecutionPlan ("CONSENT-REQUIRED:
    /// <json>"). Carries the 5-tuple + consent_ref + summary for owner approval
    /// (P4-A). Parsed here; the lease loop does not act on it yet (PR3 wires it).
    ConsentRequired(ConsentRequest),
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
    // Machine-readable consent gate (P4-A): "CONSENT-REQUIRED: <json>". Only a
    // complete, well-formed signal is honored — incomplete payloads are ignored.
    if let Some(rest) = trimmed.strip_prefix("CONSENT-REQUIRED: ")
        && let Ok(request) = serde_json::from_str::<ConsentRequest>(rest.trim())
        && request.is_valid()
    {
        return Some(ChildSignal::ConsentRequired(request));
    }
    // Primary, machine-readable ready signal: "LIFECYCLE: ready[ port=N]".
    if let Some(rest) = trimmed.strip_prefix("LIFECYCLE: ready") {
        let port = rest
            .trim()
            .strip_prefix("port=")
            .and_then(|value| value.trim().parse::<u16>().ok());
        return Some(ChildSignal::Ready { port });
    }
    if trimmed.contains("(ready event received)") {
        // Human-string fallback (older child binaries): ready, port unknown.
        return Some(ChildSignal::Ready { port: None });
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
    Ready {
        execution_id: String,
        /// Observed local workload port (from the lifecycle line); enables
        /// the root proxy + ready_url. None -> ready without a URL.
        port: Option<u16>,
    },
    /// `ato run` hit the ExecutionPlan consent gate (E302) and emitted the
    /// machine signal. Carries the 5-tuple + consent_ref + summary; the lease
    /// loop parks needs_consent and waits for the owner decision (P4-A). Routed
    /// to /consent-required, never to /status.
    ConsentRequired(ConsentRequest),
    Failed {
        code: String,
        message: String,
    },
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
        LeaseReport::Ready { .. } => {
            unreachable!("ready reports go through report_lease_ready (/ready endpoint)")
        }
        LeaseReport::ConsentRequired(_) => {
            unreachable!("consent gates go through report_consent_required (/consent-required)")
        }
        LeaseReport::Failed { code, message } => {
            // On failure ONLY, attach the scrubbed tail of the child log so an
            // operator can triage remotely (GET /v1/runs/:id error_log_tail)
            // without shelling into the runner. Failure-only + bounded = no
            // steady-state log flooding.
            let mut error = serde_json::json!({
                "code": code,
                "message": scrub_secrets(message),
            });
            // Attach the scrubbed child-log tail so the failure can be triaged
            // remotely (ato-api derives a diagnostic report from it). Best-
            // effort: a missing/empty log just omits the field.
            if let Some(tail) = read_log_tail(lease_id, LOG_TAIL_MAX_BYTES) {
                error["log_tail"] = serde_json::Value::String(tail);
            }
            serde_json::json!({ "status": "failed", "error": error })
        }
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

// ── Ready reporting (/ready) + root proxy ──

/// What the runner will claim on /ready. A ready_url appears ONLY when the
/// full local chain is proven: a public URL candidate exists for this slot
/// (`public_ready_url`) AND the observed workload port is known AND the local
/// per-slot root proxy actually came up.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadyPayload {
    pub execution_id: String,
    pub ready_url: Option<String>,
    pub local_port: Option<u16>,
}

/// Decide the /ready payload from what was actually observed/achieved.
/// `candidate_url` is the slot's honest public URL (see `public_ready_url`);
/// `proxy_started` is the result of the proxy bring-up attempt (None = not
/// attempted because no URL candidate or no port existed). The URL is claimed
/// ONLY under full proof: a candidate exists AND the workload port is known AND
/// the local proxy actually came up.
pub fn decide_ready_payload(
    execution_id: String,
    candidate_url: Option<&str>,
    port: Option<u16>,
    proxy_started: Option<bool>,
) -> ReadyPayload {
    let ready_url = match (candidate_url, port, proxy_started) {
        (Some(url), Some(_), Some(true)) => Some(url.to_string()),
        // No candidate, unknown port, or a proxy that failed to start: a URL
        // would be a fabrication — report ready without one.
        _ => None,
    };
    ReadyPayload {
        execution_id,
        ready_url,
        local_port: port,
    }
}

async fn report_lease_ready(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    lease_id: &str,
    payload: &ReadyPayload,
) -> Result<()> {
    let url = format!(
        "{}/v1/runner-leases/{}/ready",
        api_base.trim_end_matches('/'),
        lease_id
    );
    let mut body = serde_json::json!({ "execution_id": payload.execution_id });
    if let Some(ready_url) = payload.ready_url.as_deref() {
        body["ready_url"] = serde_json::Value::String(ready_url.to_string());
    }
    if let Some(port) = payload.local_port {
        body["local_port"] = serde_json::Value::Number(port.into());
    }
    let response = client
        .post(&url)
        .bearer_auth(runner_token)
        .json(&body)
        .send()
        .await
        .context("ready report request failed")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("ready report rejected (HTTP {status}): {body}");
    }
    Ok(())
}

/// Single-slot root L4 proxy: every accepted connection is piped to the ONE
/// fixed upstream `127.0.0.1:<workload_port>` — by construction this cannot
/// be an open proxy (no caller-controlled upstream exists). Root proxying
/// (not /runs/<id>/ path multiplexing) keeps root-relative app paths like
/// /assets/* and /api/* working without HTML rewriting; fine while the
/// runner is single-slot.
pub async fn start_root_proxy(
    listen: &str,
    workload_port: u16,
) -> Result<tokio::task::JoinHandle<()>> {
    start_root_proxy_to(listen, format!("127.0.0.1:{workload_port}")).await
}

/// Core of [`start_root_proxy`] with an explicit upstream address. The child-run
/// path pipes to host loopback; a restored microVM serves on its TAP guest IP
/// (e.g. `172.16.0.2:8080`), which the snapshot backend reports as the session's
/// `workload_addr` — the upstream is always a fixed, session-derived address,
/// never caller/request-controlled, so this still cannot be an open proxy.
pub async fn start_root_proxy_to(
    listen: &str,
    upstream_addr: String,
) -> Result<tokio::task::JoinHandle<()>> {
    // Refuse to come up if the upstream is not actually accepting — a proxy
    // in front of nothing would make ready_url a lie.
    tokio::net::TcpStream::connect(&upstream_addr)
        .await
        .with_context(|| format!("workload {upstream_addr} is not accepting"))?;

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind proxy listener on {listen}"))?;
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else {
                break;
            };
            let upstream_addr = upstream_addr.clone();
            tokio::spawn(async move {
                let Ok(mut upstream) = tokio::net::TcpStream::connect(&upstream_addr).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut upstream).await;
            });
        }
    });
    Ok(handle)
}

// ── Owner-initiated stop (P3-C) ──
//
// Stop is two-phase and honest: the PWA asks the API to stop, the API marks
// the run `stopping` and records the request, and ONLY after this runner
// terminates the workload, tears down the proxy, and frees its slot does it
// POST /stopped — the single place the API may claim the run is stopped.
// A teardown it cannot fully confirm is reported as such (partial cleanup),
// never laundered into a clean stop.

/// How often an active run polls the control channel for an owner stop. Snappy
/// enough for a Stop button; at most one active run exists, so the load is one
/// request every few seconds.
const STOP_POLL_SECONDS: u64 = 3;
/// Grace a SIGTERM'd workload group gets to exit before escalating to SIGKILL.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// Window to confirm the group is reaped after SIGKILL.
const STOP_KILL_GRACE: Duration = Duration::from_secs(3);
/// Window to confirm the proxy task actually terminated after abort.
const STOP_PROXY_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Deserialize)]
struct LeaseControl {
    #[serde(default)]
    stop_requested: bool,
    /// The owner's consent decision for a needs_consent lease (P4-A). null until
    /// the owner decides; the runner verifies consent_ref before acting on it.
    #[serde(default)]
    consent: Option<ConsentDecision>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConsentDecision {
    status: String,
    consent_ref: String,
}

/// Watch the lease's control channel for an owner-initiated stop. Sets the flag
/// and fires the notify the moment a stop is requested, then returns. Also
/// returns quietly when the lease is gone (404) or the runner is no longer
/// valid (401) so the task never spins; transient errors retry next tick.
async fn poll_lease_control(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    lease_id: &str,
    stop_flag: Arc<AtomicBool>,
    stop_notify: Arc<tokio::sync::Notify>,
) {
    let url = format!(
        "{}/v1/runner-leases/{}/control",
        api_base.trim_end_matches('/'),
        lease_id
    );
    // Poll first, then sleep: an already-requested stop is observed within a
    // network round-trip, and the sleep between ticks prevents a busy spin on
    // persistent errors.
    loop {
        match poll_control_once(client, &url, runner_token).await {
            ControlOutcome::Stop => {
                stop_flag.store(true, Ordering::SeqCst);
                stop_notify.notify_one();
                return;
            }
            // Lease gone or runner invalid/revoked: nothing left to watch.
            // Revocation teardown is the heartbeat loop's job; just stop here.
            ControlOutcome::Done => return,
            ControlOutcome::Continue => {}
        }
        tokio::time::sleep(Duration::from_secs(STOP_POLL_SECONDS)).await;
    }
}

enum ControlOutcome {
    /// No stop requested (or a transient error) — keep watching.
    Continue,
    /// The owner requested a stop.
    Stop,
    /// The lease/runner is gone (404/401) — stop watching.
    Done,
}

/// One control poll. Transient transport/5xx errors map to Continue (retry);
/// 404/401 map to Done; a `stop_requested` body maps to Stop.
async fn poll_control_once(
    client: &reqwest::Client,
    url: &str,
    runner_token: &str,
) -> ControlOutcome {
    let response = match client.get(url).bearer_auth(runner_token).send().await {
        Ok(response) => response,
        Err(_) => return ControlOutcome::Continue,
    };
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::UNAUTHORIZED {
        return ControlOutcome::Done;
    }
    if !status.is_success() {
        return ControlOutcome::Continue;
    }
    match response.json::<LeaseControl>().await {
        Ok(control) if control.stop_requested => ControlOutcome::Stop,
        _ => ControlOutcome::Continue,
    }
}

// ── ExecutionPlan consent gate (P4-A) ──
//
// When `ato run` gates at E302 it emits the machine-readable CONSENT-REQUIRED
// line; the runner reports it here (lease → needs_consent), waits for the owner
// to approve the EXACT consent_ref, verifies the recomputed key against both the
// child's emitted ref and the owner's approved ref, records the approval in THIS
// host's local ledger, and retries. The API holds the owner decision only — it
// never writes the host-local ledger.

/// Max consent rounds before failing closed. The normal path is one round (gate
/// → approve → retry runs). A child that keeps re-gating after a verified local
/// approval (should never happen) is bounded here rather than looping forever.
const MAX_CONSENT_ROUNDS: u32 = 3;

/// Report the consent gate: park the lease `needs_consent` with the FULL 5-tuple
/// + consent_ref + summary so the owner can approve the exact policy.
async fn report_consent_required(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    lease_id: &str,
    request: &ConsentRequest,
) -> Result<()> {
    let url = format!(
        "{}/v1/runner-leases/{}/consent-required",
        api_base.trim_end_matches('/'),
        lease_id
    );
    let body = serde_json::json!({
        "schema": request.schema,
        "consent_ref": request.consent_ref,
        "scoped_id": request.identity.scoped_id,
        "version": request.identity.version,
        "target_label": request.identity.target_label,
        "policy_segment_hash": request.identity.policy_segment_hash,
        "provisioning_policy_hash": request.identity.provisioning_policy_hash,
        "summary": request.identity.summary,
    });
    let response = client
        .post(&url)
        .bearer_auth(runner_token)
        .json(&body)
        .send()
        .await
        .context("consent-required request failed")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("consent-required report rejected (HTTP {status}): {body}");
    }
    Ok(())
}

/// The owner's decision for a needs_consent lease, as observed on /control.
enum ConsentOutcome {
    /// Owner approved this consent_ref. The runner MUST still verify the
    /// recomputed key before recording locally + retrying.
    Approved { consent_ref: String },
    /// Owner rejected — the API has already failed the run (consent_rejected).
    Rejected,
    /// TTL elapsed — the API has already failed the run (consent_timeout).
    Expired,
    /// Owner stopped the run while consent was pending.
    Stop,
    /// Lease/runner gone (404/401) — stop waiting.
    Gone,
}

/// Poll /control until the owner's consent decision resolves. Pending/transient
/// states keep polling on the stop cadence; the API's TTL is the deadline (a
/// lazy expiry surfaces as `expired`), so the runner keeps no second clock.
async fn poll_consent_decision(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    lease_id: &str,
) -> ConsentOutcome {
    let url = format!(
        "{}/v1/runner-leases/{}/control",
        api_base.trim_end_matches('/'),
        lease_id
    );
    loop {
        if let Some(outcome) = poll_consent_once(client, &url, runner_token).await {
            return outcome;
        }
        tokio::time::sleep(Duration::from_secs(STOP_POLL_SECONDS)).await;
    }
}

/// One consent-decision poll. None = still pending / transient (keep polling).
/// A stop observed here resolves the wait too, so the decision poll is robust on
/// its own even if the background stop watcher has already exited.
async fn poll_consent_once(
    client: &reqwest::Client,
    url: &str,
    runner_token: &str,
) -> Option<ConsentOutcome> {
    let response = match client.get(url).bearer_auth(runner_token).send().await {
        Ok(response) => response,
        Err(_) => return None,
    };
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::UNAUTHORIZED {
        return Some(ConsentOutcome::Gone);
    }
    if !status.is_success() {
        return None;
    }
    let control = response.json::<LeaseControl>().await.ok()?;
    if control.stop_requested {
        return Some(ConsentOutcome::Stop);
    }
    match control.consent {
        Some(decision) => match decision.status.as_str() {
            "approved" => Some(ConsentOutcome::Approved {
                consent_ref: decision.consent_ref,
            }),
            "rejected" => Some(ConsentOutcome::Rejected),
            "expired" => Some(ConsentOutcome::Expired),
            // "pending" or an unknown status: keep waiting.
            _ => None,
        },
        // Consent cleared (e.g. a stop cleared it): keep waiting — a stop will
        // surface via stop_requested above on a subsequent tick.
        None => None,
    }
}

/// What to do after the owner approves a needs_consent lease. The host-local
/// ledger is written ONLY when the child's emitted ref, the recomputed ref (from
/// the child's 5-tuple), and the owner's approved ref ALL agree. Any disagreement
/// voids the approval — re-emit needs_consent rather than admit a different plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsentVerifyAction {
    /// All three refs agree — record locally and retry.
    Record,
    /// Refs disagree — do NOT record; re-emit needs_consent (old approval void).
    ReEmit,
}

fn consent_verify_action(
    child_ref: &str,
    recomputed_ref: &str,
    approved_ref: &str,
) -> ConsentVerifyAction {
    let three_way_match =
        recomputed_ref == child_ref && recomputed_ref == approved_ref && approved_ref == child_ref;
    if three_way_match {
        ConsentVerifyAction::Record
    } else {
        ConsentVerifyAction::ReEmit
    }
}

/// The teardown outcome the runner reports on /stopped. The slot is honestly
/// free ONLY when both the workload process and the proxy are confirmed down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopCleanup {
    pub process_terminated: bool,
    pub proxy_stopped: bool,
    pub slot_released: bool,
}

impl StopCleanup {
    /// Derive the cleanup record from what was actually achieved. `slot_released`
    /// is never asserted independently: a slot is free only when the workload
    /// is gone AND the proxy is down. Anything less keeps the slot held.
    fn from_teardown(process_terminated: bool, proxy_stopped: bool) -> Self {
        Self {
            process_terminated,
            proxy_stopped,
            slot_released: process_terminated && proxy_stopped,
        }
    }
}

#[cfg(test)]
fn stopped_request_body(cleanup: &StopCleanup) -> serde_json::Value {
    stopped_request_body_with_reason(cleanup, "user_requested")
}

fn stopped_request_body_with_reason(cleanup: &StopCleanup, reason: &str) -> serde_json::Value {
    serde_json::json!({
        "reason": reason,
        "cleanup": {
            "process_terminated": cleanup.process_terminated,
            "proxy_stopped": cleanup.proxy_stopped,
            "slot_released": cleanup.slot_released,
        },
    })
}

/// Acknowledge teardown to the control plane. A 409 here is not a transport
/// error: it is the API truthfully recording an incomplete cleanup as a failed
/// stop. We accept it as a delivered ack rather than retrying.
async fn report_lease_stopped(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    lease_id: &str,
    cleanup: &StopCleanup,
) -> Result<()> {
    report_lease_stopped_with_reason(
        client,
        api_base,
        runner_token,
        lease_id,
        cleanup,
        "user_requested",
    )
    .await
}

async fn report_lease_stopped_with_reason(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    lease_id: &str,
    cleanup: &StopCleanup,
    reason: &str,
) -> Result<()> {
    let url = format!(
        "{}/v1/runner-leases/{}/stopped",
        api_base.trim_end_matches('/'),
        lease_id
    );
    let response = client
        .post(&url)
        .bearer_auth(runner_token)
        .json(&stopped_request_body_with_reason(cleanup, reason))
        .send()
        .await
        .context("stopped ack request failed")?;
    let status = response.status();
    if !status.is_success() && status != reqwest::StatusCode::CONFLICT {
        let body = response.text().await.unwrap_or_default();
        bail!("stopped ack rejected (HTTP {status}): {body}");
    }
    Ok(())
}

/// Lease ids from a GET /v1/runners/:id/leases/open body. Tolerant: entries
/// without a string `id` are skipped (a malformed row must not abort the
/// whole reconciliation).
fn parse_open_lease_ids(body: &serde_json::Value) -> Vec<String> {
    body.get("leases")
        .and_then(|v| v.as_array())
        .map(|leases| {
            leases
                .iter()
                .filter_map(|l| l.get("id").and_then(|id| id.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Startup reconciliation: settle leases the API still considers open on
/// this runner with HONEST per-lease evidence. Dispatched children lead
/// their OWN process group (see spawn_run_child), so a hard-killed runner
/// (SIGKILL / OOM) can orphan a live workload subtree — "this is a fresh
/// process" proves nothing about survivors. Every dispatch records its
/// group id next to the run log; reconcile probes that record instead of
/// assuming:
/// - no record, or the group confirmed gone → full-cleanup ack (reason
///   `runner_restarted`) so zombie stop-requested runs settle;
/// - the group may still be alive → ack with NOTHING confirmed so the
///   control plane records a failed stop and HOLDS the slot — never free a
///   slot a surviving workload may still occupy (fail closed, matching
///   perform_stop_cleanup).
///
/// Best-effort: a missing endpoint (older API) or a per-lease failure is
/// logged and never blocks serving.
async fn reconcile_open_leases(
    client: &reqwest::Client,
    api_base: &str,
    runner_id: &str,
    runner_token: &str,
) {
    let url = format!(
        "{}/v1/runners/{}/leases/open",
        api_base.trim_end_matches('/'),
        runner_id
    );
    let response = match client.get(&url).bearer_auth(runner_token).send().await {
        Ok(r) => r,
        Err(err) => {
            eprintln!("⚠️  lease reconciliation skipped (request failed: {err})");
            return;
        }
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        // Older API without the endpoint — nothing to reconcile against.
        return;
    }
    if !response.status().is_success() {
        eprintln!(
            "⚠️  lease reconciliation skipped (HTTP {})",
            response.status()
        );
        return;
    }
    let body: serde_json::Value = match response.json().await {
        Ok(v) => v,
        Err(err) => {
            eprintln!("⚠️  lease reconciliation skipped (bad response: {err})");
            return;
        }
    };
    let lease_ids = parse_open_lease_ids(&body);
    if lease_ids.is_empty() {
        return;
    }
    println!(
        "🧹 reconciling {} stale lease(s) left from a previous runner process",
        lease_ids.len()
    );
    for lease_id in lease_ids {
        let evidence = probe_workload_evidence(&run_pid_path(&lease_id));
        match evidence {
            WorkloadEvidence::ConfirmedGone => {
                // The record (if any) is settled; drop it so a later restart
                // can never probe a recycled pid.
                clear_workload_group(&lease_id);
            }
            WorkloadEvidence::PossiblyAlive(pid) => {
                let hint = pid.map_or("unknown pid".to_string(), |p| format!("pgid {p}"));
                eprintln!(
                    "   ⚠️ lease {lease_id}: a workload ({hint}) may have survived the previous runner; reporting unconfirmed cleanup so the slot stays held. Stop the survivor and restart the runner to settle."
                );
            }
        }
        let cleanup = reconcile_cleanup_for(evidence);
        match report_lease_stopped_with_reason(
            client,
            api_base,
            runner_token,
            &lease_id,
            &cleanup,
            "runner_restarted",
        )
        .await
        {
            Ok(()) => println!(
                "   ✓ lease {lease_id}: acked stopped (runner_restarted, slot_released={})",
                cleanup.slot_released
            ),
            Err(err) => eprintln!("   ⚠️ lease {lease_id}: reconcile ack failed: {err}"),
        }
    }
}

/// Liveness evidence for a previously dispatched workload group, derived
/// from the record `record_workload_group` persisted next to the run log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkloadEvidence {
    /// No group was ever recorded for the lease on this host, or the
    /// recorded group is confirmed gone: "nothing survives here" is real
    /// evidence, not an assumption.
    ConfirmedGone,
    /// The recorded group may still have a live process (or liveness cannot
    /// be probed on this platform / the record is unreadable). The pid is a
    /// hint for the operator when known.
    PossiblyAlive(Option<u32>),
}

/// Probe whether the workload group recorded at `pid_path` survived the
/// previous runner process. Anything short of confirmed absence maps to
/// `PossiblyAlive` — the caller fails closed on it.
fn probe_workload_evidence(pid_path: &Path) -> WorkloadEvidence {
    let Ok(raw) = std::fs::read_to_string(pid_path) else {
        // No record: this host never dispatched (or already confirmed the
        // teardown of) a workload for the lease.
        return WorkloadEvidence::ConfirmedGone;
    };
    let Ok(pid) = raw.trim().parse::<u32>() else {
        // A record exists, so a dispatch happened and its teardown was never
        // confirmed — an unreadable pid is not evidence of absence.
        return WorkloadEvidence::PossiblyAlive(None);
    };
    #[cfg(unix)]
    if process_group_confirmed_gone(pid) {
        return WorkloadEvidence::ConfirmedGone;
    }
    // Unix: the group still has a member (or one we may not signal).
    // Non-Unix: there is no group probe; a recorded dispatch without a
    // confirmed teardown stays possibly-alive.
    WorkloadEvidence::PossiblyAlive(Some(pid))
}

/// The honest /stopped cleanup record for one reconciled lease. Confirmed
/// absence is the ONLY thing that frees the slot; a possible survivor claims
/// nothing — not even `proxy_stopped`, since "a survivor exists" cannot rule
/// out another live serve process still owning both workload and proxy.
fn reconcile_cleanup_for(evidence: WorkloadEvidence) -> StopCleanup {
    match evidence {
        WorkloadEvidence::ConfirmedGone => StopCleanup::from_teardown(true, true),
        WorkloadEvidence::PossiblyAlive(_) => StopCleanup::from_teardown(false, false),
    }
}

/// Send `signal` to the whole process group led by `pid` (negative target).
/// ESRCH (no such group) means the group already exited — the teardown we
/// wanted — so it maps to Ok.
#[cfg(unix)]
fn kill_group(pid: u32, signal: libc::c_int) -> std::io::Result<()> {
    let rc = unsafe { libc::kill(-(pid as libc::pid_t), signal) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(err)
    }
}

/// True only when signal-0 to `pid`'s process group fails with ESRCH: every
/// member is confirmed gone. A delivered probe (rc == 0) is liveness, and so
/// is EPERM — SOME process in the group exists, we just may not signal it —
/// so both map to false (fail closed).
#[cfg(unix)]
fn process_group_confirmed_gone(pid: u32) -> bool {
    let rc = unsafe { libc::kill(-(pid as libc::pid_t), 0) };
    rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// True while any process in `pid`'s group still exists (signal 0 probes
/// without delivering). The production teardown gates on the monitor reaping
/// the child, not on this — zombies linger in the group until reaped — but it
/// is a precise check for tests.
#[cfg(all(unix, test))]
fn process_group_alive(pid: u32) -> bool {
    unsafe { libc::kill(-(pid as libc::pid_t), 0) == 0 }
}

/// True while `pid` is still in the process table (`tasklist` PID filter).
/// The production teardown gates on the monitor reaping the child, not on
/// this — it is a precise check for tests.
#[cfg(all(windows, test))]
fn windows_pid_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\"")))
        .unwrap_or(false)
}

/// `(pid, ppid, pgid)` for every process, so the teardown can walk a workload's
/// whole subtree — including a native-inference engine that put ITSELF in its
/// own process group (executors/source.rs) and would otherwise survive a single
/// `kill(-root_pgid)`. Linux reads `/proc` directly; other Unix shells to `ps`.
/// See ato#769.
#[cfg(target_os = "linux")]
fn read_process_table() -> Vec<(u32, u32, u32)> {
    let mut rows = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return rows;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // "pid (comm) state ppid pgrp …" — comm may contain spaces and ')', so
        // parse the fields AFTER the final ')'.
        let Some((_, after)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = after.split_whitespace();
        let _state = fields.next();
        let ppid = fields.next().and_then(|s| s.parse::<u32>().ok());
        let pgrp = fields.next().and_then(|s| s.parse::<u32>().ok());
        if let (Some(ppid), Some(pgrp)) = (ppid, pgrp) {
            rows.push((pid, ppid, pgrp));
        }
    }
    rows
}

#[cfg(all(unix, not(target_os = "linux")))]
fn read_process_table() -> Vec<(u32, u32, u32)> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            Some((
                cols.next()?.parse().ok()?,
                cols.next()?.parse().ok()?,
                cols.next()?.parse().ok()?,
            ))
        })
        .collect()
}

/// Every distinct process group in the subtree rooted at `root_pid` (the root
/// plus all descendants). The runner spawns `ato run` in its own group, but a
/// native-inference run's engine puts ITSELF in another group
/// (executors/source.rs `process_group(0)`), so terminating only the root group
/// strands the engine. The teardown signals every group this returns. See
/// ato#769.
#[cfg(unix)]
fn workload_subtree_groups(root_pid: u32) -> std::collections::HashSet<u32> {
    use std::collections::{HashMap, HashSet};
    let table = read_process_table();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut pgid_of: HashMap<u32, u32> = HashMap::new();
    for &(pid, ppid, pgid) in &table {
        children.entry(ppid).or_default().push(pid);
        pgid_of.insert(pid, pgid);
    }
    let mut groups: HashSet<u32> = HashSet::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut stack = vec![root_pid];
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(&pgid) = pgid_of.get(&pid).filter(|&&pgid| pgid != 0) {
            groups.insert(pgid);
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    // The root leads its own group (spawn_run_child uses process_group(0)); keep
    // it even if the table snapshot raced the root's appearance.
    groups.insert(root_pid);
    groups
}

/// Poll until every process group is confirmed gone (ESRCH) or `within`
/// elapses. A SIGKILL'd engine that reparents to init is reaped within a beat,
/// so a short poll avoids both a premature "stranded" verdict and an unbounded
/// wait. See ato#769.
#[cfg(unix)]
async fn confirm_subtree_gone(groups: &std::collections::HashSet<u32>, within: Duration) -> bool {
    tokio::time::timeout(within, async {
        while !groups.iter().all(|&g| process_group_confirmed_gone(g)) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok()
}

/// Terminate the workload's whole process-group SUBTREE and wait for it to be
/// reaped. SIGTERM first (let each app shut down cleanly), escalate to SIGKILL
/// after a bounded grace, and confirm every group is gone. A native-inference
/// run's engine runs in its OWN process group (executors/source.rs), so we
/// signal every group in the subtree — not just the `ato run` leader's — or the
/// engine is stranded (ato#769). Returns true only when termination is
/// confirmed; an unconfirmable outcome returns false so the caller fails closed.
/// Non-Unix hosts have no POSIX process groups; there `taskkill /T /F`
/// force-kills the whole subtree under the same confirm-or-fail-closed
/// contract.
async fn terminate_child_group(
    child_pid: Option<u32>,
    mut monitor: tokio::task::JoinHandle<()>,
) -> bool {
    #[cfg(unix)]
    {
        let Some(pid) = child_pid else {
            // No live PID means the child was already reaped — nothing to kill.
            let _ = monitor.await;
            return true;
        };
        // Snapshot EVERY process group in the workload subtree BEFORE killing —
        // a native-inference engine puts itself in its own group, so a single
        // kill(-pid) would strand it (ato#769).
        let mut groups = workload_subtree_groups(pid);
        // Polite first: SIGTERM every group so each app can shut down.
        for &g in &groups {
            let _ = kill_group(g, libc::SIGTERM);
        }
        let mut monitor_done = false;
        tokio::select! {
            _ = &mut monitor => { monitor_done = true; }
            _ = tokio::time::sleep(STOP_GRACE) => {}
        }
        // Force: SIGKILL every group; re-snapshot first so anything the grace
        // window spawned is caught too, then union so nothing is missed.
        groups.extend(workload_subtree_groups(pid));
        for &g in &groups {
            let _ = kill_group(g, libc::SIGKILL);
        }
        // Reap the leader via the monitor (frees the child handle / avoids a
        // lingering zombie) — but only if it has not already completed, since a
        // JoinHandle must not be polled after completion. The subtree confirm
        // below is the source of truth.
        if !monitor_done {
            let _ = tokio::time::timeout(STOP_KILL_GRACE, &mut monitor).await;
        }
        // Confirm the WHOLE subtree is gone — fail closed if any group survives.
        confirm_subtree_gone(&groups, STOP_KILL_GRACE).await
    }
    #[cfg(not(unix))]
    {
        let Some(pid) = child_pid else {
            // No live PID means the child was already reaped — nothing to kill.
            let _ = monitor.await;
            return true;
        };
        // No POSIX process groups here: `taskkill /T /F` terminates the ENTIRE
        // workload subtree — kill_on_drop alone TerminateProcess'es only the
        // direct `ato run` child and would orphan the nacelle/app
        // grandchildren. Exit code 128 means no such process: the tree already
        // exited, which is the teardown we wanted (the ESRCH analogue).
        let tree_killed = match tokio::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
        {
            Ok(status) => status.success() || status.code() == Some(128),
            Err(_) => false,
        };
        if !tree_killed {
            // The kill was refused or could not be issued, so the subtree may
            // still be alive. Reap the direct child (kill_on_drop) as a best
            // effort, but report the teardown UNCONFIRMED — fail closed so the
            // caller holds the slot.
            monitor.abort();
            let _ = monitor.await;
            return false;
        }
        // TerminateProcess cannot be refused; the monitor should reap the
        // direct child promptly. If it still does not return, we cannot
        // confirm termination — fail closed.
        (tokio::time::timeout(STOP_KILL_GRACE, &mut monitor).await).is_ok()
    }
}

/// Stop the ready_url proxy and CONFIRM it actually stopped. `JoinHandle::abort()`
/// only *requests* cancellation — the listener socket may still be bound the
/// instant it returns — so we await the task's real termination before claiming
/// `proxy_stopped`. A cancelled `JoinError` is the expected clean outcome; a
/// timeout or any other error (e.g. a panicked task) is unconfirmed and maps to
/// false so the caller fails closed. No proxy was ever up → vacuously stopped.
async fn stop_proxy(handle: Option<tokio::task::JoinHandle<()>>) -> bool {
    stop_proxy_within(handle, STOP_PROXY_GRACE).await
}

/// Core of [`stop_proxy`] with an injectable confirmation window (tests use a
/// short one to exercise the unconfirmed/timeout path deterministically).
async fn stop_proxy_within(handle: Option<tokio::task::JoinHandle<()>>, grace: Duration) -> bool {
    let Some(handle) = handle else {
        return true;
    };
    handle.abort();
    match tokio::time::timeout(grace, handle).await {
        Ok(Ok(())) => true,
        Ok(Err(err)) if err.is_cancelled() => true,
        _ => false,
    }
}

/// Tear down an active run on owner request: terminate the workload group, stop
/// the proxy, release the slot ONLY if both are confirmed, and ack /stopped.
#[allow(clippy::too_many_arguments)]
async fn perform_stop_cleanup(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    lease_id: &str,
    child_pid: Option<u32>,
    monitor: tokio::task::JoinHandle<()>,
    proxy_handle: Option<tokio::task::JoinHandle<()>>,
    slot: &SlotLease,
) {
    println!("🛑 lease {lease_id}: owner requested stop; tearing down workload");

    let process_terminated = terminate_child_group(child_pid, monitor).await;
    if process_terminated {
        // Confirmed reap: the survivor record is settled. Drop it so a later
        // restart can never probe a recycled pid.
        clear_workload_group(lease_id);
    }

    // Abort the proxy AND wait for the listener task to actually end before
    // claiming it stopped — the upstream is already dead, so in-flight
    // connections drain on their own once the listener is gone. A run that never
    // brought a proxy up is vacuously stopped; an abort we cannot confirm is
    // reported false (→ slot held).
    let proxy_stopped = stop_proxy(proxy_handle).await;

    let cleanup = StopCleanup::from_teardown(process_terminated, proxy_stopped);

    // Free the slot ONLY on a fully confirmed teardown. If we cannot confirm
    // the workload is gone, keep the slot held (fail closed) rather than offer a
    // slot a possibly-live workload still occupies.
    if cleanup.slot_released {
        slot.release();
        println!("🛑 lease {lease_id}: workload terminated, proxy stopped, slot released");
    } else {
        eprintln!(
            "⚠️  lease {lease_id}: stop cleanup incomplete (process_terminated={}, proxy_stopped={}); slot held",
            cleanup.process_terminated, cleanup.proxy_stopped
        );
    }

    if let Err(err) = report_lease_stopped(client, api_base, runner_token, lease_id, &cleanup).await
    {
        eprintln!(
            "⚠️  lease {lease_id}: stopped ack failed: {}",
            scrub_secrets(&format!("{err:#}"))
        );
    }
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

/// Upper bound on the log tail attached to a failure report. Matches the API's
/// `error.log_tail` cap (POST /v1/runner-leases/:id/status) so the server never
/// has to truncate ours.
const LOG_TAIL_MAX_BYTES: usize = 16 * 1024;

/// Read the tail (last `max_bytes`) of a lease's run log for a failure report.
/// The on-disk log is already secret-scrubbed line-by-line (BoundedLog), but we
/// scrub once more as defense in depth before it leaves the process. Returns
/// None when there is no log or it is empty. A truncated tail is prefixed with a
/// marker; when it begins mid-line (a newline exists within it) the partial
/// leading line is dropped. A single line longer than `max_bytes` (no interior
/// newline) is kept verbatim under the marker rather than blanked.
fn read_log_tail(lease_id: &str, max_bytes: usize) -> Option<String> {
    read_log_tail_from(&run_log_path(lease_id), max_bytes)
}

/// Path-taking core of [`read_log_tail`] (testable without ATO_HOME).
fn read_log_tail_from(path: &Path, max_bytes: usize) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let start = bytes.len().saturating_sub(max_bytes);
    let mut text = String::from_utf8_lossy(&bytes[start..]).into_owned();
    if start > 0 {
        // Drop the (partial) leading line only when there IS an interior
        // newline; a single over-long line with none is kept verbatim.
        if let Some(nl) = text.find('\n') {
            text = text[nl + 1..].to_string();
        }
        text = format!("...[earlier log truncated]\n{text}");
    }
    if text.trim().is_empty() {
        return None;
    }
    Some(scrub_secrets(&text))
}

/// Where the workload's process-group id for `lease_id` is recorded (next to
/// its run log).
fn run_pid_path(lease_id: &str) -> PathBuf {
    run_log_path(lease_id).with_extension("pid")
}

/// Record the dispatched workload's process-group id (== child pid, see
/// spawn_run_child) so a FUTURE serve process can probe — not assume —
/// whether the subtree survived a hard-killed runner. The children lead
/// their own group precisely so a stop can signal the whole subtree; the
/// flip side is that they outlive a SIGKILLed/OOMed runner, and this record
/// is the only evidence bridge across that restart. Best-effort: a failed
/// write only costs the record, never the dispatch.
fn record_workload_group(lease_id: &str, pid: Option<u32>) {
    let Some(pid) = pid else { return };
    let path = run_pid_path(lease_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, pid.to_string());
}

/// Drop the recorded group once its teardown is CONFIRMED (child reaped, or
/// the group kill verified). Never call this on an unconfirmed teardown: the
/// record is exactly what lets the next serve process detect a survivor.
fn clear_workload_group(lease_id: &str) {
    let _ = std::fs::remove_file(run_pid_path(lease_id));
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

/// The `ato run` arguments for a claimed lease — selected by dispatch mode
/// BEFORE spawn, never patched onto an already-built command. Sandboxed leases
/// get `--sandbox`; native-inference (host) leases do NOT, because
/// native-inference runs a managed engine as a host process and is incompatible
/// with the source sandbox (ato#762).
fn run_child_args(
    run_ref: &str,
    managed_state_root: Option<&Path>,
    mode: RunnerDispatchMode,
) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    let mut args: Vec<OsString> = vec![OsString::from("run"), OsString::from(run_ref)];
    if mode == RunnerDispatchMode::Sandboxed {
        args.push(OsString::from("--sandbox"));
    }
    args.push(OsString::from("-y"));
    // run_capsule leases bind persistent state under a runner-managed root
    // (scoped by owner + immutable capsule identity); source leases pass None.
    if let Some(root) = managed_state_root {
        args.push(OsString::from("--managed-state-root"));
        args.push(root.as_os_str().to_os_string());
    }
    args
}

fn spawn_run_child(
    run_ref: &str,
    managed_state_root: Option<&Path>,
    mode: RunnerDispatchMode,
) -> Result<tokio::process::Child> {
    let child_bin = match std::env::var("ATO_RUNNER_CHILD_BIN") {
        Ok(path) if !path.trim().is_empty() => PathBuf::from(path),
        _ => std::env::current_exe().context("failed to resolve the ato binary path")?,
    };
    let mut cmd = tokio::process::Command::new(child_bin);
    cmd.args(run_child_args(run_ref, managed_state_root, mode));
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
    // Lead a new process group (pgid == child pid) so a stop can signal the
    // ENTIRE workload subtree — `ato run` forks nacelle → bwrap → the app —
    // with a single kill(-pgid). Without this we could only reap the direct
    // child and would orphan the sandboxed grandchildren.
    #[cfg(unix)]
    cmd.process_group(0);
    // The runner token lives only in this process's memory and credentials
    // file — it is never exported to the child environment.
    cmd.kill_on_drop(true);
    cmd.spawn().context("failed to spawn ato run child")
}

/// What a claimed lease resolves to for execution: the `ato run` positional ref
/// and, for `run_capsule`, the runner-managed persistent state root.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LeaseExecution {
    run_ref: String,
    managed_state_root: Option<PathBuf>,
    dispatch_mode: RunnerDispatchMode,
}

/// Root for runner-managed persistent capsule state: `<runner-base>/state`.
/// Sits beside the per-lease run logs under the credentials directory.
fn runner_state_root() -> PathBuf {
    credentials_path()
        .parent()
        .map(|parent| parent.join("state"))
        .unwrap_or_else(|| PathBuf::from("state"))
}

/// Managed state root for a `run_capsule` lease:
/// `<runner-base>/state/<owner>/<run_ref>`. The owner is the server-confirmed
/// `owner_id`; the identity is `run_ref` — the SAME immutable ref the runner
/// executes, so the executed artifact and the state namespace can never
/// diverge. Both segments use the `path_segment` scheme the `ato run` resolver
/// uses, so the namespace is consistent and collision-free; `ato run` then
/// appends `<target>/<state_key>` beneath this.
fn run_capsule_state_root(cmd: &RunCapsuleCommand) -> PathBuf {
    runner_state_root()
        .join(crate::application::pipeline::phases::run::path_segment(
            &cmd.owner_id,
        ))
        .join(crate::application::pipeline::phases::run::path_segment(
            &cmd.run_ref,
        ))
}

/// Validate a `run_ref` before it becomes an `ato run` positional argument:
/// non-empty, no whitespace/control chars, and not a flag. Immutable refs like
/// `community/openlist@<rev>` or `capsule://…#blake3:…` are allowed; a leading
/// `-` (flag smuggling) or whitespace is not. (Immutability itself is the
/// control plane's contract — see `RunCapsuleCommand::run_ref`.)
fn validate_capsule_run_ref(run_ref: &str) -> std::result::Result<String, (String, String)> {
    let r = run_ref.trim();
    if r.is_empty() || r.starts_with('-') || r.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err((
            "invalid_command".to_string(),
            format!("run_ref is not a safe `ato run` ref: {run_ref:?}"),
        ));
    }
    Ok(r.to_string())
}

/// Resolve a claimed lease command into an executable plan, dispatching by kind.
/// `run_source_sandbox` runs a source repo (no managed state); `run_capsule`
/// runs a capsule ref with a runner-managed persistent state root. Unknown
/// kinds are rejected without executing.
fn resolve_lease_execution(
    command: &serde_json::Value,
    recipe_dir: &Path,
) -> std::result::Result<LeaseExecution, (String, String)> {
    let kind = command.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    // The runtime field (set by the control plane) selects host vs sandbox
    // dispatch, independent of the lease kind (ato#762).
    let dispatch_mode = RunnerDispatchMode::from_command(command);
    match kind {
        LEASE_COMMAND_KIND => {
            let c = parse_lease_command(command)?;
            Ok(LeaseExecution {
                run_ref: child_run_ref(&c.source_url),
                managed_state_root: None,
                dispatch_mode,
            })
        }
        RUN_CAPSULE_LEASE_KIND => {
            let c = parse_run_capsule_command(command)?;
            // Persistent state is ALWAYS keyed on the immutable `run_ref`
            // identity — whether we execute that ref directly or a materialized
            // inline recipe — so re-runs reuse the same state regardless of the
            // ephemeral recipe dir.
            let root = run_capsule_state_root(&c);
            let run_ref = match c.recipe_toml.as_deref() {
                Some(toml) => materialize_inline_recipe(recipe_dir, toml)?,
                None => validate_capsule_run_ref(&c.run_ref)?,
            };
            Ok(LeaseExecution {
                run_ref,
                managed_state_root: Some(root),
                dispatch_mode,
            })
        }
        other => Err((
            "unsupported_command".to_string(),
            format!(
                "unsupported lease command kind {other:?}; this runner executes {SUPPORTED_LEASE_KINDS:?}"
            ),
        )),
    }
}

/// Materialize an inline recipe `capsule.toml` into `recipe_dir` and return that
/// dir as the `ato run` positional. Community Store recipes have no installable
/// release (`ato run <run_ref>` → "no installable version"), so the control
/// plane ships the recipe TOML inline; the runner runs the materialized dir
/// while persistent state stays keyed on the lease's immutable `run_ref`.
fn materialize_inline_recipe(
    recipe_dir: &Path,
    recipe_toml: &str,
) -> std::result::Result<String, (String, String)> {
    std::fs::create_dir_all(recipe_dir).map_err(|e| {
        (
            "inline_recipe_write_failed".to_string(),
            format!("failed to create recipe dir {}: {e}", recipe_dir.display()),
        )
    })?;
    let toml_path = recipe_dir.join("capsule.toml");
    std::fs::write(&toml_path, recipe_toml).map_err(|e| {
        (
            "inline_recipe_write_failed".to_string(),
            format!("failed to write {}: {e}", toml_path.display()),
        )
    })?;
    Ok(recipe_dir.to_string_lossy().into_owned())
}

/// Per-lease directory for a materialized inline recipe. MUST live OUTSIDE
/// `~/.ato/` — `ato run <dir>` rejects a run target inside Ato's internal state
/// directory ("cannot be used as a run target"). Use a per-lease temp dir.
fn inline_recipe_dir(lease_id: &str) -> PathBuf {
    std::env::temp_dir()
        .join("ato-runner-recipes")
        .join(format!("{lease_id}-recipe"))
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
    // Set when a ready arrives before any receipt: receipts (stderr) and
    // lifecycle lines (stdout) race across the two stream readers, so the
    // ready is buffered (carrying its observed port) until the receipt lands
    // or the grace below elapses.
    let mut ready_awaiting_receipt: Option<Option<u16>> = None;
    // If the receipt does not follow the buffered ready before this instant,
    // the ready is unverifiable — fail closed.
    let mut receipt_wait_deadline: Option<tokio::time::Instant> = None;
    // Set when a port-LESS ready arrives; if the canonical port-bearing line
    // does not follow before this instant, settle without a port.
    let mut portless_ready_deadline: Option<tokio::time::Instant> = None;
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        tokio::select! {
            line = line_rx.recv() => {
                let Some(line) = line else {
                    // Output streams closed; wait for the exit status.
                    let status = child.wait().await.ok();
                    if !settled {
                        // A port-less ready was pending (grace not yet elapsed) when
                        // the child's streams closed — e.g. a ready-then-exit child.
                        // Honor it as ready (port-less), not a failure: we DID see a
                        // verified ready signal.
                        if let Some(execution_id) =
                            portless_ready_deadline.and(execution_id.clone())
                        {
                            let _ = reports.send(LeaseReport::Ready {
                                execution_id,
                                port: None,
                            });
                            return;
                        }
                        // A ready was still buffered awaiting its receipt when
                        // the streams closed — the receipt can no longer
                        // arrive, so the ready is unverifiable; fail closed.
                        if ready_awaiting_receipt.is_some() {
                            let _ = reports.send(LeaseReport::Failed {
                                code: "execution_id_unavailable".to_string(),
                                message: "child reported ready but no execution receipt was observed".to_string(),
                            });
                            return;
                        }
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
                        // A ready that outran this receipt across the
                        // stdout/stderr split was buffered — resolve it now
                        // exactly as if it had arrived after the receipt.
                        if !settled
                            && let (Some(execution_id), Some(port)) =
                                (execution_id.clone(), ready_awaiting_receipt)
                        {
                            ready_awaiting_receipt = None;
                            receipt_wait_deadline = None;
                            if port.is_some() {
                                settled = true;
                                let _ = reports.send(LeaseReport::Ready { execution_id, port });
                            } else if portless_ready_deadline.is_none() {
                                portless_ready_deadline =
                                    Some(tokio::time::Instant::now() + READY_PORT_GRACE);
                            }
                        }
                    }
                    Some(ChildSignal::Ready { port }) if !settled => {
                        match (execution_id.clone(), port) {
                            (Some(execution_id), Some(_)) => {
                                // Verified ready WITH an observed port — the best
                                // signal; settle and let the proxy + ready_url come up.
                                settled = true;
                                let _ = reports.send(LeaseReport::Ready { execution_id, port });
                            }
                            (Some(_), None) => {
                                // Verified ready but no port (the human "[✓] ready"
                                // echo). The canonical "LIFECYCLE: ready port=N" line
                                // races in on a separate stream — hold briefly for it
                                // rather than settle portless and drop the ready_url.
                                if portless_ready_deadline.is_none() {
                                    portless_ready_deadline =
                                        Some(tokio::time::Instant::now() + READY_PORT_GRACE);
                                }
                            }
                            (None, _) => {
                                // No receipt observed YET — but the receipt
                                // (stderr) and this line (stdout) ride separate
                                // streams merged in arrival order, so this ready
                                // may simply have outrun its receipt. Buffer it
                                // for a bounded window instead of killing a
                                // healthy workload; fail closed only if no
                                // receipt follows (grace arm below).
                                ready_awaiting_receipt =
                                    Some(port.or(ready_awaiting_receipt.flatten()));
                                if receipt_wait_deadline.is_none() {
                                    receipt_wait_deadline =
                                        Some(tokio::time::Instant::now() + READY_RECEIPT_GRACE);
                                }
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
                    Some(ChildSignal::ConsentRequired(request)) if !settled => {
                        // The child gated at E302 and exits now. Surface the gate
                        // and stop monitoring — the orchestrator parks needs_consent
                        // and (on approval) re-spawns. Returning here (rather than
                        // setting `settled`) skips the exit_before_ready path the
                        // closing streams would otherwise take.
                        let _ = reports.send(LeaseReport::ConsentRequired(request));
                        return;
                    }
                    _ => {}
                }
            }
            // Receipt grace elapsed with a ready still buffered — a ready we
            // cannot tie to an execution receipt is unverifiable; fail closed
            // rather than report it. Pending forever when unset.
            _ = async {
                match receipt_wait_deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending::<()>().await,
                }
            }, if !settled => {
                settled = true;
                let _ = reports.send(LeaseReport::Failed {
                    code: "execution_id_unavailable".to_string(),
                    message: "child reported ready but no execution receipt was observed".to_string(),
                });
                let _ = child.start_kill();
            }
            // Port-less ready grace elapsed without a port line — settle without a
            // port (honest ready, no ready_url). Pending forever when unset.
            _ = async {
                match portless_ready_deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending::<()>().await,
                }
            }, if !settled => {
                if let Some(execution_id) = execution_id.clone() {
                    settled = true;
                    let _ = reports.send(LeaseReport::Ready { execution_id, port: None });
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

/// True while the restored VMM process is still alive (`kill(pid, 0)` probes without
/// delivering a signal). Unlike the child-run path this is a single process pid, not a
/// process group.
#[cfg(unix)]
fn vmm_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
#[cfg(not(unix))]
fn vmm_alive(_pid: i32) -> bool {
    true
}

/// How often the restore hold-loop polls `/control` for a stop + checks the VMM is alive.
const RESTORE_HOLD_POLL_SECS: u64 = 2;

/// Track E (#912): restore a sealed Ready-State snapshot for a `restore_snapshot` lease
/// (Track D dispatch, ato-api#159) and expose it — owning the full
/// fetch → VERIFY → restore → proxy → ready → hold → teardown lifecycle.
///
/// The lease is REFERENCE-ONLY: `artifact_location` is a hint, and the identity fields
/// are verified against the fetched manifest by `load_and_verify_manifest` (incl. the
/// `manifest.id() == artifact_manifest_hash` gate that `backend.restore` does not
/// provide) BEFORE anything is restored. Every failure reports a typed `Failed` and
/// releases the slot; no secret is ever consumed (a no-binding artifact only).
#[allow(clippy::too_many_arguments)]
async fn handle_restore_snapshot_lease(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    lease: LeaseDto,
    slot: SlotLease,
    public_base_url: Option<String>,
    public_url_template: Option<String>,
) {
    use crate::application::ready_state::backend::select_backend;
    use crate::application::ready_state::restore::{restore_and_expose, teardown};
    use crate::application::ready_state::restore_lease::{
        load_and_verify_manifest, locate_artifact, parse_restore_snapshot_command,
    };
    use capsulefs::CasStore;

    let lease_id = lease.id.clone();

    // ── Track R1 (ato#948): restore-prep phase profiling ────────────────────
    // The measured claimed→firecracker gap (~1.7s) hid inside this handler.
    // Every phase is timed and emitted as ONE stable key=value line
    // (`RESTORE_PROF …`) on the success path — ids only, never secrets/URLs.
    // `spans=` carries the snapshot backend's internal bench spans
    // (rehydrate/cache/spawn/health…), populated when ATO_READY_STATE_BENCH=1.
    let prof_total = std::time::Instant::now();
    let mut prof_last = std::time::Instant::now();
    let mut prof_parts: Vec<String> = Vec::new();
    macro_rules! prof_mark {
        ($name:literal) => {{
            prof_parts.push(format!(concat!($name, "={}"), prof_last.elapsed().as_millis()));
            prof_last = std::time::Instant::now();
        }};
    }

    // Report a typed failure and release the slot (fail-closed on every reject path).
    async fn fail(
        client: &reqwest::Client,
        api_base: &str,
        runner_token: &str,
        lease_id: &str,
        slot: SlotLease,
        code: &str,
        message: String,
    ) {
        eprintln!("⚠️  restore lease {lease_id} rejected: {}", scrub_secrets(&message));
        let report = LeaseReport::Failed { code: code.to_string(), message };
        if let Err(err) = report_lease_status(client, api_base, runner_token, lease_id, &report).await
        {
            eprintln!(
                "⚠️  restore lease {lease_id}: failure report failed: {}",
                scrub_secrets(&format!("{err:#}"))
            );
        }
        slot.release();
    }

    // 1. Parse the reference-only command (every identity field required + non-empty).
    let cmd = match parse_restore_snapshot_command(&lease.command) {
        Ok(c) => c,
        Err((code, message)) => {
            fail(client, api_base, runner_token, &lease_id, slot, &code, message).await;
            return;
        }
    };
    println!(
        "📦 restore lease {lease_id}: snapshot {} (capsule {}, {}/{})",
        cmd.snapshot_id, cmd.capsule_id, cmd.target_label, cmd.profile
    );
    prof_mark!("parse_ms");
    // Track R3 (ato#948): the Preparing report is a PROGRESS HINT, not a
    // terminal contract — R1 measured it at p50 ~1.1s of pure control-plane
    // round-trip sitting on the restore critical path. Fire it in the
    // background (ONE task per restore, no retry loop) and start the restore
    // immediately. Ready/Failed reporting stays awaited — the terminal
    // contract is unchanged.
    {
        let client = client.clone();
        let api_base = api_base.to_string();
        let runner_token = runner_token.to_string();
        let lease_id = lease_id.clone();
        tokio::spawn(async move {
            if let Err(err) = report_lease_status(
                &client,
                &api_base,
                &runner_token,
                &lease_id,
                &LeaseReport::Preparing,
            )
            .await
            {
                eprintln!(
                    "⚠️  restore lease {lease_id}: preparing report failed (non-fatal): {}",
                    scrub_secrets(&format!("{err:#}"))
                );
            }
        });
    }
    prof_mark!("report_preparing_spawn_ms");

    // 2. Locate the fetched artifact on this host (v1 same-host CAS, ato#928 layout).
    let artifact_root = match std::env::var("ATO_SNAPSHOT_ARTIFACT_ROOT") {
        Ok(v) if !v.trim().is_empty() => std::path::PathBuf::from(v),
        _ => {
            fail(
                client,
                api_base,
                runner_token,
                &lease_id,
                slot,
                "artifact_unavailable",
                "ATO_SNAPSHOT_ARTIFACT_ROOT is not configured on this runner".to_string(),
            )
            .await;
            return;
        }
    };
    let paths = match locate_artifact(&cmd.artifact_location, &artifact_root) {
        Ok(p) => p,
        Err((code, message)) => {
            fail(client, api_base, runner_token, &lease_id, slot, &code, message).await;
            return;
        }
    };
    prof_mark!("locate_artifact_ms");

    // 3. VERIFY the fetched manifest IS exactly the sealed artifact (integrity gate).
    let manifest = match load_and_verify_manifest(&paths.manifest_json, &cmd) {
        Ok(m) => m,
        Err((code, message)) => {
            fail(client, api_base, runner_token, &lease_id, slot, &code, message).await;
            return;
        }
    };
    prof_mark!("verify_manifest_ms");

    // 4. Open the artifact's CAS + select the host backend (both fail-closed).
    let store = match CasStore::open(&paths.cas_dir) {
        Ok(s) => s,
        Err(e) => {
            fail(
                client,
                api_base,
                runner_token,
                &lease_id,
                slot,
                "artifact_unavailable",
                format!("open CAS: {e:#}"),
            )
            .await;
            return;
        }
    };
    let backend = match select_backend() {
        Ok(b) => b,
        Err(e) => {
            fail(
                client,
                api_base,
                runner_token,
                &lease_id,
                slot,
                "backend_unavailable",
                format!("{e:#}"),
            )
            .await;
            return;
        }
    };
    prof_mark!("cas_open_backend_select_ms");

    // 5. Restore + expose. The disposable overlay is destroyed on teardown. host_runner
    // class stays None so `backend.restore` re-gates the manifest's runner class against
    // THIS host (fail-closed, defence in depth over the pre-restore verify).
    let overlay_root = std::env::temp_dir()
        .join("ato-restore-overlays")
        .join(&lease_id);
    // Drain any stale spans so `spans=` below carries THIS restore only.
    let _ = snapshot::bench::drain();
    let receipt =
        match restore_and_expose(backend.as_ref(), &store, manifest, overlay_root, None, false) {
            Ok(r) => r,
            Err(e) => {
                fail(
                    client,
                    api_base,
                    runner_token,
                    &lease_id,
                    slot,
                    "restore_failed",
                    format!("{e:#}"),
                )
                .await;
                return;
            }
        };
    prof_mark!("backend_restore_ms");
    let prof_spans: Vec<String> = snapshot::bench::drain()
        .into_iter()
        .map(|s| format!("{}={}ms", s.name, s.micros / 1000))
        .collect();
    let session = receipt.session;

    let Some(guest_port) = session.guest_port else {
        // Nothing to expose (e.g. a Fake/KVM-free backend) — a public run needs a served
        // port. Tear the session down and fail rather than report a portless ready.
        let _ = teardown(backend.as_ref(), session);
        fail(
            client,
            api_base,
            runner_token,
            &lease_id,
            slot,
            "restore_no_port",
            "restored session exposed no guest port; this runner cannot serve it".to_string(),
        )
        .await;
        return;
    };
    // Where the restored workload ACTUALLY accepts connections — backend-authoritative
    // (Firecracker serves on the TAP guest IP, e.g. 172.16.0.2:8080, not host loopback).
    // Missing addr ⇒ nothing to honestly proxy; ready is reported without a URL below.
    let workload_addr = session.workload_addr.clone();

    // 6. Bring the per-slot root proxy up BEFORE claiming a URL — a proxy that failed (or
    // a slot with no URL to claim, or a session with no dialable workload address)
    // reports ready WITHOUT a fabricated ready_url.
    let candidate = public_ready_url(
        public_base_url.as_deref(),
        public_url_template.as_deref(),
        &slot,
    );
    let (proxy_handle, proxy_started) = match (candidate.as_deref(), workload_addr.as_deref()) {
        (Some(_), Some(addr)) => match start_root_proxy_to(&slot.proxy_listen, addr.to_string()).await {
            Ok(handle) => {
                println!(
                    "🔀 restore lease {lease_id}: slot {} proxy {} -> {}",
                    slot.index, slot.proxy_listen, addr
                );
                (Some(handle), Some(true))
            }
            Err(err) => {
                eprintln!(
                    "⚠️  restore lease {lease_id}: proxy failed; reporting ready WITHOUT ready_url: {}",
                    scrub_secrets(&format!("{err:#}"))
                );
                (None, Some(false))
            }
        },
        (Some(_), None) => {
            eprintln!(
                "⚠️  restore lease {lease_id}: session reported no workload address; reporting ready WITHOUT ready_url"
            );
            (None, Some(false))
        }
        _ => (None, None),
    };
    let payload = decide_ready_payload(
        cmd.execution_id.clone(),
        candidate.as_deref(),
        Some(guest_port),
        proxy_started,
    );
    prof_mark!("proxy_setup_ms");
    println!(
        "📨 restore lease {lease_id}: ready ({}, ready_url={})",
        payload.execution_id,
        payload.ready_url.as_deref().unwrap_or("none")
    );
    if let Err(err) = report_lease_ready(client, api_base, runner_token, &lease_id, &payload).await {
        eprintln!(
            "⚠️  restore lease {lease_id}: ready report failed: {}",
            scrub_secrets(&format!("{err:#}"))
        );
    }
    prof_mark!("ready_ack_ms");
    // Track R1 summary — one stable line per successful restore (grep: RESTORE_PROF).
    println!(
        "RESTORE_PROF lease={lease_id} snapshot={} {} total_ms={} spans=\"{}\"",
        cmd.snapshot_id,
        prof_parts.join(" "),
        prof_total.elapsed().as_millis(),
        prof_spans.join(";"),
    );

    // 7. Hold until the owner stops the run (/control) or the VMM exits.
    let control_url = format!(
        "{}/v1/runner-leases/{}/control",
        api_base.trim_end_matches('/'),
        &lease_id
    );
    let reason = loop {
        tokio::time::sleep(Duration::from_secs(RESTORE_HOLD_POLL_SECS)).await;
        if let Some(pid) = session.vmm_pid
            && !vmm_alive(pid)
        {
            break "workload_exited";
        }
        match poll_control_once(client, &control_url, runner_token).await {
            ControlOutcome::Stop => break "user_requested",
            ControlOutcome::Done => break "lease_gone",
            ControlOutcome::Continue => {}
        }
    };
    println!("🛑 restore lease {lease_id}: {reason}; tearing down");

    // 8. Teardown: stop the VM + destroy the overlay, stop the proxy, ack /stopped, and
    // free the slot ONLY on a fully confirmed teardown (fail closed — a slot held is
    // safer than one offered while a VM may still be up).
    let vm_stopped = match teardown(backend.as_ref(), session) {
        Ok(_) => true,
        Err(e) => {
            eprintln!(
                "⚠️  restore lease {lease_id}: backend stop failed: {}",
                scrub_secrets(&format!("{e:#}"))
            );
            false
        }
    };
    let proxy_stopped = stop_proxy(proxy_handle).await;
    let cleanup = StopCleanup::from_teardown(vm_stopped, proxy_stopped);
    if let Err(err) =
        report_lease_stopped_with_reason(client, api_base, runner_token, &lease_id, &cleanup, reason)
            .await
    {
        eprintln!(
            "⚠️  restore lease {lease_id}: stopped ack failed: {}",
            scrub_secrets(&format!("{err:#}"))
        );
    }
    if cleanup.slot_released {
        slot.release();
        println!("🔓 restore lease {lease_id}: VM stopped, proxy down, slot released");
    } else {
        eprintln!(
            "⚠️  restore lease {lease_id}: teardown incomplete (vm_stopped={}, proxy_stopped={}); slot held",
            cleanup.process_terminated, cleanup.proxy_stopped
        );
    }
}

async fn handle_claimed_lease(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    lease: LeaseDto,
    slot: SlotLease,
    public_base_url: Option<String>,
    public_url_template: Option<String>,
) {
    println!("📦 lease {} claimed (run {})", lease.id, lease.run_id);
    // Track E (#912): a restore_snapshot lease restores a sealed Ready-State snapshot
    // (Track D dispatch, ato-api#159). It is NOT a child-process sandbox run, so it owns
    // its own fetch → verify → restore → expose → teardown lifecycle — routed here,
    // before the run_source/run_capsule machinery that would reject the kind.
    if lease.command.get("kind").and_then(|v| v.as_str())
        == Some(crate::application::ready_state::restore_lease::RESTORE_SNAPSHOT_LEASE_KIND)
    {
        handle_restore_snapshot_lease(
            client,
            api_base,
            runner_token,
            lease,
            slot,
            public_base_url,
            public_url_template,
        )
        .await;
        return;
    }
    // Fail closed FIRST — before resolving/materializing the lease or reporting
    // Preparing: a native-inference lease must only run where this host can
    // actually run native-inference. The control plane already capability-gates
    // dispatch; this re-check is defence in depth, so a mis-dispatched
    // native-inference lease is rejected with NO side effects (no recipe written,
    // no Preparing churn) and is never forced into the sandbox.
    if let Err((code, message)) = ensure_dispatch_supported(
        RunnerDispatchMode::from_command(&lease.command),
        native_inference_ready(),
    ) {
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
        slot.release();
        return;
    }
    let execution = match resolve_lease_execution(&lease.command, &inline_recipe_dir(&lease.id)) {
        Ok(execution) => execution,
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
            // Release the slot the serve loop acquired — SlotLease has no Drop, so
            // an early return otherwise strands it occupied (pre-existing on this
            // reject path; release() is idempotent).
            slot.release();
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

    let log_path = run_log_path(&lease.id);

    // The slot was acquired by the serve loop before dispatch and is held for
    // the whole gated lifetime (preparing → optional needs_consent → running),
    // released only on settle / stop / terminal.
    let client = client.clone();
    let api_base = api_base.to_string();
    let runner_token = runner_token.to_string();
    let lease_id = lease.id.clone();
    let LeaseExecution {
        run_ref,
        managed_state_root,
        dispatch_mode,
    } = execution;
    tokio::spawn(async move {
        // (native-inference dispatch capability was already fail-closed checked in
        // handle_claimed_lease before resolve/Preparing — see ensure_dispatch_supported.)
        // One control watcher for the whole lease lifetime: it flips stop_flag +
        // notifies on an owner stop in EITHER phase (needs_consent or running).
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_notify = Arc::new(tokio::sync::Notify::new());
        let control = tokio::spawn({
            let client = client.clone();
            let api_base = api_base.clone();
            let runner_token = runner_token.clone();
            let lease_id = lease_id.clone();
            let stop_flag = Arc::clone(&stop_flag);
            let stop_notify = Arc::clone(&stop_notify);
            async move {
                poll_lease_control(
                    &client,
                    &api_base,
                    &runner_token,
                    &lease_id,
                    stop_flag,
                    stop_notify,
                )
                .await;
            }
        });

        // ── Consent gate (P4-A): spawn `ato run`; if it gates at E302, park
        // needs_consent, wait for the owner decision, verify the exact key,
        // record locally, and retry. The loop yields the run-phase handoff
        // (report channel + monitor + first decisive report) once the child
        // runs past the gate; terminal/stop outcomes return from the task. ──
        let mut round: u32 = 0;
        let (mut report_rx, monitor, child_pid, first_report) = loop {
            round += 1;
            let child =
                match spawn_run_child(&run_ref, managed_state_root.as_deref(), dispatch_mode) {
                    Ok(child) => child,
                    Err(err) => {
                        let report = LeaseReport::Failed {
                            code: "spawn_failed".to_string(),
                            message: format!("{err:#}"),
                        };
                        let _ = report_lease_status(
                            &client,
                            &api_base,
                            &runner_token,
                            &lease_id,
                            &report,
                        )
                        .await;
                        control.abort();
                        slot.release();
                        return;
                    }
                };
            // PID == process-group id (see spawn_run_child); a stop signals the
            // whole workload group.
            let child_pid = child.id();
            // Persist the group id BEFORE driving the child: if this process
            // is hard-killed from here on, the next serve's reconcile must
            // probe the possible survivor instead of fabricating teardown
            // evidence (#645).
            record_workload_group(&lease_id, child_pid);
            println!(
                "🚀 lease {lease_id}: ato run {run_ref} --sandbox (round {round}, log: {})",
                log_path.display()
            );
            let (report_tx, mut report_rx) = tokio::sync::mpsc::unbounded_channel::<LeaseReport>();
            let monitor = tokio::spawn(run_lease_child(
                child,
                log_path.clone(),
                ready_timeout(),
                report_tx,
            ));

            // Wait for the first decisive signal, honoring a stop during
            // preparing/gate.
            let first = tokio::select! {
                biased;
                _ = stop_notify.notified() => {
                    perform_stop_cleanup(
                        &client, &api_base, &runner_token, &lease_id,
                        child_pid, monitor, None, &slot,
                    )
                    .await;
                    control.abort();
                    return;
                }
                maybe = report_rx.recv() => maybe,
            };
            let Some(first) = first else {
                // Monitor closed without a decisive report (run_lease_child always
                // reports before returning, so this is defensive). Idle out.
                let _ = monitor.await;
                clear_workload_group(&lease_id);
                control.abort();
                slot.release();
                return;
            };

            let LeaseReport::ConsentRequired(request) = first else {
                // Running / Ready / Failed: the child is past the gate — hand off
                // to the run phase with the still-running monitor.
                break (report_rx, monitor, child_pid, first);
            };

            // The child exited at the gate; reap its monitor before waiting.
            let _ = monitor.await;
            clear_workload_group(&lease_id);
            if round > MAX_CONSENT_ROUNDS {
                // Bounded: the gate kept re-emitting (a persistent recompute
                // mismatch, or a local approval that never clears it). Fail
                // closed and release the slot rather than loop forever.
                eprintln!(
                    "⚠️  lease {lease_id}: consent gate did not clear in {MAX_CONSENT_ROUNDS} rounds"
                );
                let report = LeaseReport::Failed {
                    code: "consent_retry_exhausted".to_string(),
                    message: format!(
                        "consent gate did not clear within {MAX_CONSENT_ROUNDS} rounds"
                    ),
                };
                let _ = report_lease_status(&client, &api_base, &runner_token, &lease_id, &report)
                    .await;
                control.abort();
                slot.release();
                println!("🔓 lease {lease_id}: slot released");
                return;
            }
            // Park needs_consent with the full 5-tuple for owner approval.
            if let Err(err) =
                report_consent_required(&client, &api_base, &runner_token, &lease_id, &request)
                    .await
            {
                eprintln!(
                    "⚠️  lease {lease_id}: consent-required report failed: {}",
                    scrub_secrets(&format!("{err:#}"))
                );
                let report = LeaseReport::Failed {
                    code: "consent_report_failed".to_string(),
                    message: "could not surface the consent gate to the control plane".to_string(),
                };
                let _ = report_lease_status(&client, &api_base, &runner_token, &lease_id, &report)
                    .await;
                control.abort();
                slot.release();
                return;
            }
            println!(
                "⏸  lease {lease_id}: needs consent ({})",
                request.consent_ref
            );

            // Wait for the owner decision; a stop during the wait wins.
            let outcome = tokio::select! {
                biased;
                _ = stop_notify.notified() => ConsentOutcome::Stop,
                decision = poll_consent_decision(&client, &api_base, &runner_token, &lease_id) => decision,
            };
            let ConsentOutcome::Approved {
                consent_ref: approved_ref,
            } = outcome
            else {
                // Rejected / Expired / Gone / Stop: the API has already settled
                // the run terminal (or cancelled it on stop). The child exited at
                // the gate, so no workload/proxy exists — releasing the slot
                // (busy=false) IS the honest teardown; there is nothing to /stop.
                match outcome {
                    ConsentOutcome::Rejected => {
                        println!("🚫 lease {lease_id}: consent rejected by owner")
                    }
                    ConsentOutcome::Expired => {
                        println!("⌛ lease {lease_id}: consent timed out")
                    }
                    ConsentOutcome::Stop => {
                        println!("🛑 lease {lease_id}: stopped by owner during consent")
                    }
                    _ => {}
                }
                control.abort();
                slot.release();
                println!("🔓 lease {lease_id}: slot released (no workload was running)");
                return;
            };

            // INVARIANT: write the host-local ledger ONLY when the child's
            // emitted ref, the recomputed ref (from the child's 5-tuple), and the
            // owner's approved ref ALL agree. A recompute error is unrecoverable
            // (cannot verify) → fail closed.
            let recomputed = match capsule::execution_plan::canonical::consent_ref_from_parts(
                &request.identity.scoped_id,
                &request.identity.version,
                &request.identity.target_label,
                &request.identity.policy_segment_hash,
                &request.identity.provisioning_policy_hash,
            ) {
                Ok(value) => value,
                Err(err) => {
                    eprintln!("⚠️  lease {lease_id}: consent_ref recompute failed: {err}");
                    let report = LeaseReport::Failed {
                        code: "consent_verification_failed".to_string(),
                        message: "could not recompute the consent reference".to_string(),
                    };
                    let _ =
                        report_lease_status(&client, &api_base, &runner_token, &lease_id, &report)
                            .await;
                    control.abort();
                    slot.release();
                    println!("🔓 lease {lease_id}: slot released");
                    return;
                }
            };
            if consent_verify_action(&request.consent_ref, &recomputed, &approved_ref)
                == ConsentVerifyAction::ReEmit
            {
                // The child / recomputed / approved refs disagree. Do NOT write
                // the ledger. Re-emit needs_consent: the next spawn re-gates and
                // re-reports, superseding (voiding) the stale approval on the API.
                // A persistent mismatch is bounded by MAX_CONSENT_ROUNDS above.
                eprintln!(
                    "⚠️  lease {lease_id}: consent_ref mismatch (child/recomputed/approved disagree); re-emitting needs_consent, old approval void"
                );
                continue;
            }
            // Verified: append to THIS host's local ledger, then retry. A ledger
            // write failure is terminal — without the local record the child just
            // re-gates, so fail closed and DO NOT retry. Do NOT re-report
            // `preparing` — needs_consent → preparing is a backward transition the
            // API refuses; the retried child's `running` report is the valid move.
            if let Err(err) =
                crate::application::auth::consent_store::approve_execution_plan_consent(
                    &request.identity.scoped_id,
                    &request.identity.version,
                    &request.identity.target_label,
                    &request.identity.policy_segment_hash,
                    &request.identity.provisioning_policy_hash,
                )
            {
                eprintln!("⚠️  lease {lease_id}: local consent record failed: {err}");
                let report = LeaseReport::Failed {
                    code: "consent_local_record_failed".to_string(),
                    message: "could not record the approved consent locally".to_string(),
                };
                let _ = report_lease_status(&client, &api_base, &runner_token, &lease_id, &report)
                    .await;
                control.abort();
                slot.release();
                println!("🔓 lease {lease_id}: slot released");
                return;
            }
            println!("✅ lease {lease_id}: consent approved + recorded locally; retrying run");
        };

        // ── Run phase: the child is past the gate. Seed the first decisive
        // report, then drive readiness/stop exactly as before. ──
        let mut proxy_handle: Option<tokio::task::JoinHandle<()>> = None;
        let mut stopping = false;
        let mut pending: Option<LeaseReport> = Some(first_report);
        loop {
            let report = match pending.take() {
                Some(report) => report,
                None => tokio::select! {
                    biased;
                    _ = stop_notify.notified() => {
                        stopping = true;
                        break;
                    }
                    maybe = report_rx.recv() => match maybe {
                        Some(report) => report,
                        None => break,
                    },
                },
            };
            // A stop that landed between ticks (flag set while a report was in
            // flight): skip terminal churn and go straight to teardown so the
            // run settles as stopped, not failed.
            if stop_flag.load(Ordering::SeqCst) {
                stopping = true;
                break;
            }
            match report {
                LeaseReport::Ready { execution_id, port } => {
                    // The honest public URL for THIS slot (None for a non-zero
                    // slot without a template — never fabricated).
                    let candidate = public_ready_url(
                        public_base_url.as_deref(),
                        public_url_template.as_deref(),
                        &slot,
                    );
                    // Bring the per-slot root proxy up BEFORE claiming a URL; a
                    // proxy that failed (or was never attempted because there is
                    // no URL to claim) means ready is reported without ready_url
                    // — never a fabricated one.
                    let proxy_started = match (candidate.as_deref(), port) {
                        (Some(_), Some(workload_port)) => {
                            match start_root_proxy(&slot.proxy_listen, workload_port).await {
                                Ok(handle) => {
                                    proxy_handle = Some(handle);
                                    println!(
                                        "🔀 lease {lease_id}: slot {} proxy {} -> 127.0.0.1:{}",
                                        slot.index, slot.proxy_listen, workload_port
                                    );
                                    Some(true)
                                }
                                Err(err) => {
                                    eprintln!(
                                        "⚠️  lease {lease_id}: proxy failed; reporting ready WITHOUT ready_url: {}",
                                        scrub_secrets(&format!("{err:#}"))
                                    );
                                    Some(false)
                                }
                            }
                        }
                        _ => None,
                    };
                    let payload = decide_ready_payload(
                        execution_id,
                        candidate.as_deref(),
                        port,
                        proxy_started,
                    );
                    println!(
                        "📨 lease {lease_id}: ready ({}, ready_url={})",
                        payload.execution_id,
                        payload.ready_url.as_deref().unwrap_or("none")
                    );
                    if let Err(err) =
                        report_lease_ready(&client, &api_base, &runner_token, &lease_id, &payload)
                            .await
                    {
                        eprintln!(
                            "⚠️  lease {lease_id}: ready report failed: {}",
                            scrub_secrets(&format!("{err:#}"))
                        );
                    }
                }
                other => {
                    let label = match &other {
                        LeaseReport::Preparing => "preparing".to_string(),
                        LeaseReport::Running => {
                            "running (launched, readiness not confirmed)".to_string()
                        }
                        LeaseReport::Failed { code, .. } => format!("failed ({code})"),
                        LeaseReport::Ready { .. } => unreachable!(),
                        LeaseReport::ConsentRequired(_) => {
                            unreachable!("consent gates are handled before the run phase")
                        }
                    };
                    println!("📨 lease {lease_id}: {label}");
                    if let Err(err) =
                        report_lease_status(&client, &api_base, &runner_token, &lease_id, &other)
                            .await
                    {
                        eprintln!(
                            "⚠️  lease {lease_id}: report failed: {}",
                            scrub_secrets(&format!("{err:#}"))
                        );
                    }
                }
            }
        }
        if stopping {
            perform_stop_cleanup(
                &client,
                &api_base,
                &runner_token,
                &lease_id,
                child_pid,
                monitor,
                proxy_handle,
                &slot,
            )
            .await;
        } else {
            // Natural settle/exit: the child ran to completion on its own.
            let _ = monitor.await;
            clear_workload_group(&lease_id);
            if let Some(handle) = proxy_handle {
                handle.abort();
            }
            slot.release();
            println!("📦 lease {lease_id}: child exited; runner is idle again");
        }
        control.abort();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn lease_poll_interval_is_env_tunable_clamped_and_defaults_conservative() {
        // Default (no env / garbage): the conservative 5s stays.
        assert_eq!(lease_poll_seconds_from(None), 5);
        assert_eq!(lease_poll_seconds_from(Some("")), 5);
        assert_eq!(lease_poll_seconds_from(Some("fast")), 5);
        assert_eq!(lease_poll_seconds_from(Some("-1")), 5);
        // Staging latency override (ato#940 Track L): 1s.
        assert_eq!(lease_poll_seconds_from(Some("1")), 1);
        assert_eq!(lease_poll_seconds_from(Some(" 2 ")), 2);
        // Clamped: never sub-second hammering, never minute-scale starvation.
        assert_eq!(lease_poll_seconds_from(Some("0")), 1);
        assert_eq!(lease_poll_seconds_from(Some("999")), 60);
    }

    #[test]
    fn build_enroll_body_carries_token_and_honest_host_facts() {
        let caps = vec!["linux".to_string(), "sandbox=linux-bwrap".to_string()];
        let body = build_enroll_body("ato_enr_secret", "my-runner", &caps);
        assert_eq!(body["enrollment_token"], "ato_enr_secret");
        assert_eq!(body["display_name"], "my-runner");
        assert_eq!(body["os"], std::env::consts::OS);
        assert_eq!(body["arch"], std::env::consts::ARCH);
        assert_eq!(body["capabilities"][1], "sandbox=linux-bwrap");
    }

    #[test]
    fn enroll_failure_message_is_typed_and_never_leaks_the_token() {
        // Typed API error → actionable message surfaced verbatim.
        let m = enroll_failure_message(
            410,
            r#"{"error":"expired","message":"Enrollment token has expired."}"#,
        );
        assert!(m.contains("410"));
        assert!(m.contains("Enrollment token has expired."));
        // A body that somehow reflected the raw token must never surface it:
        // only the server's typed fields are used, never the raw body.
        let leaky = "ato_enr_should_never_appear_in_logs";
        let m2 = enroll_failure_message(401, leaky);
        assert!(!m2.contains("ato_enr_should_never_appear_in_logs"));
    }

    #[tokio::test]
    async fn headless_enrollment_persists_runner_token_through_the_existing_store() {
        // Mock control plane: POST /v1/runners/enroll → 201 with the enrolled
        // runner + its runner token (the exact shape POST /v1/runners returns,
        // plus an extra lease_id the CLI ignores).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock api");
        let port = listener.local_addr().expect("addr").port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                let body = r#"{"runner":{"id":"01HOSTED","display_name":"Managed microvm-burst"},"runner_token":"ato_rnr_returned-secret","lease_id":"01LEASE","heartbeat":{"interval_seconds":30}}"#;
                let response = format!(
                    "HTTP/1.1 201 Created\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let api_base = format!("http://127.0.0.1:{port}");
        let creds = enroll_for_credentials(
            &api_base,
            "Managed microvm-burst",
            "ato_enr_single-use-secret".to_string(),
        )
        .await
        .expect("headless enrollment succeeds");
        server.join().ok();

        // Same credential shape device-flow login produces — readable by serve.
        assert_eq!(creds.runner_id, "01HOSTED");
        assert_eq!(creds.runner_token, "ato_rnr_returned-secret");
        assert_eq!(creds.api_base, api_base);
        assert_eq!(creds.heartbeat_interval_seconds, 30);

        // Persist through the EXISTING store + reader the serve loop uses.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("runner").join("credentials.json");
        save_credentials(&path, &creds).expect("save");
        let loaded = load_credentials(&path).expect("load");
        assert_eq!(loaded.runner_token, "ato_rnr_returned-secret");

        // The single-use enrollment token must NEVER be written to disk.
        let on_disk = std::fs::read_to_string(&path).expect("read creds");
        assert!(
            !on_disk.contains("ato_enr_single-use-secret"),
            "enrollment token must not be persisted"
        );
    }

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
        let without = build_heartbeat_body(&caps, None, "linux", "aarch64", 1, 0);
        assert!(
            without.get("public_base_url").is_none(),
            "absent public_base_url must not be sent (null would clear it server-side)"
        );
        let with = build_heartbeat_body(
            &caps,
            Some("https://oci-a1.example.com"),
            "linux",
            "aarch64",
            3,
            1,
        );
        assert_eq!(
            with["public_base_url"].as_str(),
            Some("https://oci-a1.example.com")
        );
        // Concurrency is advertised so the control plane can route by capacity.
        assert_eq!(with["max_slots"].as_u64(), Some(3));
        assert_eq!(with["active_slots"].as_u64(), Some(1));
    }

    #[test]
    fn heartbeat_body_reports_agent_version() {
        let body = build_heartbeat_body(&[], None, "linux", "aarch64", 1, 0);
        assert_eq!(body["agent_version"].as_str(), Some(agent_version()));
        assert_eq!(agent_version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn heartbeat_response_parses_update_directive() {
        let with: HeartbeatResponse = serde_json::from_value(serde_json::json!({
            "next_heartbeat_seconds": 30,
            "runner": { "online": true },
            "update": { "minimum_version": "0.7.0" },
        }))
        .expect("parse update directive");
        assert_eq!(
            with.update.map(|u| u.minimum_version).as_deref(),
            Some("0.7.0")
        );

        // Absent directive is the common case and must parse to None.
        let without: HeartbeatResponse = serde_json::from_value(serde_json::json!({
            "next_heartbeat_seconds": 30,
            "runner": { "online": true },
        }))
        .expect("parse without directive");
        assert!(without.update.is_none());
    }

    #[test]
    fn heartbeat_interval_is_clamped_on_ingest() {
        // Below the floor → busy-spin protection.
        assert_eq!(clamp_heartbeat_interval(0), MIN_HEARTBEAT_INTERVAL_SECS);
        // In range → passes through.
        assert_eq!(clamp_heartbeat_interval(30), 30);
        // A pathological server value is ceiled, not trusted.
        assert_eq!(
            clamp_heartbeat_interval(u64::MAX),
            MAX_HEARTBEAT_INTERVAL_SECS
        );
    }

    #[test]
    fn heartbeat_backoff_never_overflows_and_is_capped() {
        // Normal growth: interval × failures, capped at 300s.
        assert_eq!(heartbeat_backoff_secs(30, 1), 30);
        assert_eq!(heartbeat_backoff_secs(30, 2), 60);
        assert_eq!(heartbeat_backoff_secs(30, 4), 120);
        // The multiplier stops growing after 4 failures.
        assert_eq!(heartbeat_backoff_secs(30, 100), 120);
        assert_eq!(heartbeat_backoff_secs(90, 4), MAX_HEARTBEAT_BACKOFF_SECS);
        // Regression (#653): an out-of-range interval must saturate, not
        // overflow — wrap in release builds defeated the backoff entirely.
        assert_eq!(
            heartbeat_backoff_secs(u64::MAX, 4),
            MAX_HEARTBEAT_BACKOFF_SECS
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
    fn run_capsule_command_parses_run_ref_and_owner() {
        let cmd = parse_run_capsule_command(&serde_json::json!({
            "kind": "run_capsule",
            "run_ref": "community/openlist@7f3ac2b",
            "owner_id": "usr_01H",
            "run_id": "run_01H",
            "capsule_slug": "openlist",
        }))
        .expect("valid run_capsule");
        assert_eq!(cmd.run_ref, "community/openlist@7f3ac2b");
        assert_eq!(cmd.owner_id, "usr_01H");
        assert_eq!(cmd.run_id.as_deref(), Some("run_01H"));
        assert_eq!(cmd.capsule_slug.as_deref(), Some("openlist"));
    }

    #[test]
    fn run_capsule_command_requires_run_ref_and_owner() {
        let (code, _) = parse_run_capsule_command(&serde_json::json!({
            "kind": "run_capsule",
            "run_ref": "community/openlist@7f3ac2b",
        }))
        .unwrap_err();
        assert_eq!(code, "invalid_command", "missing owner_id must be rejected");

        let (code, _) = parse_run_capsule_command(&serde_json::json!({
            "kind": "run_capsule",
            "owner_id": "usr_01H",
        }))
        .unwrap_err();
        assert_eq!(code, "invalid_command", "missing run_ref must be rejected");
    }

    #[test]
    fn run_capsule_command_rejects_other_kinds() {
        let (code, _) = parse_run_capsule_command(&serde_json::json!({
            "kind": "run_source_sandbox",
            "source_url": "https://github.com/x/y",
        }))
        .unwrap_err();
        assert_eq!(code, "unsupported_command");
    }

    // ── Lease execution dispatch ──

    #[test]
    fn resolve_lease_execution_source_sandbox_has_no_managed_state() {
        let exec = resolve_lease_execution(
            &serde_json::json!({
                "kind": "run_source_sandbox",
                "source_url": "https://github.com/Koh0920/hello-capsule",
            }),
            std::path::Path::new("unused-recipe"),
        )
        .expect("valid source lease");
        assert_eq!(exec.run_ref, "github.com/Koh0920/hello-capsule");
        assert!(
            exec.managed_state_root.is_none(),
            "source runs use no managed state root"
        );
    }

    #[test]
    fn resolve_lease_execution_run_capsule_keys_state_on_the_executed_ref() {
        use crate::application::pipeline::phases::run::path_segment;
        // An immutable, point-in-time ref (revision-pinned).
        let run_ref = "community/openlist@7f3ac2b";
        let exec = resolve_lease_execution(
            &serde_json::json!({
                "kind": "run_capsule",
                "run_ref": run_ref,
                "owner_id": "usr_01H",
            }),
            std::path::Path::new("unused-recipe"),
        )
        .expect("valid capsule lease");
        // The runner executes exactly the ref it was given...
        assert_eq!(exec.run_ref, run_ref);
        let root = exec
            .managed_state_root
            .expect("capsule runs get a managed state root");
        let s = root.to_string_lossy();
        // ...and keys state on the SAME ref, so execution and state can never
        // diverge — namespaced by owner, both path-safe via the shared scheme.
        assert!(
            s.contains(&path_segment("usr_01H")),
            "owner segment present"
        );
        assert!(
            s.ends_with(&path_segment(run_ref)),
            "state identity is the executed run_ref"
        );
        // Raw ref/owner strings are never used verbatim.
        assert!(!s.contains(run_ref));
    }

    #[test]
    fn resolve_lease_execution_distinct_run_refs_get_distinct_state() {
        let a = resolve_lease_execution(
            &serde_json::json!({
                "kind": "run_capsule", "run_ref": "community/x@rev1", "owner_id": "u",
            }),
            std::path::Path::new("unused-recipe"),
        )
        .unwrap()
        .managed_state_root
        .unwrap();
        let b = resolve_lease_execution(
            &serde_json::json!({
                "kind": "run_capsule", "run_ref": "community/x@rev2", "owner_id": "u",
            }),
            std::path::Path::new("unused-recipe"),
        )
        .unwrap()
        .managed_state_root
        .unwrap();
        assert_ne!(
            a, b,
            "a different (immutable) run_ref gets a distinct state dir"
        );
    }

    #[test]
    fn resolve_lease_execution_inline_recipe_runs_dir_keyed_on_run_ref() {
        use crate::application::pipeline::phases::run::path_segment;
        // Community recipes have no installable release, so the lease ships the
        // recipe TOML inline. The runner materializes + runs the DIR, but state
        // stays keyed on the immutable run_ref identity.
        let dir = tempfile::tempdir().expect("tempdir");
        let run_ref = "community/openlist-google-drive-crypt-openlist";
        let recipe = "schema_version = \"0.3\"\n[targets.app]\nruntime = \"oci\"\n";
        let exec = resolve_lease_execution(
            &serde_json::json!({
                "kind": "run_capsule",
                "run_ref": run_ref,
                "owner_id": "usr_9",
                "recipe_toml": recipe,
            }),
            dir.path(),
        )
        .expect("valid inline-recipe lease");
        // Executes the materialized recipe dir, not the uninstallable run_ref.
        assert_eq!(exec.run_ref, dir.path().to_string_lossy());
        let written = std::fs::read_to_string(dir.path().join("capsule.toml"))
            .expect("capsule.toml materialized");
        assert_eq!(written, recipe);
        // State is STILL keyed on the immutable run_ref identity, not the dir.
        let s = exec
            .managed_state_root
            .expect("managed state root")
            .to_string_lossy()
            .into_owned();
        assert!(s.contains(&path_segment("usr_9")), "owner segment present");
        assert!(
            s.ends_with(&path_segment(run_ref)),
            "state keyed on the immutable run_ref, not the recipe dir"
        );
    }

    #[test]
    fn resolve_lease_execution_rejects_unsafe_ref_and_unknown_kind() {
        // Note: leading/trailing whitespace is trimmed before validation, so the
        // bad cases use a flag prefix, an internal space/tab, or an empty ref.
        for bad in ["--help", "-rf", "a b", "x\ty", ""] {
            let (code, _) = resolve_lease_execution(
                &serde_json::json!({
                    "kind": "run_capsule", "run_ref": bad, "owner_id": "u",
                }),
                std::path::Path::new("unused-recipe"),
            )
            .unwrap_err();
            assert_eq!(code, "invalid_command", "must reject run_ref {bad:?}");
        }
        let (code, _) = resolve_lease_execution(
            &serde_json::json!({
                "kind": "shell", "command": "rm -rf /",
            }),
            std::path::Path::new("unused-recipe"),
        )
        .unwrap_err();
        assert_eq!(code, "unsupported_command");
    }

    #[test]
    fn heartbeat_advertises_supported_lease_kinds() {
        let body = build_heartbeat_body(&[], None, "linux", "aarch64", 1, 0);
        let kinds: Vec<&str> = body["supported_lease_kinds"]
            .as_array()
            .expect("supported_lease_kinds is an array")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            kinds.contains(&LEASE_COMMAND_KIND),
            "must advertise run_source_sandbox"
        );
        // run_capsule execution is wired (resolve_lease_execution), so it is
        // advertised and the control plane may dispatch it.
        assert!(
            kinds.contains(&RUN_CAPSULE_LEASE_KIND),
            "must advertise run_capsule now that execution is wired"
        );
    }

    #[test]
    fn advertised_lease_kinds_appends_native_inference_only_when_ready() {
        // Not ready: base kinds only, NO native-inference advertised — so the
        // control plane will not dispatch native-inference here (the slice-1 gate
        // requires the capability).
        let base = advertised_lease_kinds_for(false, false);
        assert!(base.iter().any(|k| k == LEASE_COMMAND_KIND));
        assert!(base.iter().any(|k| k == RUN_CAPSULE_LEASE_KIND));
        assert!(
            !base.iter().any(|k| k == NATIVE_INFERENCE_RUNTIME),
            "must NOT advertise native-inference when the host is not ready"
        );
        // Ready: base kinds preserved + native-inference appended.
        let ready = advertised_lease_kinds_for(true, false);
        assert!(ready.iter().any(|k| k == RUN_CAPSULE_LEASE_KIND));
        assert!(
            ready.iter().any(|k| k == NATIVE_INFERENCE_RUNTIME),
            "must advertise native-inference when the host is ready"
        );
    }

    #[test]
    fn advertised_lease_kinds_appends_restore_snapshot_only_when_ready() {
        use crate::application::ready_state::restore_lease::RESTORE_SNAPSHOT_LEASE_KIND;
        // KVM-free host: restore_snapshot is NOT advertised, so the control plane will
        // not dispatch a restore here (a Fake/KVM-free host cannot serve a real app_url).
        assert!(
            !advertised_lease_kinds_for(false, false)
                .iter()
                .any(|k| k == RESTORE_SNAPSHOT_LEASE_KIND),
            "must NOT advertise restore_snapshot when the host cannot restore+serve"
        );
        // Restore-ready host: base kinds preserved + restore_snapshot appended.
        let ready = advertised_lease_kinds_for(false, true);
        assert!(ready.iter().any(|k| k == RUN_CAPSULE_LEASE_KIND));
        assert!(ready.iter().any(|k| k == RESTORE_SNAPSHOT_LEASE_KIND));
    }

    fn argv(args: Vec<std::ffi::OsString>) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn run_child_args_sandboxes_source_runs() {
        // Default (source/sandbox) dispatch still passes --sandbox — ato#762
        // leaves sandboxed source execution unchanged.
        let a = argv(run_child_args(
            "github.com/x/y",
            None,
            RunnerDispatchMode::Sandboxed,
        ));
        assert_eq!(a, vec!["run", "github.com/x/y", "--sandbox", "-y"]);
    }

    #[test]
    fn run_child_args_omits_sandbox_for_native_inference() {
        // native-inference is host execution — the child must NOT be sandboxed.
        let a = argv(run_child_args(
            "community/local-llm-chat",
            None,
            RunnerDispatchMode::NativeInferenceHost,
        ));
        assert_eq!(a, vec!["run", "community/local-llm-chat", "-y"]);
        assert!(
            !a.iter().any(|x| x == "--sandbox"),
            "native-inference dispatch must not append --sandbox"
        );
    }

    #[test]
    fn run_child_args_keeps_managed_state_root_in_both_modes() {
        let root = std::path::Path::new("/state/owner/ref");
        for mode in [
            RunnerDispatchMode::Sandboxed,
            RunnerDispatchMode::NativeInferenceHost,
        ] {
            let a = argv(run_child_args("r", Some(root), mode));
            let i = a
                .iter()
                .position(|x| x == "--managed-state-root")
                .expect("managed-state-root flag present");
            assert_eq!(a[i + 1], "/state/owner/ref");
        }
    }

    #[test]
    fn dispatch_mode_only_exact_native_inference_selects_host() {
        // Exactly "native-inference" → host; absent / any other runtime → sandbox.
        // No generic host-exec: "host-exec", "oci", "source/native" stay sandboxed.
        assert_eq!(
            RunnerDispatchMode::from_command(&serde_json::json!({"runtime": "native-inference"})),
            RunnerDispatchMode::NativeInferenceHost
        );
        for other in [
            serde_json::json!({}),
            serde_json::json!({ "runtime": "oci" }),
            serde_json::json!({ "runtime": "host-exec" }),
            serde_json::json!({ "runtime": "source/native" }),
            serde_json::json!({ "runtime": "native-inference-x" }),
        ] {
            assert_eq!(
                RunnerDispatchMode::from_command(&other),
                RunnerDispatchMode::Sandboxed,
                "only the exact native-inference runtime selects host dispatch; got {other}"
            );
        }
    }

    #[test]
    fn ensure_dispatch_supported_fails_closed_without_native_inference_capability() {
        // native-inference on a host that can't run it → typed failure, no spawn.
        let (code, _) =
            ensure_dispatch_supported(RunnerDispatchMode::NativeInferenceHost, false).unwrap_err();
        assert_eq!(code, "native_inference_unavailable");
        // Capable host → ok; sandboxed runs are never gated by this capability.
        assert!(ensure_dispatch_supported(RunnerDispatchMode::NativeInferenceHost, true).is_ok());
        assert!(ensure_dispatch_supported(RunnerDispatchMode::Sandboxed, false).is_ok());
    }

    #[test]
    fn resolve_lease_execution_sets_dispatch_mode_from_runtime() {
        // A run_capsule lease carrying runtime=native-inference resolves to host
        // dispatch; the same lease without the field stays sandboxed.
        let native = resolve_lease_execution(
            &serde_json::json!({
                "kind": "run_capsule",
                "run_ref": "community/local-llm-chat@1.0.0",
                "owner_id": "u",
                "runtime": NATIVE_INFERENCE_RUNTIME,
            }),
            std::path::Path::new("unused-recipe"),
        )
        .expect("valid native-inference lease");
        assert_eq!(
            native.dispatch_mode,
            RunnerDispatchMode::NativeInferenceHost
        );

        let sandboxed = resolve_lease_execution(
            &serde_json::json!({
                "kind": "run_capsule", "run_ref": "community/x@1.0.0", "owner_id": "u",
            }),
            std::path::Path::new("unused-recipe"),
        )
        .expect("valid sandboxed lease");
        assert_eq!(sandboxed.dispatch_mode, RunnerDispatchMode::Sandboxed);
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
            Some(ChildSignal::Ready { port: None })
        );
        assert_eq!(
            parse_child_line("[✓] Command started (ready event received)"),
            Some(ChildSignal::Ready { port: None })
        );
        // Primary machine-readable line wins, with and without a port.
        assert_eq!(
            parse_child_line("LIFECYCLE: ready port=8000"),
            Some(ChildSignal::Ready { port: Some(8000) })
        );
        assert_eq!(
            parse_child_line("LIFECYCLE: ready"),
            Some(ChildSignal::Ready { port: None })
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
    fn consent_required_line_parses_only_complete_well_formed_signal() {
        let valid = r#"{"schema":"execution_plan_consent_v1","consent_ref":"blake3:ref","scoped_id":"community/hello-capsule","version":"0.3.0","target_label":"main","policy_segment_hash":"blake3:p","provisioning_policy_hash":"blake3:q","summary":"network: api.example.com\nfs-rw: /data"}"#;
        match parse_child_line(&format!("CONSENT-REQUIRED: {valid}")) {
            Some(ChildSignal::ConsentRequired(req)) => {
                assert_eq!(req.consent_ref, "blake3:ref");
                assert_eq!(req.identity.scoped_id, "community/hello-capsule");
                assert_eq!(req.identity.version, "0.3.0");
                assert_eq!(req.identity.target_label, "main");
                assert_eq!(req.identity.policy_segment_hash, "blake3:p");
                assert_eq!(req.identity.provisioning_policy_hash, "blake3:q");
                assert!(req.identity.summary.contains("api.example.com"));
            }
            other => panic!("expected ConsentRequired, got {other:?}"),
        }

        // summary present-but-empty is allowed; a MISSING summary is not.
        let empty_summary = r#"{"schema":"execution_plan_consent_v1","consent_ref":"blake3:r","scoped_id":"a","version":"1","target_label":"main","policy_segment_hash":"blake3:p","provisioning_policy_hash":"blake3:q","summary":""}"#;
        assert!(matches!(
            parse_child_line(&format!("CONSENT-REQUIRED: {empty_summary}")),
            Some(ChildSignal::ConsentRequired(_))
        ));

        // Incomplete or malformed signals must NEVER parse as a consent gate.
        let rejects = [
            "CONSENT-REQUIRED: not json",
            "CONSENT-REQUIRED: {}",
            // wrong schema
            r#"CONSENT-REQUIRED: {"schema":"WRONG","consent_ref":"blake3:r","scoped_id":"a","version":"1","target_label":"main","policy_segment_hash":"blake3:p","provisioning_policy_hash":"blake3:q","summary":"s"}"#,
            // missing consent_ref
            r#"CONSENT-REQUIRED: {"schema":"execution_plan_consent_v1","scoped_id":"a","version":"1","target_label":"main","policy_segment_hash":"blake3:p","provisioning_policy_hash":"blake3:q","summary":"s"}"#,
            // non-blake3 consent_ref
            r#"CONSENT-REQUIRED: {"schema":"execution_plan_consent_v1","consent_ref":"sha256:r","scoped_id":"a","version":"1","target_label":"main","policy_segment_hash":"blake3:p","provisioning_policy_hash":"blake3:q","summary":"s"}"#,
            // missing a 5-tuple field (target_label)
            r#"CONSENT-REQUIRED: {"schema":"execution_plan_consent_v1","consent_ref":"blake3:r","scoped_id":"a","version":"1","policy_segment_hash":"blake3:p","provisioning_policy_hash":"blake3:q","summary":"s"}"#,
            // empty identity field (scoped_id)
            r#"CONSENT-REQUIRED: {"schema":"execution_plan_consent_v1","consent_ref":"blake3:r","scoped_id":"","version":"1","target_label":"main","policy_segment_hash":"blake3:p","provisioning_policy_hash":"blake3:q","summary":"s"}"#,
            // empty / non-blake3 policy hash
            r#"CONSENT-REQUIRED: {"schema":"execution_plan_consent_v1","consent_ref":"blake3:r","scoped_id":"a","version":"1","target_label":"main","policy_segment_hash":"","provisioning_policy_hash":"blake3:q","summary":"s"}"#,
            // missing summary (must be present)
            r#"CONSENT-REQUIRED: {"schema":"execution_plan_consent_v1","consent_ref":"blake3:r","scoped_id":"a","version":"1","target_label":"main","policy_segment_hash":"blake3:p","provisioning_policy_hash":"blake3:q"}"#,
        ];
        for line in rejects {
            assert_eq!(parse_child_line(line), None, "must reject: {line}");
        }
    }

    /// Regression (#661): the `CONSENT-REQUIRED:` line is the SHARED
    /// `protocol` type, so a payload serialized from
    /// `protocol::consent::ConsentRequiredLine` — exactly what the CLI
    /// producer emits — round-trips through the runner's `parse_child_line`.
    /// This binds producer and consumer to one type + one validation: a
    /// schema bump or field rename can no longer compile on both sides while
    /// silently failing `is_valid()` (the original triplication hazard). The
    /// schema field is sourced from the same `CONSENT_REF_SCHEMA` constant the
    /// validator checks, so the two cannot disagree.
    #[test]
    fn consent_required_line_uses_shared_wire_type_end_to_end() {
        let payload = protocol::consent::ConsentRequiredLine {
            schema: capsule::execution_plan::canonical::CONSENT_REF_SCHEMA.to_string(),
            consent_ref: "blake3:bind".to_string(),
            identity: protocol::consent::ConsentIdentity {
                scoped_id: "community/hello-capsule".to_string(),
                version: "0.3.0".to_string(),
                target_label: "main".to_string(),
                policy_segment_hash: "blake3:seg".to_string(),
                provisioning_policy_hash: "blake3:prov".to_string(),
                summary: "network: api.example.com".to_string(),
            },
        };
        let line = format!(
            "CONSENT-REQUIRED: {}",
            serde_json::to_string(&payload).expect("serialize wire payload")
        );
        match parse_child_line(&line) {
            Some(ChildSignal::ConsentRequired(parsed)) => assert_eq!(parsed, payload),
            other => panic!("producer's wire line must parse as ConsentRequired, got {other:?}"),
        }
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

    /// ato#702: a sandboxed child's `[child] …` log tail can carry traceback
    /// secrets that reach the persisted runner failure report. `scrub_secrets`
    /// is the single common boundary all four runner sinks (saved log_tail,
    /// failure report, lease error, run error) route through before
    /// persistence; assert it redacts every secret class from the acceptance
    /// criteria while preserving the surrounding traceback shape.
    #[test]
    fn scrub_redacts_child_log_tail_secrets() {
        // A realistic "[child] …" tail as it would be persisted into the
        // failure report / run log: an env dump, a GitHub token, an
        // OpenAI-style key, and an Anthropic key inside a traceback line.
        let raw = concat!(
            "[child] Traceback (most recent call last):\n",
            "[child]   File \"app.py\", line 42, in connect\n",
            "[child] OPENAI_API_KEY=sk-proj-abc123DEF456ghi789jkl012MNO\n",
            "[child] export GITHUB_TOKEN=ghp_AbCdEf0123456789AbCdEf0123456789AbCd\n",
            "[child] api_key: \"sk-ant-api03-SECRETvalue-XYZ_123\"\n",
            "[child] DATABASE_URL=postgres://user:s3cr3tP4ss@db.example.com:5432/app\n",
            "[child] aws id AKIAIOSFODNN7EXAMPLE rejected\n",
            "[child] sent Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payloadpart.signature\n",
        );
        let scrubbed = scrub_secrets(raw);

        // Acceptance criteria: none of these secret values may remain.
        assert!(
            !scrubbed.contains("sk-proj-abc123DEF456ghi789jkl012MNO"),
            "OpenAI key leaked: {scrubbed}"
        );
        assert!(
            !scrubbed.contains("ghp_AbCdEf0123456789AbCdEf0123456789AbCd"),
            "GitHub token leaked: {scrubbed}"
        );
        assert!(
            !scrubbed.contains("sk-ant-api03-SECRETvalue-XYZ_123"),
            "Anthropic key leaked: {scrubbed}"
        );
        assert!(
            !scrubbed.contains("s3cr3tP4ss"),
            "URL credential leaked: {scrubbed}"
        );
        assert!(
            !scrubbed.contains("AKIAIOSFODNN7EXAMPLE"),
            "AWS access key id leaked: {scrubbed}"
        );
        assert!(
            !scrubbed.contains("eyJhbGciOiJIUzI1NiJ9.payloadpart.signature"),
            "bearer token leaked: {scrubbed}"
        );

        // The traceback shape is preserved (only values are redacted).
        assert!(scrubbed.contains("Traceback (most recent call last):"));
        assert!(scrubbed.contains("File \"app.py\", line 42, in connect"));
        assert!(scrubbed.contains("[REDACTED]"));
    }

    /// `.env`-style `KEY=value` / `KEY: value` secret assignments are redacted
    /// by key name AND by UPPER_SNAKE convention, while non-secret structure
    /// (e.g. short numeric assignments) is left intact.
    #[test]
    fn scrub_redacts_env_style_secrets() {
        let scrubbed = scrub_secrets("API_KEY=supersecretvalue123 password: hunter2hunter");
        assert!(!scrubbed.contains("supersecretvalue123"), "{scrubbed}");
        assert!(!scrubbed.contains("hunter2hunter"), "{scrubbed}");
        assert!(scrubbed.contains("API_KEY=[REDACTED]"), "{scrubbed}");

        // A short, non-secret-looking value is not a credential — left alone so
        // tracebacks stay readable.
        assert_eq!(scrub_secrets("exit code 137"), "exit code 137");
        assert_eq!(scrub_secrets("retry=3"), "retry=3");
    }

    /// The persisted log tail (`BoundedLog::line`) and the lease failure
    /// message both route through `scrub_secrets`, so a secret in a child line
    /// must not survive into the written run log.
    #[test]
    fn bounded_log_scrubs_child_secrets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("run.log");
        let mut log = BoundedLog::create(&log_path);
        log.line("[child] OPENAI_API_KEY=sk-proj-LEAKsecretVALUE0123456789abcd");
        log.line("[child] using ghp_LEAKgithubTOKEN0123456789abcdefABCDEF12");
        drop(log);
        let written = std::fs::read_to_string(&log_path).expect("read log");
        assert!(
            !written.contains("sk-proj-LEAKsecretVALUE0123456789abcd"),
            "persisted log leaked OpenAI key: {written}"
        );
        assert!(
            !written.contains("ghp_LEAKgithubTOKEN0123456789abcdefABCDEF12"),
            "persisted log leaked GitHub token: {written}"
        );
        assert!(written.contains("[REDACTED]"), "{written}");
    }

    #[test]
    fn log_tail_returns_whole_small_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lease.log");
        std::fs::write(&path, "line one\nline two\nline three\n").unwrap();
        let tail = read_log_tail_from(&path, 16 * 1024).expect("tail");
        assert!(tail.contains("line one"));
        assert!(tail.contains("line three"));
        assert!(!tail.contains("earlier log truncated"));
    }

    #[test]
    fn log_tail_truncates_to_last_bytes_and_drops_partial_first_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lease.log");
        // 5 lines; cap small enough to keep only the last couple.
        std::fs::write(&path, "aaaa\nbbbb\ncccc\ndddd\neeee\n").unwrap();
        let tail = read_log_tail_from(&path, 12).expect("tail");
        assert!(tail.starts_with("...[earlier log truncated]"));
        assert!(tail.contains("eeee")); // the end is kept
        assert!(!tail.contains("aaaa")); // the start is dropped
    }

    #[test]
    fn log_tail_keeps_a_single_overlong_line_verbatim_under_the_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lease.log");
        // One line longer than the cap, with NO interior newline.
        std::fs::write(&path, "x".repeat(100)).unwrap();
        let tail = read_log_tail_from(&path, 10).expect("tail");
        assert!(tail.starts_with("...[earlier log truncated]"));
        // Nothing is blanked: the kept bytes survive under the marker.
        assert!(
            tail.trim_start_matches("...[earlier log truncated]")
                .contains("x")
        );
    }

    #[test]
    fn log_tail_scrubs_secrets_and_handles_missing_or_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lease.log");
        std::fs::write(&path, "token leak ato_rnr_SECRET_value here\n").unwrap();
        let tail = read_log_tail_from(&path, 16 * 1024).expect("tail");
        assert!(!tail.contains("SECRET_value"));
        assert!(tail.contains("ato_rnr_[REDACTED]"));

        std::fs::write(&path, "").unwrap();
        assert_eq!(read_log_tail_from(&path, 16 * 1024), None); // empty → None
        assert_eq!(
            read_log_tail_from(&dir.path().join("nope.log"), 16 * 1024),
            None
        ); // missing → None
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
                execution_id: "blake3:ready-e2e".to_string(),
                port: None,
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
    async fn ready_outrunning_receipt_across_streams_still_reports_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        let receipt = dir.path().join("receipt.json");
        std::fs::write(&receipt, r#"{"execution_id":"blake3:race-e2e"}"#).unwrap();
        // ready (stdout) lands first; RECEIPT (stderr) follows later — the
        // arrival order the two independent stream readers can always
        // produce. The ready must be held for the receipt, not killed.
        let script = format!(
            "echo 'LIFECYCLE: ready port=8000'; sleep 1; echo 'RECEIPT: {}' >&2; exec sleep 30",
            receipt.display()
        );
        let dir2 = tempfile::tempdir().expect("tempdir");
        let log_path = dir2.path().join("run.log");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let monitor = tokio::spawn(run_lease_child(
            fake_child(&script),
            log_path,
            Duration::from_secs(20),
            tx,
        ));
        let report = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("a report before the child exits");
        assert_eq!(
            report,
            Some(LeaseReport::Ready {
                execution_id: "blake3:race-e2e".to_string(),
                port: Some(8000),
            }),
            "a ready that outruns its receipt across the stdout/stderr split must not kill a healthy workload"
        );
        monitor.abort();
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

    #[cfg(unix)]
    #[tokio::test]
    async fn lifecycle_line_carries_observed_port_through_ready_report() {
        let dir = tempfile::tempdir().expect("tempdir");
        let receipt = dir.path().join("receipt.json");
        std::fs::write(&receipt, r#"{"execution_id":"blake3:port-e2e"}"#).unwrap();
        let script = format!(
            "echo 'RECEIPT: {}'; echo 'LIFECYCLE: ready port=8000'; exit 0",
            receipt.display()
        );
        let (reports, _logdir) = collect_reports(&script, Duration::from_secs(20)).await;
        assert_eq!(
            reports,
            vec![LeaseReport::Ready {
                execution_id: "blake3:port-e2e".to_string(),
                port: Some(8000),
            }]
        );
    }

    #[test]
    fn ready_payload_only_claims_a_url_under_full_proof() {
        let id = || "blake3:x".to_string();
        // Full proof: candidate URL + port + proxy up -> that URL.
        let payload =
            decide_ready_payload(id(), Some("https://r.example.com/"), Some(8000), Some(true));
        assert_eq!(payload.ready_url.as_deref(), Some("https://r.example.com/"));
        assert_eq!(payload.local_port, Some(8000));

        // No candidate URL (e.g. a non-zero slot without a template) -> no URL.
        assert_eq!(
            decide_ready_payload(id(), None, Some(8000), None).ready_url,
            None
        );
        // Port unknown -> no URL.
        assert_eq!(
            decide_ready_payload(id(), Some("https://r.example.com/"), None, None).ready_url,
            None
        );
        // Proxy failed to start -> no URL (never fabricate reachability).
        assert_eq!(
            decide_ready_payload(
                id(),
                Some("https://r.example.com/"),
                Some(8000),
                Some(false)
            )
            .ready_url,
            None
        );
    }

    #[test]
    fn clamp_max_slots_bounds_to_one_through_ceiling() {
        assert_eq!(clamp_max_slots(0), 1);
        assert_eq!(clamp_max_slots(1), 1);
        assert_eq!(clamp_max_slots(8), 8);
        assert_eq!(clamp_max_slots(MAX_SLOTS_CEILING + 100), MAX_SLOTS_CEILING);
    }

    #[test]
    fn parse_proxy_listen_splits_host_and_base_port() {
        assert_eq!(
            parse_proxy_listen("127.0.0.1:8420").unwrap(),
            ("127.0.0.1".to_string(), 8420)
        );
        assert_eq!(
            parse_proxy_listen("0.0.0.0:9000").unwrap(),
            ("0.0.0.0".to_string(), 9000)
        );
        assert!(parse_proxy_listen("127.0.0.1").is_err()); // no port
        assert!(parse_proxy_listen("127.0.0.1:notaport").is_err());
        assert!(parse_proxy_listen(":8420").is_err()); // no host
    }

    #[test]
    fn validate_slot_port_range_rejects_overflow() {
        // Normal config fits.
        assert!(validate_slot_port_range(8420, 64).is_ok());
        // The very last port with a single slot is fine (base + 0 == 65535).
        assert!(validate_slot_port_range(u16::MAX, 1).is_ok());
        // Two slots from the last port would need 65536 -> rejected.
        assert!(validate_slot_port_range(u16::MAX, 2).is_err());
        // High base + many slots overflows -> rejected.
        assert!(validate_slot_port_range(65530, 64).is_err());
        // Exact boundary: base 65472 + 64 slots tops out at 65535 -> ok.
        assert!(validate_slot_port_range(65535 - 63, 64).is_ok());
    }

    #[test]
    fn validate_public_url_template_requires_a_placeholder() {
        // Distinguishing placeholders pass.
        assert!(validate_public_url_template(Some("https://{slot}.runner.example.com/")).is_ok());
        assert!(validate_public_url_template(Some("https://runner.example.com:{port}/")).is_ok());
        // A placeholder-less template would give every slot the same URL.
        assert!(validate_public_url_template(Some("https://runner.example.com/")).is_err());
        // No template is fine — legacy slot-0-only behavior.
        assert!(validate_public_url_template(None).is_ok());
    }

    #[test]
    fn slot_pool_allocates_lowest_free_index_with_per_slot_port() {
        let pool = SlotPool::new(3, "127.0.0.1".to_string(), 8420);
        assert_eq!(pool.capacity(), 3);
        assert_eq!(pool.active(), 0);
        assert!(pool.has_free());

        let a = pool.acquire().expect("slot 0");
        assert_eq!(a.index, 0);
        assert_eq!(a.proxy_port, 8420);
        assert_eq!(a.proxy_listen, "127.0.0.1:8420");
        let b = pool.acquire().expect("slot 1");
        assert_eq!(b.index, 1);
        assert_eq!(b.proxy_port, 8421);
        let _c = pool.acquire().expect("slot 2");
        assert_eq!(pool.active(), 3);
        assert!(!pool.has_free());
        // At capacity: no more slots.
        assert!(pool.acquire().is_none());

        // Releasing the middle slot frees exactly that index for reuse.
        b.release();
        assert_eq!(pool.active(), 2);
        assert!(pool.has_free());
        let d = pool.acquire().expect("reused slot 1");
        assert_eq!(d.index, 1);
        assert_eq!(d.proxy_port, 8421);
    }

    #[test]
    fn slot_release_is_idempotent_across_clones() {
        let pool = SlotPool::new(1, "127.0.0.1".to_string(), 8420);
        let slot = pool.acquire().expect("slot 0");
        assert_eq!(pool.active(), 1);
        slot.release();
        slot.release(); // second release must not double-free
        assert_eq!(pool.active(), 0);
        // A clone shares the released flag — also a no-op now.
        slot.clone().release();
        assert_eq!(pool.active(), 0);
    }

    #[test]
    fn unconfirmed_teardown_keeps_only_that_slot_held() {
        // A slot whose workload couldn't be confirmed gone is simply never
        // released (fail closed): its index stays held while the OTHER slot
        // remains usable — strictly better than the single-slot lockup.
        let pool = SlotPool::new(2, "127.0.0.1".to_string(), 8420);
        let _held = pool.acquire().expect("slot 0"); // no release()
        assert_eq!(pool.active(), 1);
        assert!(pool.has_free());
    }

    #[test]
    fn public_ready_url_template_fills_port_and_slot() {
        let pool = SlotPool::new(2, "127.0.0.1".to_string(), 8420);
        let s0 = pool.acquire().unwrap();
        let s1 = pool.acquire().unwrap();
        // {slot} subdomain form.
        assert_eq!(
            public_ready_url(None, Some("https://{slot}.runner.example.com/"), &s1).as_deref(),
            Some("https://1.runner.example.com/")
        );
        // {port} form.
        assert_eq!(
            public_ready_url(None, Some("https://runner.example.com:{port}/"), &s1).as_deref(),
            Some("https://runner.example.com:8421/")
        );
        // A template wins even when a base URL is also configured.
        assert_eq!(
            public_ready_url(
                Some("https://base.example.com"),
                Some("https://{port}.x/"),
                &s0
            )
            .as_deref(),
            Some("https://8420.x/")
        );
    }

    #[test]
    fn public_ready_url_without_template_is_slot0_only() {
        let pool = SlotPool::new(2, "127.0.0.1".to_string(), 8420);
        let s0 = pool.acquire().unwrap();
        let s1 = pool.acquire().unwrap();
        // Legacy single mapping: slot 0 reaches public_base_url; others don't
        // (no fabricated URL for a slot the ingress can't reach).
        assert_eq!(
            public_ready_url(Some("https://r.example.com"), None, &s0).as_deref(),
            Some("https://r.example.com/")
        );
        assert_eq!(
            public_ready_url(Some("https://r.example.com"), None, &s1),
            None
        );
        // No base and no template -> never a URL.
        assert_eq!(public_ready_url(None, None, &s0), None);
    }

    #[tokio::test]
    async fn proxy_maps_root_to_observed_local_port_and_nothing_else() {
        // Upstream HTTP server serving multiple connections — the proxy's
        // bring-up probe consumes one connection before the real request.
        let upstream_listener = TcpListener::bind("127.0.0.1:0").expect("bind upstream");
        let upstream_port = upstream_listener.local_addr().expect("addr").port();
        let upstream = std::thread::spawn(move || {
            for _ in 0..3 {
                let Ok((mut stream, _)) = upstream_listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = "{\"hello\":\"from-upstream\"}";
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        // Proxy on its own ephemeral port: bind via port 0 is not expressible
        // through start_root_proxy's listen string with assertions, so pick a
        // free port first.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("probe port");
        let listen = listener.local_addr().expect("addr").to_string();
        drop(listener);

        let handle = start_root_proxy(&listen, upstream_port)
            .await
            .expect("proxy starts against a live upstream");

        let response = reqwest::Client::new()
            .get(format!("http://{listen}/anything"))
            .send()
            .await
            .expect("request through proxy");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body = response.text().await.expect("body");
        assert!(body.contains("from-upstream"));
        drop(upstream); // serving thread parks on accept; process teardown reaps it
        handle.abort();

        // The only upstream the proxy can reach is the fixed workload port —
        // there is no caller-controlled upstream input at the type level; a
        // dead upstream refuses bring-up entirely:
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("probe port");
        let dead_port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("dead");
            let p = l.local_addr().unwrap().port();
            drop(l);
            p
        };
        let listen2 = listener.local_addr().expect("addr").to_string();
        drop(listener);
        assert!(
            start_root_proxy(&listen2, dead_port).await.is_err(),
            "proxy must refuse to come up in front of a dead workload"
        );
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
            1,
            0,
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
                ..
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
        let body = build_heartbeat_body(&[], None, "linux", "aarch64", 1, 0);
        let outcome =
            send_heartbeat_once(&client, &base, "01TEST", "ato_rnr_test-token", &body).await;
        let _ = server.join();
        assert!(
            matches!(outcome, HeartbeatOutcome::Revoked),
            "revoked must be terminal, got {outcome:?}"
        );
    }

    // ── Owner-initiated stop (P3-C) ──

    #[test]
    fn stop_cleanup_frees_slot_only_when_fully_torn_down() {
        // Both teardowns confirmed -> the slot is honestly free.
        assert!(StopCleanup::from_teardown(true, true).slot_released);
        // Either teardown unconfirmed -> the slot stays held (fail closed).
        // This is the cleanup_failure_does_not_release_slot_as_clean invariant.
        assert!(!StopCleanup::from_teardown(false, true).slot_released);
        assert!(!StopCleanup::from_teardown(true, false).slot_released);
        assert!(!StopCleanup::from_teardown(false, false).slot_released);
    }

    #[test]
    fn stopped_body_carries_reason_and_cleanup_flags() {
        let clean = stopped_request_body(&StopCleanup::from_teardown(true, true));
        assert_eq!(clean["reason"], "user_requested");
        assert_eq!(clean["cleanup"]["process_terminated"], true);
        assert_eq!(clean["cleanup"]["proxy_stopped"], true);
        assert_eq!(clean["cleanup"]["slot_released"], true);

        // A partial teardown reports the partial flags truthfully — the API,
        // not the runner, decides that means "not a clean stop".
        let partial = stopped_request_body(&StopCleanup::from_teardown(true, false));
        assert_eq!(partial["cleanup"]["proxy_stopped"], false);
        assert_eq!(partial["cleanup"]["slot_released"], false);
    }

    #[test]
    fn lease_control_deserializes_stop_request() {
        let stop: LeaseControl = serde_json::from_str(
            "{\"lease_id\":\"01L\",\"status\":\"ready\",\"stop_requested\":true,\"stop_requested_at\":\"2026-06-10T00:00:00Z\"}",
        )
        .expect("parse");
        assert!(stop.stop_requested);
        let go: LeaseControl = serde_json::from_str("{\"stop_requested\":false}").expect("parse");
        assert!(!go.stop_requested);
        // A missing field is "no stop requested", never a parse error.
        let empty: LeaseControl = serde_json::from_str("{}").expect("parse");
        assert!(!empty.stop_requested);
    }

    #[tokio::test]
    async fn poll_control_once_maps_status_to_outcome() {
        let client = reqwest::Client::new();

        // 200 + stop_requested true -> Stop, with runner-token bearer auth.
        let (base, server) = one_shot_http(
            "HTTP/1.1 200 OK",
            "{\"lease_id\":\"01L\",\"stop_requested\":true,\"stop_requested_at\":\"t\"}",
        );
        let url = format!("{base}/v1/runner-leases/01L/control");
        let outcome = poll_control_once(&client, &url, "ato_rnr_t").await;
        let request = server.join().expect("server");
        assert!(request.contains("GET /v1/runner-leases/01L/control"));
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer ato_rnr_t")
        );
        assert!(matches!(outcome, ControlOutcome::Stop));

        // 404 (lease gone) -> Done: stop watching, do not spin.
        let (base, server) = one_shot_http("HTTP/1.1 404 Not Found", "{\"error\":\"not_found\"}");
        let url = format!("{base}/v1/runner-leases/01L/control");
        let outcome = poll_control_once(&client, &url, "ato_rnr_t").await;
        let _ = server.join();
        assert!(matches!(outcome, ControlOutcome::Done));

        // 200 + stop_requested false -> Continue.
        let (base, server) = one_shot_http("HTTP/1.1 200 OK", "{\"stop_requested\":false}");
        let url = format!("{base}/v1/runner-leases/01L/control");
        let outcome = poll_control_once(&client, &url, "ato_rnr_t").await;
        let _ = server.join();
        assert!(matches!(outcome, ControlOutcome::Continue));
    }

    // ── ExecutionPlan consent gate (P4-A) ──

    #[test]
    fn lease_control_deserializes_consent_decision() {
        let approved: LeaseControl = serde_json::from_str(
            "{\"stop_requested\":false,\"consent\":{\"status\":\"approved\",\"consent_ref\":\"blake3:ref\"}}",
        )
        .expect("parse");
        let decision = approved.consent.expect("consent present");
        assert_eq!(decision.status, "approved");
        assert_eq!(decision.consent_ref, "blake3:ref");
        // A null/absent consent is "no decision in play", never a parse error.
        let none: LeaseControl =
            serde_json::from_str("{\"stop_requested\":false,\"consent\":null}").expect("parse");
        assert!(none.consent.is_none());
        assert!(
            serde_json::from_str::<LeaseControl>("{}")
                .expect("parse")
                .consent
                .is_none()
        );
    }

    fn sample_consent_request() -> ConsentRequest {
        ConsentRequest {
            schema: capsule::execution_plan::canonical::CONSENT_REF_SCHEMA.to_string(),
            consent_ref: "blake3:ref".to_string(),
            identity: protocol::consent::ConsentIdentity {
                scoped_id: "community/hello-capsule".to_string(),
                version: "0.3.0".to_string(),
                target_label: "main".to_string(),
                policy_segment_hash: "blake3:seg".to_string(),
                provisioning_policy_hash: "blake3:prov".to_string(),
                summary: "network: api.example.com\nfs-rw: /data".to_string(),
            },
        }
    }

    #[tokio::test]
    async fn report_consent_required_posts_full_tuple() {
        let (base, server) = one_shot_http("HTTP/1.1 200 OK", "{\"ok\":true}");
        let client = reqwest::Client::new();
        let request = sample_consent_request();
        report_consent_required(&client, &base, "ato_rnr_t", "01L", &request)
            .await
            .expect("report ok");
        let captured = server.join().expect("server");
        assert!(captured.contains("POST /v1/runner-leases/01L/consent-required"));
        assert!(
            captured
                .to_lowercase()
                .contains("authorization: bearer ato_rnr_t")
        );
        // The FULL 5-tuple (the decision contract) + consent_ref + summary are on
        // the wire — not just the binding hash.
        for needle in [
            "\"consent_ref\":\"blake3:ref\"",
            "\"scoped_id\":\"community/hello-capsule\"",
            "\"version\":\"0.3.0\"",
            "\"target_label\":\"main\"",
            "\"policy_segment_hash\":\"blake3:seg\"",
            "\"provisioning_policy_hash\":\"blake3:prov\"",
            "\"summary\":",
        ] {
            assert!(
                captured.contains(needle),
                "body missing {needle}: {captured}"
            );
        }
    }

    #[tokio::test]
    async fn poll_consent_once_maps_decisions() {
        let client = reqwest::Client::new();
        let url_for = |base: &str| format!("{base}/v1/runner-leases/01L/control");

        // Approved carries the exact consent_ref the runner must verify.
        let (base, server) = one_shot_http(
            "HTTP/1.1 200 OK",
            "{\"consent\":{\"status\":\"approved\",\"consent_ref\":\"blake3:ref\"}}",
        );
        let outcome = poll_consent_once(&client, &url_for(&base), "ato_rnr_t").await;
        let _ = server.join();
        assert!(matches!(
            outcome,
            Some(ConsentOutcome::Approved { consent_ref }) if consent_ref == "blake3:ref"
        ));

        // Rejected / expired are resolved terminals.
        let (base, server) = one_shot_http(
            "HTTP/1.1 200 OK",
            "{\"consent\":{\"status\":\"rejected\",\"consent_ref\":\"blake3:ref\"}}",
        );
        let outcome = poll_consent_once(&client, &url_for(&base), "ato_rnr_t").await;
        let _ = server.join();
        assert!(matches!(outcome, Some(ConsentOutcome::Rejected)));

        let (base, server) = one_shot_http(
            "HTTP/1.1 200 OK",
            "{\"consent\":{\"status\":\"expired\",\"consent_ref\":\"blake3:ref\"}}",
        );
        let outcome = poll_consent_once(&client, &url_for(&base), "ato_rnr_t").await;
        let _ = server.join();
        assert!(matches!(outcome, Some(ConsentOutcome::Expired)));

        // Pending keeps polling (None).
        let (base, server) = one_shot_http(
            "HTTP/1.1 200 OK",
            "{\"consent\":{\"status\":\"pending\",\"consent_ref\":\"blake3:ref\"}}",
        );
        let outcome = poll_consent_once(&client, &url_for(&base), "ato_rnr_t").await;
        let _ = server.join();
        assert!(outcome.is_none());

        // A stop observed during the consent wait resolves it (even if the
        // background stop watcher already exited).
        let (base, server) = one_shot_http(
            "HTTP/1.1 200 OK",
            "{\"stop_requested\":true,\"consent\":null}",
        );
        let outcome = poll_consent_once(&client, &url_for(&base), "ato_rnr_t").await;
        let _ = server.join();
        assert!(matches!(outcome, Some(ConsentOutcome::Stop)));

        // Lease gone -> stop waiting.
        let (base, server) = one_shot_http("HTTP/1.1 404 Not Found", "{\"error\":\"not_found\"}");
        let outcome = poll_consent_once(&client, &url_for(&base), "ato_rnr_t").await;
        let _ = server.join();
        assert!(matches!(outcome, Some(ConsentOutcome::Gone)));
    }

    #[test]
    fn consent_verify_action_requires_three_way_match() {
        let r = "blake3:ref";
        // child == recomputed == approved → record locally + retry.
        assert_eq!(consent_verify_action(r, r, r), ConsentVerifyAction::Record);
        // ANY pair disagreeing → re-emit needs_consent, never record (the host
        // ledger is never written for a plan the three refs don't all bind to).
        assert_eq!(
            consent_verify_action(r, r, "blake3:other"),
            ConsentVerifyAction::ReEmit,
            "approved differs"
        );
        assert_eq!(
            consent_verify_action(r, "blake3:other", r),
            ConsentVerifyAction::ReEmit,
            "recomputed differs"
        );
        assert_eq!(
            consent_verify_action("blake3:other", r, r),
            ConsentVerifyAction::ReEmit,
            "child differs"
        );
        assert_eq!(
            consent_verify_action("a", "b", "c"),
            ConsentVerifyAction::ReEmit,
            "all differ"
        );
    }

    #[test]
    fn stopped_request_body_with_reason_overrides_reason_only() {
        let cleanup = StopCleanup::from_teardown(true, true);
        let body = stopped_request_body_with_reason(&cleanup, "runner_restarted");
        assert_eq!(body["reason"], "runner_restarted");
        assert_eq!(body["cleanup"]["slot_released"], true);
        // The default body keeps the user_requested reason.
        assert_eq!(stopped_request_body(&cleanup)["reason"], "user_requested");
    }

    #[test]
    fn parse_open_lease_ids_is_tolerant() {
        let body: serde_json::Value = serde_json::json!({
            "leases": [
                { "id": "01LEASEA", "run_id": "r1", "status": "ready", "stop_requested": true },
                { "run_id": "r2" },              // malformed: no id → skipped
                { "id": 42 },                     // malformed: non-string → skipped
                { "id": "01LEASEB", "status": "claimed" },
            ],
        });
        assert_eq!(parse_open_lease_ids(&body), vec!["01LEASEA", "01LEASEB"]);
        assert!(parse_open_lease_ids(&serde_json::json!({})).is_empty());
        assert!(parse_open_lease_ids(&serde_json::json!({ "leases": "nope" })).is_empty());
    }

    #[tokio::test]
    async fn reconcile_open_leases_hits_the_open_endpoint_and_tolerates_404() {
        // Older API without the endpoint: reconcile must be a silent no-op.
        let (base, server) = one_shot_http("HTTP/1.1 404 Not Found", "{\"error\":\"not_found\"}");
        let client = reqwest::Client::new();
        reconcile_open_leases(&client, &base, "01RUNNER", "ato_rnr_t").await;
        let request = server.join().expect("server");
        assert!(request.contains("GET /v1/runners/01RUNNER/leases/open"));
        assert!(
            request.contains("authorization: Bearer ato_rnr_t")
                || request.contains("Authorization: Bearer ato_rnr_t")
        );
    }

    // ── Startup reconcile: probe survivors, never fabricate teardown (#645) ──

    #[test]
    fn reconcile_cleanup_claims_full_teardown_only_on_confirmed_gone_evidence() {
        let clean = reconcile_cleanup_for(WorkloadEvidence::ConfirmedGone);
        assert!(clean.process_terminated && clean.proxy_stopped && clean.slot_released);
        // A possibly-surviving workload confirms NOTHING, so nothing may be
        // claimed — the slot must stay held (fail closed, matching
        // perform_stop_cleanup).
        for evidence in [
            WorkloadEvidence::PossiblyAlive(Some(4242)),
            WorkloadEvidence::PossiblyAlive(None),
        ] {
            let held = reconcile_cleanup_for(evidence);
            assert!(!held.process_terminated);
            assert!(!held.proxy_stopped);
            assert!(
                !held.slot_released,
                "a possible survivor must never free the slot: {evidence:?}"
            );
        }
    }

    #[test]
    fn probe_workload_evidence_without_a_record_confirms_gone() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No record: this host never dispatched (or already confirmed the
        // teardown of) a workload for the lease.
        assert_eq!(
            probe_workload_evidence(&dir.path().join("01LEASE.pid")),
            WorkloadEvidence::ConfirmedGone
        );
    }

    #[test]
    fn probe_workload_evidence_fails_closed_on_an_unreadable_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("01LEASE.pid");
        std::fs::write(&path, "not-a-pid").expect("write");
        // A record exists, so a dispatch happened and its teardown was never
        // confirmed — an unreadable pid is not evidence of absence.
        assert_eq!(
            probe_workload_evidence(&path),
            WorkloadEvidence::PossiblyAlive(None)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_workload_evidence_tracks_a_real_group_lifecycle() {
        // A live recorded group must read possibly-alive (the orphan of
        // #645: children lead their OWN group, so they survive a hard-killed
        // runner); once the group is gone it must read confirmed-gone so the
        // slot can be freed honestly.
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg("sleep 300")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = cmd.spawn().expect("spawn group");
        let pid = child.id().expect("child pid");

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("01LEASE.pid");
        std::fs::write(&path, pid.to_string()).expect("write record");

        assert_eq!(
            probe_workload_evidence(&path),
            WorkloadEvidence::PossiblyAlive(Some(pid)),
            "a live workload group must never be reconciled as torn down"
        );

        kill_group(pid, libc::SIGKILL).expect("kill group");
        let _ = child.wait().await; // reap: a zombie still occupies the group
        let mut gone = false;
        for _ in 0..100 {
            if probe_workload_evidence(&path) == WorkloadEvidence::ConfirmedGone {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(gone, "a reaped group is honest full-teardown evidence");
    }

    #[tokio::test]
    async fn reconcile_acks_full_teardown_for_leases_with_no_recorded_workload() {
        // Two-shot server: GET /leases/open lists one lease, then the
        // /stopped ack is captured. No workload group was ever recorded for
        // the lease on this host, so the full-cleanup claim is honest.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let mut captured = Vec::new();
            for body in [
                "{\"leases\":[{\"id\":\"01RECONCILE645NORECORD\"}]}",
                "{\"ok\":true}",
            ] {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                captured.push(String::from_utf8_lossy(&buf[..n]).to_string());
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).expect("write");
            }
            captured
        });
        let client = reqwest::Client::new();
        reconcile_open_leases(&client, &format!("http://{addr}"), "01RUNNER", "ato_rnr_t").await;
        let captured = server.join().expect("server");
        assert!(captured[0].contains("GET /v1/runners/01RUNNER/leases/open"));
        assert!(captured[1].contains("POST /v1/runner-leases/01RECONCILE645NORECORD/stopped"));
        assert!(captured[1].contains("\"reason\":\"runner_restarted\""));
        assert!(
            captured[1].contains("\"slot_released\":true"),
            "no recorded workload → full teardown is honest evidence: {}",
            captured[1]
        );
    }

    #[tokio::test]
    async fn report_lease_stopped_posts_cleanup_and_accepts_409() {
        // Partial cleanup -> the API answers 409 stop_cleanup_incomplete. That
        // is a truthful outcome, not a transport failure: the ack must succeed.
        let (base, server) = one_shot_http(
            "HTTP/1.1 409 Conflict",
            "{\"error\":\"stop_cleanup_incomplete\",\"status\":\"failed\"}",
        );
        let client = reqwest::Client::new();
        let cleanup = StopCleanup::from_teardown(true, false);
        let result = report_lease_stopped(&client, &base, "ato_rnr_t", "01LEASE", &cleanup).await;
        let request = server.join().expect("server");
        assert!(request.contains("POST /v1/runner-leases/01LEASE/stopped"));
        assert!(request.contains("\"process_terminated\":true"));
        assert!(request.contains("\"proxy_stopped\":false"));
        assert!(request.contains("\"slot_released\":false"));
        assert!(
            result.is_ok(),
            "409 (recorded as a failed stop) is a delivered ack, not an error: {result:?}"
        );
    }

    #[tokio::test]
    async fn report_lease_stopped_errors_on_unexpected_status() {
        let (base, server) = one_shot_http("HTTP/1.1 500 Internal Server Error", "{}");
        let client = reqwest::Client::new();
        let cleanup = StopCleanup::from_teardown(true, true);
        let result = report_lease_stopped(&client, &base, "ato_rnr_t", "01LEASE", &cleanup).await;
        let _ = server.join();
        assert!(
            result.is_err(),
            "an unexpected 5xx must surface, not be swallowed"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_child_group_kills_whole_group_and_confirms() {
        // `sh -c 'sleep 300 & sleep 300'` builds a group with >1 member, so a
        // successful teardown proves we signal the GROUP (kill -pgid), not just
        // the direct child — the requirement to reap the whole workload subtree.
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg("sleep 300 & sleep 300")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = cmd.spawn().expect("spawn process group");
        let pid = child.id().expect("child pid");
        // Drop the pipe read-ends so the child exiting closes them, mirroring
        // run_lease_child's reap path.
        child.stdout.take();
        child.stderr.take();
        assert!(
            process_group_alive(pid),
            "the workload group must be running before the stop"
        );

        let monitor = tokio::spawn(async move {
            let _ = child.wait().await;
        });
        let terminated = terminate_child_group(Some(pid), monitor).await;
        assert!(
            terminated,
            "terminating the group must be confirmed (monitor reaps the leader)"
        );

        // The reparented grandchild is reaped by init shortly after; confirm the
        // group is fully gone (no survivor occupies the slot).
        for _ in 0..100 {
            if !process_group_alive(pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !process_group_alive(pid),
            "no process in the workload group may survive the stop"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_child_group_kills_engine_in_its_own_group() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        // ato#769: `ato run`'s host executor spawns the native-inference engine
        // in its OWN process group (executors/source.rs `process_group(0)`), so a
        // single kill(-ato_run_pgid) strands the engine. `bash`'s `set -m` (job
        // control) makes the backgrounded `sleep` a CHILD of the shell but in its
        // own group — the exact escape. (`bash` specifically: dash's `set -m`
        // leaves the child in the shell's group.) The teardown must reap BOTH
        // groups.
        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-c")
            .arg("set -m; sleep 300 & echo $!; wait")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = cmd.spawn().expect("spawn workload");
        let pid = child.id().expect("child pid");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut lines = BufReader::new(stdout).lines();
        let engine: u32 = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await
            .expect("engine pid printed within timeout")
            .expect("read engine pid line")
            .expect("engine pid line present")
            .trim()
            .parse()
            .expect("engine pid parses");

        // The engine leads its OWN process group, distinct from the shell's —
        // the ato#769 escape that a single kill(-shell_pgid) would strand.
        assert_ne!(
            pid, engine,
            "engine must lead a different group than the shell"
        );
        assert!(
            process_group_alive(pid),
            "shell group must be alive before the stop"
        );
        assert!(
            process_group_alive(engine),
            "engine group must be alive before the stop"
        );

        let monitor = tokio::spawn(async move {
            let _ = child.wait().await;
        });
        let terminated = terminate_child_group(Some(pid), monitor).await;
        assert!(
            terminated,
            "teardown must confirm the whole subtree (shell + engine) is gone"
        );

        // Neither group may survive — the engine must NOT be stranded (ato#769).
        for _ in 0..100 {
            if !process_group_alive(pid) && !process_group_alive(engine) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !process_group_alive(pid),
            "shell group must be gone after the stop"
        );
        assert!(
            !process_group_alive(engine),
            "engine group must be gone after the stop — not stranded (ato#769)"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_child_group_repeated_stop_is_idempotent() {
        // A redundant second owner stop (the run is already torn down) must still
        // confirm `true`, never fail closed — repeated stops are idempotent.
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg("sleep 300")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = cmd.spawn().expect("spawn workload");
        let pid = child.id().expect("child pid");

        // First stop tears the workload down and confirms it.
        let monitor = tokio::spawn(async move {
            let _ = child.wait().await;
        });
        assert!(
            terminate_child_group(Some(pid), monitor).await,
            "first stop must confirm the workload is gone"
        );

        // Second stop on the already-gone workload is a confirmed no-op.
        let monitor = tokio::spawn(async {});
        assert!(
            terminate_child_group(Some(pid), monitor).await,
            "a repeated stop on a gone workload must remain idempotent (true)"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn terminate_child_group_kills_whole_tree_and_confirms() {
        // PowerShell starts ping as a grandchild and prints its PID, so a
        // successful teardown proves we kill the TREE (taskkill /T), not just
        // the direct child — the requirement to reap the whole workload
        // subtree on a host without POSIX process groups.
        let mut cmd = tokio::process::Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-Command",
            "$p = Start-Process -FilePath ping -ArgumentList '-n','300','127.0.0.1' \
             -PassThru -WindowStyle Hidden; Write-Output $p.Id; Wait-Process -Id $p.Id",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
        let mut child = cmd.spawn().expect("spawn workload tree");
        let pid = child.id().expect("child pid");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut lines = BufReader::new(stdout).lines();
        let grandchild: u32 = tokio::time::timeout(Duration::from_secs(60), lines.next_line())
            .await
            .expect("grandchild pid printed within timeout")
            .expect("read grandchild pid line")
            .expect("grandchild pid line present")
            .trim()
            .parse()
            .expect("grandchild pid parses");
        assert!(
            windows_pid_alive(grandchild),
            "the workload grandchild must be running before the stop"
        );

        let monitor = tokio::spawn(async move {
            let _ = child.wait().await;
        });
        let terminated = terminate_child_group(Some(pid), monitor).await;
        assert!(
            terminated,
            "terminating the tree must be confirmed (monitor reaps the leader)"
        );

        // taskkill /T terminated the grandchild directly; confirm nothing from
        // the workload tree survives to occupy the slot.
        for _ in 0..100 {
            if !windows_pid_alive(grandchild) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !windows_pid_alive(grandchild),
            "no process in the workload tree may survive the stop"
        );
    }

    #[tokio::test]
    async fn proxy_abort_must_be_awaited_before_clean_stop() {
        // Live upstream so the proxy comes up (it refuses bring-up otherwise).
        let upstream_listener = TcpListener::bind("127.0.0.1:0").expect("bind upstream");
        let upstream_port = upstream_listener.local_addr().expect("addr").port();
        let upstream = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((mut stream, _)) = upstream_listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
            }
        });
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe port");
        let listen = probe.local_addr().expect("addr").to_string();
        drop(probe);

        let handle = start_root_proxy(&listen, upstream_port)
            .await
            .expect("proxy starts against a live upstream");

        // stop_proxy AWAITS the aborted task, so by the time it returns true the
        // listener has been dropped and the port refuses connections WITHOUT any
        // retry/poll loop. If abort were treated as fire-and-forget, this
        // immediate connect could still succeed — the whole point of the fix.
        let stopped = stop_proxy(Some(handle)).await;
        assert!(
            stopped,
            "a cleanly cancelled proxy task is a confirmed stop"
        );
        assert!(
            tokio::net::TcpStream::connect(&listen).await.is_err(),
            "once stop_proxy confirms termination the listen port must be released",
        );
        drop(upstream);
    }

    #[tokio::test]
    async fn stop_proxy_with_no_proxy_is_vacuously_stopped() {
        assert!(stop_proxy(None).await, "no proxy up == nothing to stop");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cleanup_ack_reports_proxy_stopped_false_when_proxy_abort_not_confirmed() {
        // A proxy task stuck in a blocking section cannot honor abort within the
        // grace window, so stop_proxy times out. It must report the teardown
        // UNCONFIRMED (false), and the slot must stay held (fail closed) rather
        // than advertise a slot whose proxy we never proved was down — exactly
        // the bug the await-the-abort fix closes.
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            // Signal that we are running, THEN block past any await point so a
            // later abort cannot cancel us pre-poll (which would be a clean
            // cancellation, not the timeout this test needs to exercise).
            let _ = started_tx.send(());
            std::thread::sleep(Duration::from_millis(400));
        });
        started_rx.await.expect("proxy task started");
        let proxy_stopped = stop_proxy_within(Some(handle), Duration::from_millis(50)).await;
        assert!(
            !proxy_stopped,
            "a proxy abort we cannot confirm within the grace must not be a clean stop",
        );
        let cleanup = StopCleanup::from_teardown(true, proxy_stopped);
        assert!(!cleanup.proxy_stopped);
        assert!(
            !cleanup.slot_released,
            "unconfirmed proxy teardown must keep the slot held",
        );
    }

    // ── Readiness port race (LIFECYCLE line vs human "[✓] ready" echo) ──

    fn write_receipt(dir: &std::path::Path, execution_id: &str) -> PathBuf {
        let receipt = dir.join("receipt.json");
        std::fs::write(&receipt, format!("{{\"execution_id\":\"{execution_id}\"}}"))
            .expect("write receipt");
        receipt
    }

    async fn first_report(script: String) -> LeaseReport {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = cmd.spawn().expect("spawn child");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<LeaseReport>();
        tokio::spawn(run_lease_child(
            child,
            dir.path().join("run.log"),
            Duration::from_secs(20),
            tx,
        ));
        let report = tokio::time::timeout(Duration::from_secs(8), rx.recv())
            .await
            .expect("a report within 8s")
            .expect("a report");
        // Keep `dir` alive until after we have the report (receipt is read lazily).
        drop(dir);
        report
    }

    #[tokio::test]
    async fn portless_ready_adopts_the_lifecycle_port_when_it_lands_late() {
        // The race that dropped ready_url: the human "(ready event received)"
        // line lands BEFORE the canonical "LIFECYCLE: ready port=N" line. The
        // lease must still settle WITH the port so the proxy + ready_url come up.
        let dir = tempfile::tempdir().expect("tempdir");
        let receipt = write_receipt(dir.path(), "blake3:abc");
        let script = format!(
            "echo 'RECEIPT: {}'; echo 'Service x is ready (ready event received)'; echo 'LIFECYCLE: ready port=8000'; sleep 2",
            receipt.display(),
        );
        match first_report(script).await {
            LeaseReport::Ready { execution_id, port } => {
                assert_eq!(
                    port,
                    Some(8000),
                    "a late LIFECYCLE port line must win over the portless echo"
                );
                assert_eq!(execution_id, "blake3:abc");
            }
            other => panic!("expected Ready with port 8000, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn portless_ready_settles_without_port_after_grace() {
        // No LIFECYCLE port line ever arrives: after the grace window the lease
        // settles as ready WITHOUT a port (honest ready, no ready_url) — the fix
        // must not hang a genuinely port-less ready forever.
        let dir = tempfile::tempdir().expect("tempdir");
        let receipt = write_receipt(dir.path(), "blake3:def");
        let script = format!(
            "echo 'RECEIPT: {}'; echo 'Service x is ready (ready event received)'; sleep 6",
            receipt.display(),
        );
        match first_report(script).await {
            LeaseReport::Ready { port, .. } => {
                assert_eq!(
                    port, None,
                    "with no port line, settle portless after the grace"
                );
            }
            other => panic!("expected a portless Ready, got {other:?}"),
        }
    }

    // ── Connected Runner ⇄ real NodeCompat foreground readiness (regression
    //    lock for #693 / #703) ──
    //
    // The runner's `run_lease_child` settles a lease as Ready only when the
    // dispatched `ato run <source> --sandbox -y` child emits the machine
    // `RECEIPT:` line plus the canonical `LIFECYCLE: ready port=N` line. Before
    // #693, foreground NodeCompat runs took the blocking `execute()` path, which
    // wired no lifecycle pump: a dispatched node capsule emitted neither line,
    // so `run_lease_child` ran out the 600s ready deadline and the lease was
    // reported failed(readiness_timeout). #693 added
    // `node_compat::spawn_foreground` so the foreground node path TCP-probes the
    // declared port and prints `LIFECYCLE: ready port=N` exactly like the host
    // source executor.
    //
    // The other readiness tests in this module drive `run_lease_child` against a
    // *synthetic* shell script that echoes those lines — they pin the runner's
    // parsing, not that a real foreground node run actually emits them. This
    // test closes that gap end to end: it spawns the *real* `ato` binary the way
    // the runner does (`spawn_run_child`'s exact `run <path> --sandbox -y`
    // shape) against the real `installed-relaunch-node` NodeCompat fixture
    // (declared `port = 18880`), feeds its real stdout/stderr through the real
    // `run_lease_child`, and asserts it reaches `LeaseReport::Ready` with the
    // observed port — the assertion that timed out before #693.

    /// Resolve the `ato` binary that sits beside this test's `current_exe`.
    /// Cargo links integration/unit test binaries into `…/target/<profile>/deps`
    /// while the `ato` bin lands in `…/target/<profile>/ato`, so walk up from the
    /// test binary and look for a sibling `ato` in an ancestor directory.
    #[cfg(unix)]
    fn resolve_ato_binary() -> Option<PathBuf> {
        if let Ok(path) = std::env::var("ATO_RUNNER_CHILD_BIN")
            && !path.trim().is_empty()
        {
            let candidate = PathBuf::from(path);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        let exe = std::env::current_exe().ok()?;
        for ancestor in exe.ancestors() {
            let candidate = ancestor.join("ato");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    /// Resolve the `nacelle` engine the source/node toolchain run needs.
    /// Mirrors the convention in `tests/provider_npm_run_e2e.rs`: honor an
    /// explicit `NACELLE_PATH`, else fall back to the sibling crate's debug
    /// build.
    #[cfg(unix)]
    fn resolve_test_nacelle() -> Option<PathBuf> {
        if let Ok(path) = std::env::var("NACELLE_PATH") {
            let candidate = PathBuf::from(path);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        let candidate =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../nacelle/target/debug/nacelle");
        candidate.exists().then_some(candidate)
    }

    /// The real NodeCompat capsule fixture: a tiny `node server.js` that binds
    /// the declared port (18880) and stays up — the minimal shape a dispatched
    /// run must drive to ready.
    #[cfg(unix)]
    fn node_compat_fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("installed-relaunch-node")
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "spawns the real ato binary + a real source/node runtime via the managed toolchain (needs node + nacelle); run with --ignored"]
    async fn node_compat_capsule_dispatched_through_runner_reaches_ready() {
        let Some(ato_bin) = resolve_ato_binary() else {
            panic!("could not resolve the built `ato` binary beside the test executable");
        };
        let Some(nacelle) = resolve_test_nacelle() else {
            panic!(
                "could not resolve `nacelle`; set NACELLE_PATH or build crates/nacelle (cargo build -p nacelle)"
            );
        };
        let fixture = node_compat_fixture_dir();
        assert!(
            fixture.join("capsule.toml").is_file(),
            "missing NodeCompat fixture at {}",
            fixture.display()
        );

        // Hermetic state: a throwaway ATO_HOME/HOME and unroutable Store/GitHub
        // bases so the dispatched run never touches the developer's real ~/.ato
        // or the network. Mirrors tests/installed_relaunch_port_remap_e2e.rs.
        let scratch = tempfile::tempdir().expect("temp scratch");
        let ato_home = scratch.path().join("ato-home");
        let home = scratch.path().join("home");
        std::fs::create_dir_all(&ato_home).expect("create ato_home");
        std::fs::create_dir_all(&home).expect("create home");

        // Spawn the dispatched child with the EXACT shape `spawn_run_child`
        // builds — `ato run <path> --sandbox -y` in its own process group, with
        // piped stdout/stderr — plus the operator-host `--nacelle` extra the
        // runner would inject via ATO_RUNNER_RUN_ARGS.
        let mut cmd = tokio::process::Command::new(&ato_bin);
        cmd.arg("run")
            .arg(&fixture)
            .arg("--sandbox")
            .arg("-y")
            .arg("--nacelle")
            .arg(&nacelle);
        cmd.env("ATO_HOME", &ato_home)
            .env("HOME", &home)
            .env("ATO_STORE_API_URL", "http://127.0.0.1:1")
            .env("ATO_GITHUB_API_BASE_URL", "http://127.0.0.1:1")
            .env("ATO_TELEMETRY", "0")
            .env("NACELLE_PATH", &nacelle);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Lead a new process group (pgid == child pid) so a hard test teardown
        // can reap the whole `ato run` → nacelle → bwrap → node subtree, exactly
        // as the runner's `spawn_run_child` does.
        cmd.process_group(0);
        let child = cmd
            .spawn()
            .expect("spawn real `ato run --sandbox -y` child");

        let log_dir = tempfile::tempdir().expect("log tempdir");
        let log_path = log_dir.path().join("run.log");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<LeaseReport>();
        // 90s ceiling: long enough for a cold toolchain provision on a real host,
        // far below the 600s production ready deadline this regression is about.
        let monitor = tokio::spawn(run_lease_child(
            child,
            log_path.clone(),
            Duration::from_secs(90),
            tx,
        ));

        let report = tokio::time::timeout(Duration::from_secs(120), rx.recv())
            .await
            .expect("the runner must settle the lease before the 120s test ceiling")
            .expect("run_lease_child always emits a decisive report before returning");
        monitor.abort();

        match report {
            LeaseReport::Ready { execution_id, port } => {
                // The exact outcome that hung before #693: the dispatched
                // foreground NodeCompat run emitted `LIFECYCLE: ready port=N`
                // (consumed here) instead of timing out at the ready deadline.
                assert_eq!(
                    port,
                    Some(18880),
                    "the dispatched NodeCompat run must report ready on its declared port 18880"
                );
                assert!(
                    !execution_id.is_empty(),
                    "a ready lease must carry the receipt-derived execution_id"
                );
            }
            LeaseReport::Failed { code, message } => {
                let tail = std::fs::read_to_string(&log_path).unwrap_or_default();
                panic!(
                    "NodeCompat dispatch must reach ready, not fail ({code}: {message})\nrun log:\n{tail}"
                );
            }
            other => panic!("expected LeaseReport::Ready on the declared port, got {other:?}"),
        }
    }
}

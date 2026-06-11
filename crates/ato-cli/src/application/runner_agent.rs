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
    proxy_listen: Option<String>,
) -> Result<()> {
    let proxy_listen = proxy_listen.unwrap_or_else(|| DEFAULT_PROXY_LISTEN.to_string());
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

    // Settle leases orphaned by a previous runner process BEFORE the first
    // heartbeat/claim, so stop-requested zombies clear immediately on boot.
    reconcile_open_leases(&client, &api_base, &creds.runner_id, &creds.runner_token).await;

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
                        public_base_url.clone(),
                        proxy_listen.clone(),
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
/// After a port-LESS readiness signal (the human "[✓] ready" echo), hold this
/// long for the canonical "LIFECYCLE: ready port=N" line — they race on separate
/// streams and the port line usually lands within a line or two. Without this,
/// whichever wins decides whether the proxy + ready_url come up.
const READY_PORT_GRACE: Duration = Duration::from_millis(2500);
/** Cap per-run log files so a chatty child cannot fill the disk. */
const MAX_RUN_LOG_BYTES: usize = 2 * 1024 * 1024;

pub const LEASE_COMMAND_KIND: &str = "run_source_sandbox";
pub const DEFAULT_PROXY_LISTEN: &str = "127.0.0.1:8420";

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

/// Parsed payload of a `CONSENT-REQUIRED: <json>` line from `ato run` (P4-A).
/// The full identity 5-tuple is the decision contract; `consent_ref` is its
/// hash (blake3(JCS(schema + 5-tuple))). The runner reports this as
/// needs_consent and, only after the owner approves this exact `consent_ref`,
/// calls the local `approve-execution-plan` primitive and retries.
// All fields are required (no serde defaults): a CONSENT-REQUIRED line missing
// any field fails to deserialize and is NOT treated as a consent signal — an
// incomplete signal must never be reported to the control plane.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ConsentRequest {
    pub schema: String,
    pub consent_ref: String,
    pub scoped_id: String,
    pub version: String,
    pub target_label: String,
    pub policy_segment_hash: String,
    pub provisioning_policy_hash: String,
    /// Human policy summary. Must be PRESENT (may be empty); a missing summary
    /// fails to deserialize.
    pub summary: String,
}

impl ConsentRequest {
    /// A consent signal is honored only if it is complete AND well-formed: the
    /// exact schema, a blake3 consent_ref, a non-empty identity, and blake3
    /// policy hashes. Anything less is not a valid consent gate.
    fn is_valid(&self) -> bool {
        self.schema == capsule_core::execution_plan::canonical::CONSENT_REF_SCHEMA
            && self.consent_ref.starts_with("blake3:")
            && !self.scoped_id.is_empty()
            && !self.version.is_empty()
            && !self.target_label.is_empty()
            && self.policy_segment_hash.starts_with("blake3:")
            && self.provisioning_policy_hash.starts_with("blake3:")
    }
}

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
            if let Some(tail) = read_run_log_tail(lease_id) {
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
/// full local chain is proven: public_base_url configured AND the observed
/// workload port is known AND the local root proxy actually came up.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadyPayload {
    pub execution_id: String,
    pub ready_url: Option<String>,
    pub local_port: Option<u16>,
}

/// Decide the /ready payload from what was actually observed/achieved.
/// `proxy_started` is the result of the proxy bring-up attempt (None = not
/// attempted because base or port was missing).
pub fn decide_ready_payload(
    execution_id: String,
    public_base_url: Option<&str>,
    port: Option<u16>,
    proxy_started: Option<bool>,
) -> ReadyPayload {
    let ready_url = match (public_base_url, port, proxy_started) {
        (Some(base), Some(_), Some(true)) => Some(format!("{}/", base.trim_end_matches('/'))),
        // Missing base, unknown port, or a proxy that failed to start: a URL
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
    // Refuse to come up if the upstream is not actually accepting — a proxy
    // in front of nothing would make ready_url a lie.
    tokio::net::TcpStream::connect(("127.0.0.1", workload_port))
        .await
        .with_context(|| format!("workload 127.0.0.1:{workload_port} is not accepting"))?;

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind proxy listener on {listen}"))?;
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let Ok(mut upstream) =
                    tokio::net::TcpStream::connect(("127.0.0.1", workload_port)).await
                else {
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
        "scoped_id": request.scoped_id,
        "version": request.version,
        "target_label": request.target_label,
        "policy_segment_hash": request.policy_segment_hash,
        "provisioning_policy_hash": request.provisioning_policy_hash,
        "summary": request.summary,
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
    let three_way_match = recomputed_ref == child_ref
        && recomputed_ref == approved_ref
        && approved_ref == child_ref;
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

/// Startup reconciliation: a freshly started serve process runs ZERO
/// workloads. Dispatched children live in this process's process group
/// (and under systemd, its cgroup), so they did not survive the previous
/// process's exit — "nothing is running here" is honest runner evidence,
/// not an API fabrication. For every lease the API still considers open on
/// this runner, ack /stopped with reason `runner_restarted` so zombie
/// stop-requested runs settle and the control plane matches reality.
/// INVARIANT this leans on: spawn_run_child keeps workloads in our process
/// group / unit cgroup. If a future runner detaches workloads, this ack is
/// no longer honest and must be gated on actual process evidence.
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
    // This process owns no workloads, so the slot is free and the previous
    // children are gone: full-cleanup evidence.
    let cleanup = StopCleanup::from_teardown(true, true);
    for lease_id in lease_ids {
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
            Ok(()) => println!("   ✓ lease {lease_id}: acked stopped (runner_restarted)"),
            Err(err) => eprintln!("   ⚠️ lease {lease_id}: reconcile ack failed: {err}"),
        }
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

/// True while any process in `pid`'s group still exists (signal 0 probes
/// without delivering). The production teardown gates on the monitor reaping
/// the child, not on this — zombies linger in the group until reaped — but it
/// is a precise check for tests.
#[cfg(all(unix, test))]
fn process_group_alive(pid: u32) -> bool {
    unsafe { libc::kill(-(pid as libc::pid_t), 0) == 0 }
}

/// Terminate the workload's process group and wait for it to be reaped.
/// SIGTERM first (let the app shut down cleanly), escalate to SIGKILL after a
/// bounded grace, and confirm via the monitor task draining the child's output
/// and reaping it. Returns true only when termination is confirmed; an
/// unconfirmable outcome returns false so the caller can fail closed.
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
        // Polite first: SIGTERM the whole group so the app can shut down.
        let _ = kill_group(pid, libc::SIGTERM);
        tokio::select! {
            _ = &mut monitor => return true,
            _ = tokio::time::sleep(STOP_GRACE) => {}
        }
        // Still alive after the grace window: force-kill the group.
        let _ = kill_group(pid, libc::SIGKILL);
        // SIGKILL cannot be caught; the monitor should reap promptly. If it
        // still does not return, we cannot confirm termination — fail closed.
        (tokio::time::timeout(STOP_KILL_GRACE, &mut monitor).await).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = child_pid;
        // No process groups: abort the monitor; kill_on_drop reaps the direct
        // child when its handle drops. Best effort on non-Unix hosts.
        monitor.abort();
        let _ = monitor.await;
        true
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
    busy: &Arc<AtomicBool>,
) {
    println!("🛑 lease {lease_id}: owner requested stop; tearing down workload");

    let process_terminated = terminate_child_group(child_pid, monitor).await;

    // Abort the proxy AND wait for the listener task to actually end before
    // claiming it stopped — the upstream is already dead, so in-flight
    // connections drain on their own once the listener is gone. A run that never
    // brought a proxy up is vacuously stopped; an abort we cannot confirm is
    // reported false (→ slot held).
    let proxy_stopped = stop_proxy(proxy_handle).await;

    let cleanup = StopCleanup::from_teardown(process_terminated, proxy_stopped);

    // Free the single slot ONLY on a fully confirmed teardown. If we cannot
    // confirm the workload is gone, stay busy (fail closed) rather than offer a
    // slot a possibly-live workload still occupies.
    if cleanup.slot_released {
        busy.store(false, Ordering::SeqCst);
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

/// Bytes of the child log tail attached to a failure report (well within the
/// API's 16 KiB cap; the file itself is already bounded at MAX_RUN_LOG_BYTES).
const RUN_LOG_TAIL_BYTES: usize = 12 * 1024;

/// Read the tail of a lease's run log, scrubbed, for remote failure triage.
/// Returns None when the log is absent/empty. A partial leading line (from
/// cutting mid-file) is dropped so the excerpt starts on a line boundary.
fn read_run_log_tail(lease_id: &str) -> Option<String> {
    read_run_log_tail_at(&run_log_path(lease_id))
}

fn read_run_log_tail_at(path: &std::path::Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    let start = data.len().saturating_sub(RUN_LOG_TAIL_BYTES);
    let text = String::from_utf8_lossy(&data[start..]);
    let text = if start > 0 {
        match text.find('\n') {
            Some(i) => &text[i + 1..],
            None => text.as_ref(),
        }
    } else {
        text.as_ref()
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        // The log writer already scrubs each line; scrub again defensively in
        // case a future caller logs a raw token.
        Some(scrub_secrets(trimmed))
    }
}

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

async fn handle_claimed_lease(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    lease: LeaseDto,
    busy: Arc<AtomicBool>,
    public_base_url: Option<String>,
    proxy_listen: String,
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

    let log_path = run_log_path(&lease.id);

    // The slot is held for the whole gated lifetime (preparing → optional
    // needs_consent → running), released only on settle / stop / terminal.
    busy.store(true, Ordering::SeqCst);
    let client = client.clone();
    let api_base = api_base.to_string();
    let runner_token = runner_token.to_string();
    let lease_id = lease.id.clone();
    let source_url = command.source_url.clone();
    tokio::spawn(async move {
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
            let child = match spawn_run_child(&source_url) {
                Ok(child) => child,
                Err(err) => {
                    let report = LeaseReport::Failed {
                        code: "spawn_failed".to_string(),
                        message: format!("{err:#}"),
                    };
                    let _ =
                        report_lease_status(&client, &api_base, &runner_token, &lease_id, &report)
                            .await;
                    control.abort();
                    busy.store(false, Ordering::SeqCst);
                    return;
                }
            };
            // PID == process-group id (see spawn_run_child); a stop signals the
            // whole workload group.
            let child_pid = child.id();
            println!(
                "🚀 lease {lease_id}: ato run {source_url} --sandbox (round {round}, log: {})",
                log_path.display()
            );
            let (report_tx, mut report_rx) =
                tokio::sync::mpsc::unbounded_channel::<LeaseReport>();
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
                        child_pid, monitor, None, &busy,
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
                control.abort();
                busy.store(false, Ordering::SeqCst);
                return;
            };

            let LeaseReport::ConsentRequired(request) = first else {
                // Running / Ready / Failed: the child is past the gate — hand off
                // to the run phase with the still-running monitor.
                break (report_rx, monitor, child_pid, first);
            };

            // The child exited at the gate; reap its monitor before waiting.
            let _ = monitor.await;
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
                let _ =
                    report_lease_status(&client, &api_base, &runner_token, &lease_id, &report).await;
                control.abort();
                busy.store(false, Ordering::SeqCst);
                println!("🔓 lease {lease_id}: slot released");
                return;
            }
            // Park needs_consent with the full 5-tuple for owner approval.
            if let Err(err) =
                report_consent_required(&client, &api_base, &runner_token, &lease_id, &request).await
            {
                eprintln!(
                    "⚠️  lease {lease_id}: consent-required report failed: {}",
                    scrub_secrets(&format!("{err:#}"))
                );
                let report = LeaseReport::Failed {
                    code: "consent_report_failed".to_string(),
                    message: "could not surface the consent gate to the control plane".to_string(),
                };
                let _ =
                    report_lease_status(&client, &api_base, &runner_token, &lease_id, &report).await;
                control.abort();
                busy.store(false, Ordering::SeqCst);
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
                busy.store(false, Ordering::SeqCst);
                println!("🔓 lease {lease_id}: slot released (no workload was running)");
                return;
            };

            // INVARIANT: write the host-local ledger ONLY when the child's
            // emitted ref, the recomputed ref (from the child's 5-tuple), and the
            // owner's approved ref ALL agree. A recompute error is unrecoverable
            // (cannot verify) → fail closed.
            let recomputed = match capsule_core::execution_plan::canonical::consent_ref_from_parts(
                &request.scoped_id,
                &request.version,
                &request.target_label,
                &request.policy_segment_hash,
                &request.provisioning_policy_hash,
            ) {
                Ok(value) => value,
                Err(err) => {
                    eprintln!("⚠️  lease {lease_id}: consent_ref recompute failed: {err}");
                    let report = LeaseReport::Failed {
                        code: "consent_verification_failed".to_string(),
                        message: "could not recompute the consent reference".to_string(),
                    };
                    let _ = report_lease_status(
                        &client, &api_base, &runner_token, &lease_id, &report,
                    )
                    .await;
                    control.abort();
                    busy.store(false, Ordering::SeqCst);
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
            if let Err(err) = crate::application::auth::consent_store::approve_execution_plan_consent(
                &request.scoped_id,
                &request.version,
                &request.target_label,
                &request.policy_segment_hash,
                &request.provisioning_policy_hash,
            ) {
                eprintln!("⚠️  lease {lease_id}: local consent record failed: {err}");
                let report = LeaseReport::Failed {
                    code: "consent_local_record_failed".to_string(),
                    message: "could not record the approved consent locally".to_string(),
                };
                let _ =
                    report_lease_status(&client, &api_base, &runner_token, &lease_id, &report).await;
                control.abort();
                busy.store(false, Ordering::SeqCst);
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
                    // Bring the root proxy up BEFORE claiming a URL; a proxy
                    // that failed (or was never attempted) means ready is
                    // reported without ready_url — never a fabricated one.
                    let proxy_started = match (public_base_url.as_deref(), port) {
                        (Some(_), Some(workload_port)) => {
                            match start_root_proxy(&proxy_listen, workload_port).await {
                                Ok(handle) => {
                                    proxy_handle = Some(handle);
                                    println!(
                                        "🔀 lease {lease_id}: root proxy {} -> 127.0.0.1:{}",
                                        proxy_listen, workload_port
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
                        public_base_url.as_deref(),
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
                &busy,
            )
            .await;
        } else {
            // Natural settle/exit: the child ran to completion on its own.
            let _ = monitor.await;
            if let Some(handle) = proxy_handle {
                handle.abort();
            }
            busy.store(false, Ordering::SeqCst);
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
                assert_eq!(req.scoped_id, "community/hello-capsule");
                assert_eq!(req.version, "0.3.0");
                assert_eq!(req.target_label, "main");
                assert_eq!(req.policy_segment_hash, "blake3:p");
                assert_eq!(req.provisioning_policy_hash, "blake3:q");
                assert!(req.summary.contains("api.example.com"));
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
        // Full proof: base + port + proxy up -> root URL.
        let payload =
            decide_ready_payload(id(), Some("https://r.example.com"), Some(8000), Some(true));
        assert_eq!(payload.ready_url.as_deref(), Some("https://r.example.com/"));
        assert_eq!(payload.local_port, Some(8000));

        // No public base -> no URL.
        assert_eq!(
            decide_ready_payload(id(), None, Some(8000), None).ready_url,
            None
        );
        // Port unknown -> no URL.
        assert_eq!(
            decide_ready_payload(id(), Some("https://r.example.com"), None, None).ready_url,
            None
        );
        // Proxy failed to start -> no URL (never fabricate reachability).
        assert_eq!(
            decide_ready_payload(id(), Some("https://r.example.com"), Some(8000), Some(false))
                .ready_url,
            None
        );
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
            schema: capsule_core::execution_plan::canonical::CONSENT_REF_SCHEMA.to_string(),
            consent_ref: "blake3:ref".to_string(),
            scoped_id: "community/hello-capsule".to_string(),
            version: "0.3.0".to_string(),
            target_label: "main".to_string(),
            policy_segment_hash: "blake3:seg".to_string(),
            provisioning_policy_hash: "blake3:prov".to_string(),
            summary: "network: api.example.com\nfs-rw: /data".to_string(),
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
            assert!(captured.contains(needle), "body missing {needle}: {captured}");
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
        let (base, server) =
            one_shot_http("HTTP/1.1 200 OK", "{\"stop_requested\":true,\"consent\":null}");
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
        assert!(request.contains("authorization: Bearer ato_rnr_t") || request.contains("Authorization: Bearer ato_rnr_t"));
    }

    #[test]
    fn log_tail_keeps_the_end_on_a_line_boundary_and_scrubs() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("ato-logtail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.log");
        let mut f = std::fs::File::create(&path).unwrap();
        // Far more than RUN_LOG_TAIL_BYTES so the head is dropped.
        for i in 0..4000 {
            writeln!(f, "line {i} ato_rnr_secrettokenvalue padding-padding-padding").unwrap();
        }
        writeln!(f, "FINAL ModuleNotFoundError: No module named 'uvicorn'").unwrap();
        drop(f);

        let tail = read_run_log_tail_at(&path).expect("tail");
        assert!(tail.len() <= RUN_LOG_TAIL_BYTES);
        // The most recent, diagnostic line survives.
        assert!(tail.contains("ModuleNotFoundError"));
        // Head was cut → the excerpt starts on a clean line boundary.
        assert!(tail.starts_with("line "));
        // Tokens are scrubbed, never echoed in the excerpt.
        assert!(!tail.contains("secrettokenvalue"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn log_tail_is_none_for_absent_or_empty_log() {
        let missing = std::env::temp_dir().join("ato-logtail-does-not-exist.log");
        assert!(read_run_log_tail_at(&missing).is_none());
        let dir = std::env::temp_dir().join(format!("ato-logtail-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.log");
        std::fs::write(&empty, b"   \n  \n").unwrap();
        assert!(read_run_log_tail_at(&empty).is_none());
        std::fs::remove_dir_all(&dir).ok();
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
}

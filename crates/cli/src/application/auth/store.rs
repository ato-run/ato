use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use rand::RngCore;
use rand::rngs::OsRng;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

use super::prompt::try_open_browser;
use super::publisher::{
    PublisherMeResponse, fetch_publisher_me_blocking, run_publisher_onboarding_flow,
};
use super::storage::{TokenStorageLocation, merge_metadata};
use super::{
    AuthManager, Credentials, DEFAULT_STORE_API_URL, DEFAULT_STORE_SITE_URL, ENV_STORE_API_URL,
    ENV_STORE_SITE_URL, read_env_non_empty,
};

#[derive(Debug, Deserialize)]
struct BridgeInitResponse {
    session_id: String,
    user_code: String,
    expires_in: u64,
    #[serde(default)]
    poll_interval_sec: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct BridgePollResponse {
    code: String,
    #[serde(default)]
    poll_interval_sec: Option<u64>,
    #[serde(default)]
    auth_code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BridgeExchangeResponse {
    access_token: String,
    #[serde(default)]
    handle: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RetryAfterResponse {
    retry_after: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(super) struct StoreSessionUser {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StoreSessionResponse {
    #[serde(default)]
    user: Option<StoreSessionUser>,
}

#[derive(Debug, Serialize)]
struct DesktopAuthHandoffResponse<'a> {
    session_token: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    publisher_handle: Option<String>,
    site_base_url: String,
    api_base_url: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct StoreErrorResponse {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

pub fn current_session_token() -> Option<String> {
    let auth = AuthManager::new().ok()?;
    auth.resolve_session_token().ok().flatten()
}

pub fn require_session_token() -> Result<String> {
    let auth = AuthManager::new()?;
    let Some(token) = auth.resolve_session_token()? else {
        anyhow::bail!(
            "Authentication required. Run `ato login` again, or set `ATO_TOKEN` for this shell."
        );
    };
    Ok(token)
}

pub fn current_publisher_handle() -> Result<Option<String>> {
    let manager = AuthManager::new()?;
    Ok(
        hydrate_publisher_identity_with(&manager, fetch_publisher_me_blocking)?
            .and_then(|creds| cached_publisher_handle(&creds)),
    )
}

pub fn default_store_registry_url() -> String {
    store_api_base_url()
}

/// Returns the canonical human-readable base URL for share display links
/// (e.g., `https://ato.run`). Respects `ATO_STORE_SITE_URL` override.
pub(crate) fn share_display_base_url() -> String {
    store_site_base_url()
}

fn trim_trailing_slash(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

fn to_base64_url(bytes: &[u8]) -> String {
    BASE64_STANDARD
        .encode(bytes)
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string()
}

fn generate_pkce_verifier() -> String {
    let mut bytes = [0u8; 64];
    OsRng.fill_bytes(&mut bytes);
    to_base64_url(&bytes)
}

fn generate_pkce_challenge_s256(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    to_base64_url(&hasher.finalize())
}

pub(crate) fn store_api_base_url() -> String {
    trim_trailing_slash(
        &read_env_non_empty(ENV_STORE_API_URL).unwrap_or_else(|| DEFAULT_STORE_API_URL.to_string()),
    )
}

pub(crate) fn store_site_base_url() -> String {
    trim_trailing_slash(
        &read_env_non_empty(ENV_STORE_SITE_URL)
            .unwrap_or_else(|| DEFAULT_STORE_SITE_URL.to_string()),
    )
}

pub(super) fn is_local_store_api_base_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn fetch_store_session_user(session_token: &str) -> Result<Option<StoreSessionUser>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(format!("{}/api/auth/session", store_api_base_url()))
        .header("Accept", "application/json")
        .header("Cookie", store_session_cookie_header(session_token))
        .send()
        .context("Failed to fetch Store session")?;

    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        return Ok(None);
    }

    if !response.status().is_success() {
        anyhow::bail!("Store session lookup failed (HTTP {})", response.status());
    }

    let body = response
        .json::<StoreSessionResponse>()
        .context("Failed to parse Store session response")?;

    Ok(body.user)
}

pub(super) fn store_session_cookie_header(session_token: &str) -> String {
    format!(
        "better-auth.session_token={}; __Secure-better-auth.session_token={}",
        session_token, session_token
    )
}

pub fn desktop_auth_handoff() -> Result<()> {
    let session_token = require_session_token()?;
    if fetch_store_session_user(&session_token)?.is_none() {
        anyhow::bail!("Store session is expired or unavailable. Run `ato login` again.");
    }

    let manager = AuthManager::new()?;
    let publisher_handle = manager
        .load()?
        .and_then(|creds| cached_publisher_handle(&creds));
    let response = DesktopAuthHandoffResponse {
        session_token: &session_token,
        publisher_handle,
        site_base_url: store_site_base_url(),
        api_base_url: store_api_base_url(),
    };

    serde_json::to_writer(std::io::stdout(), &response)
        .context("Failed to write desktop auth handoff JSON")?;
    println!();
    Ok(())
}

fn cached_publisher_handle(creds: &Credentials) -> Option<String> {
    creds.publisher_handle.as_ref().and_then(|handle| {
        let trimmed = handle.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub(super) fn hydrate_publisher_identity_with<F>(
    manager: &AuthManager,
    fetcher: F,
) -> Result<Option<Credentials>>
where
    F: FnOnce(&str) -> Result<Option<PublisherMeResponse>>,
{
    let mut creds = manager.load()?.unwrap_or_default();
    if cached_publisher_handle(&creds).is_some() {
        return Ok(Some(creds));
    }

    let Some(session_token) = manager.resolve_session_token()? else {
        return Ok(None);
    };

    let Some(me) = fetcher(&session_token)? else {
        return Ok(None);
    };

    super::publisher::merge_publisher_identity(&mut creds, &me);
    manager.save(&creds)?;
    Ok(Some(creds))
}

pub(super) fn parse_store_error_text(body: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<StoreErrorResponse>(body) {
        match (parsed.error, parsed.message) {
            (Some(error), Some(message)) if !message.is_empty() => {
                return format!("{}: {}", error, message);
            }
            (Some(error), _) => return error,
            (_, Some(message)) if !message.is_empty() => return message,
            _ => {}
        }
    }
    body.trim().to_string()
}

/// Shared poll-timing derivation for every bridge device-code flow variant
/// (plain interactive `ato login` via `bridge_authenticate_ephemeral`, and
/// the Desktop path in `login_with_store_device_flow_desktop`). Extracted
/// into one pure, unit-tested function so the two call sites cannot
/// silently diverge — two independent copies of this arithmetic, one of
/// them unreachable while the fail-closed gate is shut, is exactly the
/// kind of drift a later refactor could introduce without either call
/// site's tests (or lack thereof) ever noticing (round-4 review finding,
/// ato#1077: "confirmation that the poll loop is truly byte-for-byte
/// unchanged").
pub(super) fn compute_poll_timing(expires_in: u64, poll_interval_sec: Option<u64>) -> (u64, u64) {
    let poll_timeout_secs = expires_in.min(300);
    let poll_interval_secs = poll_interval_sec.unwrap_or(2).max(1);
    (poll_timeout_secs, poll_interval_secs)
}

/// Pure construction of the `desktop_browser_launch_failed` NDJSON payload
/// (user-facing message text + the JSON event), split out of the call site
/// in `login_with_store_device_flow_desktop` so the exact schema can be
/// unit-tested directly. Round-4 review finding (Major, test-coverage):
/// while `EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED` is `false` the call
/// site itself never runs in any test — this function is the one piece of
/// that new logic that can be, and now is, exercised without depending on
/// the gate or on an actual OS browser launch.
pub(super) fn browser_launch_failed_event(
    login_url: &str,
    error: &anyhow::Error,
) -> (String, serde_json::Value) {
    let message = format!("Could not open your browser automatically: {}", error);
    let event = serde_json::json!({
        "type": "desktop_browser_launch_failed",
        "login_url": login_url,
        "message": message,
    });
    (message, event)
}

/// Pure construction of the terminal `desktop_login_completed` NDJSON payload
/// emitted once the bridge exchange succeeds.
///
/// Extracted from the call site in `login_with_store_device_flow_desktop` so
/// its exact shape is unit-testable, and — the reason it is worth a dedicated
/// function and test — to pin the invariant that the one success signal
/// crossing the CLI→Desktop boundary carries only the publisher *handle* and
/// the storage *location label*, never the session/device credential itself.
/// The exchanged access token is persisted to the age/credentials store and
/// must never travel over stdout into the Dock (which forwards these events
/// into user-facing toasts and `tracing` logs). A future edit that "helpfully"
/// added the token to this event would be caught by
/// `desktop_login_completed_event_never_carries_a_token` in tests.rs.
pub(super) fn desktop_login_completed_event(
    publisher_handle: Option<&str>,
    storage: &str,
) -> serde_json::Value {
    serde_json::json!({
        "type": "desktop_login_completed",
        "publisher_handle": publisher_handle,
        "storage": storage,
    })
}

/// Splits a raw bridge-auth failure — carrying the live HTTP status code
/// and ato-api's raw response body — into a short, generic `message` meant
/// for the Dock's user-facing toast, and a `detail` string that keeps the
/// full raw text for `eprintln!`/logs only.
///
/// `on_login_completion` (ato-desktop `mod.rs`) forwards a
/// `desktop_login_failed` event's `message` field verbatim into a Dock
/// toast. Unlike the fail-closed gate above (`desktop_login_gate_messages`,
/// which already keeps this split), the ordinary poll/exchange/init
/// failure paths in `login_with_store_device_flow_desktop` used to put raw
/// ato-api response text straight into that field — a new, unreviewed
/// exposure surface the embedded-WebView flow this PR replaces never had
/// (round-4 review finding, Major, information-disclosure-ux). Nothing
/// proves today's ato-api error bodies are sensitive, but there was no
/// sanitization boundary here to rely on that they never will be; `detail`
/// is carried alongside `message` in the NDJSON event so ato-desktop can
/// still log the full diagnostic via `tracing::warn!` without ever putting
/// it in front of the user.
pub(super) fn sanitize_bridge_failure(user_summary: &str, detail: String) -> (String, String) {
    let user_message = format!("{user_summary} Run `ato login` from a terminal for more detail.");
    (user_message, detail)
}

#[allow(clippy::needless_return)]
pub async fn login_with_store_device_flow(headless: bool) -> Result<()> {
    // Bootstrap the age identity before the browser flow. Without an identity
    // the session token can only be held in memory and will not survive the
    // process — we'd rather the user deal with the one-time prompt here than
    // complete the browser handshake and discover afterward that nothing was
    // persisted. Headless mode writes to the canonical TOML file directly and
    // does not need age, so we skip the prompt there.
    if !headless
        && let Err(error) =
            tokio::task::spawn_blocking(crate::application::secrets::ensure_identity_interactive)
                .await
                .context("failed to run age identity bootstrap")?
    {
        eprintln!("⚠️  Skipping age identity bootstrap: {error}");
        eprintln!("   Login will proceed, but the token may not persist across sessions.");
    }

    let api_base = store_api_base_url();
    let site_base = store_site_base_url();
    let bridge = bridge_authenticate_ephemeral(&api_base, &site_base, headless).await?;
    let session_token = bridge.access_token;

    let manager = AuthManager::new()?;
    let storage = manager
        .persist_session_token(session_token.clone(), headless)
        .await?;
    let mut creds = manager.load()?.unwrap_or_default();
    creds.publisher_handle = bridge.handle.clone();
    if headless {
        let mut persisted = manager.load_canonical_credentials()?.unwrap_or_default();
        persisted.session_token = Some(session_token.clone());
        merge_metadata(&mut persisted, &creds);
        manager.write_canonical_credentials(&persisted)?;
    }

    let session_token_for_setup = session_token.clone();
    println!("🧪 Running publisher onboarding...");
    let onboarding = run_publisher_onboarding_flow(
        &session_token_for_setup,
        creds.github_username.as_deref(),
        false,
    )
    .await?;
    creds.publisher_id = Some(onboarding.publisher_id);
    creds.publisher_handle = Some(onboarding.publisher_handle);
    creds.publisher_did = Some(onboarding.publisher_did);
    if let Some(installation) = onboarding.installation {
        creds.github_app_installation_id = Some(installation.installation_id);
        creds.github_app_account_login = Some(installation.account_login);
    }
    if headless {
        let mut persisted = manager.load_canonical_credentials()?.unwrap_or_default();
        persisted.session_token = Some(session_token.clone());
        merge_metadata(&mut persisted, &creds);
        manager.write_canonical_credentials(&persisted)?;
    }

    println!("✅ Login completed successfully");
    if let Some(handle) = creds.publisher_handle.as_deref() {
        println!("   Publisher: {}", handle);
    }
    if let Some(id) = creds.github_app_installation_id {
        println!("   GitHub App Installation: {}", id);
    }
    match storage {
        TokenStorageLocation::AgeFile => {
            println!(
                "   Store session saved to: {} ({})",
                storage.display(),
                manager
                    .age_home
                    .join(".ato/credentials/auth/session.age")
                    .display()
            );
        }
        TokenStorageLocation::CanonicalFile => {
            println!(
                "   Store session saved to: {:?}",
                manager.credentials_path()
            );
        }
        TokenStorageLocation::Memory => {
            println!("   Store session saved to: {}", storage.display());
            println!(
                "   ⚠️  Token will not survive this process. Re-run `ato login` in an interactive shell, or run `ato secrets init` to create an age identity."
            );
        }
    }
    if headless {
        println!("   Metadata file: {:?}", manager.credentials_path());
    }
    Ok(())
}

/// In-memory result of a Store bridge authentication.
pub(crate) struct BridgeSessionToken {
    pub access_token: String,
    pub handle: Option<String>,
}

/// Run the Store bridge (device-code style) authentication WITHOUT persisting
/// anything: init → show user_code/URL → poll → exchange. Returns the session
/// token in memory only.
///
/// Used by `ato login` (which then persists the token and runs publisher
/// onboarding) and by `ato runner login` (which uses the token exactly once to
/// register the runner device and then discards it — a runner host must never
/// hold a long-lived user session).
pub(crate) async fn bridge_authenticate_ephemeral(
    api_base: &str,
    site_base: &str,
    headless: bool,
) -> Result<BridgeSessionToken> {
    let client = reqwest::Client::new();
    let code_verifier = generate_pkce_verifier();
    let code_challenge = generate_pkce_challenge_s256(&code_verifier);

    let start_response = client
        .post(format!("{}/v1/auth/bridge/init", api_base))
        .json(&serde_json::json!({
            "code_challenge": code_challenge,
            "method": "S256",
            "device_info": format!("ato-cli/{}", env!("CARGO_PKG_VERSION")),
        }))
        .send()
        .await
        .with_context(|| "Failed to start Store bridge authentication")?;

    if !start_response.status().is_success() {
        let status = start_response.status();
        let body = start_response.text().await.unwrap_or_default();
        let mut message = format!("Bridge auth init failed ({}): {}", status, body);
        if status.is_server_error() && is_local_store_api_base_url(api_base) {
            message.push_str(
                "\nLocal ato-store may be missing DB migrations. Run `pnpm -C apps/ato-store db:migrate` and restart `pnpm -C apps/ato-store dev`.",
            );
        }
        anyhow::bail!(message);
    }

    let start: BridgeInitResponse = start_response
        .json()
        .await
        .context("Invalid bridge auth init response")?;

    let session_id = start.session_id.clone();
    let activate_url = format!(
        "{}/v1/auth/bridge/activate?session_id={}",
        api_base, session_id
    );

    let login_url = format!(
        "{}/auth?next={}",
        site_base,
        urlencoding::encode(&activate_url)
    );

    if headless {
        println!("🧩 Headless login mode");
        println!("   Open this URL on another authenticated browser session:");
        println!("   {}", login_url);
        println!("🔑 Verification code: {}", start.user_code);
        println!("⏳ Waiting for remote approval...");
    } else {
        println!("🌐 Opening browser for Ato sign-in...");
        println!("   URL: {}", login_url);
        println!("🔑 Verification code: {}", start.user_code);

        if let Err(error) = try_open_browser(&login_url) {
            eprintln!("⚠️  Could not open browser automatically: {}", error);
            eprintln!("   Open the URL manually to continue sign-in.");
        }

        println!("⏳ Waiting for browser authentication...");
    }

    let (poll_timeout_secs, mut poll_interval_secs) =
        compute_poll_timing(start.expires_in, start.poll_interval_sec);
    let started_at = Instant::now();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = client
                    .post(format!("{}/v1/auth/bridge/cancel", api_base))
                    .json(&serde_json::json!({
                        "session_id": &session_id,
                        "reason": "cli_interrupted",
                    }))
                    .send()
                    .await;
                anyhow::bail!("Authentication cancelled by user (Ctrl+C)");
            }
            _ = tokio::time::sleep(Duration::from_secs(poll_interval_secs)) => {}
        }

        if started_at.elapsed() >= Duration::from_secs(poll_timeout_secs) {
            anyhow::bail!(
                "Authentication timed out after {} seconds. Run the login command again.",
                poll_timeout_secs
            );
        }

        let poll_response = client
            .post(format!("{}/v1/auth/bridge/poll", api_base))
            .json(&serde_json::json!({
                "session_id": &session_id,
                "code_verifier": &code_verifier,
            }))
            .send()
            .await
            .with_context(|| "Failed to poll bridge authentication state")?;

        if poll_response.status() == StatusCode::TOO_MANY_REQUESTS {
            let body =
                poll_response
                    .json::<RetryAfterResponse>()
                    .await
                    .unwrap_or(RetryAfterResponse {
                        retry_after: Some(poll_interval_secs),
                    });
            let retry_after = body.retry_after.unwrap_or(poll_interval_secs).max(1);
            tokio::time::sleep(Duration::from_secs(retry_after)).await;
            continue;
        }

        if poll_response.status() == StatusCode::CONFLICT {
            anyhow::bail!("Authentication denied or cancelled. Run the login command again.");
        }

        if poll_response.status() == StatusCode::GONE {
            anyhow::bail!("Authentication expired. Run the login command again.");
        }

        if poll_response.status() == StatusCode::BAD_REQUEST {
            let body = poll_response.text().await.unwrap_or_default();
            anyhow::bail!("Authentication failed: {}", body);
        }

        if !poll_response.status().is_success() {
            let status = poll_response.status();
            let body = poll_response.text().await.unwrap_or_default();
            anyhow::bail!("Bridge auth poll failed ({}): {}", status, body);
        }

        let poll: BridgePollResponse = poll_response
            .json()
            .await
            .context("Invalid bridge auth poll response")?;

        match poll.code.as_str() {
            "PENDING" => {
                poll_interval_secs = poll.poll_interval_sec.unwrap_or(poll_interval_secs).max(1);
            }
            "SUCCESS" => {
                let auth_code = poll
                    .auth_code
                    .context("Bridge auth approved but no auth code was returned")?;

                let exchange_response = client
                    .post(format!("{}/v1/auth/bridge/exchange", api_base))
                    .json(&serde_json::json!({
                        "session_id": &session_id,
                        "auth_code": auth_code,
                        "code_verifier": &code_verifier,
                    }))
                    .send()
                    .await
                    .context("Failed to exchange bridge auth code")?;

                if !exchange_response.status().is_success() {
                    let status = exchange_response.status();
                    let body = exchange_response.text().await.unwrap_or_default();
                    anyhow::bail!("Bridge auth exchange failed ({}): {}", status, body);
                }

                let exchange: BridgeExchangeResponse = exchange_response
                    .json()
                    .await
                    .context("Invalid bridge auth exchange response")?;

                return Ok(BridgeSessionToken {
                    access_token: exchange.access_token,
                    handle: exchange.handle,
                });
            }
            other => {
                anyhow::bail!("Unexpected authentication status: {}", other);
            }
        }
    }
}

/// Release gate for the Desktop external-browser login path (ato#1077,
/// Auth Phase 1c).
///
/// This path routes Desktop login through the OS default browser instead of
/// the app-isolated embedded WebView it replaces. ato#1077 and RFC
/// ato-api#261 rev.3 §6 both state this MUST NOT ship ahead of ato-api#275
/// (Phase 1b: auth_bridge explicit-confirmation + exchange-time device
/// credential mint) actually landing on ato-api's `main` and being deployed
/// to production. Until then, the live `auth_bridge.ts` auto-approves
/// `GET /activate` for any already-signed-in browser and returns the
/// browser's own better-auth session token verbatim as the device
/// credential — widening exposure the moment a real OS browser (far more
/// likely than an app-scoped WebView to already hold a live session) is put
/// in front of it.
///
/// Round-3 review note (ato#1077): plain `ato login`'s existing non-headless
/// branch (`bridge_authenticate_ephemeral` below, reached from
/// `login_with_store_device_flow`) *already* opens a real OS browser against
/// these same `/v1/auth/bridge/*` endpoints today on `main` — it carries the
/// identical residual exposure described above, right down to "a real OS
/// browser is far more likely to already hold a live session." That is a
/// pre-existing, already-shipped gap, not one newly introduced or newly
/// accepted by this gate. Gating the plain-CLI path too is deliberately out
/// of scope here: ato#1077 is the Desktop entry point only, and widening this
/// PR to also change `login_with_store_device_flow`'s shipping behavior would
/// blow past that scope. This constant exists solely to stop *this PR* from
/// widening the exposure to the separate (larger, less technical) Desktop
/// app population before ato-api#275 lands — CLI and Desktop share the same
/// underlying bug and will both close it together the moment #275 deploys.
/// Concretely: the fallback copy in `desktop_login_gate_messages()` below
/// must never be read as a claim that the terminal path is *safe* — it is
/// simply the one login path that currently still functions, with a known,
/// separately-tracked gap of its own.
///
/// RELEASE-SEQUENCING NOTE (round-4 review findings, Major, reported
/// independently by two reviewers): this PR removes the embedded-WebView
/// Desktop login window in the same change that introduces this
/// permanently-`false` gate. Until ato-api#275 lands, deploys, AND this
/// flag is flipped, Desktop has **no working in-app sign-in at all** — only
/// the `ato login` terminal fallback named above, which the "separate,
/// larger, less technical Desktop app population" this PR itself calls out
/// is unlikely to know about or use. That is a deliberate, reasoned choice
/// (shipping the unhardened WebView-free gap is still strictly safer than
/// shipping the unhardened browser path), not a defect in this gate — but
/// it is a real functional regression relative to today's `main`, and it
/// must be an explicit, acknowledged release-sequencing decision (hold this
/// branch at `nightly`/`dev` until ato-api#275 is ready, or coordinate a
/// combined release with the follow-up flip commit) rather than something
/// that merges to `main` quietly. Do not treat this code comment as that
/// sign-off — it is a flag for whoever promotes this branch to get one.
///
/// Flip to `true` only once ato-api#275 has merged AND deployed; that's a
/// one-line follow-up commit, not a reason to relax this check speculatively.
///
/// Round-4 review finding (Major, cross-repo-dependency): a bare boolean
/// flip is a manual, unenforced obligation — nothing here or in CI stopped
/// a future contributor from flipping this the moment ato-api's PR merges
/// to *its* `main`, before that merge is actually deployed to ato-api
/// production (the two are different events, and #1077 depends on both).
/// `EXTERNAL_BROWSER_LOGIN_HARDENING_EVIDENCE` below is a second,
/// independent piece of state that must be edited in the *same* commit as
/// the flip, and `hardening_flag_requires_recorded_evidence_when_enabled`
/// (tests.rs) fails the suite — and therefore CI — if the flag is ever
/// `true` while the evidence string is still the `PENDING` sentinel. This
/// turns "flip only once landed AND deployed" from a comment a reviewer
/// has to trust into something CI actually checks.
pub(super) const EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED: bool = false;

/// Evidence backing the flag above: the merged ato-api commit/PR reference
/// for #275 plus confirmation it is deployed to ato-api production (e.g. a
/// deploy log timestamp, release tag, or incident-free-since note). Must
/// be replaced with real, checkable evidence in the *same* commit that
/// flips `EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED` to `true` — see
/// `hardening_flag_requires_recorded_evidence_when_enabled` in tests.rs,
/// which fails CI if that commit forgets to.
///
/// Only read by that test today (there is no other runtime check of this
/// value yet), so it is dead code outside `#[cfg(test)]` builds.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) const EXTERNAL_BROWSER_LOGIN_HARDENING_EVIDENCE: &str =
    "PENDING: ato-api#275 not yet merged to ato-api's main and deployed to ato-api production";

/// Message pair for the fail-closed gate above.
///
/// `.0` (developer detail) is safe for stderr / the process's exit error —
/// it may name internal tracking issues and is never seen by an end user.
/// `.1` (user-facing message) is what actually reaches a real person: it is
/// put verbatim into the `desktop_login_failed` NDJSON event's `message`
/// field, which `ato_dock::classify_ndjson_line` (ato-desktop) forwards
/// as-is into a Dock toast. A raw "disabled pending ato-api#275" paragraph
/// means nothing to that person and gives them no next step — round-2
/// review finding for ato#1077 — so the two are kept deliberately separate
/// here rather than reusing one string for both.
///
/// Round-3 review finding for ato#1077 (Major): the round-2 wording ("...to
/// sign in **instead**") read as though the terminal fallback were a safe
/// substitute for the very thing this gate blocks. It is not — see the
/// round-3 note on `EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED` above; plain
/// `ato login` already puts a real OS browser in front of the same
/// unhardened endpoint. The wording below deliberately avoids "instead" (or
/// any other framing that implies equivalence-in-safety) — it still names
/// `ato login` because that is the only login path that currently works and
/// withholding it would leave the user with no next step at all, but it
/// makes no claim about that path being risk-free.
///
/// Split out as a pure function (no I/O) so the "no jargon in the
/// user-facing half" contract can be unit-tested directly instead of
/// requiring a stdout-capturing integration test.
pub(super) fn desktop_login_gate_messages() -> (&'static str, &'static str) {
    (
        "desktop login disabled pending ato-api#275 (auth_bridge explicit-confirmation \
         + exchange-time device credential hardening, required by ato#1077); flip \
         EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED once that has merged AND deployed \
         to ato-api's production",
        "Sign-in from Ato Desktop isn't available in this version yet. Check for an \
         app update, or run `ato login` from a terminal in the meantime.",
    )
}

/// Login flow for `ato login --desktop`.
///
/// Used when ato-desktop spawns the CLI as a child process (no TTY, no
/// interactive stdin). Unlike the normal interactive flow, this:
/// - Does not prompt the user interactively.
/// - Auto-creates a passphrase-free age identity if none exists.
/// - Opens the OS default browser at `login_url` via `try_open_browser` —
///   the same helper the plain interactive flow uses (RFC 8252: OAuth for
///   native apps must use the system browser, not an app-embedded WebView).
/// - Emits NDJSON events on stdout so a caller without a TTY can watch the
///   flow to completion:
///   `{"type":"desktop_login_started", "login_url":"...", "user_code":"...", "expires_in":N, "poll_interval_sec":N}`
///   `{"type":"desktop_browser_launch_failed", "login_url":"...", "message":"..."}` (non-terminal)
///   `{"type":"desktop_login_completed", "publisher_handle":"...", "storage":"age_file"}`
///   `{"type":"desktop_login_failed", "message":"...", "detail":"..."}` (`detail` only present
///   for failures sourced from a live ato-api response; see `sanitize_bridge_failure` — `message`
///   is always safe for a Dock toast, `detail` is for logs only and must never be shown to a user)
/// - Exits with a non-zero code on failure.
///
/// Fails closed (see `EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED`) until the
/// bridge-hardening dependency has actually landed.
#[allow(clippy::needless_return)]
pub async fn login_with_store_device_flow_desktop() -> Result<()> {
    if !EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED {
        let (dev_detail, user_message) = desktop_login_gate_messages();
        eprintln!("[ato-desktop] {dev_detail}");
        println!(
            "{}",
            serde_json::json!({"type": "desktop_login_failed", "message": user_message})
        );
        anyhow::bail!("{}", dev_detail);
    }

    use crate::application::credential::AgeFileBackend;
    use capsule::common::paths::nacelle_home_dir;

    // ── Age identity bootstrap (non-interactive) ──────────────────────────────
    let ato_home = nacelle_home_dir().context("failed to resolve ato home")?;
    let age = AgeFileBackend::new(ato_home.clone());

    if age.identity_exists() {
        // Check whether it can be unlocked without a passphrase.
        if age.load_identity_with_passphrase(None).is_err() {
            let msg = "age identity is passphrase-protected; unlock with ato session start";
            println!(
                "{}",
                serde_json::json!({"type": "desktop_login_failed", "message": msg})
            );
            anyhow::bail!("{}", msg);
        }
    } else {
        // Create a passphrase-free identity so the session token survives the process.
        age.init_identity(None)
            .context("failed to create age identity for desktop login")?;
    }

    // ── Bridge device-code flow ────────────────────────────────────────────────
    let api_base = store_api_base_url();
    let site_base = store_site_base_url();
    let client = reqwest::Client::new();
    let code_verifier = generate_pkce_verifier();
    let code_challenge = generate_pkce_challenge_s256(&code_verifier);

    let start_response = client
        .post(format!("{}/v1/auth/bridge/init", api_base))
        .json(&serde_json::json!({
            "code_challenge": code_challenge,
            "method": "S256",
            "device_info": format!("ato-desktop/{}", env!("CARGO_PKG_VERSION")),
        }))
        .send()
        .await
        .with_context(|| "Failed to start Store bridge authentication")?;

    if !start_response.status().is_success() {
        let status = start_response.status();
        let body = start_response.text().await.unwrap_or_default();
        let raw = format!("Bridge auth init failed ({}): {}", status, body);
        let (message, detail) = sanitize_bridge_failure("Could not start sign-in.", raw);
        eprintln!("[ato-desktop] {}", detail);
        println!(
            "{}",
            serde_json::json!({"type": "desktop_login_failed", "message": message, "detail": detail})
        );
        anyhow::bail!("{}", detail);
    }

    let start: BridgeInitResponse = start_response
        .json()
        .await
        .context("Invalid bridge auth init response")?;

    let session_id = start.session_id.clone();
    let activate_url = format!(
        "{}/v1/auth/bridge/activate?session_id={}",
        api_base, session_id
    );
    let login_url = format!(
        "{}/auth?next={}",
        site_base,
        urlencoding::encode(&activate_url)
    );

    // Emit the started event first, so a caller tailing stdout can observe
    // login_url / user_code even if the automatic browser launch below
    // fails for some reason.
    println!(
        "{}",
        serde_json::json!({
            "type": "desktop_login_started",
            "login_url": login_url,
            "user_code": start.user_code,
            "expires_in": start.expires_in,
            "poll_interval_sec": start.poll_interval_sec.unwrap_or(2),
        })
    );

    // Open the OS default browser at `login_url`, exactly like the plain
    // interactive `ato login` flow does. Non-fatal: the poll loop below
    // continues regardless, and the user can open the URL manually if the
    // automatic launch fails. Emitted as a structured NDJSON event (not just
    // an eprintln!) so a non-TTY caller (ato-desktop) can actually surface
    // `login_url` to the user instead of silently discarding the failure.
    if let Err(error) = try_open_browser(&login_url) {
        let (msg, event) = browser_launch_failed_event(&login_url, &error);
        eprintln!("[ato-desktop] {}", msg);
        println!("{}", event);
    }

    let (poll_timeout_secs, mut poll_interval_secs) =
        compute_poll_timing(start.expires_in, start.poll_interval_sec);
    let started_at = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = client
                    .post(format!("{}/v1/auth/bridge/cancel", api_base))
                    .json(&serde_json::json!({
                        "session_id": &session_id,
                        "reason": "desktop_cancelled",
                    }))
                    .send()
                    .await;
                let msg = "Authentication cancelled";
                println!(
                    "{}",
                    serde_json::json!({"type": "desktop_login_failed", "message": msg})
                );
                anyhow::bail!("{}", msg);
            }
            _ = tokio::time::sleep(Duration::from_secs(poll_interval_secs)) => {}
        }

        if started_at.elapsed() >= Duration::from_secs(poll_timeout_secs) {
            let msg = format!(
                "Authentication timed out after {} seconds",
                poll_timeout_secs
            );
            println!(
                "{}",
                serde_json::json!({"type": "desktop_login_failed", "message": msg})
            );
            anyhow::bail!("{}", msg);
        }

        let poll_response = client
            .post(format!("{}/v1/auth/bridge/poll", api_base))
            .json(&serde_json::json!({
                "session_id": &session_id,
                "code_verifier": &code_verifier,
            }))
            .send()
            .await
            .with_context(|| "Failed to poll bridge authentication state")?;

        if poll_response.status() == StatusCode::TOO_MANY_REQUESTS {
            let body =
                poll_response
                    .json::<RetryAfterResponse>()
                    .await
                    .unwrap_or(RetryAfterResponse {
                        retry_after: Some(poll_interval_secs),
                    });
            let retry_after = body.retry_after.unwrap_or(poll_interval_secs).max(1);
            tokio::time::sleep(Duration::from_secs(retry_after)).await;
            continue;
        }

        if poll_response.status() == StatusCode::CONFLICT {
            let msg = "Authentication denied or cancelled";
            println!(
                "{}",
                serde_json::json!({"type": "desktop_login_failed", "message": msg})
            );
            anyhow::bail!("{}", msg);
        }

        if poll_response.status() == StatusCode::GONE {
            let msg = "Authentication session expired";
            println!(
                "{}",
                serde_json::json!({"type": "desktop_login_failed", "message": msg})
            );
            anyhow::bail!("{}", msg);
        }

        if poll_response.status() == StatusCode::BAD_REQUEST {
            let body = poll_response.text().await.unwrap_or_default();
            let raw = format!("Authentication failed: {}", body);
            let (message, detail) = sanitize_bridge_failure("Sign-in was rejected.", raw);
            eprintln!("[ato-desktop] {}", detail);
            println!(
                "{}",
                serde_json::json!({"type": "desktop_login_failed", "message": message, "detail": detail})
            );
            anyhow::bail!("{}", detail);
        }

        if !poll_response.status().is_success() {
            let status = poll_response.status();
            let body = poll_response.text().await.unwrap_or_default();
            let raw = format!("Bridge auth poll failed ({}): {}", status, body);
            let (message, detail) =
                sanitize_bridge_failure("Sign-in is temporarily unavailable.", raw);
            eprintln!("[ato-desktop] {}", detail);
            println!(
                "{}",
                serde_json::json!({"type": "desktop_login_failed", "message": message, "detail": detail})
            );
            anyhow::bail!("{}", detail);
        }

        let poll: BridgePollResponse = poll_response
            .json()
            .await
            .context("Invalid bridge auth poll response")?;

        match poll.code.as_str() {
            "PENDING" => {
                poll_interval_secs = poll.poll_interval_sec.unwrap_or(poll_interval_secs).max(1);
            }
            "SUCCESS" => {
                let auth_code = poll
                    .auth_code
                    .context("Bridge auth approved but no auth code was returned")?;

                let exchange_response = client
                    .post(format!("{}/v1/auth/bridge/exchange", api_base))
                    .json(&serde_json::json!({
                        "session_id": &session_id,
                        "auth_code": auth_code,
                        "code_verifier": &code_verifier,
                    }))
                    .send()
                    .await
                    .context("Failed to exchange bridge auth code")?;

                if !exchange_response.status().is_success() {
                    let status = exchange_response.status();
                    let body = exchange_response.text().await.unwrap_or_default();
                    let raw = format!("Bridge auth exchange failed ({}): {}", status, body);
                    let (message, detail) =
                        sanitize_bridge_failure("Sign-in failed to complete.", raw);
                    eprintln!("[ato-desktop] {}", detail);
                    println!(
                        "{}",
                        serde_json::json!({"type": "desktop_login_failed", "message": message, "detail": detail})
                    );
                    anyhow::bail!("{}", detail);
                }

                let exchange: BridgeExchangeResponse = exchange_response
                    .json()
                    .await
                    .context("Invalid bridge auth exchange response")?;

                let session_token = exchange.access_token;
                let manager = AuthManager::new()?;
                let storage = manager
                    .persist_session_token(session_token.clone(), false)
                    .await?;

                let mut creds = manager.load()?.unwrap_or_default();
                creds.publisher_handle = exchange.handle.clone();

                // Publisher onboarding is best-effort in non-interactive (desktop) mode.
                // The session token is already persisted above, so `ato whoami`
                // returns authenticated regardless.  Any onboarding failure must
                // not block the `desktop_login_completed` event.
                match run_publisher_onboarding_flow(
                    &session_token,
                    creds.github_username.as_deref(),
                    true,
                )
                .await
                {
                    Ok(onboarding) => {
                        creds.publisher_id = Some(onboarding.publisher_id);
                        creds.publisher_handle = Some(onboarding.publisher_handle);
                        creds.publisher_did = Some(onboarding.publisher_did);
                        if let Some(installation) = onboarding.installation {
                            creds.github_app_installation_id = Some(installation.installation_id);
                            creds.github_app_account_login = Some(installation.account_login);
                        }
                        let _ = manager.save(&creds);
                    }
                    Err(e) => {
                        // Non-fatal: log to stderr (not stdout) so the NDJSON
                        // stream stays clean for the Desktop watcher.
                        eprintln!("[ato-desktop] publisher onboarding skipped: {}", e);
                    }
                }

                println!(
                    "{}",
                    desktop_login_completed_event(
                        creds.publisher_handle.as_deref(),
                        storage.display(),
                    )
                );
                return Ok(());
            }
            other => {
                let raw = format!("Unexpected authentication status: {}", other);
                let (message, detail) =
                    sanitize_bridge_failure("Sign-in returned an unexpected response.", raw);
                eprintln!("[ato-desktop] {}", detail);
                println!(
                    "{}",
                    serde_json::json!({"type": "desktop_login_failed", "message": message, "detail": detail})
                );
                anyhow::bail!("{}", detail);
            }
        }
    }
}

#[allow(clippy::needless_return)]
pub fn logout() -> Result<()> {
    let manager = AuthManager::new()?;

    if !manager.has_persisted_local_state()? {
        println!("ℹ️  Not currently logged in");
        return Ok(());
    }

    manager.delete()?;
    println!("✅ Logged out successfully");
    println!(
        "   Purged auth tokens from: age file, memory cache, and {:?}",
        manager.credentials_path()
    );
    if manager.legacy_credentials_path().exists() {
        println!(
            "   Legacy metadata file was left untouched: {:?}",
            manager.legacy_credentials_path()
        );
    }

    Ok(())
}

pub fn status() -> Result<()> {
    let manager = AuthManager::new()?;

    match manager.require() {
        Ok(creds) => {
            println!("✅ Authenticated");
            if let Some(session_token) = &creds.session_token {
                println!("   Store session: configured");
                match fetch_store_session_user(session_token) {
                    Ok(Some(user)) => {
                        println!("   User ID: {}", user.id);
                        if let Some(name) = user.name {
                            println!("   Name: {}", name);
                        }
                        if let Some(email) = user.email {
                            println!("   Email: {}", email);
                        }
                    }
                    Ok(None) => {
                        println!("   User: session expired or unavailable");
                    }
                    Err(err) => {
                        println!("   User: failed to fetch ({})", err);
                    }
                }
            }
            if creds.github_token.is_some() {
                println!("   GitHub token: configured");
            }
            if let Some(username) = &creds.github_username {
                println!("   GitHub: @{}", username);
            }
            if let Some(did) = &creds.publisher_did {
                println!("   Publisher DID: {}", did);
            }
            if let Some(handle) = &creds.publisher_handle {
                println!("   Publisher Handle: {}", handle);
            }
            if let Some(id) = creds.github_app_installation_id {
                println!("   GitHub App Installation ID: {}", id);
            }
            if let Some(login) = &creds.github_app_account_login {
                println!("   GitHub App Account: {}", login);
            }
            let auth_store = manager.auth_store();
            if creds.session_token.is_some() {
                println!(
                    "   Session storage: {}",
                    auth_store.primary_write_backend_label()
                );
            }
            if manager.credentials_path().exists() {
                println!("   Credential file: {:?}", manager.credentials_path());
            } else if manager.legacy_credentials_path().exists() {
                println!(
                    "   Legacy credential file: {:?}",
                    manager.legacy_credentials_path()
                );
            }
        }
        Err(_) => {
            println!("❌ Not authenticated");
            println!("   Run: ato login");
            println!();
            println!("   Headless/CI/agent fallback:");
            println!("   Set ATO_TOKEN or run `ato login --headless`");
        }
    }

    Ok(())
}

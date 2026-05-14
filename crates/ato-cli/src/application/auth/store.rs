use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rand::rngs::OsRng;
use rand::RngCore;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

use super::prompt::try_open_browser;
use super::publisher::{
    fetch_publisher_me_blocking, run_publisher_onboarding_flow, PublisherMeResponse,
};
use super::storage::{merge_metadata, TokenStorageLocation};
use super::{
    read_env_non_empty, AuthManager, Credentials, DEFAULT_STORE_API_URL, DEFAULT_STORE_SITE_URL,
    ENV_STORE_API_URL, ENV_STORE_SITE_URL,
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

pub(super) fn store_api_base_url() -> String {
    trim_trailing_slash(
        &read_env_non_empty(ENV_STORE_API_URL).unwrap_or_else(|| DEFAULT_STORE_API_URL.to_string()),
    )
}

fn store_site_base_url() -> String {
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

#[allow(clippy::needless_return)]
pub async fn login_with_store_device_flow(headless: bool) -> Result<()> {
    // Bootstrap the age identity before the browser flow. Without an identity
    // the session token can only be held in memory and will not survive the
    // process — we'd rather the user deal with the one-time prompt here than
    // complete the browser handshake and discover afterward that nothing was
    // persisted. Headless mode writes to the canonical TOML file directly and
    // does not need age, so we skip the prompt there.
    if !headless {
        if let Err(error) =
            tokio::task::spawn_blocking(crate::application::secrets::ensure_identity_interactive)
                .await
                .context("failed to run age identity bootstrap")?
        {
            eprintln!("⚠️  Skipping age identity bootstrap: {error}");
            eprintln!("   Login will proceed, but the token may not persist across sessions.");
        }
    }

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
            "device_info": format!("ato-cli/{}", env!("CARGO_PKG_VERSION")),
        }))
        .send()
        .await
        .with_context(|| "Failed to start Store bridge authentication")?;

    if !start_response.status().is_success() {
        let status = start_response.status();
        let body = start_response.text().await.unwrap_or_default();
        let mut message = format!("Bridge auth init failed ({}): {}", status, body);
        if status.is_server_error() && is_local_store_api_base_url(&api_base) {
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

    let poll_timeout_secs = start.expires_in.min(300);
    let mut poll_interval_secs = start.poll_interval_sec.unwrap_or(2).max(1);
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
                "Authentication timed out after {} seconds. Run `ato login` again.",
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
            anyhow::bail!("Authentication denied or cancelled. Run `ato login` again.");
        }

        if poll_response.status() == StatusCode::GONE {
            anyhow::bail!("Authentication expired. Run `ato login` again.");
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

                let session_token = exchange.access_token;

                let manager = AuthManager::new()?;
                let storage = manager
                    .persist_session_token(session_token.clone(), headless)
                    .await?;
                let mut creds = manager.load()?.unwrap_or_default();
                creds.publisher_handle = exchange.handle.clone();
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
                return Ok(());
            }
            other => {
                anyhow::bail!("Unexpected authentication status: {}", other);
            }
        }
    }
}

/// Login flow for `ato login --desktop-webview`.
///
/// Unlike the normal interactive flow, this:
/// - Does not prompt the user interactively (no TTY required).
/// - Auto-creates a passphrase-free age identity if none exists.
/// - Emits NDJSON events on stdout:
///   `{"type":"desktop_login_started", "login_url":"...", "user_code":"...", "expires_in":N, "poll_interval_sec":N}`
///   `{"type":"desktop_login_completed", "publisher_handle":"...", "storage":"age_file"}`
///   `{"type":"desktop_login_failed", "message":"..."}`
/// - Exits with a non-zero code on failure.
#[allow(clippy::needless_return)]
pub async fn login_with_store_device_flow_desktop() -> Result<()> {
    use capsule_core::common::paths::nacelle_home_dir;
    use crate::application::credential::AgeFileBackend;

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
        let msg = format!("Bridge auth init failed ({}): {}", status, body);
        println!(
            "{}",
            serde_json::json!({"type": "desktop_login_failed", "message": msg})
        );
        anyhow::bail!("{}", msg);
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

    // Emit the started event so the Desktop can open the WebView.
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

    let poll_timeout_secs = start.expires_in.min(300);
    let mut poll_interval_secs = start.poll_interval_sec.unwrap_or(2).max(1);
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
            let body = poll_response
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
            let msg = format!("Authentication failed: {}", body);
            println!(
                "{}",
                serde_json::json!({"type": "desktop_login_failed", "message": msg})
            );
            anyhow::bail!("{}", msg);
        }

        if !poll_response.status().is_success() {
            let status = poll_response.status();
            let body = poll_response.text().await.unwrap_or_default();
            let msg = format!("Bridge auth poll failed ({}): {}", status, body);
            println!(
                "{}",
                serde_json::json!({"type": "desktop_login_failed", "message": msg})
            );
            anyhow::bail!("{}", msg);
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
                    let msg = format!("Bridge auth exchange failed ({}): {}", status, body);
                    println!(
                        "{}",
                        serde_json::json!({"type": "desktop_login_failed", "message": msg})
                    );
                    anyhow::bail!("{}", msg);
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

                let onboarding = run_publisher_onboarding_flow(
                    &session_token,
                    creds.github_username.as_deref(),
                    true,
                )
                .await?;
                creds.publisher_id = Some(onboarding.publisher_id);
                creds.publisher_handle = Some(onboarding.publisher_handle);
                creds.publisher_did = Some(onboarding.publisher_did);
                if let Some(installation) = onboarding.installation {
                    creds.github_app_installation_id = Some(installation.installation_id);
                    creds.github_app_account_login = Some(installation.account_login);
                }
                manager.save(&creds)?;

                println!(
                    "{}",
                    serde_json::json!({
                        "type": "desktop_login_completed",
                        "publisher_handle": creds.publisher_handle,
                        "storage": storage.display(),
                    })
                );
                return Ok(());
            }
            other => {
                let msg = format!("Unexpected authentication status: {}", other);
                println!(
                    "{}",
                    serde_json::json!({"type": "desktop_login_failed", "message": msg})
                );
                anyhow::bail!("{}", msg);
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

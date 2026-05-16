use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use capsule_core::common::paths::ato_path;
use ed25519_dalek::Signer;
use reqwest::StatusCode;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use super::github::{
    ensure_github_app_installation_with_tui, fetch_github_app_installations, GitHubAppInstallation,
};
use super::prompt::prompt_line;
use super::store::{parse_store_error_text, store_api_base_url, store_session_cookie_header};
use super::Credentials;

#[derive(Debug, Deserialize)]
pub(super) struct PublisherMeResponse {
    pub id: String,
    pub handle: String,
    pub author_did: String,
}

#[derive(Debug, Deserialize)]
struct PublisherRegisterResponse {
    id: String,
    handle: String,
    author_did: String,
}

pub(super) fn merge_publisher_identity(creds: &mut Credentials, me: &PublisherMeResponse) {
    if !me.id.trim().is_empty() {
        creds.publisher_id = Some(me.id.clone());
    }
    if !me.handle.trim().is_empty() {
        creds.publisher_handle = Some(me.handle.clone());
    }
    if !me.author_did.trim().is_empty() {
        creds.publisher_did = Some(me.author_did.clone());
    }
}

pub(super) fn fetch_publisher_me_blocking(
    session_token: &str,
) -> Result<Option<PublisherMeResponse>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(format!("{}/v1/publishers/me", store_api_base_url()))
        .header("Accept", "application/json")
        .header("Cookie", store_session_cookie_header(session_token))
        .send()
        .context("Failed to fetch publisher profile")?;

    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        anyhow::bail!("Store session is not authorized for publisher lookup");
    }
    if !response.status().is_success() {
        anyhow::bail!("Publisher lookup failed (HTTP {})", response.status());
    }

    let body = response
        .json::<PublisherMeResponse>()
        .context("Failed to parse publisher profile response")?;

    Ok(Some(body))
}

fn normalize_handle_candidate(input: &str) -> String {
    let lowered = input.trim().to_ascii_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut prev_dash = false;
    for ch in lowered.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        if ok {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let mut normalized = out.trim_matches('-').to_string();
    if normalized.len() < 3 {
        normalized.push_str("-pub");
    }
    if normalized.len() > 63 {
        normalized.truncate(63);
        normalized = normalized.trim_end_matches('-').to_string();
    }
    if normalized.len() < 3 {
        normalized = "ato-publisher".to_string();
    }
    normalized
}

fn is_valid_handle(value: &str) -> bool {
    if value.len() < 3 || value.len() > 63 {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes.first() == Some(&b'-') || bytes.last() == Some(&b'-') {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn publisher_signing_key_path() -> Result<PathBuf> {
    ato_path("keys/publisher-signing-key.json").context("Cannot determine ato home directory")
}

fn ensure_publisher_signing_key() -> Result<capsule_core::types::signing::StoredKey> {
    let key_path = publisher_signing_key_path()?;
    if key_path.exists() {
        return capsule_core::types::signing::StoredKey::read(&key_path)
            .with_context(|| format!("Failed to read {}", key_path.display()));
    }

    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let generated = capsule_core::types::signing::StoredKey::generate();
    generated
        .write(&key_path)
        .with_context(|| format!("Failed to write {}", key_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&key_path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&key_path, perms)?;
    }

    Ok(generated)
}

#[derive(Debug, Clone)]
pub(super) struct PublisherOnboardingInfo {
    pub publisher_id: String,
    pub publisher_handle: String,
    pub publisher_did: String,
    pub installation: Option<GitHubAppInstallation>,
}

async fn fetch_publisher_me(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
) -> Result<Option<PublisherMeResponse>> {
    let response = client
        .get(format!("{}/v1/publishers/me", api_base))
        .header("Accept", "application/json")
        .header("Cookie", store_session_cookie_header(session_token))
        .send()
        .await
        .context("Failed to fetch publisher profile")?;

    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        anyhow::bail!("Store session is not authorized for publisher lookup");
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "Publisher lookup failed ({}): {}",
            status,
            parse_store_error_text(&body)
        );
    }

    let payload = response
        .json::<PublisherMeResponse>()
        .await
        .context("Failed to parse publisher profile response")?;
    Ok(Some(payload))
}

/// Registers a new publisher record without any interactive prompts.
/// Used in `--desktop-webview` mode where no TTY is available.
/// Tries the canonical handle derived from `hint`; on conflict appends
/// incrementing numeric suffixes (hint-2, hint-3, …) up to 5 attempts.
async fn register_publisher_silently(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
    hint: Option<&str>,
) -> Result<PublisherMeResponse> {
    let signing_key = ensure_publisher_signing_key()?;
    let did = signing_key.did()?;

    let base = normalize_handle_candidate(hint.filter(|v| !v.trim().is_empty()).unwrap_or("pub"));

    for attempt in 0u32..5 {
        let handle = if attempt == 0 {
            base.clone()
        } else {
            let candidate = format!("{}-{}", base, attempt + 1);
            normalize_handle_candidate(&candidate)
        };

        if !is_valid_handle(&handle) {
            continue;
        }

        let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let signature = signing_key
            .to_signing_key()?
            .sign(timestamp.as_bytes())
            .to_bytes();
        let signature_b64 = BASE64_STANDARD.encode(signature);

        let payload = serde_json::json!({
            "handle": handle,
            "author_did": did,
            "did_proof": {
                "did": did,
                "timestamp": timestamp,
                "signature": signature_b64,
            }
        });

        let response = client
            .post(format!("{}/v1/publishers/register", api_base))
            .header("Accept", "application/json")
            .header("Cookie", store_session_cookie_header(session_token))
            .json(&payload)
            .send()
            .await
            .context("Failed to register publisher")?;

        if response.status().is_success() {
            let reg = response
                .json::<PublisherRegisterResponse>()
                .await
                .context("Failed to parse publisher register response")?;
            return Ok(PublisherMeResponse {
                id: reg.id,
                handle: reg.handle,
                author_did: reg.author_did,
            });
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let err_text = parse_store_error_text(&body);

        if status == StatusCode::CONFLICT && err_text.contains("already_registered") {
            if let Some(me) = fetch_publisher_me(client, api_base, session_token).await? {
                return Ok(me);
            }
        }

        if status == StatusCode::CONFLICT && err_text.contains("handle_taken") {
            continue;
        }

        anyhow::bail!("Publisher registration failed ({}): {}", status, err_text);
    }

    anyhow::bail!(
        "Publisher registration failed: all handle candidates were taken. \
         Run `ato publish` interactively to choose a handle."
    )
}

async fn register_publisher_with_prompt(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
    github_username: Option<&str>,
) -> Result<PublisherRegisterResponse> {
    let signing_key = ensure_publisher_signing_key()?;
    let did = signing_key.did()?;

    let default_handle = normalize_handle_candidate(
        github_username
            .filter(|v| !v.trim().is_empty())
            .unwrap_or("ato-publisher"),
    );

    println!();
    println!("🧩 Publisher setup is required for publishing.");

    for _ in 0..5 {
        let entered = prompt_line(&format!("👤 Publisher handle [{}]: ", default_handle))?;
        let handle = if entered.is_empty() {
            default_handle.clone()
        } else {
            normalize_handle_candidate(&entered)
        };

        if !is_valid_handle(&handle) {
            eprintln!("⚠️  Invalid handle. Use 3-63 chars, lowercase letters/digits/hyphen.");
            continue;
        }

        let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let signature = signing_key
            .to_signing_key()?
            .sign(timestamp.as_bytes())
            .to_bytes();
        let signature_b64 = BASE64_STANDARD.encode(signature);

        let payload = serde_json::json!({
            "handle": handle,
            "author_did": did,
            "did_proof": {
                "did": did,
                "timestamp": timestamp,
                "signature": signature_b64,
            }
        });

        let response = client
            .post(format!("{}/v1/publishers/register", api_base))
            .header("Accept", "application/json")
            .header("Cookie", store_session_cookie_header(session_token))
            .json(&payload)
            .send()
            .await
            .context("Failed to register publisher")?;

        if response.status().is_success() {
            return response
                .json::<PublisherRegisterResponse>()
                .await
                .context("Failed to parse publisher register response");
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let err_text = parse_store_error_text(&body);

        if status == StatusCode::CONFLICT && err_text.contains("handle_taken") {
            eprintln!("⚠️  Handle is already taken. Choose another one.");
            continue;
        }
        if status == StatusCode::CONFLICT && err_text.contains("already_registered") {
            if let Some(me) = fetch_publisher_me(client, api_base, session_token).await? {
                return Ok(PublisherRegisterResponse {
                    id: me.id,
                    handle: me.handle,
                    author_did: me.author_did,
                });
            }
        }

        anyhow::bail!("Publisher registration failed ({}): {}", status, err_text);
    }

    anyhow::bail!("Publisher setup aborted: failed to select a valid/available handle")
}

pub(super) async fn run_publisher_onboarding_flow(
    session_token: &str,
    github_username: Option<&str>,
    desktop_webview: bool,
) -> Result<PublisherOnboardingInfo> {
    let api_base = store_api_base_url();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("Failed to create HTTP client")?;

    let publisher =
        if let Some(existing) = fetch_publisher_me(&client, &api_base, session_token).await? {
            // Ensure the local signing key exists even when the publisher is already registered.
            // On a fresh machine or after keys are wiped, the key file won't exist even though
            // the publisher record is already in the store.
            let local_key = ensure_publisher_signing_key()?;
            let local_did = local_key.did()?;
            if local_did != existing.author_did {
                // Local key was just generated (or is stale) — sync the new DID to the store.
                update_publisher_did(&client, &api_base, session_token, &local_key).await?;
            }
            existing
        } else if desktop_webview {
            // In desktop-webview mode there is no TTY for interactive prompts.
            // Try a silent registration with the supplied username hint; if it
            // fails (handle taken, network error, etc.) bail so the caller can
            // fall through to the Failure path — the session token is already
            // persisted so the Dock will still show as authenticated.
            register_publisher_silently(&client, &api_base, session_token, github_username).await?
        } else {
            let created =
                register_publisher_with_prompt(&client, &api_base, session_token, github_username)
                    .await?;
            PublisherMeResponse {
                id: created.id,
                handle: created.handle,
                author_did: created.author_did,
            }
        };

    let installation = if desktop_webview {
        // In desktop-webview mode there is no TTY for interactive prompts.
        // Try to pick up an existing *active* installation; if none is found
        // or the lookup fails, leave it unset — the user can link a GitHub
        // App later via `ato publish`.
        let existing = fetch_github_app_installations(&client, &api_base, session_token)
            .await
            .unwrap_or_default();
        existing
            .into_iter()
            .find(|i| i.status.eq_ignore_ascii_case("active"))
    } else {
        Some(ensure_github_app_installation_with_tui(&client, &api_base, session_token).await?)
    };

    Ok(PublisherOnboardingInfo {
        publisher_id: publisher.id,
        publisher_handle: publisher.handle,
        publisher_did: publisher.author_did,
        installation,
    })
}

async fn update_publisher_did(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
    signing_key: &capsule_core::types::signing::StoredKey,
) -> Result<()> {
    let did = signing_key.did()?;
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let signature = signing_key
        .to_signing_key()?
        .sign(timestamp.as_bytes())
        .to_bytes();
    let signature_b64 = BASE64_STANDARD.encode(signature);

    let payload = serde_json::json!({
        "author_did": did,
        "did_proof": {
            "did": did,
            "timestamp": timestamp,
            "signature": signature_b64,
        }
    });

    let response = client
        .patch(format!("{}/v1/publishers/me", api_base))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("Cookie", store_session_cookie_header(session_token))
        .json(&payload)
        .send()
        .await
        .context("Failed to update publisher DID")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "Publisher DID update failed ({}): {}",
            status,
            parse_store_error_text(&body)
        );
    }

    Ok(())
}

use anyhow::{Context, Result};
use reqwest::StatusCode;
use serde::Deserialize;
use std::time::{Duration, Instant};

use super::prompt::{prompt_line, prompt_yes_no, try_open_browser};
use super::store::{parse_store_error_text, store_session_cookie_header};
use super::{
    AuthManager, GITHUB_APP_INSTALL_NOTICE_INTERVAL_SECS, GITHUB_APP_INSTALL_POLL_SECS,
    GITHUB_APP_INSTALL_TIMEOUT_SECS, GITHUB_APP_INSTALL_TROUBLESHOOT_AFTER_SECS,
};

#[derive(Debug, Deserialize)]
struct GitHubUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GitHubInstallationsResponse {
    installations: Vec<GitHubAppInstallation>,
}

#[derive(Debug, Deserialize, Clone)]
pub(super) struct GitHubAppInstallation {
    pub installation_id: u64,
    pub account_login: String,
    pub status: String,
}

#[derive(Debug, Deserialize)]
struct GitHubAppInstallUrlResponse {
    install_url: String,
    #[serde(default)]
    callback_url: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GitHubAppCallbackResponse {
    installation_id: u64,
    account_login: String,
}

pub(super) async fn fetch_github_app_installations(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
) -> Result<Vec<GitHubAppInstallation>> {
    let response = client
        .get(format!("{}/v1/sources/github/app/installations", api_base))
        .header("Accept", "application/json")
        .header("Cookie", store_session_cookie_header(session_token))
        .send()
        .await
        .context("Failed to fetch GitHub App installations")?;

    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        anyhow::bail!("GitHub App installation lookup is unauthorized for current session");
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "Failed to fetch GitHub App installations ({}): {}",
            status,
            parse_store_error_text(&body)
        );
    }

    let payload = response
        .json::<GitHubInstallationsResponse>()
        .await
        .context("Failed to parse GitHub App installations response")?;
    Ok(payload.installations)
}

fn choose_active_installation(
    installations: &[GitHubAppInstallation],
) -> Option<GitHubAppInstallation> {
    installations
        .iter()
        .find(|i| i.status.eq_ignore_ascii_case("active"))
        .cloned()
        .or_else(|| installations.first().cloned())
}

async fn fetch_github_app_install_url(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
) -> Result<GitHubAppInstallUrlResponse> {
    let response = client
        .get(format!("{}/v1/sources/github/app/install-url", api_base))
        .header("Accept", "application/json")
        .header("Cookie", store_session_cookie_header(session_token))
        .send()
        .await
        .context("Failed to request GitHub App install URL")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "Failed to request GitHub App install URL ({}): {}",
            status,
            parse_store_error_text(&body)
        );
    }

    let payload = response
        .json::<GitHubAppInstallUrlResponse>()
        .await
        .context("Failed to parse GitHub App install URL response")?;
    Ok(payload)
}

async fn link_github_app_installation_manually(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
    installation_id: u64,
    state: Option<&str>,
) -> Result<GitHubAppInstallation> {
    let mut request = client
        .get(format!("{}/v1/sources/github/app/callback", api_base))
        .header("Accept", "application/json")
        .header("Cookie", store_session_cookie_header(session_token))
        .query(&[
            ("installation_id", installation_id.to_string()),
            ("setup_action", "install".to_string()),
        ]);
    if let Some(non_empty_state) = state.filter(|value| !value.trim().is_empty()) {
        request = request.query(&[("state", non_empty_state.to_string())]);
    }

    let response = request
        .send()
        .await
        .context("Failed to call GitHub App callback endpoint")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "Manual callback failed ({}): {}",
            status,
            parse_store_error_text(&body)
        );
    }

    let payload = response
        .json::<GitHubAppCallbackResponse>()
        .await
        .context("Failed to parse GitHub App callback response")?;

    let installations = fetch_github_app_installations(client, api_base, session_token).await?;
    if let Some(found) = installations
        .into_iter()
        .find(|item| item.installation_id == payload.installation_id)
    {
        return Ok(found);
    }

    Ok(GitHubAppInstallation {
        installation_id: payload.installation_id,
        account_login: payload.account_login,
        status: "active".to_string(),
    })
}

pub(super) async fn ensure_github_app_installation_with_tui(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
) -> Result<GitHubAppInstallation> {
    let existing = fetch_github_app_installations(client, api_base, session_token).await?;
    if let Some(active) = choose_active_installation(&existing) {
        return Ok(active);
    }

    let install = fetch_github_app_install_url(client, api_base, session_token).await?;
    println!();
    println!("🔌 GitHub App installation is required.");
    println!("   URL: {}", install.install_url);
    if let Some(callback_url) = install.callback_url.as_deref() {
        println!("   Callback: {}", callback_url);
    }
    if let Some(expires_in) = install.expires_in {
        println!("   Link expires in: {}s", expires_in);
    }
    if let Some(state) = install.state.as_deref() {
        println!("   State: {}", state);
    }

    if let Err(error) = try_open_browser(&install.install_url) {
        eprintln!("⚠️  Could not open browser automatically: {}", error);
        eprintln!("   Open the URL manually to continue.");
    }

    if !prompt_yes_no("GitHub App install page opened. Start linking now?", true)? {
        anyhow::bail!("GitHub App installation was cancelled");
    }

    println!("⏳ Waiting for GitHub App installation to be linked...");
    let started = Instant::now();
    let mut last_notice = Instant::now();
    let mut troubleshooting_printed = false;
    loop {
        if started.elapsed() >= Duration::from_secs(GITHUB_APP_INSTALL_TIMEOUT_SECS) {
            let mut hint = String::from(
                "Timed out waiting for GitHub App installation to be linked.\n\
                 Re-check that installation completed in GitHub and run `ato login` again.",
            );
            if let Some(callback_url) = install.callback_url.as_deref() {
                hint.push_str(&format!("\nExpected callback endpoint: {}", callback_url));
            }
            println!();
            println!("⚠️  {}", hint);
            println!(
                "   You can link manually by entering installation_id (from GitHub installation URL)."
            );
            let manual_input =
                prompt_line("   installation_id (blank to cancel and retry later): ")?;
            if manual_input.trim().is_empty() {
                anyhow::bail!(
                    "Timed out waiting for GitHub App installation. Complete linking and run `ato login` again."
                );
            }
            let installation_id = manual_input
                .trim()
                .parse::<u64>()
                .with_context(|| format!("Invalid installation_id: {}", manual_input.trim()))?;
            let linked = link_github_app_installation_manually(
                client,
                api_base,
                session_token,
                installation_id,
                install.state.as_deref(),
            )
            .await?;
            println!(
                "   ✔ Linked installation {} ({})",
                linked.installation_id, linked.account_login
            );
            return Ok(linked);
        }

        let installations = fetch_github_app_installations(client, api_base, session_token).await?;
        if let Some(active) = choose_active_installation(&installations) {
            return Ok(active);
        }

        let elapsed = started.elapsed().as_secs();
        if !troubleshooting_printed && elapsed >= GITHUB_APP_INSTALL_TROUBLESHOOT_AFTER_SECS {
            println!(
                "   • still waiting ({}s). If you already installed, callback may not have reached Store.",
                elapsed
            );
            println!("   • Ensure the final GitHub install step completed for the target account.");
            println!(
                "   • If this repeats, verify GitHub App setup URL points to /v1/sources/github/app/callback."
            );
            troubleshooting_printed = true;
            last_notice = Instant::now();
        } else if last_notice.elapsed()
            >= Duration::from_secs(GITHUB_APP_INSTALL_NOTICE_INTERVAL_SECS)
        {
            println!("   • waiting for installation link... ({}s)", elapsed);
            last_notice = Instant::now();
        }

        tokio::time::sleep(Duration::from_secs(GITHUB_APP_INSTALL_POLL_SECS)).await;
    }
}

async fn verify_github_token(token: &str) -> Result<String> {
    let client = reqwest::Client::new();

    let response = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", token))
        .header("User-Agent", "ato-cli")
        .send()
        .await
        .context("Failed to connect to GitHub API")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_default();

        anyhow::bail!("Invalid GitHub token (HTTP {}): {}", status, error_text);
    }

    let user: GitHubUser = response
        .json()
        .await
        .context("Failed to parse GitHub user response")?;

    Ok(user.login)
}

pub async fn login_with_token(token: String) -> Result<()> {
    println!("🔐 Verifying GitHub token...");

    let username = verify_github_token(&token).await?;

    let manager = AuthManager::new()?;
    let write_location = manager.save_github_token_async(token).await?;
    let mut creds = manager.load()?.unwrap_or_default();
    creds.github_username = Some(username.clone());
    manager.save(&creds)?;

    println!("✅ Authenticated as @{}", username);
    println!("   GitHub token storage: {}", write_location.display());
    if manager.credentials_path().exists() {
        println!("   Metadata file: {:?}", manager.credentials_path());
    }

    Ok(())
}

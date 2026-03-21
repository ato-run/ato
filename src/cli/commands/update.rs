use anyhow::{bail, Context, Result};
use axoupdater::AxoUpdater;
use semver::Version;
use serde::Deserialize;

use std::fs;
use std::process::Command;

const DEFAULT_INSTALLER_URL: &str = "https://ato.run/install.sh";
const INSTALLER_URL_ENV: &str = "ATO_INSTALLER_URL";
const UPDATE_RELEASE_API_URL_ENV: &str = "ATO_UPDATE_RELEASE_API_URL";

#[derive(Debug, Deserialize)]
struct GithubLatestRelease {
    tag_name: String,
}

/// Update the ato CLI to the latest version
pub fn update() -> Result<()> {
    println!("🔍 更新を確認中...");

    let mut updater = AxoUpdater::new_for("ato");
    if let Err(receipt_err) = updater.load_receipt() {
        eprintln!("ℹ️  install receipt が見つからないため installer fallback で更新します...");
        return fallback_update_via_installer().with_context(|| {
            format!(
                "ato のインストール情報を読み込めませんでした: {}",
                receipt_err
            )
        });
    }

    updater.disable_installer_output();

    let update_result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("更新確認用のランタイムを初期化できませんでした")?
        .block_on(updater.run())
        .context("ato CLI の更新に失敗しました")?;

    match update_result {
        Some(result) => println!("✅ 最新版 (v{}) に更新しました", result.new_version),
        None => println!("✨ すでに最新版です"),
    }

    Ok(())
}

fn resolve_installer_url() -> String {
    std::env::var(INSTALLER_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_INSTALLER_URL.to_string())
}

fn resolve_latest_release_api_url() -> Result<String> {
    if let Some(explicit) = std::env::var(UPDATE_RELEASE_API_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(explicit);
    }

    let repository = env!("CARGO_PKG_REPOSITORY").trim().trim_end_matches('/');
    let path = repository
        .strip_prefix("https://github.com/")
        .or_else(|| repository.strip_prefix("http://github.com/"))
        .or_else(|| repository.strip_prefix("git@github.com:"))
        .ok_or_else(|| {
            anyhow::anyhow!("unsupported repository url for update source: {repository}")
        })?;
    let path = path.trim_end_matches(".git");
    let mut segments = path.split('/');
    let owner = segments.next().unwrap_or("").trim();
    let repo = segments.next().unwrap_or("").trim();
    if owner.is_empty() || repo.is_empty() || segments.next().is_some() {
        bail!("invalid repository url for update source: {repository}");
    }

    Ok(format!(
        "https://api.github.com/repos/{owner}/{repo}/releases/latest"
    ))
}

fn normalize_release_tag_to_version(tag_name: &str) -> Result<Version> {
    let normalized = tag_name.trim().trim_start_matches('v');
    Version::parse(normalized)
        .with_context(|| format!("latest release tag is not a valid semver version: {tag_name}"))
}

fn fetch_latest_release_version() -> Result<Version> {
    let api_url = resolve_latest_release_api_url()?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(format!("ato/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("latest version 確認用の HTTP client を初期化できませんでした")?;
    let payload: GithubLatestRelease = client
        .get(&api_url)
        .send()
        .with_context(|| format!("latest release 情報を取得できませんでした: {api_url}"))?
        .error_for_status()
        .with_context(|| format!("latest release 情報を取得できませんでした: {api_url}"))?
        .json()
        .with_context(|| format!("latest release 応答を解析できませんでした: {api_url}"))?;
    normalize_release_tag_to_version(&payload.tag_name)
}

#[cfg(not(windows))]
fn fallback_update_via_installer() -> Result<()> {
    let installer_url = resolve_installer_url();
    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))
        .context("現在の ato version を解析できませんでした")?;

    match fetch_latest_release_version() {
        Ok(latest_version) if latest_version <= current_version => {
            println!("✨ すでに最新版です");
            return Ok(());
        }
        Ok(latest_version) => {
            println!(
                "⬆️  v{} から v{} へ更新します...",
                current_version, latest_version
            );
        }
        Err(err) => {
            eprintln!("⚠️  最新版の確認に失敗したため installer fallback を継続します: {err}");
        }
    }

    let current_exe =
        std::env::current_exe().context("現在の ato 実行パスを取得できませんでした")?;
    let install_dir = current_exe.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "現在の ato 実行パスから install directory を解決できませんでした: {}",
            current_exe.display()
        )
    })?;

    let installer_body = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("installer ダウンロード用の HTTP client を初期化できませんでした")?
        .get(&installer_url)
        .send()
        .with_context(|| format!("installer を取得できませんでした: {installer_url}"))?
        .error_for_status()
        .with_context(|| format!("installer を取得できませんでした: {installer_url}"))?
        .text()
        .with_context(|| format!("installer の応答を読み取れませんでした: {installer_url}"))?;

    let temp_dir =
        tempfile::tempdir().context("installer 実行用の一時ディレクトリを作成できませんでした")?;
    let installer_path = temp_dir.path().join("ato-install.sh");
    fs::write(&installer_path, installer_body).with_context(|| {
        format!(
            "installer を保存できませんでした: {}",
            installer_path.display()
        )
    })?;

    let status = Command::new("sh")
        .arg(&installer_path)
        .env("ATO_INSTALL_DIR", install_dir)
        .env("ATO_SKIP_NACELLE_INSTALL", "1")
        .status()
        .context("installer fallback を起動できませんでした")?;

    if !status.success() {
        bail!(
            "installer fallback が失敗しました (exit code: {})",
            status
                .code()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "signal".to_string())
        );
    }

    println!("✅ installer fallback により ato を更新しました");
    Ok(())
}

#[cfg(windows)]
fn fallback_update_via_installer() -> Result<()> {
    bail!(
        "receipt がないため update を継続できません。Windows では手動で再インストールしてください"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_release_tag_to_version, resolve_installer_url, resolve_latest_release_api_url,
        DEFAULT_INSTALLER_URL, INSTALLER_URL_ENV, UPDATE_RELEASE_API_URL_ENV,
    };
    use serial_test::serial;

    #[test]
    #[serial]
    fn resolve_installer_url_uses_default_when_env_missing() {
        std::env::remove_var(INSTALLER_URL_ENV);
        assert_eq!(resolve_installer_url(), DEFAULT_INSTALLER_URL);
    }

    #[test]
    #[serial]
    fn resolve_installer_url_prefers_env_override() {
        std::env::set_var(INSTALLER_URL_ENV, "https://example.test/install.sh");
        assert_eq!(resolve_installer_url(), "https://example.test/install.sh");
        std::env::remove_var(INSTALLER_URL_ENV);
    }

    #[test]
    fn normalize_release_tag_to_version_accepts_v_prefix() {
        let version = normalize_release_tag_to_version("v0.4.14").expect("parse version");
        assert_eq!(version.to_string(), "0.4.14");
    }

    #[test]
    #[serial]
    fn resolve_latest_release_api_url_prefers_env_override() {
        std::env::set_var(
            UPDATE_RELEASE_API_URL_ENV,
            "https://example.test/repos/Koh0920/ato-cli/releases/latest",
        );
        assert_eq!(
            resolve_latest_release_api_url().expect("resolve api url"),
            "https://example.test/repos/Koh0920/ato-cli/releases/latest"
        );
        std::env::remove_var(UPDATE_RELEASE_API_URL_ENV);
    }
}

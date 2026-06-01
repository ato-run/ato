use crate::error::{CapsuleError, Result};
use crate::security;
use reqwest::StatusCode;
use reqwest::redirect::Policy;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use tracing::info;

/// Validate that `url` uses the HTTPS scheme and is well-formed.
/// Returns the parsed URL on success.
fn validate_https_url(raw: &str) -> Result<()> {
    let parsed =
        url::Url::parse(raw).map_err(|e| CapsuleError::Config(format!("Invalid URL: {}", e)))?;
    if parsed.scheme() != "https" {
        return Err(CapsuleError::Config(format!(
            "only HTTPS URLs are allowed (got {})",
            parsed.scheme()
        )));
    }
    Ok(())
}

/// Build a `reqwest::Client` that refuses to follow redirects to
/// non-HTTPS URLs. The initial URL is already validated by the caller,
/// but a redirect could downgrade to HTTP — this policy prevents that.
fn https_only_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(Policy::custom(|attempt| {
            if attempt.url().scheme() != "https" {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .expect("https_only_client must always build")
}

/// Download a file from a URL to a destination path
///
/// # Arguments
/// * `url` - The URL to download from (must be HTTPS)
/// * `destination` - The local path to save the file to
/// * `allowed_paths` - List of allowed host paths
///
/// # Returns
/// The number of bytes downloaded
///
/// # Security
/// Validates that `destination` is within allowed paths using `security::validate_path`.
/// Refuses non-HTTPS URLs and redirects to non-HTTPS targets.
pub async fn download_file(url: &str, destination: &str, allowed_paths: &[String]) -> Result<u64> {
    // 1. Security Validation
    security::validate_path(destination, allowed_paths)
        .map_err(|e| CapsuleError::Config(format!("Invalid destination path: {}", e)))?;

    // 2. Validate URL scheme — only HTTPS allowed
    validate_https_url(url)?;

    info!("Starting download from {} to {}", url, destination);

    // 3. Create destination directory if it doesn't exist
    let dest_path = Path::new(destination);
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // 4. Perform Download with HTTPS-only redirect policy
    let client = https_only_client();
    let response = client
        .get(url)
        .send()
        .await
        .map_err(CapsuleError::Network)?;
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(CapsuleError::AuthRequired(url.to_string()));
    }
    if status == StatusCode::NOT_FOUND {
        return Err(CapsuleError::NotFound(url.to_string()));
    }
    if !status.is_success() {
        return Err(CapsuleError::Network(
            response.error_for_status().unwrap_err(),
        ));
    }

    let content = response.bytes().await.map_err(CapsuleError::Network)?;
    let bytes_downloaded = content.len() as u64;

    // 5. Write to file
    let mut file = File::create(dest_path)?;
    file.write_all(&content)?;

    info!(
        "Download completed successfully: {} ({} bytes)",
        destination, bytes_downloaded
    );
    Ok(bytes_downloaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── validate_https_url ──────────────────────────────────────────

    #[test]
    fn validate_https_url_accepts_https() {
        assert!(validate_https_url("https://example.com/model.bin").is_ok());
    }

    #[test]
    fn validate_https_url_rejects_http() {
        let err = validate_https_url("http://example.com/model.bin").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("only HTTPS URLs are allowed"),
            "expected rejection message, got: {msg}",
        );
    }

    #[test]
    fn validate_https_url_rejects_file() {
        let err = validate_https_url("file:///etc/passwd").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("only HTTPS URLs are allowed"),
            "expected rejection message, got: {msg}",
        );
    }

    #[test]
    fn validate_https_url_rejects_invalid() {
        let err = validate_https_url("not-a-url").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid URL"),
            "expected parse error, got: {msg}",
        );
    }

    #[test]
    fn validate_https_url_accepts_uppercase_scheme() {
        assert!(validate_https_url("HTTPS://example.com/model.bin").is_ok());
    }

    // ── https_only_client redirect policy ───────────────────────────

    #[test]
    fn https_only_client_has_custom_redirect_policy() {
        let client = https_only_client();
        // Verify the client was built successfully — the custom policy
        // is exercised at request time, so we just confirm construction.
        let _ = client;
    }

    // ── download_file path validation (stays, url now https://) ─────

    #[tokio::test]
    async fn test_download_file_security_check() {
        let allowed_paths = vec!["/opt/models".to_string()];
        let result = download_file("https://example.com", "/tmp/malicious", &allowed_paths).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid destination path") || err_msg.contains("path traversal"),
            "expected path validation error, got: {err_msg}",
        );
    }
}

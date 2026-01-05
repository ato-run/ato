use crate::security;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use tracing::info;

/// Download a file from a URL to a destination path
///
/// # Arguments
/// * `url` - The URL to download from
/// * `destination` - The local path to save the file to
/// * `allowed_paths` - List of allowed host paths
///
/// # Returns
/// The number of bytes downloaded
///
/// # Security
/// Validates that `destination` is within allowed paths using `security::validate_path`.
pub async fn download_file(url: &str, destination: &str, allowed_paths: &[String]) -> Result<u64> {
    // 1. Security Validation
    security::validate_path(destination, allowed_paths)
        .map_err(|e| anyhow::anyhow!("Invalid destination path: {}", e))?;

    info!("Starting download from {} to {}", url, destination);

    // 2. Create destination directory if it doesn't exist
    let dest_path = Path::new(destination);
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create parent directory")?;
    }

    // 3. Perform Download
    let response = reqwest::get(url).await.context("Failed to send request")?;
    let content = response
        .bytes()
        .await
        .context("Failed to read response body")?;
    let bytes_downloaded = content.len() as u64;

    // 4. Write to file
    let mut file = File::create(dest_path).context("Failed to create file")?;
    file.write_all(&content)
        .context("Failed to write to file")?;

    info!(
        "Download completed successfully: {} ({} bytes)",
        destination, bytes_downloaded
    );
    Ok(bytes_downloaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_download_file_security_check() {
        let allowed_paths = vec!["/opt/models".to_string()];
        // Should fail because path is not in allowlist
        let result = download_file("http://example.com", "/tmp/malicious", &allowed_paths).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Invalid destination path") || err_msg.contains("path traversal"));
    }
}

use anyhow::{Context, Result};

use super::http::http_get_text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NacelleRelease {
    pub version: String,
    pub binary_name: String,
    pub url: String,
    pub sha256: String,
}

pub(super) fn fetch_latest_nacelle_version_from_base_url(release_base_url: &str) -> Result<String> {
    let (status, resp) = http_get_text(&format!(
        "{}/latest.txt",
        release_base_url.trim_end_matches('/')
    ))
    .context("Failed to fetch latest nacelle version")?;
    if !(200..300).contains(&status) {
        anyhow::bail!(
            "Latest nacelle version download failed with status: {}",
            status
        );
    }
    let version = resp.trim();
    if version.is_empty() {
        anyhow::bail!("Latest nacelle version response was empty");
    }
    Ok(version.to_string())
}

pub(crate) fn fetch_release_sha256(base_url: &str, binary_name: &str) -> Result<String> {
    let checksum_urls = [
        format!("{}/{}.sha256", base_url, binary_name),
        format!("{}/SHA256SUMS", base_url),
        format!("{}/SHA256SUMS.txt", base_url),
        format!("{}/sha256sums.txt", base_url),
    ];

    for checksum_url in checksum_urls {
        let (status, body) = match http_get_text(&checksum_url) {
            Ok(response) => response,
            Err(_) => continue,
        };
        if !(200..300).contains(&status) {
            continue;
        }

        if let Some(hash) = parse_sha256_for_artifact(&body, binary_name) {
            return Ok(hash);
        }

        if checksum_url.ends_with(".sha256") {
            if let Some(hash) = extract_first_sha256_hex(&body) {
                return Ok(hash);
            }
        }
    }

    anyhow::bail!(
        "Failed to resolve SHA256 for {} (checked release checksum endpoints)",
        binary_name
    )
}

pub(crate) fn parse_sha256_for_artifact(checksum_body: &str, binary_name: &str) -> Option<String> {
    for line in checksum_body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some((name, hash)) = trimmed
            .strip_prefix("SHA256 (")
            .and_then(|rest| rest.split_once(") = "))
            .or_else(|| {
                trimmed
                    .strip_prefix("sha256 (")
                    .and_then(|rest| rest.split_once(") = "))
            })
        {
            if name.trim() == binary_name && is_sha256_hex(hash) {
                return Some(hash.to_ascii_lowercase());
            }
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        let normalized_name = name.trim_start_matches('*');
        if normalized_name == binary_name && is_sha256_hex(hash) {
            return Some(hash.to_ascii_lowercase());
        }
    }

    None
}

pub(crate) fn extract_first_sha256_hex(raw: &str) -> Option<String> {
    raw.split_whitespace()
        .find(|token| is_sha256_hex(token))
        .map(|value| value.to_ascii_lowercase())
}

pub(super) fn resolve_nacelle_release(
    requested_version: &str,
    release_base_url: &str,
    skip_verify: bool,
) -> Result<NacelleRelease> {
    let target_triple = host_target_triple()?;
    let normalized_base = release_base_url.trim_end_matches('/');
    let archive_ext = host_archive_extension()?;
    let binary_name = format!("nacelle-{target_triple}.{archive_ext}");
    let version_base_url = format!("{}/{}", normalized_base, requested_version);
    let url = format!("{}/{}", version_base_url, binary_name);
    let sha256 = if skip_verify {
        String::new()
    } else {
        fetch_release_sha256(&version_base_url, &binary_name)?
    };

    Ok(NacelleRelease {
        version: requested_version.to_string(),
        binary_name,
        url,
        sha256,
    })
}

fn host_target_triple() -> Result<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok("x86_64-apple-darwin")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Ok("aarch64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok("x86_64-pc-windows-msvc")
    } else {
        anyhow::bail!("Unsupported nacelle release target");
    }
}

fn host_archive_extension() -> Result<&'static str> {
    if cfg!(target_os = "windows") {
        Ok("zip")
    } else if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
        Ok("tar.xz")
    } else {
        anyhow::bail!("Unsupported nacelle release archive format");
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit())
}

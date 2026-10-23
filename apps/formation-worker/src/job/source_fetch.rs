//! Source transport only: fixed policy, frozen deadline, bounded raw bytes.
//! Archive interpretation remains in the existing FileVerifiedArchive authority.

use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use ato_formation::source::SourceError;
use reqwest::blocking::{Client, RequestBuilder};
use sha2::{Digest, Sha256};

pub(crate) fn client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .no_proxy()
        // Count the compressed archive bytes actually received. Feature
        // unification elsewhere must never silently enable HTTP decoding here.
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .build()
        .context("cannot configure the bounded source transport")
}

pub(crate) fn remaining(deadline: Instant) -> Result<std::time::Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| SourceError::DeadlineExceeded.into())
}

pub(super) fn github_url(source: &serde_json::Value) -> Result<String> {
    let owner = source["owner"].as_str().context("source has no owner")?;
    let repository = source["repository"]
        .as_str()
        .context("source has no repository")?;
    let commit = source["resolved_commit_sha"]
        .as_str()
        .context("source is not pinned")?;
    ensure!(
        !owner.is_empty()
            && owner.len() <= 39
            && !owner.starts_with('-')
            && !owner.ends_with('-')
            && owner
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "invalid GitHub source owner"
    );
    ensure!(
        !repository.is_empty()
            && repository.len() <= 100
            && !matches!(repository, "." | "..")
            && repository
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
        "invalid GitHub source repository"
    );
    ensure!(
        commit.len() == 40
            && commit
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "GitHub source must name a pinned commit SHA"
    );
    // None of the validated path segments can replace the host, add a query,
    // or select a branch/ref instead of the Coordinator's resolved commit.
    Ok(format!(
        "https://codeload.github.com/{owner}/{repository}/tar.gz/{commit}"
    ))
}

fn limit() -> anyhow::Error {
    SourceError::LimitExceeded {
        limit: "archive_bytes",
    }
    .into()
}

/// At most maximum+1 bytes are read, and at most maximum are written. The
/// extra byte establishes over-limit failure without reading the remainder.
/// No allocation is sized from Content-Length or archive/member metadata.
pub(super) fn copy_bounded(
    mut source: impl Read,
    mut target: impl Write,
    maximum: u64,
    deadline: Instant,
) -> Result<String> {
    ensure!(maximum > 0, "source transport byte limit must be positive");
    let mut total = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        remaining(deadline)?;
        let capacity = (maximum - total).saturating_add(1).min(buffer.len() as u64) as usize;
        let count = source.read(&mut buffer[..capacity]).map_err(|error| {
            if error.kind() == std::io::ErrorKind::TimedOut || Instant::now() >= deadline {
                SourceError::DeadlineExceeded
            } else {
                SourceError::Unusable("source archive transfer failed".to_owned())
            }
        })?;
        remaining(deadline)?;
        if count == 0 {
            break;
        }
        if count as u64 > maximum - total {
            return Err(limit());
        }
        total += count as u64;
        target
            .write_all(&buffer[..count])
            .context("write bounded source archive")?;
        hasher.update(&buffer[..count]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub(super) fn download(
    request: RequestBuilder,
    target: impl Write,
    maximum: u64,
    deadline: Instant,
) -> Result<String> {
    ensure!(maximum > 0, "source transport byte limit must be positive");
    let timeout = remaining(deadline)?;
    let response = request
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        // reqwest carries this total request deadline through the response
        // body, so a blocking read cannot extend it with slow drip chunks.
        .timeout(timeout)
        .send()
        .map_err(|error| {
            if error.is_timeout() || Instant::now() >= deadline {
                SourceError::DeadlineExceeded
            } else {
                SourceError::Unusable("source archive request failed".to_owned())
            }
        })?;
    remaining(deadline)?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "source archive unavailable (HTTP {})",
        response.status().as_u16()
    );
    if response
        .content_length()
        .is_some_and(|length| length > maximum)
    {
        return Err(limit());
    }
    copy_bounded(response, target, maximum, deadline)
}

pub(super) fn temporary_file(root: &Path, maximum: u64, deadline: Instant) -> Result<File> {
    remaining(deadline)?;
    ensure!(
        maximum > 0 && maximum <= crate::runtime_network::MAX_SOURCE_OBJECT_BYTES,
        "source transport cap exceeds the existing Runtime object ceiling"
    );
    std::fs::create_dir_all(root).context("create source transport work root")?;
    let canonical_root = std::fs::canonicalize(root)?;
    let root = root.join(".tmp");
    std::fs::create_dir_all(&root).context("create source transport scratch directory")?;
    let metadata = std::fs::symlink_metadata(&root)?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && std::fs::canonicalize(&root)? == canonical_root.join(".tmp"),
        "source transport scratch must remain inside its owned work root"
    );
    ensure!(
        fs2::available_space(&root)? >= maximum,
        "insufficient source transport scratch capacity"
    );
    // Explicit owned directory, anonymous/unlinked file, RAII cleanup on
    // success, rejection, timeout, cancellation or archive verification error.
    tempfile::tempfile_in(root).context("create owned source archive file")
}

pub(crate) fn download_file(
    request: RequestBuilder,
    root: &Path,
    maximum: u64,
    deadline: Instant,
) -> Result<(File, String)> {
    let mut file = temporary_file(root, maximum, deadline)?;
    let digest = download(request, &mut file, maximum, deadline)?;
    rewind(file, digest)
}

pub(super) fn rewind(mut file: File, digest: String) -> Result<(File, String)> {
    file.rewind().context("rewind acquired source archive")?;
    Ok((file, digest))
}

#[cfg(test)]
mod tests;

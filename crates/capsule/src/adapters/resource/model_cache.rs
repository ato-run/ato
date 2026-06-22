//! Content-addressed cache for managed native-inference model files.
//!
//! Independent of the engine fetcher: resolves a `model_url` + `model_sha256`
//! into a verified, content-addressed blob under `~/.ato/store/blobs/sha256-…`
//! and reuses it offline on subsequent runs. Inc3 supports direct `http(s)` URLs
//! only — no `hf://`, auth, or gated models.

use std::io::Read;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use crate::common::paths::ato_store_blobs_dir;
use crate::error::{CapsuleError, Result};

/// Deterministic content-addressed path for a model by its normalized sha256
/// hex. Known WITHOUT downloading, so the launcher can resolve it during
/// preflight; [`ensure_model`] guarantees the file exists by spawn time.
pub fn model_blob_path(sha256_hex: &str) -> PathBuf {
    ato_store_blobs_dir().join(format!("sha256-{sha256_hex}"))
}

/// Ensure the managed model identified by `sha256_hex` (already normalized to
/// 64-char lowercase hex) is present in the content-addressed cache, and return
/// its path. A valid cached blob is reused offline; otherwise `url` is streamed
/// to a temp file, the sha256 is verified, and the blob is atomically installed
/// read-only. A hash mismatch or a corrupt cached blob is a hard error / forces
/// a rebuild — never a silently wrong model.
pub async fn ensure_model(url: &str, sha256_hex: &str) -> Result<PathBuf> {
    let blob = model_blob_path(sha256_hex);

    if blob.exists() {
        if file_sha256(&blob)? == sha256_hex {
            return Ok(blob);
        }
        // A content-addressed blob whose bytes don't match its name is corrupt;
        // discard and re-fetch rather than serving a bad model.
        make_writable(&blob);
        let _ = std::fs::remove_file(&blob);
    }

    if let Some(parent) = blob.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CapsuleError::Pack(format!("create model cache dir: {e}")))?;
    }

    let tmp = blob.with_file_name(format!(
        "sha256-{sha256_hex}.download-{}.tmp",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);

    if let Err(e) = download_to_file(url, &tmp).await {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    let got = file_sha256(&tmp)?;
    if got != sha256_hex {
        let _ = std::fs::remove_file(&tmp);
        return Err(CapsuleError::Pack(format!(
            "model sha256 mismatch from {url}: expected {sha256_hex}, downloaded {got}"
        )));
    }

    // Atomic same-directory install, then make the blob immutable.
    std::fs::rename(&tmp, &blob)
        .map_err(|e| CapsuleError::Pack(format!("install model blob: {e}")))?;
    make_read_only(&blob);
    Ok(blob)
}

async fn download_to_file(url: &str, dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(CapsuleError::Network)?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(CapsuleError::Network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(CapsuleError::Pack(format!(
            "model download failed: HTTP {} from {url}",
            status.as_u16()
        )));
    }

    let mut file = std::fs::File::create(dest)
        .map_err(|e| CapsuleError::Pack(format!("create {dest:?}: {e}")))?;
    let mut stream = response.bytes_stream();
    use std::io::Write;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(CapsuleError::Network)?;
        file.write_all(&chunk)
            .map_err(|e| CapsuleError::Pack(format!("write {dest:?}: {e}")))?;
    }
    file.flush()
        .map_err(|e| CapsuleError::Pack(format!("flush {dest:?}: {e}")))?;
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| CapsuleError::Pack(format!("open {path:?}: {e}")))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| CapsuleError::Pack(format!("read {path:?}: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn make_writable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o644);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    {
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
}

fn make_read_only(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_readonly(true);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_path_is_content_addressed() {
        let hex = "a".repeat(64);
        let p = model_blob_path(&hex);
        assert!(p.to_string_lossy().ends_with(&format!("sha256-{hex}")));
        assert!(p.to_string_lossy().contains("blobs"));
    }

    #[test]
    fn file_sha256_matches_known_value() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x");
        std::fs::write(&f, b"abc").unwrap();
        // sha256("abc")
        assert_eq!(
            file_sha256(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

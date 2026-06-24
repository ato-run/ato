//! Content-addressed cache for managed native-inference model files.
//!
//! Independent of the engine fetcher: resolves a `model_url` + `model_sha256`
//! into a verified, content-addressed blob under `~/.ato/store/blobs/sha256-…`
//! and reuses it offline on subsequent runs. Inc3 supports direct `http(s)` URLs
//! only — no `hf://`, auth, or gated models.
//!
//! For multi-file engines (e.g. SGLang) the [`ensure_model_repo`] path
//! downloads a whole Hugging Face repo, pinned to an immutable commit, into a
//! content-addressed `~/.ato/store/repos/sha256-…/` directory (the multi-file
//! analogue of a single blob) — gated by a digest-of-digests over the file set.

use std::io::Read;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use crate::common::paths::{ato_store_blobs_dir, ato_store_repos_dir};
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

/// Default Hugging Face file allowlist for a transformer model repo when a
/// capsule does not specify `model_repo_include`: config, weights, and tokenizer
/// (everything SGLang needs to load a model; large unrelated assets are skipped).
const DEFAULT_HF_INCLUDE: &[&str] = &[
    "config.json",
    "generation_config.json",
    "*.safetensors",
    "*.safetensors.index.json",
    "tokenizer*",
    "*.model",
    "special_tokens_map.json",
];

/// A pinned, content-addressed Hugging Face model-repo request.
///
/// `repo_sha256` is the digest-of-digests over the included file set (computed by
/// `ato lock`); it is BOTH the cache directory name and the integrity gate, so a
/// repo whose downloaded bytes don't reproduce it is rejected rather than served.
#[derive(Debug, Clone)]
pub struct HfRepoSpec<'a> {
    /// `<org>/<name>` repo id (already validated by `is_safe_hf_repo`).
    pub repo: &'a str,
    /// Immutable 40-hex commit (already validated by `is_safe_hf_revision`).
    pub revision: &'a str,
    /// Digest-of-digests over the included file set (normalized 64-hex).
    pub repo_sha256: &'a str,
    /// Glob allowlist of files to include; empty = [`DEFAULT_HF_INCLUDE`].
    pub include: &'a [String],
    /// Whether the repo is gated (send `HF_TOKEN` as a bearer credential).
    pub gated: bool,
}

/// Deterministic content-addressed path for a managed model REPO by its
/// digest-of-digests (normalized 64-char lowercase hex). Known WITHOUT
/// downloading, so the launcher can resolve `--model-path` during preflight;
/// [`ensure_model_repo`] guarantees the directory exists by spawn time.
pub fn model_repo_path(repo_sha256_hex: &str) -> PathBuf {
    ato_store_repos_dir().join(format!("sha256-{repo_sha256_hex}"))
}

/// One file in a Hugging Face repo tree at the pinned commit.
#[derive(Debug, Clone)]
struct HfTreeEntry {
    path: String,
}

/// Ensure the managed model repo described by `spec` is materialized in the
/// content-addressed cache, and return its directory.
///
/// Reproducible + content-addressed: the repo tree is listed at the pinned
/// commit, filtered by the include globs, each file streamed into the shared
/// `blobs/` CAS (verified by its own sha256), a digest-of-digests is computed
/// over the sorted `(path, sha256)` pairs and gated against `spec.repo_sha256`,
/// then the blobs are hard-linked (copy fallback) into a temp repo dir, atomically
/// renamed into place, and made read-only. A digest mismatch is a hard error —
/// never a silently wrong model. A valid cached repo is reused offline.
pub async fn ensure_model_repo(spec: &HfRepoSpec<'_>) -> Result<PathBuf> {
    let repo_sha = crate::foundation::types::manifest::normalize_model_sha256(spec.repo_sha256)
        .ok_or_else(|| {
            CapsuleError::Pack(format!(
                "model_repo_sha256 must be a 64-char hex digest, got {:?}",
                spec.repo_sha256
            ))
        })?;
    let repo_dir = model_repo_path(&repo_sha);

    // A repo dir is content-addressed by the digest-of-digests, so a present,
    // marked-complete directory is trusted offline (re-hashing every shard on
    // each run would be prohibitive for multi-GB weights). The `.ato-complete`
    // marker is written only after the digest gate passed and the rename landed.
    if repo_dir.join(".ato-complete").exists() {
        return Ok(repo_dir);
    }
    // A partial/incomplete dir from an interrupted run is discarded and rebuilt.
    if repo_dir.exists() {
        make_tree_writable(&repo_dir);
        let _ = std::fs::remove_dir_all(&repo_dir);
    }

    let token = if spec.gated {
        let t = std::env::var("HF_TOKEN")
            .or_else(|_| std::env::var("HUGGING_FACE_HUB_TOKEN"))
            .map_err(|_| {
                CapsuleError::AuthRequired(format!(
                    "model_repo {} is gated; set HF_TOKEN to a Hugging Face access token",
                    spec.repo
                ))
            })?;
        Some(t)
    } else {
        None
    };

    // 1. List the repo tree at the pinned commit, keep the included files.
    let entries = list_hf_repo_tree(spec.repo, spec.revision, token.as_deref()).await?;
    let include = if spec.include.is_empty() {
        DEFAULT_HF_INCLUDE
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    } else {
        spec.include.to_vec()
    };
    let mut selected: Vec<HfTreeEntry> = entries
        .into_iter()
        .filter(|e| include.iter().any(|p| glob_match(p, file_name_of(&e.path))))
        .collect();
    selected.sort_by(|a, b| a.path.cmp(&b.path));
    selected.dedup_by(|a, b| a.path == b.path);
    if selected.is_empty() {
        return Err(CapsuleError::Pack(format!(
            "model_repo {}@{} matched no files for include={:?}",
            spec.repo, spec.revision, include
        )));
    }

    // 2. Stream each file into the shared blob CAS (verified by its own sha256),
    //    collecting (path, sha256) pairs for the digest-of-digests.
    let blobs_dir = ato_store_blobs_dir();
    std::fs::create_dir_all(&blobs_dir)
        .map_err(|e| CapsuleError::Pack(format!("create blobs dir: {e}")))?;
    let mut file_digests: Vec<(String, String)> = Vec::with_capacity(selected.len());
    for entry in &selected {
        let sha = ensure_hf_file_blob(spec.repo, spec.revision, &entry.path, token.as_deref())
            .await?;
        file_digests.push((entry.path.clone(), sha));
    }

    // 3. Digest-of-digests gate (fail closed on mismatch).
    let computed = digest_of_digests(&file_digests);
    if computed != repo_sha {
        return Err(CapsuleError::Pack(format!(
            "model_repo {}@{} digest mismatch: expected {repo_sha}, computed {computed} \
             over {} files (run `ato lock` to refresh the pin)",
            spec.repo,
            spec.revision,
            file_digests.len()
        )));
    }

    // 4. Hard-link (copy fallback) blobs into a temp repo dir preserving the
    //    repo's relative paths, then atomically install + make read-only.
    if let Some(parent) = repo_dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CapsuleError::Pack(format!("create repos dir: {e}")))?;
    }
    let tmp = repo_dir.with_file_name(format!(
        "sha256-{repo_sha}.materialize-{}.tmp",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)
        .map_err(|e| CapsuleError::Pack(format!("create temp repo dir: {e}")))?;
    for (path, sha) in &file_digests {
        let blob = model_blob_path(sha);
        let dest = tmp.join(path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CapsuleError::Pack(format!("create {parent:?}: {e}")))?;
        }
        link_or_copy(&blob, &dest)?;
    }
    // Mark complete only after every file landed, then publish atomically.
    std::fs::write(tmp.join(".ato-complete"), b"")
        .map_err(|e| CapsuleError::Pack(format!("write completion marker: {e}")))?;
    std::fs::rename(&tmp, &repo_dir).map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        CapsuleError::Pack(format!("install model repo: {e}"))
    })?;
    make_tree_read_only(&repo_dir);
    Ok(repo_dir)
}

/// List the file paths in a Hugging Face repo at `revision` via the public tree
/// API (`/api/models/<repo>/tree/<revision>?recursive=true`), following
/// pagination via the `Link: …; rel="next"` header. Only `type == "file"`
/// entries are returned. Sends `HF_TOKEN` when `bearer` is set (gated repos).
async fn list_hf_repo_tree(
    repo: &str,
    revision: &str,
    bearer: Option<&str>,
) -> Result<Vec<HfTreeEntry>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(CapsuleError::Network)?;
    let mut url = format!("https://huggingface.co/api/models/{repo}/tree/{revision}?recursive=true");
    let mut out = Vec::new();
    loop {
        let mut request = client.get(&url);
        if let Some(token) = bearer.map(str::trim).filter(|t| !t.is_empty()) {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(CapsuleError::Network)?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(CapsuleError::AuthRequired(format!(
                "Hugging Face repo {repo} (gated or private); set HF_TOKEN"
            )));
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(CapsuleError::NotFound(format!(
                "Hugging Face repo {repo}@{revision} not found"
            )));
        }
        if !status.is_success() {
            return Err(CapsuleError::Pack(format!(
                "Hugging Face tree listing failed: HTTP {} for {repo}@{revision}",
                status.as_u16()
            )));
        }
        let next = response
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_link_next);
        let page: serde_json::Value = response.json().await.map_err(CapsuleError::Network)?;
        let array = page.as_array().ok_or_else(|| {
            CapsuleError::Pack(format!("Hugging Face tree for {repo} was not a JSON array"))
        })?;
        for item in array {
            let is_file = item
                .get("type")
                .and_then(|t| t.as_str())
                .map(|t| t == "file")
                .unwrap_or(false);
            if !is_file {
                continue;
            }
            if let Some(path) = item.get("path").and_then(|p| p.as_str()) {
                out.push(HfTreeEntry {
                    path: path.to_string(),
                });
            }
        }
        match next {
            Some(n) => url = n,
            None => break,
        }
    }
    Ok(out)
}

/// Ensure one repo file is present in the shared blob CAS and return its sha256.
/// Streams `/resolve/<revision>/<path>` to a temp file, hashes it, then installs
/// it as `blobs/sha256-<hash>` (atomic, read-only). A blob already present for an
/// identical file is reused across repos.
async fn ensure_hf_file_blob(
    repo: &str,
    revision: &str,
    path: &str,
    bearer: Option<&str>,
) -> Result<String> {
    let url = format!("https://huggingface.co/{repo}/resolve/{revision}/{path}");
    let blobs_dir = ato_store_blobs_dir();
    let tmp = blobs_dir.join(format!(
        "hf-{}-{}.download-{}.tmp",
        sanitize_component(repo),
        sanitize_component(path),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) = download_to_file_with(&url, &tmp, bearer).await {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let sha = match file_sha256(&tmp) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    let blob = model_blob_path(&sha);
    if blob.exists() {
        // Already content-addressed in cache → drop the duplicate download.
        let _ = std::fs::remove_file(&tmp);
    } else {
        std::fs::rename(&tmp, &blob).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            CapsuleError::Pack(format!("install repo-file blob {path}: {e}"))
        })?;
        make_read_only(&blob);
    }
    Ok(sha)
}

/// The repo's content digest: sha256 over the sorted `"<path>\0<sha256>\n"`
/// lines. Stable across runs/hosts for the same file set, so it is the
/// reproducible cache key + integrity gate (the multi-file analogue of a single
/// file's sha256).
fn digest_of_digests(file_digests: &[(String, String)]) -> String {
    let mut sorted = file_digests.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (path, sha) in &sorted {
        hasher.update(path.as_bytes());
        hasher.update([0u8]);
        hasher.update(sha.as_bytes());
        hasher.update([b'\n']);
    }
    hex::encode(hasher.finalize())
}

/// Parse the `next` URL out of an RFC-5988 `Link` header, if present.
fn parse_link_next(header: &str) -> Option<String> {
    for part in header.split(',') {
        let part = part.trim();
        if !part.contains("rel=\"next\"") {
            continue;
        }
        let start = part.find('<')?;
        let end = part.find('>')?;
        if end > start + 1 {
            return Some(part[start + 1..end].to_string());
        }
    }
    None
}

/// The final path component (filename) of a repo-relative path.
fn file_name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Minimal glob match for HF include patterns: supports a leading and/or
/// trailing `*` (e.g. `*.safetensors`, `tokenizer*`, `*`) plus exact names. This
/// is matched against the FILENAME only (not the full path).
fn glob_match(pattern: &str, name: &str) -> bool {
    match (pattern.strip_prefix('*'), pattern.strip_suffix('*')) {
        // `*` alone → match everything.
        _ if pattern == "*" => true,
        // `*suffix`
        (Some(suffix), None) => name.ends_with(suffix),
        // `prefix*`
        (None, Some(prefix)) => name.starts_with(prefix),
        // `*middle*`
        (Some(_), Some(_)) => {
            let middle = &pattern[1..pattern.len() - 1];
            middle.is_empty() || name.contains(middle)
        }
        // exact
        (None, None) => name == pattern,
    }
}

/// Sanitize a string for use as a temp-file name component.
fn sanitize_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Hard-link `src` → `dest`, falling back to a byte copy when hard-linking is not
/// possible (cross-device, or a filesystem without hard links). Hard-linking
/// keeps the multi-GB weights single-instanced in the blob CAS.
fn link_or_copy(src: &Path, dest: &Path) -> Result<()> {
    if std::fs::hard_link(src, dest).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dest)
        .map(|_| ())
        .map_err(|e| CapsuleError::Pack(format!("materialize {dest:?} from blob: {e}")))
}

fn make_tree_read_only(root: &Path) {
    visit_tree(root, &|p| make_read_only(p));
}

fn make_tree_writable(root: &Path) {
    visit_tree(root, &|p| make_writable(p));
}

/// Apply `f` to every file under `root` (best-effort; ignores I/O errors).
fn visit_tree(root: &Path, f: &dyn Fn(&Path)) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_tree(&path, f);
        } else {
            f(&path);
        }
    }
}

async fn download_to_file(url: &str, dest: &Path) -> Result<()> {
    download_to_file_with(url, dest, None).await
}

/// Stream `url` to `dest`. When `bearer` is `Some`, send it as an
/// `Authorization: Bearer …` header (gated Hugging Face repos). The token is
/// used transiently and never logged or written to disk.
async fn download_to_file_with(url: &str, dest: &Path, bearer: Option<&str>) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(CapsuleError::Network)?;
    let mut request = client.get(url);
    if let Some(token) = bearer.map(str::trim).filter(|t| !t.is_empty()) {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(CapsuleError::Network)?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(CapsuleError::AuthRequired(url.to_string()));
    }
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

    #[test]
    fn repo_path_is_content_addressed() {
        let hex = "b".repeat(64);
        let p = model_repo_path(&hex);
        assert!(p.to_string_lossy().ends_with(&format!("sha256-{hex}")));
        assert!(p.to_string_lossy().contains("repos"));
    }

    #[test]
    fn glob_match_supports_hf_include_patterns() {
        assert!(glob_match("*.safetensors", "model-00001-of-00002.safetensors"));
        assert!(!glob_match("*.safetensors", "config.json"));
        assert!(glob_match("tokenizer*", "tokenizer.json"));
        assert!(glob_match("tokenizer*", "tokenizer_config.json"));
        assert!(!glob_match("tokenizer*", "vocab.txt"));
        assert!(glob_match("config.json", "config.json"));
        assert!(!glob_match("config.json", "generation_config.json"));
        assert!(glob_match("*", "anything.bin"));
    }

    #[test]
    fn glob_match_uses_filename_only() {
        // The matcher is fed the filename via file_name_of, so a directory
        // prefix never defeats a filename pattern.
        assert_eq!(file_name_of("sub/dir/model.safetensors"), "model.safetensors");
        assert!(glob_match("*.safetensors", file_name_of("a/b/x.safetensors")));
    }

    #[test]
    fn digest_of_digests_is_order_independent_and_path_bound() {
        let a = vec![
            ("config.json".to_string(), "11".repeat(32)),
            ("model.safetensors".to_string(), "22".repeat(32)),
        ];
        // Reordering the input must not change the digest (it sorts by path).
        let mut b = a.clone();
        b.reverse();
        assert_eq!(digest_of_digests(&a), digest_of_digests(&b));
        // Changing a path (not just a hash) changes the digest.
        let c = vec![
            ("CONFIG.json".to_string(), "11".repeat(32)),
            ("model.safetensors".to_string(), "22".repeat(32)),
        ];
        assert_ne!(digest_of_digests(&a), digest_of_digests(&c));
        // It is a 64-hex sha256.
        let d = digest_of_digests(&a);
        assert_eq!(d.len(), 64);
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn parse_link_next_extracts_pagination_url() {
        let header =
            "<https://huggingface.co/api/models/x/tree/abc?cursor=2>; rel=\"next\", <https://x>; rel=\"first\"";
        assert_eq!(
            parse_link_next(header).as_deref(),
            Some("https://huggingface.co/api/models/x/tree/abc?cursor=2")
        );
        assert_eq!(parse_link_next("<https://x>; rel=\"prev\""), None);
        assert_eq!(parse_link_next(""), None);
    }
}

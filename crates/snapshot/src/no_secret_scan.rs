//! L4 (#912): a **reusable no-secret scanner** — the release-gate utility.
//!
//! PR #915's bound-run E2E proved a live secret never leaks into any host artifact by
//! scanning CAS / manifest / rootfs / vmstate / memory / overlay for the raw secret.
//! This module extracts that scan so the run/stop paths, the Track C builder
//! validation, and the store benchmark harness (L6) can all reuse ONE strict scanner
//! that emits a structured result.
//!
//! Strict by default: there is **no allowlist**. A hit anywhere fails the gate. Results
//! carry paths only — **never** the secret content.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// The set of host artifacts a Ready-State no-secret scan covers. Callers pass the
/// roots they have; missing paths are skipped (recorded in `skipped`).
#[derive(Debug, Clone, Default)]
pub struct ScanTargets {
    pub cas: Option<PathBuf>,
    pub manifest: Option<PathBuf>,
    pub rootfs: Option<PathBuf>,
    pub vmstate: Option<PathBuf>,
    pub memory: Option<PathBuf>,
    pub overlay: Option<PathBuf>,
    pub receipts: Option<PathBuf>,
    pub logs: Option<PathBuf>,
    /// Any additional roots (e.g. a materialized work dir).
    pub extra: Vec<PathBuf>,
}

impl ScanTargets {
    fn labelled_roots(&self) -> Vec<(&'static str, PathBuf)> {
        let mut v = Vec::new();
        let mut push = |name: &'static str, p: &Option<PathBuf>| {
            if let Some(p) = p {
                v.push((name, p.clone()));
            }
        };
        push("cas", &self.cas);
        push("manifest", &self.manifest);
        push("rootfs", &self.rootfs);
        push("vmstate", &self.vmstate);
        push("memory", &self.memory);
        push("overlay", &self.overlay);
        push("receipts", &self.receipts);
        push("logs", &self.logs);
        for e in &self.extra {
            v.push(("extra", e.clone()));
        }
        v
    }
}

/// A place a secret was found — path + which target root it belonged to. Never carries
/// the secret content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SecretHit {
    pub target: String,
    pub path: String,
}

/// Structured no-secret scan result. `clean` is the release-gate boolean.
#[derive(Debug, Clone, Serialize)]
pub struct NoSecretScanResult {
    pub clean: bool,
    /// Roots that existed and were scanned (target labels).
    pub scanned: Vec<String>,
    /// Roots that were requested but absent (e.g. an overlay destroyed on stop).
    pub skipped: Vec<String>,
    pub files_scanned: usize,
    /// Path-only hits (never the secret content).
    pub hits: Vec<SecretHit>,
}

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                walk(&e.path(), out);
            }
        }
    } else if p.is_file() {
        out.push(p.to_path_buf());
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Scan `targets` for any of the raw `secrets`. A file matching any secret is a hit.
/// Strict: a single hit ⇒ `clean = false`.
pub fn scan(targets: &ScanTargets, secrets: &[&[u8]]) -> NoSecretScanResult {
    let mut scanned = Vec::new();
    let mut skipped = Vec::new();
    let mut hits = Vec::new();
    let mut files_scanned = 0usize;

    for (label, root) in targets.labelled_roots() {
        if !root.exists() {
            skipped.push(label.to_string());
            continue;
        }
        scanned.push(label.to_string());
        let mut files = Vec::new();
        walk(&root, &mut files);
        for f in files {
            files_scanned += 1;
            if let Ok(bytes) = std::fs::read(&f)
                && secrets.iter().any(|s| contains(&bytes, s))
            {
                hits.push(SecretHit { target: label.to_string(), path: f.display().to_string() });
            }
        }
    }

    NoSecretScanResult { clean: hits.is_empty(), scanned, skipped, files_scanned, hits }
}

/// Convenience for a single in-memory blob (e.g. a serialized manifest JSON) — returns
/// true if the blob is clean of every secret.
pub fn blob_is_clean(blob: &[u8], secrets: &[&[u8]]) -> bool {
    !secrets.iter().any(|s| contains(blob, s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_flags_a_secret_and_reports_path_only() {
        let dir = tempfile::tempdir().unwrap();
        let cas = dir.path().join("cas");
        std::fs::create_dir_all(&cas).unwrap();
        std::fs::write(cas.join("blob1"), b"harmless content").unwrap();
        std::fs::write(cas.join("blob2"), b"prefix SECRET-XYZ suffix").unwrap();

        let targets = ScanTargets { cas: Some(cas.clone()), ..Default::default() };
        let r = scan(&targets, &[b"SECRET-XYZ"]);
        assert!(!r.clean, "a secret present ⇒ not clean");
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].target, "cas");
        assert!(r.hits[0].path.ends_with("blob2"));
        // path only — the result never carries the secret.
        assert!(!serde_json::to_string(&r).unwrap().contains("SECRET-XYZ"));
        assert_eq!(r.files_scanned, 2);
    }

    #[test]
    fn clean_when_absent_and_skips_missing_roots() {
        let dir = tempfile::tempdir().unwrap();
        let cas = dir.path().join("cas");
        std::fs::create_dir_all(&cas).unwrap();
        std::fs::write(cas.join("b"), b"no secret here").unwrap();
        let targets = ScanTargets {
            cas: Some(cas),
            overlay: Some(dir.path().join("gone")), // absent ⇒ skipped (destroyed on stop)
            ..Default::default()
        };
        let r = scan(&targets, &[b"SECRET-XYZ"]);
        assert!(r.clean);
        assert!(r.scanned.contains(&"cas".to_string()));
        assert!(r.skipped.contains(&"overlay".to_string()));
    }

    #[test]
    fn multiple_secrets_and_blob_helper() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), b"has TOKEN-A only").unwrap();
        let targets = ScanTargets { extra: vec![dir.path().to_path_buf()], ..Default::default() };
        let r = scan(&targets, &[b"TOKEN-A", b"TOKEN-B"]);
        assert!(!r.clean, "any of the secrets present ⇒ not clean");
        assert!(blob_is_clean(b"clean blob", &[b"TOKEN-A", b"TOKEN-B"]));
        assert!(!blob_is_clean(b"has TOKEN-B", &[b"TOKEN-A", b"TOKEN-B"]));
    }
}

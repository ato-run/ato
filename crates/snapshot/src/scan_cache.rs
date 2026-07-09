//! Content-addressed cache of per-layer heuristic scan results.
//!
//! Key = (layer content hash = `BlobManifest::id().hex()`, [`SCANNER_VERSION`],
//! [`POLICY_VERSION`]) — all three encoded in the on-disk PATH, so a bump of
//! either version yields a different subtree ⇒ a guaranteed miss ⇒ a forced
//! re-scan (no cross-version reuse). Lives under the CAS root next to `blobs/`.
//!
//! Security boundary: this caches ONLY the **advisory** heuristic findings of the
//! large opaque layers (rootfs/runtime/vmstate/memory). Declared markers and the
//! app/dependency blocking checks are NEVER cached — they are scanned fresh every
//! build — so no cache state can ever suppress a fail-closed decision. Reads fail
//! **closed to a MISS** on any IO/parse/version mismatch: a bad entry is never
//! trusted and never causes a scan to be skipped.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::scanner::{FindingKind, POLICY_VERSION, SCANNER_VERSION, SecretFinding};

/// A finding without its layer — the layer is implied by the lookup key, so it is
/// re-attached on read. (Avoids serializing the `&'static str` layer field.)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedFinding {
    offset: usize,
    len: usize,
    kind: FindingKind,
    detail: String,
}

/// The on-disk per-layer scan receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedLayerScan {
    blob_id: String,
    scanner_version: String,
    policy_version: String,
    /// True when the advisory scan was budget-capped (partial coverage).
    capped: bool,
    findings: Vec<CachedFinding>,
}

/// A cache hit: the layer's heuristic findings plus whether the scan was capped.
pub struct CachedScan {
    pub findings: Vec<SecretFinding>,
    pub capped: bool,
}

/// Filesystem-backed scan cache rooted under a CAS store.
pub struct ScanCache {
    dir: PathBuf,
}

impl ScanCache {
    /// Open (lazily) the scan cache under a CAS root: `<cas_root>/scans/`.
    pub fn open(cas_root: &Path) -> Self {
        ScanCache {
            dir: cas_root.join("scans"),
        }
    }

    fn path(&self, blob_hex: &str) -> PathBuf {
        // Version components first so a bump is an entirely different subtree.
        self.dir
            .join(sanitize(SCANNER_VERSION))
            .join(sanitize(POLICY_VERSION))
            .join(format!("{}.json", sanitize(blob_hex)))
    }

    /// Look up the cached heuristic findings for a layer blob, re-attaching
    /// `layer`. Fails closed to `None` (miss) on any IO/parse/version/hash
    /// mismatch.
    pub fn get(&self, blob_hex: &str, layer: &'static str) -> Option<CachedScan> {
        let bytes = std::fs::read(self.path(blob_hex)).ok()?;
        let rec: CachedLayerScan = serde_json::from_slice(&bytes).ok()?;
        if rec.blob_id != blob_hex
            || rec.scanner_version != SCANNER_VERSION
            || rec.policy_version != POLICY_VERSION
        {
            return None;
        }
        let findings = rec
            .findings
            .into_iter()
            .map(|f| SecretFinding {
                layer,
                offset: f.offset,
                len: f.len,
                kind: f.kind,
                detail: f.detail,
            })
            .collect();
        Some(CachedScan {
            findings,
            capped: rec.capped,
        })
    }

    /// Store the heuristic findings for a layer blob (best-effort; atomic write).
    pub fn put(&self, blob_hex: &str, capped: bool, findings: &[SecretFinding]) {
        let rec = CachedLayerScan {
            blob_id: blob_hex.to_string(),
            scanner_version: SCANNER_VERSION.to_string(),
            policy_version: POLICY_VERSION.to_string(),
            capped,
            findings: findings
                .iter()
                .map(|f| CachedFinding {
                    offset: f.offset,
                    len: f.len,
                    kind: f.kind,
                    detail: f.detail.clone(),
                })
                .collect(),
        };
        let path = self.path(blob_hex);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(json) = serde_json::to_vec(&rec) else {
            return;
        };
        let tmp = path.with_extension(capsulefs::unique_tmp_suffix());
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn sanitize(v: &str) -> String {
    v.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(layer: &'static str) -> SecretFinding {
        SecretFinding {
            layer,
            offset: 10,
            len: 5,
            kind: FindingKind::HighEntropyToken,
            detail: "e=4.2".into(),
        }
    }

    #[test]
    fn put_then_get_roundtrips_and_reattaches_layer() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ScanCache::open(dir.path());
        cache.put("abc123", false, &[finding("rootfs")]);
        let hit = cache.get("abc123", "rootfs").expect("hit");
        assert!(!hit.capped);
        assert_eq!(hit.findings.len(), 1);
        assert_eq!(hit.findings[0].layer, "rootfs");
        assert_eq!(hit.findings[0].offset, 10);
    }

    #[test]
    fn missing_entry_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ScanCache::open(dir.path());
        assert!(cache.get("nope", "rootfs").is_none());
    }

    #[test]
    fn corrupt_entry_fails_closed_to_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ScanCache::open(dir.path());
        let p = cache.path("deadbeef");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"{not valid json").unwrap();
        assert!(
            cache.get("deadbeef", "rootfs").is_none(),
            "corrupt entry must be a miss"
        );
    }

    #[test]
    fn blob_id_mismatch_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ScanCache::open(dir.path());
        // Write a record whose internal blob_id disagrees with the path key.
        let rec = CachedLayerScan {
            blob_id: "other".into(),
            scanner_version: SCANNER_VERSION.into(),
            policy_version: POLICY_VERSION.into(),
            capped: false,
            findings: vec![],
        };
        let p = cache.path("abc");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_vec(&rec).unwrap()).unwrap();
        assert!(
            cache.get("abc", "rootfs").is_none(),
            "blob_id mismatch must be a miss"
        );
    }

    #[test]
    fn capped_flag_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ScanCache::open(dir.path());
        cache.put("xyz", true, &[]);
        assert!(cache.get("xyz", "memory").unwrap().capped);
    }
}

//! Inspect and prune the local A1 dependency cache.
//!
//! `ato cache stats` and `ato cache clear` both go through this module so
//! the rules for "what counts as a blob", "what counts as a reference", and
//! "how do we recognize a referenced blob" live in exactly one place.
//!
//! ## Scope notes (A1)
//!
//! - GC is **not** implemented in A1. `ato cache clear` is a user-driven
//!   reset (default: clear everything), not an automatic policy.
//! - References are read from `store/refs/deps/<ecosystem>/*.json`. Future
//!   strong roots (`installed/`, `pins/`, active runs) plug in via the
//!   `referenced_blobs()` helper.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use capsule_core::common::paths::{ato_store_blobs_dir, ato_store_dir, ato_store_refs_dir};
use capsule_core::common::store::BlobAddress;
use serde::{Deserialize, Serialize};

use crate::application::dependency_materializer::StoreRefRecord;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobSummary {
    pub blob_hash: String,
    pub bytes: u64,
    pub created_at_unix: Option<u64>,
    pub referenced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub store_root: PathBuf,
    pub blob_count: usize,
    pub total_bytes: u64,
    pub ref_count: usize,
    pub unreferenced_blob_count: usize,
    pub unreferenced_blob_bytes: u64,
    pub oldest_blob_age_seconds: Option<u64>,
    pub largest_blob_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheClearOutcome {
    pub blobs_removed: usize,
    pub refs_removed: usize,
    pub meta_files_removed: usize,
    pub bytes_freed: u64,
    pub skipped_referenced: Vec<String>,
}

/// Walks the on-disk store and aggregates a [`CacheStats`].
pub fn collect_cache_stats() -> Result<CacheStats> {
    let store_root = ato_store_dir();
    let blobs = enumerate_blobs()?;
    let refs = enumerate_refs()?;

    let referenced: BTreeSet<String> = refs.iter().filter_map(|r| r.blob_hash.clone()).collect();

    let mut total_bytes = 0u64;
    let mut largest_blob_bytes: Option<u64> = None;
    let mut oldest_unix: Option<u64> = None;
    let mut unreferenced_blob_count = 0usize;
    let mut unreferenced_blob_bytes = 0u64;

    for blob in &blobs {
        total_bytes = total_bytes.saturating_add(blob.bytes);
        largest_blob_bytes = Some(
            largest_blob_bytes
                .map(|prev| prev.max(blob.bytes))
                .unwrap_or(blob.bytes),
        );
        if let Some(unix) = blob.created_at_unix {
            oldest_unix = Some(oldest_unix.map(|prev| prev.min(unix)).unwrap_or(unix));
        }
        if !referenced.contains(&blob.blob_hash) {
            unreferenced_blob_count += 1;
            unreferenced_blob_bytes = unreferenced_blob_bytes.saturating_add(blob.bytes);
        }
    }

    let oldest_age_seconds = oldest_unix.and_then(|unix| {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|now| now.as_secs().checked_sub(unix))
    });

    Ok(CacheStats {
        store_root,
        blob_count: blobs.len(),
        total_bytes,
        ref_count: refs.len(),
        unreferenced_blob_count,
        unreferenced_blob_bytes,
        oldest_blob_age_seconds: oldest_age_seconds,
        largest_blob_bytes,
    })
}

/// Removes every blob, ref, meta record, and lock from the local store.
///
/// Use `clear_derivation` for surgical removal. Refs/blobs that the future
/// strong-root system flags as in-use will be reported in
/// `CacheClearOutcome::skipped_referenced`.
pub fn clear_all() -> Result<CacheClearOutcome> {
    let blobs = enumerate_blobs()?;
    let refs = enumerate_refs()?;
    let referenced = referenced_blobs();

    let mut outcome = CacheClearOutcome {
        blobs_removed: 0,
        refs_removed: 0,
        meta_files_removed: 0,
        bytes_freed: 0,
        skipped_referenced: Vec::new(),
    };

    for blob in blobs {
        if referenced.contains(&blob.blob_hash) {
            outcome.skipped_referenced.push(blob.blob_hash);
            continue;
        }
        let address = match BlobAddress::parse(&blob.blob_hash) {
            Ok(addr) => addr,
            Err(_) => continue,
        };
        let dir = address.dir();
        if dir.is_dir() {
            fs::remove_dir_all(&dir)
                .with_context(|| format!("failed to remove {}", dir.display()))?;
            outcome.blobs_removed += 1;
            outcome.bytes_freed = outcome.bytes_freed.saturating_add(blob.bytes);
        }
        let meta_path = address.meta_path();
        if meta_path.is_file() {
            fs::remove_file(&meta_path)
                .with_context(|| format!("failed to remove {}", meta_path.display()))?;
            outcome.meta_files_removed += 1;
        }
    }

    for entry in refs {
        if let Some(hash) = entry.blob_hash.as_deref() {
            if referenced.contains(hash) {
                continue;
            }
        }
        if entry.path.is_file() {
            fs::remove_file(&entry.path)
                .with_context(|| format!("failed to remove {}", entry.path.display()))?;
            outcome.refs_removed += 1;
        }
    }

    Ok(outcome)
}

/// Removes the specific derivation's ref and the backing blob (if no other
/// ref still points at it).
pub fn clear_derivation(derivation_hash: &str) -> Result<CacheClearOutcome> {
    let mut outcome = CacheClearOutcome {
        blobs_removed: 0,
        refs_removed: 0,
        meta_files_removed: 0,
        bytes_freed: 0,
        skipped_referenced: Vec::new(),
    };

    let refs = enumerate_refs()?;
    let target_blob_hashes: BTreeSet<String> = refs
        .iter()
        .filter(|r| r.derivation_hash == derivation_hash)
        .filter_map(|r| r.blob_hash.clone())
        .collect();

    let still_referenced: BTreeSet<String> = refs
        .iter()
        .filter(|r| r.derivation_hash != derivation_hash)
        .filter_map(|r| r.blob_hash.clone())
        .collect();

    // Remove the matching ref(s).
    for entry in refs.iter().filter(|r| r.derivation_hash == derivation_hash) {
        if entry.path.is_file() {
            fs::remove_file(&entry.path)
                .with_context(|| format!("failed to remove {}", entry.path.display()))?;
            outcome.refs_removed += 1;
        }
    }

    // Remove the blob(s) only when no other derivation still points at them.
    let strong = referenced_blobs();
    for hash in target_blob_hashes {
        if still_referenced.contains(&hash) {
            outcome.skipped_referenced.push(hash);
            continue;
        }
        if strong.contains(&hash) {
            outcome.skipped_referenced.push(hash);
            continue;
        }
        let address = match BlobAddress::parse(&hash) {
            Ok(addr) => addr,
            Err(_) => continue,
        };
        let dir = address.dir();
        if dir.is_dir() {
            let bytes = directory_size(&dir).unwrap_or(0);
            fs::remove_dir_all(&dir)
                .with_context(|| format!("failed to remove {}", dir.display()))?;
            outcome.blobs_removed += 1;
            outcome.bytes_freed = outcome.bytes_freed.saturating_add(bytes);
        }
        let meta_path = address.meta_path();
        if meta_path.is_file() {
            fs::remove_file(&meta_path)
                .with_context(|| format!("failed to remove {}", meta_path.display()))?;
            outcome.meta_files_removed += 1;
        }
    }

    Ok(outcome)
}

/// Returns the set of blob hashes considered "strong roots" — references
/// that `clear_all` and `clear_derivation` must never remove.
///
/// A1 has no `installed/` or `pins/` indices yet; this helper exists so
/// future commits can wire them in without touching the clear/stats paths.
pub fn referenced_blobs() -> BTreeSet<String> {
    BTreeSet::new()
}

#[derive(Debug, Clone)]
struct RefEntry {
    path: PathBuf,
    derivation_hash: String,
    blob_hash: Option<String>,
}

fn enumerate_refs() -> Result<Vec<RefEntry>> {
    let deps_root = ato_store_refs_dir().join("deps");
    if !deps_root.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in walkdir::WalkDir::new(&deps_root).min_depth(1) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let bytes = match fs::read(entry.path()) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let record: StoreRefRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(_) => continue,
        };
        entries.push(RefEntry {
            path: entry.path().to_path_buf(),
            derivation_hash: record.derivation_hash,
            blob_hash: record.blob_hash,
        });
    }
    Ok(entries)
}

fn enumerate_blobs() -> Result<Vec<BlobSummary>> {
    let blobs_root = ato_store_blobs_dir();
    if !blobs_root.is_dir() {
        return Ok(Vec::new());
    }

    let mut summaries = Vec::new();
    for entry in walkdir::WalkDir::new(&blobs_root).min_depth(3).max_depth(3) {
        let entry = entry?;
        if !entry.file_type().is_dir() {
            continue;
        }
        let path = entry.path();
        let payload = path.join("payload");
        if !payload.is_dir() {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        let blob_hash = match read_manifest_blob_hash(&manifest_path) {
            Some(hash) => hash,
            None => continue,
        };
        let bytes = directory_size(&payload).unwrap_or(0);
        let created_at_unix = parse_manifest_created_at(&manifest_path);
        summaries.push(BlobSummary {
            blob_hash,
            bytes,
            created_at_unix,
            referenced: false,
        });
    }
    Ok(summaries)
}

fn read_manifest_blob_hash(manifest_path: &Path) -> Option<String> {
    let bytes = fs::read(manifest_path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get("blob_hash")?.as_str().map(str::to_string)
}

fn parse_manifest_created_at(manifest_path: &Path) -> Option<u64> {
    let bytes = fs::read(manifest_path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let raw = value.get("created_at")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.timestamp().max(0) as u64)
}

fn directory_size(path: &Path) -> Option<u64> {
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(path) {
        let entry = entry.ok()?;
        if entry.file_type().is_file() {
            if let Ok(metadata) = entry.path().symlink_metadata() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Some(total)
}

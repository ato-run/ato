//! Atomic freeze of an installed dependency tree into the immutable store.
//!
//! The freeze path is the second half of the A1 cache miss flow:
//!
//! 1. `plan()` reports a cache miss (or cache disabled).
//! 2. The install actually runs and writes its output into
//!    `runs/<session>/deps/` (existing A0 behavior).
//! 3. `freeze_dep_tree(...)` is invoked. It hashes the tree, copies it into
//!    a sibling staging directory under `store/blobs/`, writes the
//!    manifest, atomically renames the staging dir into place, and finally
//!    drops the weak ref + meta records that index the new blob.
//!
//! All of these writes are guarded by an advisory `flock(2)` on a
//! per-derivation lock file so a second concurrent install for the same
//! derivation either blocks on the lock and observes the resulting cache
//! hit, or — if it raced past the recheck — performs a no-op overwrite that
//! the rename collision logic recovers from.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::blob::{hash_tree, BlobManifest, TreeHash};
use capsule_core::common::paths::ato_store_meta_dir;
use capsule_core::common::store::{
    ato_store_dep_ref_path, ato_store_derivation_lock_path, ato_store_locks_dir, BlobAddress,
};
use fs2::FileExt;
use serde::Serialize;

use super::StoreRefRecord;

/// Outcome of a successful freeze.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreezeOutcome {
    /// Hash of the frozen tree. Same identifier the cache index now points at.
    pub blob_hash: String,
    /// Final on-disk location of the immutable payload directory.
    pub blob_dir: PathBuf,
    /// Whether this freeze actually moved bytes (`true`) or merely observed
    /// that another process had already produced an identical blob (`false`).
    pub did_freeze: bool,
}

/// RAII guard that holds an exclusive flock on the per-derivation lock file.
///
/// Drop the guard to release the lock; the underlying file is left in place
/// (touching it is much cheaper than racing a `mkdir` + `rename`).
pub struct DerivationLock {
    _file: File,
    path: PathBuf,
}

impl DerivationLock {
    /// Acquires an exclusive flock on the lock file for `derivation_hash`.
    pub fn acquire(derivation_hash: &str) -> Result<Self> {
        let path = ato_store_derivation_lock_path(derivation_hash);
        fs::create_dir_all(ato_store_locks_dir())
            .with_context(|| format!("failed to create {}", ato_store_locks_dir().display()))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("failed to open derivation lock {}", path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("failed to acquire flock on {}", path.display()))?;
        Ok(Self { _file: file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Freezes `deps_path` into the store under the address derived from its
/// content hash and writes the weak ref / meta records that index the blob.
///
/// `deps_path` must be the dependency directory the install just wrote
/// (typically `runs/<session>/deps/`). The freeze is idempotent: if the
/// blob already exists with a matching manifest, this function only refreshes
/// the ref and meta records, charging zero extra disk for the payload bytes.
pub fn freeze_dep_tree(
    deps_path: &Path,
    derivation_hash: &str,
    ecosystem: &str,
) -> Result<FreezeOutcome> {
    if !deps_path.is_dir() {
        anyhow::bail!("cannot freeze {}: not a directory", deps_path.display());
    }

    let started = std::time::Instant::now();

    // Acquire the per-derivation lock so concurrent freezes for the same
    // derivation hash serialize through `flock(2)`.
    let _lock = DerivationLock::acquire(derivation_hash)?;

    let tree =
        hash_tree(deps_path).with_context(|| format!("failed to hash {}", deps_path.display()))?;
    let blob_hash = tree.blob_hash.clone();
    let address = BlobAddress::parse(&blob_hash)
        .with_context(|| format!("blob hash {blob_hash} could not be parsed"))?;

    // Recheck after taking the lock: another process may have just won.
    let already_frozen = address.payload_dir().is_dir()
        && BlobManifest::read_from(&address.manifest_path())
            .ok()
            .map(|m| m.matches_blob_hash(&blob_hash))
            .unwrap_or(false);

    let did_freeze = if already_frozen {
        false
    } else {
        write_blob_atomically(&address, &tree, deps_path, derivation_hash)?;
        true
    };

    write_ref_atomically(ecosystem, derivation_hash, &blob_hash)?;
    write_blob_meta_atomically(&address, derivation_hash, did_freeze)?;

    let freeze_duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        derivation_hash = derivation_hash,
        ecosystem = ecosystem,
        blob_hash = %blob_hash,
        cache_result = if did_freeze { "miss" } else { "observe" },
        did_freeze,
        freeze_duration_ms,
        file_count = tree.file_count,
        symlink_count = tree.symlink_count,
        total_bytes = tree.total_bytes,
        "A1 freeze complete"
    );

    Ok(FreezeOutcome {
        blob_hash,
        blob_dir: address.dir(),
        did_freeze,
    })
}

/// Stages the payload + manifest in a sibling tmp directory and renames it
/// into place atomically.
fn write_blob_atomically(
    address: &BlobAddress,
    tree: &TreeHash,
    deps_path: &Path,
    derivation_hash: &str,
) -> Result<()> {
    let suffix = format!("{:016x}", rand::random::<u64>());
    let staging = address.staging_dir(&suffix);

    if staging.exists() {
        fs::remove_dir_all(&staging)
            .with_context(|| format!("failed to clean staging directory {}", staging.display()))?;
    }
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let staging_payload = staging.join("payload");
    fs::create_dir_all(&staging_payload).with_context(|| {
        format!(
            "failed to create staging payload {}",
            staging_payload.display()
        )
    })?;
    copy_tree_into(&staging_payload, deps_path)?;

    let manifest = BlobManifest::from_tree_hash(tree, derivation_hash, now_rfc3339());
    manifest
        .write_to(&staging.join("manifest.json"))
        .context("failed to write blob manifest into staging")?;

    if let Some(parent) = address.dir().parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    match fs::rename(&staging, address.dir()) {
        Ok(()) => Ok(()),
        Err(err) if address.dir().is_dir() => {
            // Rename failed because the target now exists. Treat it as
            // success and clean up the staging tree; another process won
            // the race and has already populated the blob.
            let _ = fs::remove_dir_all(&staging);
            tracing::debug!(
                blob_dir = %address.dir().display(),
                "freeze rename observed an existing blob: {err}"
            );
            Ok(())
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "failed to rename {} to {}",
                staging.display(),
                address.dir().display()
            )
        }),
    }
}

/// Writes `value` to `path` atomically by writing to a sibling tmp file and
/// renaming it into place. The rename guarantees that observers either see
/// the previous version or the new one, never a partially written file.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("path {} has no parent", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let suffix = format!("{:016x}", rand::random::<u64>());
    let file_name = path
        .file_name()
        .with_context(|| format!("path {} has no file name", path.display()))?
        .to_string_lossy()
        .into_owned();
    let tmp = parent.join(format!("{file_name}.tmp-{suffix}"));
    let bytes = serde_json::to_vec_pretty(value).context("failed to serialize json")?;
    fs::write(&tmp, [bytes, b"\n".to_vec()].concat())
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename {} to {}", tmp.display(), path.display()))
}

fn write_ref_atomically(ecosystem: &str, derivation_hash: &str, blob_hash: &str) -> Result<()> {
    let path = ato_store_dep_ref_path(ecosystem, derivation_hash);
    let record = StoreRefRecord {
        schema_version: "1".to_string(),
        ecosystem: ecosystem.to_string(),
        derivation_hash: derivation_hash.to_string(),
        blob_hash: Some(blob_hash.to_string()),
        cache_status: "frozen".to_string(),
        created_at: now_rfc3339(),
    };
    atomic_write_json(&path, &record)
}

fn write_blob_meta_atomically(
    address: &BlobAddress,
    derivation_hash: &str,
    did_freeze: bool,
) -> Result<()> {
    let meta_path = address.meta_path();
    if let Some(parent) = meta_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let previous = fs::read(&meta_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<BlobMeta>(&bytes).ok());
    let now = now_rfc3339();
    let meta = BlobMeta {
        schema_version: 1,
        blob_hash: address.as_str(),
        derivation_hashes: merge_derivation_hashes(previous.as_ref(), derivation_hash),
        first_seen_at: previous
            .as_ref()
            .map(|m| m.first_seen_at.clone())
            .unwrap_or_else(|| now.clone()),
        last_seen_at: now,
        last_event: if did_freeze { "freeze" } else { "observe" }.to_string(),
    };
    let meta_blobs_root = ato_store_meta_dir().join("blobs");
    fs::create_dir_all(&meta_blobs_root)
        .with_context(|| format!("failed to create {}", meta_blobs_root.display()))?;
    atomic_write_json(&meta_path, &meta)
}

/// Mutable observation record per blob. Lives under `~/.ato/store/meta/`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BlobMeta {
    schema_version: u32,
    blob_hash: String,
    derivation_hashes: Vec<String>,
    first_seen_at: String,
    last_seen_at: String,
    last_event: String,
}

fn merge_derivation_hashes(previous: Option<&BlobMeta>, current: &str) -> Vec<String> {
    let mut hashes = previous
        .map(|m| m.derivation_hashes.clone())
        .unwrap_or_default();
    if !hashes.iter().any(|h| h == current) {
        hashes.push(current.to_string());
    }
    hashes.sort();
    hashes.dedup();
    hashes
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Recursively copies the contents of `src` into `dst`. Files are copied byte
/// for byte; symlinks have their target preserved.
fn copy_tree_into(dst: &Path, src: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in walkdir::WalkDir::new(src).min_depth(1) {
        let entry = entry.with_context(|| format!("failed to walk {}", src.display()))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .expect("walkdir entry inside src");
        let target = dst.join(rel);
        let file_type = entry.file_type();
        if file_type.is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("failed to create {}", target.display()))?;
        } else if file_type.is_symlink() {
            let link = fs::read_link(entry.path())
                .with_context(|| format!("failed to read symlink {}", entry.path().display()))?;
            #[cfg(unix)]
            {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                std::os::unix::fs::symlink(&link, &target).with_context(|| {
                    format!(
                        "failed to recreate symlink {} -> {}",
                        target.display(),
                        link.display()
                    )
                })?;
            }
            #[cfg(not(unix))]
            return Err(anyhow::anyhow!(
                "symlink projection is not supported on this platform"
            ));
        } else if file_type.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            copy_file(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn copy_file(src: &Path, dst: &Path) -> io::Result<()> {
    fs::copy(src, dst).map(|_| ())
}

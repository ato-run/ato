//! Ref-count / pin garbage collection over the chunk store.
//!
//! Implements the reclamation that `installed_state` left as scaffold (its docs
//! list "GC/ref-count maintenance" as out of scope). The reachable set is the
//! union of every chunk referenced by a live [`BlobManifest`] plus an explicit
//! pin set (hot capsules / in-flight overlays). Everything else is unreferenced
//! and reclaimable.
//!
//! Evictions are **reported, never silent** (plan §7): [`collect_garbage`]
//! returns the list of deleted chunks and the bytes reclaimed so the caller can
//! log them.

use std::collections::HashSet;

use crate::cas::CasStore;
use crate::hash::ContentHash;
use crate::manifest::BlobManifest;
use crate::Result;

/// What a GC pass kept and reclaimed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Number of chunks retained (reachable or pinned).
    pub kept: usize,
    /// Chunks deleted because nothing reachable referenced them.
    pub deleted: Vec<ContentHash>,
    /// Total bytes reclaimed by the deletions.
    pub reclaimed_bytes: u64,
}

impl GcReport {
    /// Number of chunks deleted.
    pub fn deleted_count(&self) -> usize {
        self.deleted.len()
    }
}

/// Delete every chunk in `store` not referenced by any manifest in
/// `live_manifests` and not present in `pinned`.
///
/// `pinned` lets a caller keep chunks alive that are not (yet) reachable through
/// a manifest — e.g. a capsule pinned hot, or chunks staged for an in-flight
/// build. The function is safe to run concurrently with reads of *live* blobs:
/// it only removes chunks outside the reachable ∪ pinned set.
pub fn collect_garbage(
    store: &CasStore,
    live_manifests: &[BlobManifest],
    pinned: &HashSet<ContentHash>,
) -> Result<GcReport> {
    // Reachable = referenced-by-a-live-manifest ∪ pinned.
    let mut reachable: HashSet<ContentHash> = pinned.clone();
    for manifest in live_manifests {
        for hash in manifest.referenced_chunks() {
            reachable.insert(hash.clone());
        }
    }

    let mut report = GcReport::default();
    for hash in store.list_chunks()? {
        if reachable.contains(&hash) {
            report.kept += 1;
        } else {
            report.reclaimed_bytes += store.remove_chunk(&hash)?;
            report.deleted.push(hash);
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{ChunkingKind, LayerKind};
    use crate::store_blob;

    #[test]
    fn unreferenced_chunks_are_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();

        let live = store_blob(
            &store,
            LayerKind::App,
            &vec![1u8; 300_000],
            ChunkingKind::ContentDefined,
        )
        .unwrap();
        // A second blob that we will NOT keep alive.
        let _dead = store_blob(
            &store,
            LayerKind::App,
            &vec![2u8; 300_000],
            ChunkingKind::ContentDefined,
        )
        .unwrap();

        let before = store.list_chunks().unwrap().len();
        let report = collect_garbage(&store, std::slice::from_ref(&live), &HashSet::new()).unwrap();

        assert!(report.deleted_count() > 0, "dead blob's chunks should be reclaimed");
        assert_eq!(report.kept, live.chunks.len());
        assert_eq!(
            store.list_chunks().unwrap().len(),
            live.chunks.len(),
            "only live chunks remain"
        );
        assert!(before > live.chunks.len());

        // The live blob is still fully readable after GC.
        let reader = crate::LazyBlobReader::new(&store, &live);
        assert_eq!(reader.read_all().unwrap(), vec![1u8; 300_000]);
    }

    #[test]
    fn pinned_chunks_survive_even_without_a_live_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let blob = store_blob(
            &store,
            LayerKind::Memory,
            &vec![7u8; 200_000],
            ChunkingKind::PageAligned { page_size: 64 * 1024 },
        )
        .unwrap();

        // Pin every chunk; pass no live manifests.
        let pinned: HashSet<ContentHash> =
            blob.referenced_chunks().cloned().collect();
        let report = collect_garbage(&store, &[], &pinned).unwrap();

        assert_eq!(report.deleted_count(), 0, "pinned chunks must not be deleted");
        assert_eq!(report.kept, pinned.len());
    }

    #[test]
    fn nothing_live_and_nothing_pinned_reclaims_all() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        store_blob(
            &store,
            LayerKind::App,
            b"some bytes",
            ChunkingKind::ContentDefined,
        )
        .unwrap();
        let report = collect_garbage(&store, &[], &HashSet::new()).unwrap();
        assert_eq!(report.kept, 0);
        assert!(store.list_chunks().unwrap().is_empty());
    }
}

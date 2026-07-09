//! Hotset profile — the ordered set of chunks a restore touches between
//! LoadSnapshot and the first healthy request (plan §7).
//!
//! Recorded during build (by observing which memory pages / rootfs blocks the
//! boot-to-readiness path touches), it lets a restore prefetch the hot chunks
//! sequentially from local SSD before/while resuming, minimizing first-request
//! latency. Stage 1 uses it to warm the page cache via [`LazyBlobReader`];
//! Stage 2's UFFD page server will use the same ordering.

use serde::{Deserialize, Serialize};

use crate::hash::ContentHash;
use crate::manifest::BlobManifest;

/// An ordered list of chunk hashes in first-touch order. Deduplicated: a chunk
/// touched twice appears once, at its first occurrence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotsetProfile {
    /// Chunks in the order they were first accessed.
    pub ordered_chunks: Vec<ContentHash>,
}

impl HotsetProfile {
    /// Number of chunks in the profile.
    pub fn len(&self) -> usize {
        self.ordered_chunks.len()
    }

    /// Whether the profile is empty.
    pub fn is_empty(&self) -> bool {
        self.ordered_chunks.is_empty()
    }
}

/// Builds a [`HotsetProfile`] by recording chunk accesses in order, ignoring
/// repeats so the profile is the *first-touch* sequence.
#[derive(Debug, Default)]
pub struct HotsetRecorder {
    ordered: Vec<ContentHash>,
    seen: std::collections::HashSet<ContentHash>,
}

impl HotsetRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a chunk access. The first time a chunk is seen it is appended;
    /// later repeats are ignored.
    pub fn record(&mut self, hash: &ContentHash) {
        if self.seen.insert(hash.clone()) {
            self.ordered.push(hash.clone());
        }
    }

    /// Record every chunk of `manifest` that overlaps the byte range
    /// `[offset, offset+len)`, in manifest (offset) order — the build-time
    /// "these pages were touched" hook. Overflow-safe (a chunk whose
    /// `offset+length` would overflow is skipped). Returns how many chunks were
    /// newly added.
    pub fn record_range(&mut self, manifest: &BlobManifest, offset: u64, len: u64) -> usize {
        let end = offset.saturating_add(len);
        let mut added = 0;
        for chunk in &manifest.chunks {
            let chunk_end = match chunk.offset.checked_add(chunk.length) {
                Some(e) => e,
                None => continue, // malformed chunk: skip rather than panic
            };
            if chunk_end <= offset || chunk.offset >= end {
                continue; // no overlap
            }
            if self.seen.insert(chunk.hash.clone()) {
                self.ordered.push(chunk.hash.clone());
                added += 1;
            }
        }
        added
    }

    /// Record every chunk of `manifest` in order (a whole layer is hot).
    /// Returns how many chunks were newly added.
    pub fn extend_from_manifest(&mut self, manifest: &BlobManifest) -> usize {
        let mut added = 0;
        for chunk in &manifest.chunks {
            if self.seen.insert(chunk.hash.clone()) {
                self.ordered.push(chunk.hash.clone());
                added += 1;
            }
        }
        added
    }

    /// Finish recording and produce the profile.
    pub fn finish(self) -> HotsetProfile {
        HotsetProfile {
            ordered_chunks: self.ordered,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_bytes;

    #[test]
    fn recorder_keeps_first_touch_order_and_dedups() {
        let a = hash_bytes(b"a");
        let b = hash_bytes(b"b");
        let c = hash_bytes(b"c");
        let mut r = HotsetRecorder::new();
        r.record(&b);
        r.record(&a);
        r.record(&b); // repeat ignored
        r.record(&c);
        r.record(&a); // repeat ignored
        let profile = r.finish();
        assert_eq!(profile.ordered_chunks, vec![b, a, c]);
        assert_eq!(profile.len(), 3);
    }

    #[test]
    fn empty_profile() {
        let p = HotsetRecorder::new().finish();
        assert!(p.is_empty());
    }

    use crate::chunk::Chunk;
    use crate::manifest::{BlobManifest, ChunkingKind, LayerKind};

    fn page_blob() -> BlobManifest {
        // Two 4-byte "pages" at offsets 0 and 4.
        BlobManifest::new(
            LayerKind::Memory,
            8,
            ChunkingKind::PageAligned { page_size: 4 },
            vec![
                Chunk { hash: hash_bytes(b"page0"), offset: 0, length: 4 },
                Chunk { hash: hash_bytes(b"page1"), offset: 4, length: 4 },
            ],
        )
    }

    #[test]
    fn record_range_maps_touched_bytes_to_overlapping_chunks_in_order() {
        let blob = page_blob();
        let mut r = HotsetRecorder::new();
        // A range 2..6 spans both pages.
        let added = r.record_range(&blob, 2, 4);
        assert_eq!(added, 2);
        assert_eq!(
            r.finish().ordered_chunks,
            vec![hash_bytes(b"page0"), hash_bytes(b"page1")]
        );
    }

    #[test]
    fn record_range_is_overflow_safe_and_skips_non_overlapping() {
        let blob = BlobManifest::new(
            LayerKind::Memory,
            u64::MAX,
            ChunkingKind::ContentDefined,
            vec![
                // offset+length overflows -> skipped, no panic.
                Chunk { hash: hash_bytes(b"ovf"), offset: u64::MAX, length: 10 },
                // far away from the queried range -> skipped.
                Chunk { hash: hash_bytes(b"far"), offset: 1_000_000, length: 4 },
            ],
        );
        let mut r = HotsetRecorder::new();
        assert_eq!(r.record_range(&blob, 0, 10), 0);
        assert!(r.finish().is_empty());
    }

    #[test]
    fn extend_from_manifest_records_all_chunks_in_order() {
        let blob = page_blob();
        let mut r = HotsetRecorder::new();
        assert_eq!(r.extend_from_manifest(&blob), 2);
        // Calling again adds nothing (dedup) and preserves first-touch order.
        assert_eq!(r.extend_from_manifest(&blob), 0);
        assert_eq!(
            r.finish().ordered_chunks,
            vec![hash_bytes(b"page0"), hash_bytes(b"page1")]
        );
    }
}

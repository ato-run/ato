//! File-backed lazy reader.
//!
//! Given a [`BlobManifest`] and the [`CasStore`] holding its chunks, reassemble
//! the whole blob, a byte range, or warm a hotset — reading only the chunks a
//! request actually needs. Stage 1 reads chunks from local SSD on demand; Stage
//! 2 swaps in a UFFD page server behind the same surface.

use crate::cas::CasStore;
use crate::hash::ContentHash;
use crate::hotset::HotsetProfile;
use crate::manifest::BlobManifest;
use crate::{CapsuleFsError, Result};

/// A lazy view over one stored blob.
pub struct LazyBlobReader<'a> {
    store: &'a CasStore,
    manifest: &'a BlobManifest,
}

impl<'a> LazyBlobReader<'a> {
    pub fn new(store: &'a CasStore, manifest: &'a BlobManifest) -> Self {
        Self { store, manifest }
    }

    /// Reassemble and return the entire blob. Verifies each chunk on read.
    pub fn read_all(&self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(self.manifest.total_len as usize);
        for chunk in &self.manifest.chunks {
            out.extend_from_slice(&self.store.get_chunk(&chunk.hash)?);
        }
        Ok(out)
    }

    /// Read `len` bytes starting at `offset`, touching only the chunks that
    /// overlap the range (lazy). Errors if the range is outside the blob.
    pub fn read_range(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let end = offset.saturating_add(len);
        if end > self.manifest.total_len {
            return Err(CapsuleFsError::RangeOutOfBounds {
                offset,
                end,
                len: self.manifest.total_len,
            });
        }
        let mut out = Vec::with_capacity(len as usize);
        for chunk in &self.manifest.chunks {
            let chunk_start = chunk.offset;
            let chunk_end = chunk.offset + chunk.length;
            // Skip chunks entirely before or after the requested range.
            if chunk_end <= offset || chunk_start >= end {
                continue;
            }
            let bytes = self.store.get_chunk(&chunk.hash)?;
            let from = offset.saturating_sub(chunk_start);
            let to = (end - chunk_start).min(chunk.length);
            out.extend_from_slice(&bytes[from as usize..to as usize]);
        }
        Ok(out)
    }

    /// Warm the page cache by reading the hotset chunks in their recorded order.
    /// Only chunks this blob actually references are touched; an entry naming a
    /// chunk outside this blob is skipped (a hotset may span several layers).
    /// Returns the number of chunks prefetched.
    pub fn prefetch_hotset(&self, hotset: &HotsetProfile) -> Result<usize> {
        let referenced: std::collections::HashSet<&ContentHash> =
            self.manifest.referenced_chunks().collect();
        let mut warmed = 0;
        for hash in &hotset.ordered_chunks {
            if referenced.contains(hash) {
                // Read-and-verify; the bytes land in the OS page cache.
                let _ = self.store.get_chunk(hash)?;
                warmed += 1;
            }
        }
        Ok(warmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{MEMORY_PAGE_CHUNK_SIZE, chunk_page_aligned};
    use crate::hotset::HotsetRecorder;
    use crate::manifest::{ChunkingKind, LayerKind};
    use crate::store_blob;

    fn store_with_blob(payload: &[u8]) -> (tempfile::TempDir, CasStore, BlobManifest) {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let manifest = store_blob(
            &store,
            LayerKind::Memory,
            payload,
            ChunkingKind::PageAligned {
                page_size: 64 * 1024,
            },
        )
        .unwrap();
        (dir, store, manifest)
    }

    #[test]
    fn read_all_reassembles_original() {
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 256) as u8).collect();
        let (_dir, store, manifest) = store_with_blob(&payload);
        let reader = LazyBlobReader::new(&store, &manifest);
        assert_eq!(reader.read_all().unwrap(), payload);
        assert_eq!(manifest.total_len, payload.len() as u64);
    }

    #[test]
    fn read_range_returns_subslice() {
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 256) as u8).collect();
        let (_dir, store, manifest) = store_with_blob(&payload);
        let reader = LazyBlobReader::new(&store, &manifest);
        // A range spanning a chunk boundary (page = 64 KiB).
        let off = 60_000u64;
        let len = 10_000u64;
        let got = reader.read_range(off, len).unwrap();
        assert_eq!(got, &payload[off as usize..(off + len) as usize]);
    }

    #[test]
    fn read_range_out_of_bounds_errors() {
        let (_dir, store, manifest) = store_with_blob(b"short");
        let reader = LazyBlobReader::new(&store, &manifest);
        assert!(matches!(
            reader.read_range(3, 10),
            Err(CapsuleFsError::RangeOutOfBounds { .. })
        ));
    }

    #[test]
    fn prefetch_hotset_warms_only_referenced_chunks() {
        // Distinct content per page so the three 64 KiB chunks have distinct
        // hashes (identical pages would dedup to one chunk). Mix in the page
        // index so each 64 KiB page differs.
        let page = 64 * 1024u32;
        let payload: Vec<u8> = (0..3 * page)
            .map(|i| (((i / page) * 97 + i) % 256) as u8)
            .collect();
        let (_dir, store, manifest) = store_with_blob(&payload);
        let distinct: std::collections::HashSet<_> =
            manifest.referenced_chunks().collect();
        assert_eq!(distinct.len(), 3, "pages must be distinct for this test");

        // Build a hotset from this blob's chunks plus one foreign chunk.
        let mut rec = HotsetRecorder::new();
        for c in &manifest.chunks {
            rec.record(&c.hash);
        }
        rec.record(&crate::hash::hash_bytes(b"foreign-not-in-this-blob"));
        let hotset = rec.finish();

        let reader = LazyBlobReader::new(&store, &manifest);
        let warmed = reader.prefetch_hotset(&hotset).unwrap();
        assert_eq!(warmed, 3, "foreign chunk must be skipped");
    }

    #[test]
    fn page_chunk_size_constant_is_used_for_memory_layers() {
        // Sanity that the memory page constant chunks as expected.
        let payload = vec![1u8; MEMORY_PAGE_CHUNK_SIZE + 10];
        let chunks = chunk_page_aligned(&payload, MEMORY_PAGE_CHUNK_SIZE);
        assert_eq!(chunks.len(), 2);
    }
}

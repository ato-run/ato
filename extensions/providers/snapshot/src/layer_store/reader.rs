//! File-backed lazy reader.
//!
//! Given a [`BlobManifest`] and the [`CasStore`] holding its chunks, reassemble
//! the whole blob, a byte range, or warm a hotset — reading only the chunks a
//! request actually needs. Stage 1 reads chunks from local SSD on demand; Stage
//! 2 swaps in a UFFD page server behind the same surface.

use super::cas::CasStore;
use super::hash::ContentHash;
use super::hotset::HotsetProfile;
use super::manifest::{BlobManifest, validate_blob_manifest};
use super::{LayerStoreError, Result};

/// Which memory backend realizes a blob's bytes (plan §7).
///
/// Stage 1 implements [`File`](MemBackend::File): chunks are read from local SSD
/// on demand. [`Uffd`](MemBackend::Uffd) is the reserved Stage-2 seam — a UFFD
/// page server serving chunks on demand — and is `unimplemented!` for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemBackend {
    /// File-backed demand read from local SSD (Stage 1, implemented).
    #[default]
    File,
    /// UFFD page server (Stage 2, reserved — not implemented).
    Uffd,
}

/// A lazy view over one stored blob.
pub struct LazyBlobReader<'a> {
    store: &'a CasStore,
    manifest: &'a BlobManifest,
    backend: MemBackend,
}

impl<'a> LazyBlobReader<'a> {
    pub fn new(store: &'a CasStore, manifest: &'a BlobManifest) -> Self {
        Self {
            store,
            manifest,
            backend: MemBackend::File,
        }
    }

    /// Select the memory backend (builder style). Default is
    /// [`MemBackend::File`].
    pub fn with_backend(mut self, backend: MemBackend) -> Self {
        self.backend = backend;
        self
    }

    /// The selected memory backend.
    pub fn backend(&self) -> MemBackend {
        self.backend
    }

    /// Realize the whole blob through the selected backend. `File` reads it
    /// (== [`read_all`](Self::read_all)); `Uffd` is the reserved Stage-2 seam and
    /// fails closed with [`LayerStoreError::Unsupported`] (never panics) so a
    /// mis-selected backend errors cleanly.
    pub fn realize(&self) -> Result<Vec<u8>> {
        match self.backend {
            MemBackend::File => self.read_all(),
            MemBackend::Uffd => Err(LayerStoreError::Unsupported(
                "UFFD demand-paging memory backend is a Stage 2 seam; not implemented in Stage 1"
                    .to_string(),
            )),
        }
    }

    /// Reassemble and return the entire blob. Verifies each chunk on read.
    ///
    /// Validates the manifest's self-description before trusting it: the chunk
    /// lengths must sum (without overflow) to `total_len`, and each chunk's
    /// declared `length` must match the bytes it actually addresses. A manifest
    /// that lies about either is rejected with [`LayerStoreError::MalformedManifest`]
    /// rather than silently returning truncated/oversized data.
    pub fn read_all(&self) -> Result<Vec<u8>> {
        validate_blob_manifest(self.manifest)?;

        let mut out = Vec::with_capacity(self.manifest.total_len as usize);
        for chunk in &self.manifest.chunks {
            let bytes = self.store.get_chunk(&chunk.hash)?;
            if bytes.len() as u64 != chunk.length {
                return Err(LayerStoreError::MalformedManifest(format!(
                    "chunk {} declares length {} but addresses {} bytes",
                    chunk.hash,
                    chunk.length,
                    bytes.len()
                )));
            }
            out.extend_from_slice(&bytes);
        }
        Ok(out)
    }

    /// Read `len` bytes starting at `offset`, touching only the chunks that
    /// overlap the range (lazy). Errors if the range is outside the blob, or if
    /// the manifest is malformed (an `offset + length` overflow, or a chunk
    /// whose declared `length` does not match its actual bytes — which would
    /// otherwise slice out of bounds and panic).
    pub fn read_range(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let end = offset.saturating_add(len);
        if end > self.manifest.total_len {
            return Err(LayerStoreError::RangeOutOfBounds {
                offset,
                end,
                len: self.manifest.total_len,
            });
        }
        let mut out = Vec::with_capacity(len as usize);
        for chunk in &self.manifest.chunks {
            let chunk_start = chunk.offset;
            // Overflow-safe end: an untrusted manifest must not be able to wrap.
            let chunk_end = chunk_start.checked_add(chunk.length).ok_or_else(|| {
                LayerStoreError::MalformedManifest(format!(
                    "chunk offset {chunk_start} + length {} overflows u64",
                    chunk.length
                ))
            })?;
            // Skip chunks entirely before or after the requested range.
            if chunk_end <= offset || chunk_start >= end {
                continue;
            }
            let bytes = self.store.get_chunk(&chunk.hash)?;
            // The manifest's declared length must match the real chunk bytes;
            // otherwise the `to` bound below could exceed `bytes.len()`.
            if bytes.len() as u64 != chunk.length {
                return Err(LayerStoreError::MalformedManifest(format!(
                    "chunk {} declares length {} but addresses {} bytes",
                    chunk.hash,
                    chunk.length,
                    bytes.len()
                )));
            }
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
    use crate::layer_store::{
        ChunkingKind, HotsetRecorder, LayerKind, MEMORY_PAGE_CHUNK_SIZE, chunk_page_aligned,
        store_blob,
    };

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
            Err(LayerStoreError::RangeOutOfBounds { .. })
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
        let distinct: std::collections::HashSet<_> = manifest.referenced_chunks().collect();
        assert_eq!(distinct.len(), 3, "pages must be distinct for this test");

        // Build a hotset from this blob's chunks plus one foreign chunk.
        let mut rec = HotsetRecorder::new();
        for c in &manifest.chunks {
            rec.record(&c.hash);
        }
        rec.record(&crate::layer_store::hash_bytes(b"foreign-not-in-this-blob"));
        let hotset = rec.finish();

        let reader = LazyBlobReader::new(&store, &manifest);
        let warmed = reader.prefetch_hotset(&hotset).unwrap();
        assert_eq!(warmed, 3, "foreign chunk must be skipped");
    }

    #[test]
    fn read_range_errors_when_chunk_declared_length_exceeds_actual_bytes() {
        // A hostile manifest references a small real chunk but lies about its
        // length. Slicing on the lie would panic; we must error instead.
        use crate::layer_store::Chunk;
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let hash = store.put_chunk(b"hello").unwrap(); // 5 real bytes
        let manifest = BlobManifest::new(
            LayerKind::Other("evil".to_string()),
            100,
            ChunkingKind::ContentDefined,
            vec![Chunk {
                hash,
                offset: 0,
                length: 100, // lies: claims 100, holds 5
            }],
        );
        let reader = LazyBlobReader::new(&store, &manifest);
        assert!(matches!(
            reader.read_range(0, 50),
            Err(LayerStoreError::MalformedManifest(_))
        ));
    }

    #[test]
    fn read_all_errors_when_chunk_sum_does_not_match_total_len() {
        use crate::layer_store::Chunk;
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let hash = store.put_chunk(b"hello").unwrap(); // 5 real bytes
        let manifest = BlobManifest::new(
            LayerKind::Other("evil".to_string()),
            999, // lies: declared total != sum of chunk lengths (5)
            ChunkingKind::ContentDefined,
            vec![Chunk {
                hash,
                offset: 0,
                length: 5,
            }],
        );
        let reader = LazyBlobReader::new(&store, &manifest);
        assert!(matches!(
            reader.read_all(),
            Err(LayerStoreError::MalformedManifest(_))
        ));
    }

    #[test]
    fn read_range_errors_on_chunk_offset_length_overflow() {
        use crate::layer_store::Chunk;
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        // The chunk need not exist: the overflow check runs before any read.
        let hash = crate::layer_store::hash_bytes(b"x");
        let manifest = BlobManifest::new(
            LayerKind::Other("evil".to_string()),
            u64::MAX, // large enough that the range bounds-check passes
            ChunkingKind::ContentDefined,
            vec![Chunk {
                hash,
                offset: u64::MAX,
                length: 10, // offset + length overflows u64
            }],
        );
        let reader = LazyBlobReader::new(&store, &manifest);
        assert!(matches!(
            reader.read_range(0, 10),
            Err(LayerStoreError::MalformedManifest(_))
        ));
    }

    #[test]
    fn page_chunk_size_constant_is_used_for_memory_layers() {
        // Sanity that the memory page constant chunks as expected.
        let payload = vec![1u8; MEMORY_PAGE_CHUNK_SIZE + 10];
        let chunks = chunk_page_aligned(&payload, MEMORY_PAGE_CHUNK_SIZE);
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn mem_backend_default_is_file() {
        let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 256) as u8).collect();
        let (_dir, store, manifest) = store_with_blob(&payload);
        let reader = LazyBlobReader::new(&store, &manifest);
        assert_eq!(reader.backend(), MemBackend::File);
        assert_eq!(reader.realize().unwrap(), reader.read_all().unwrap());
    }

    #[test]
    fn mem_backend_uffd_fails_closed_not_panic() {
        let (_dir, store, manifest) = store_with_blob(b"some bytes");
        let err = LazyBlobReader::new(&store, &manifest)
            .with_backend(MemBackend::Uffd)
            .realize()
            .unwrap_err();
        assert!(matches!(err, LayerStoreError::Unsupported(_)));
    }
}

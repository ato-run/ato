//! Build-artifact path: chunk a layer's bytes, store the chunks, return a
//! [`BlobManifest`] referencing them.
//!
//! This is the inverse of [`LazyBlobReader`](crate::LazyBlobReader): together
//! they are the round-trip the Ready-State build (seal) and run (restore) rely
//! on — content-address a layer on build, read it back lazily on restore.

use crate::Result;
use crate::cas::CasStore;
use crate::chunk::{ChunkParams, chunk_content_defined, chunk_page_aligned};
use crate::manifest::{BlobManifest, ChunkingKind, LayerKind};

/// Chunk `payload` according to `chunking`, store every chunk in `store`, and
/// return the manifest. Storing is idempotent, so re-sealing identical bytes
/// reuses existing chunks (cross-layer and cross-version dedup).
pub fn store_blob(
    store: &CasStore,
    layer: LayerKind,
    payload: &[u8],
    chunking: ChunkingKind,
) -> Result<BlobManifest> {
    let chunks = match chunking {
        ChunkingKind::ContentDefined => chunk_content_defined(payload, ChunkParams::default()),
        ChunkingKind::PageAligned { page_size } => chunk_page_aligned(payload, page_size as usize),
    };
    // Persist each chunk's bytes. The chunk descriptors already carry the
    // content hash; re-deriving it inside put_chunk is cheap and keeps the
    // store the sole authority on what is on disk.
    for chunk in &chunks {
        let start = chunk.offset as usize;
        let end = start + chunk.length as usize;
        let stored = store.put_chunk(&payload[start..end])?;
        debug_assert_eq!(&stored, &chunk.hash, "chunk hash must match stored address");
    }
    Ok(BlobManifest::new(
        layer,
        payload.len() as u64,
        chunking,
        chunks,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LazyBlobReader;

    #[test]
    fn store_then_read_round_trips_content_defined() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let payload: Vec<u8> = (0..500_000u32).map(|i| (i * 31 % 256) as u8).collect();

        let manifest = store_blob(
            &store,
            LayerKind::App,
            &payload,
            ChunkingKind::ContentDefined,
        )
        .unwrap();
        let reader = LazyBlobReader::new(&store, &manifest);
        assert_eq!(reader.read_all().unwrap(), payload);
    }

    #[test]
    fn identical_layers_share_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let payload = vec![5u8; 300_000];

        let m1 = store_blob(
            &store,
            LayerKind::Rootfs,
            &payload,
            ChunkingKind::ContentDefined,
        )
        .unwrap();
        let count_after_first = store.list_chunks().unwrap().len();
        // Storing identical bytes as a different layer reuses the same chunks.
        let m2 = store_blob(
            &store,
            LayerKind::Runtime,
            &payload,
            ChunkingKind::ContentDefined,
        )
        .unwrap();
        let count_after_second = store.list_chunks().unwrap().len();

        assert_eq!(m1.chunks, m2.chunks, "same bytes → same chunk refs");
        assert_eq!(
            count_after_first, count_after_second,
            "no new chunks written for identical content"
        );
    }
}

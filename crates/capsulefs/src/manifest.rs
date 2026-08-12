//! Blob manifest — the ordered list of chunk refs for one Ready-State layer.
//!
//! A [`BlobManifest`] is the "manifest holds refs only" piece: the bytes live in
//! the [`CasStore`](crate::CasStore); the manifest records which chunks, in what
//! order, reassemble the layer. Its own id is a `blake3:<hex>` over the JCS
//! canonical form (structural-id family — see crate docs).

use serde::{Deserialize, Serialize};

use crate::chunk::Chunk;
use crate::hash::ContentHash;
use crate::{CapsuleFsError, Result};

/// Which Ready-State layer a blob holds. Mirrors the manifest layer refs in the
/// Ready-State plan §2.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    /// Read-only base rootfs image.
    Rootfs,
    /// Language runtime / interpreter layer.
    Runtime,
    /// Resolved dependencies / build output.
    Dependency,
    /// Application source / build output.
    App,
    /// VMM VM state file (device + CPU + vcpu state).
    VmState,
    /// Guest memory image (page-chunked for lazy load).
    Memory,
    /// Anything else, named.
    Other(String),
}

/// How a blob was chunked (so a reader knows the layout, and a hotset profile
/// can be interpreted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ChunkingKind {
    /// FastCDC content-defined chunking.
    ContentDefined,
    /// Fixed page-aligned chunks of `page_size` bytes (last may be shorter).
    PageAligned { page_size: u64 },
}

/// The chunk list and metadata for one stored layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobManifest {
    /// Which layer this blob is.
    pub layer: LayerKind,
    /// Total length of the reassembled blob in bytes.
    pub total_len: u64,
    /// Chunking strategy used.
    pub chunking: ChunkingKind,
    /// Ordered chunks. Reassembling them in order yields the original bytes.
    pub chunks: Vec<Chunk>,
}

impl BlobManifest {
    /// Build a manifest from an ordered chunk list and the layer's total length.
    pub fn new(
        layer: LayerKind,
        total_len: u64,
        chunking: ChunkingKind,
        chunks: Vec<Chunk>,
    ) -> Self {
        Self {
            layer,
            total_len,
            chunking,
            chunks,
        }
    }

    /// Content-addressed id of this manifest: `blake3:<hex>` over the JCS
    /// canonical form. Two manifests with identical layer/length/chunking/chunks
    /// (in any field order) get the same id.
    pub fn id(&self) -> ContentHash {
        let canonical = serde_jcs::to_vec(self)
            .expect("BlobManifest is always JCS-canonicalizable (no floats / non-string keys)");
        ContentHash::new_unchecked(format!("blake3:{}", blake3::hash(&canonical).to_hex()))
    }

    /// The distinct chunk hashes this manifest references (deduplicated).
    pub fn referenced_chunks(&self) -> impl Iterator<Item = &ContentHash> {
        self.chunks.iter().map(|c| &c.hash)
    }
}

/// Validate an untrusted blob layout without reading or materializing bytes.
pub fn validate_blob_manifest(manifest: &BlobManifest) -> Result<()> {
    let mut expected_offset = 0_u64;
    for chunk in &manifest.chunks {
        if chunk.offset != expected_offset {
            return Err(CapsuleFsError::MalformedManifest(format!(
                "chunk {} starts at {} but expected contiguous offset {expected_offset}",
                chunk.hash, chunk.offset
            )));
        }
        expected_offset = chunk.offset.checked_add(chunk.length).ok_or_else(|| {
            CapsuleFsError::MalformedManifest(format!(
                "chunk offset {} + length {} overflows u64",
                chunk.offset, chunk.length
            ))
        })?;
    }
    if expected_offset != manifest.total_len {
        return Err(CapsuleFsError::MalformedManifest(format!(
            "chunk layout ends at {expected_offset} but total_len is {}",
            manifest.total_len
        )));
    }
    if let ChunkingKind::PageAligned { page_size } = manifest.chunking {
        if page_size == 0 {
            return Err(CapsuleFsError::MalformedManifest(
                "page-aligned chunking requires a non-zero page size".to_string(),
            ));
        }
        for (index, chunk) in manifest.chunks.iter().enumerate() {
            let is_last = index + 1 == manifest.chunks.len();
            if chunk.offset % page_size != 0
                || chunk.length == 0
                || chunk.length > page_size
                || (!is_last && chunk.length != page_size)
            {
                return Err(CapsuleFsError::MalformedManifest(format!(
                    "chunk {} violates page-aligned layout with page size {page_size}",
                    chunk.hash
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_bytes;

    fn sample() -> BlobManifest {
        let chunks = vec![
            Chunk {
                hash: hash_bytes(b"a"),
                offset: 0,
                length: 1,
            },
            Chunk {
                hash: hash_bytes(b"bb"),
                offset: 1,
                length: 2,
            },
        ];
        BlobManifest::new(LayerKind::Memory, 3, ChunkingKind::ContentDefined, chunks)
    }

    #[test]
    fn manifest_id_is_stable_and_content_sensitive() {
        let m = sample();
        assert!(m.id().as_str().starts_with("blake3:"));
        assert_eq!(m.id(), sample().id());

        let mut other = sample();
        other.total_len = 4;
        assert_ne!(m.id(), other.id());
    }

    #[test]
    fn manifest_round_trips_through_json() {
        let m = sample();
        let json = serde_json::to_string(&m).unwrap();
        let back: BlobManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.id(), m.id());
    }

    #[test]
    fn referenced_chunks_lists_all() {
        let m = sample();
        assert_eq!(m.referenced_chunks().count(), 2);
    }

    #[test]
    fn layer_kind_other_serializes_with_name() {
        let m = BlobManifest::new(
            LayerKind::Other("model_weights".into()),
            0,
            ChunkingKind::ContentDefined,
            vec![],
        );
        let json = serde_json::to_string(&m.layer).unwrap();
        assert!(json.contains("model_weights"), "{json}");
    }

    #[test]
    fn structural_validation_requires_contiguous_page_aligned_layout() {
        let manifest = sample();
        assert!(validate_blob_manifest(&manifest).is_ok());

        let mut gap = manifest.clone();
        gap.chunks[1].offset += 1;
        assert!(validate_blob_manifest(&gap).is_err());

        let mut bad_page = manifest;
        bad_page.chunking = ChunkingKind::PageAligned { page_size: 2 };
        assert!(validate_blob_manifest(&bad_page).is_err());
    }
}

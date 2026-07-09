//! Chunking strategies.
//!
//! * Content-defined ([`chunk_content_defined`]) — FastCDC, for rootfs / app /
//!   dependency layers where dedup across versions matters.
//! * Page-aligned ([`chunk_page_aligned`]) — fixed-size, for the guest memory
//!   image so chunks map cleanly onto demand paging (plan §7).

use serde::{Deserialize, Serialize};

use crate::hash::{ContentHash, hash_bytes};

/// Fixed chunk size for memory images: 2 MiB, page-aligned for demand paging.
pub const MEMORY_PAGE_CHUNK_SIZE: usize = 2 * 1024 * 1024;

/// One chunk of a blob: its content address plus its position in the blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// `blake3:<hex>` of the chunk bytes.
    pub hash: ContentHash,
    /// Byte offset of this chunk within the reassembled blob.
    pub offset: u64,
    /// Length of this chunk in bytes.
    pub length: u64,
}

/// FastCDC content-defined chunking parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkParams {
    pub min_size: u32,
    pub avg_size: u32,
    pub max_size: u32,
}

impl Default for ChunkParams {
    /// 16 KiB / 64 KiB / 256 KiB — the same min/avg/max the capsule CAS chunker
    /// uses, so boundaries (and therefore chunk hashes) match.
    fn default() -> Self {
        Self {
            min_size: 16 * 1024,
            avg_size: 64 * 1024,
            max_size: 256 * 1024,
        }
    }
}

/// Content-defined chunking via FastCDC (v2020). Deterministic: identical bytes
/// and params always yield identical chunk boundaries and hashes. Byte-for-byte
/// compatible with `capsule::resource::cas::chunk_bytes_fastcdc`.
pub fn chunk_content_defined(payload: &[u8], params: ChunkParams) -> Vec<Chunk> {
    if payload.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for entry in
        fastcdc::v2020::FastCDC::new(payload, params.min_size, params.avg_size, params.max_size)
    {
        let begin = entry.offset;
        let end = entry.offset + entry.length;
        out.push(Chunk {
            hash: hash_bytes(&payload[begin..end]),
            offset: begin as u64,
            length: entry.length as u64,
        });
    }
    out
}

/// Fixed page-aligned chunking. The final chunk may be shorter than
/// `page_size`. Use [`MEMORY_PAGE_CHUNK_SIZE`] for guest memory images.
pub fn chunk_page_aligned(payload: &[u8], page_size: usize) -> Vec<Chunk> {
    assert!(page_size > 0, "page_size must be non-zero");
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < payload.len() {
        let end = (offset + page_size).min(payload.len());
        let slice = &payload[offset..end];
        out.push(Chunk {
            hash: hash_bytes(slice),
            offset: offset as u64,
            length: slice.len() as u64,
        });
        offset = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_defined_is_deterministic() {
        let data = vec![0x42u8; 512 * 1024];
        let a = chunk_content_defined(&data, ChunkParams::default());
        let b = chunk_content_defined(&data, ChunkParams::default());
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn content_defined_chunks_cover_payload_contiguously() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let chunks = chunk_content_defined(&data, ChunkParams::default());
        let mut cursor = 0u64;
        for c in &chunks {
            assert_eq!(c.offset, cursor, "chunks must be contiguous");
            cursor += c.length;
        }
        assert_eq!(cursor, data.len() as u64, "chunks must cover the whole payload");
    }

    #[test]
    fn content_defined_matches_capsule_cas_expression() {
        // The capsule CAS chunker formats each chunk hash as
        // blake3:{hash(chunk).to_hex()} over identical FastCDC boundaries.
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 256) as u8).collect();
        let p = ChunkParams::default();
        let ours = chunk_content_defined(&data, p);
        for entry in fastcdc::v2020::FastCDC::new(&data, p.min_size, p.avg_size, p.max_size)
            .zip(ours.iter())
        {
            let (fc, chunk) = entry;
            let slice = &data[fc.offset..fc.offset + fc.length];
            let expected = format!("blake3:{}", blake3::hash(slice).to_hex());
            assert_eq!(chunk.hash.as_str(), expected);
        }
    }

    #[test]
    fn empty_payload_yields_no_chunks() {
        assert!(chunk_content_defined(&[], ChunkParams::default()).is_empty());
        assert!(chunk_page_aligned(&[], MEMORY_PAGE_CHUNK_SIZE).is_empty());
    }

    #[test]
    fn page_aligned_splits_at_page_boundaries() {
        let data = vec![7u8; 5 * 1024 * 1024 + 123];
        let page = MEMORY_PAGE_CHUNK_SIZE;
        let chunks = chunk_page_aligned(&data, page);
        // ceil((5 MiB + 123) / 2 MiB) = 3 chunks: 2 MiB, 2 MiB, (1 MiB + 123 B).
        let expected = data.len().div_ceil(page);
        assert_eq!(chunks.len(), expected);
        assert_eq!(chunks.len(), 3);
        // All but the last are exactly page-sized; offsets are page-aligned.
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.offset, (i * page) as u64);
            if i + 1 < chunks.len() {
                assert_eq!(c.length, page as u64);
            } else {
                assert_eq!(c.length, (data.len() - i * page) as u64);
            }
        }
    }
}

//! CapsuleFS — content-addressed, chunked, lazily-read local store for
//! Ready-State Capsule layers.
//!
//! Every Ready-State layer (read-only base rootfs, language runtime, resolved
//! dependencies, app build output, the Firecracker VM state file, and the guest
//! **memory image**) is stored as immutable, content-addressed, chunked blobs
//! so that a restore lazily loads only the pages/blocks it actually touches —
//! the memory/model layers never force a full copy on every run (plan §7).
//!
//! ## Stage 1 (this milestone): File backend
//!
//! - Content-defined chunking (FastCDC) for rootfs/app/deps; **fixed
//!   page-aligned chunks** (2 MiB) for the memory image so it maps cleanly to
//!   demand paging.
//! - A local CAS ([`CasStore`]) under `<root>/blobs/blake3/<hex>`; chunks are
//!   verified by content hash on every read.
//! - A [`BlobManifest`] holds ordered chunk refs only — the bytes live in CAS.
//! - File-backed lazy reads ([`LazyBlobReader`]) reassemble a blob (or a byte
//!   range) on demand, and a [`HotsetProfile`] records the access order so a
//!   restore can prefetch the hot pages first.
//! - Ref-count / pin garbage collection ([`gc`]) reclaims unreferenced chunks;
//!   evictions are reported, never silently dropped.
//!
//! Stage 2 (later, not here) replaces the File backend's full-resident memory
//! file with a UFFD page server that serves chunks on demand.
//!
//! ## Hash scheme — reconciled, no new scheme invented
//!
//! Ato already uses two content-hash families and CapsuleFS adopts the matching
//! one for each concern rather than minting a third:
//!
//! * **Chunk / blob content** → `blake3:<hex>` over the raw chunk bytes. This is
//!   byte-for-byte the scheme the existing CAS chunker
//!   (`capsule::resource::cas::chunk_bytes_fastcdc`) uses, so a chunk produced
//!   here is interchangeable with one produced there.
//! * **Structural ids** (a [`BlobManifest`]'s id, and the Ready-State manifest /
//!   runner-class ids in the `capsule`/`snapshot` crates) → `blake3:<hex>` over
//!   the JSON-Canonical-Serialized (JCS) form, matching
//!   `install_lifecycle::hashing::canonical_hash` and execution identity.
//!
//! The `ato-blob-v1` **sha256 tree hash** (`capsule::foundation::blob`) is a
//! distinct concern — whole-source-tree identity — and is intentionally *not*
//! used here; CapsuleFS addresses chunks and blobs, not source trees. Keeping
//! both blob-content and structural ids on blake3 means there is exactly one
//! hash family on the CapsuleFS hot path.

mod cas;
mod chunk;
pub mod gc;
mod hash;
mod hotset;
mod manifest;
mod reader;
mod writer;

pub use cas::CasStore;
pub use chunk::{
    Chunk, ChunkParams, MEMORY_PAGE_CHUNK_SIZE, chunk_content_defined, chunk_page_aligned,
};
pub use gc::{GcReport, collect_garbage};
pub use hash::{ContentHash, InvalidContentHash, hash_bytes};
pub use hotset::{HotsetProfile, HotsetRecorder};
pub use manifest::{BlobManifest, ChunkingKind, LayerKind};
pub use reader::LazyBlobReader;
pub use writer::store_blob;

/// Errors raised by the CapsuleFS store.
#[derive(Debug, thiserror::Error)]
pub enum CapsuleFsError {
    /// A chunk referenced by a manifest is missing from the store.
    #[error("chunk {0} not found in store")]
    MissingChunk(ContentHash),

    /// A chunk's bytes did not hash to its content address (corruption /
    /// tampering). Fail-closed: a mismatching chunk is never returned.
    #[error("integrity check failed for chunk {expected}: bytes hash to {actual}")]
    IntegrityMismatch {
        expected: ContentHash,
        actual: ContentHash,
    },

    /// A requested byte range fell outside the blob.
    #[error("range {offset}..{end} is outside blob of length {len}")]
    RangeOutOfBounds { offset: u64, end: u64, len: u64 },

    /// Underlying I/O failure.
    #[error("capsulefs io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience result alias for CapsuleFS operations.
pub type Result<T> = std::result::Result<T, CapsuleFsError>;

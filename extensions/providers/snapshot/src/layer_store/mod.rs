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

mod binding;
mod cas;
mod chunk;
pub mod gc;

/// A process- and thread-unique temp filename suffix for atomic temp+rename
/// writes (`<dest>.<suffix>`, then rename over `<dest>`). Dependency-free (no
/// rand crate): process id + a process-global atomic counter (disambiguates two
/// threads writing identical content concurrently — the pid-only collision the
/// hardening calls out) + current nanos for entropy across rapid reuse. Always
/// begins `tmp.` so `.tmp.` skip/orphan filters keep matching.
pub fn unique_tmp_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("tmp.{}.{:x}.{:x}", std::process::id(), n, nanos)
}
mod hash;
mod hotset;
mod manifest;
mod reader;
mod writer;

pub use binding::{FileBindingSpec, WritebackMode};
pub use cas::CasStore;
pub use chunk::{
    Chunk, ChunkParams, MEMORY_PAGE_CHUNK_SIZE, chunk_content_defined, chunk_page_aligned,
};
pub use gc::{EvictedChunk, GcReport, collect_garbage};
pub use hash::{ContentHash, InvalidContentHash, hash_bytes};
pub use hotset::{HotsetProfile, HotsetRecorder};
pub use manifest::{BlobManifest, ChunkingKind, LayerKind, validate_blob_manifest};
pub use reader::{LazyBlobReader, MemBackend};
pub use writer::store_blob;

/// Errors raised by the CapsuleFS store.
#[derive(Debug, thiserror::Error)]
pub enum LayerStoreError {
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

    /// A [`BlobManifest`]'s self-described layout is internally inconsistent —
    /// a chunk's declared `length` does not match the bytes it addresses, the
    /// chunk lengths do not sum to `total_len`, or an `offset + length` would
    /// overflow. A manifest is untrusted input (it may arrive from another
    /// host), so a malformed layout is rejected here as an error rather than
    /// being trusted into an out-of-bounds slice / panic.
    #[error("malformed blob manifest: {0}")]
    MalformedManifest(String),

    /// A requested capability is not implemented in this stage (e.g. the UFFD
    /// memory backend is a Stage-2 seam). Fail-closed rather than panic so a
    /// mis-selected backend errors cleanly.
    #[error("layer store unsupported: {0}")]
    Unsupported(String),

    /// Underlying I/O failure.
    #[error("layer store io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience result alias for CapsuleFS operations.
pub type Result<T> = std::result::Result<T, LayerStoreError>;

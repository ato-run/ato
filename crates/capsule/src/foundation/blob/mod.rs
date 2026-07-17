//! Blob freeze and tree hashing primitives.
//!
//! These utilities turn a directory tree into a content-addressable identity
//! suitable for use as a [`crate::common::store::BlobAddress`]. The hash is
//! locked down by `docs/rfcs/accepted/A1_BLOB_HASH.md` and **must not change
//! once shipped**: doing so would invalidate every existing cache entry.

pub mod manifest;
pub mod source_archive;
pub mod source_tree;
pub mod tree_hash;

pub use manifest::{BLOB_MANIFEST_SCHEMA_VERSION, BLOB_TREE_ALGORITHM, BlobManifest};
pub use source_archive::{
    MAX_COMPRESSED_BYTES, MAX_UNCOMPRESSED_BYTES, MaterializedSource, SourceMaterializeError,
    materialize_source_archive,
};
pub use source_tree::{
    MAX_FILE_COUNT, MAX_FILE_SIZE_BYTES, SourceAdmissibilityError, materialized_source_tree_hash,
    source_archive_hash,
};
pub use tree_hash::{TreeHash, TreeHashError, hash_tree};

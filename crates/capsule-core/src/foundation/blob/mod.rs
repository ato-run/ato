//! Blob freeze and tree hashing primitives.
//!
//! These utilities turn a directory tree into a content-addressable identity
//! suitable for use as a [`crate::common::store::BlobAddress`]. The hash is
//! locked down by `docs/rfcs/accepted/A1_BLOB_HASH.md` and **must not change
//! once shipped**: doing so would invalidate every existing cache entry.

pub mod tree_hash;

pub use tree_hash::{hash_tree, TreeHash, TreeHashError};

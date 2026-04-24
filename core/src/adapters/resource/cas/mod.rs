//! Content-Addressable Storage (CAS) client abstraction.
//!
//! Provides a unified interface for fetching and storing content-addressed blobs,
//! supporting both local filesystem and remote HTTP backends.

mod bloom;
mod chunker;
mod client;
mod index;

pub use bloom::{
    AtoBloomFilter, AtoBloomWire, DEFAULT_BLOOM_FALSE_POSITIVE_RATE, DEFAULT_BLOOM_SEED,
};
pub use chunker::chunk_bytes_fastcdc;
pub use client::{create_cas_client_from_env, CasClient, HttpCasClient, LocalCasClient};
pub use index::LocalCasIndex;

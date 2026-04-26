//! Shared constants used across the Nacelle codebase.
//!
//! This module contains canonical definitions for magic bytes, version identifiers,
//! and other constants that must be consistent across library and CLI components.

/// Magic bytes to identify self-extracting v2 bundles.
///
/// Format: Binary (variable size) | Compressed Bundle (variable size) | MAGIC (18 bytes) | Size (8 bytes)
pub const BUNDLE_MAGIC: &[u8] = b"NACELLE_V2_BUNDLE";

/// Expected length of the bundle magic byte sequence.
pub const BUNDLE_MAGIC_LEN: usize = 18; // Length of NACELLE_V2_BUNDLE

//! Canonical content hashing for launch-reuse records.
//!
//! Every stable `*_hash` used by the launch-reuse model (requirement graph,
//! binding set, network/capability policy, state contract, launch template key)
//! is a `blake3:<hex>` digest of a JSON-Canonical-Serialized (JCS, RFC 8785)
//! value. JCS is chosen so the digest is independent of object key ordering and
//! matches the canonicalization used by [`crate::engine::execution_identity`]
//! (`canonicalization = "jcs"`, `hash_algorithm = "blake3-256"`). That lets a
//! launch-reuse hash and an execution-identity hash share one canonical form.
//!
//! # What must NOT be hashed here
//!
//! Callers must never feed session-specific or runtime-observed facts into a
//! value that is hashed for a cache key: session ids, dynamic ports, process /
//! container ids, live routes, log cursors, observed status, timestamps, or
//! secret values. Those belong on the per-session
//! [`crate::foundation::install_lifecycle::materialization::LaunchMaterializationRecord`],
//! never on a [`crate::foundation::install_lifecycle::launch_template::LaunchTemplateKey`].

use anyhow::{Context, Result};
use serde::Serialize;

/// Compute a deterministic `blake3:<hex>` content hash of any serializable
/// value using JSON Canonical Serialization.
///
/// Returns `Err` only if the value cannot be canonicalized (e.g. a map with
/// non-string keys or a float NaN); the plain records in this module never
/// trip that.
pub fn canonical_hash<T: Serialize>(value: &T) -> Result<String> {
    let canonical = serde_jcs::to_vec(value).context("canonicalize value for content hashing")?;
    Ok(format!("blake3:{}", blake3::hash(&canonical).to_hex()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_hash_is_stable_and_key_order_independent() {
        // JCS sorts object keys, so two maps with the same entries in different
        // insertion order must hash identically.
        let mut a = std::collections::BTreeMap::new();
        a.insert("b", 2);
        a.insert("a", 1);
        let h1 = canonical_hash(&a).unwrap();

        let mut b = std::collections::HashMap::new();
        b.insert("a", 1);
        b.insert("b", 2);
        let h2 = canonical_hash(&b).unwrap();

        assert_eq!(h1, h2, "JCS hash must be key-order independent");
        assert!(h1.starts_with("blake3:"), "hash must be blake3:<hex>");
    }

    #[test]
    fn canonical_hash_changes_with_content() {
        let h1 = canonical_hash(&"alpha").unwrap();
        let h2 = canonical_hash(&"beta").unwrap();
        assert_ne!(h1, h2);
    }
}

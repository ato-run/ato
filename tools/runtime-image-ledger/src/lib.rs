//! Runtime image ledger: validate *declared* image and patch-candidate
//! evidence.
//!
//! This crate is deliberately not an image inspector. It reads a versioned
//! inventory file, checks that its declarations are internally consistent and
//! cover the evidence each status claims, and reports what is declared and what
//! is missing. It never pulls an image, generates an SBOM, runs a scanner,
//! verifies a signature, or decides Contract satisfaction. The only Contract K
//! authority remains `accept_receipt` / `accept_verified_route` in
//! `lib/formation/src/receipt.rs`.

pub mod model;
pub mod report;
pub mod rfc3339;
pub mod validate;

use sha2::{Digest, Sha256};

/// SHA-256 of raw inventory bytes, as `sha256:` plus lowercase hex.
///
/// This identifies which declaration file was validated. It says nothing about
/// the images the file describes.
pub fn sha256_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(7 + digest.len() * 2);
    output.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_bytes_as_a_sha256_reference() {
        assert_eq!(
            sha256_digest(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(sha256_digest(b"ato").len(), 71);
    }
}

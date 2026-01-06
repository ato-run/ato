//! Sign command - create Ed25519 signature over Cap'n Proto canonical bytes
//!
//! UARC V1.1.0 Normative Decision #2:
//! "Cap'n Proto canonical bytes are the sole signing ground truth"

use anyhow::{Context, Result};
use capsuled::capsule_types::capsule_v1::CapsuleManifestV1;
use capsuled::schema::converter::manifest_to_capnp_bytes;
use ed25519_dalek::{Signature, Signer, SigningKey};
use std::fs;
use std::path::PathBuf;

/// Arguments for the sign command
pub struct SignArgs {
    pub manifest_path: PathBuf,
    pub key_path: PathBuf,
    pub output: Option<PathBuf>,
}

/// Sign a capsule manifest using Ed25519 over canonical bytes
pub fn execute(args: SignArgs) -> Result<()> {
    println!("✍️  Signing capsule...");
    println!("Manifest: {}", args.manifest_path.display());
    println!("Key:      {}", args.key_path.display());

    // 1. Load manifest
    let manifest_content =
        fs::read_to_string(&args.manifest_path).context("Failed to read manifest file")?;

    // Parse based on file extension
    let manifest: CapsuleManifestV1 = if args.manifest_path.extension().and_then(|e| e.to_str()) == Some("toml") {
        toml::from_str(&manifest_content).context("Failed to parse TOML manifest")?
    } else {
        // Assume JSON for .capsule files
        serde_json::from_str(&manifest_content).context("Failed to parse JSON manifest")?
    };

    println!("✓ Loaded manifest: {} v{}", manifest.name, manifest.version);

    // 2. Generate Cap'n Proto canonical bytes
    let canonical_bytes =
        manifest_to_capnp_bytes(&manifest).context("Failed to generate canonical bytes")?;
    println!(
        "✓ Generated canonical bytes ({} bytes)",
        canonical_bytes.len()
    );

    // 3. Load Ed25519 private key
    let secret_bytes = fs::read(&args.key_path)
        .with_context(|| format!("Failed to read private key: {}", args.key_path.display()))?;

    if secret_bytes.len() != 32 {
        anyhow::bail!(
            "Invalid private key length: expected 32 bytes, got {}",
            secret_bytes.len()
        );
    }

    let signing_key = SigningKey::from_bytes(
        &secret_bytes
            .try_into()
            .expect("Already verified length is 32"),
    );
    println!("✓ Loaded signing key");

    // 4. Sign canonical bytes
    let signature: Signature = signing_key.sign(&canonical_bytes);
    let signature_bytes = signature.to_bytes();

    println!(
        "✓ Generated signature: {}",
        hex::encode(&signature_bytes[..16])
    );

    // 5. Determine output path
    let output_path = args.output.unwrap_or_else(|| {
        let mut path = args.manifest_path.clone();
        path.set_extension("sig");
        path
    });

    // 6. Write .sig file (raw 64-byte signature)
    fs::write(&output_path, &signature_bytes)
        .with_context(|| format!("Failed to write signature file: {}", output_path.display()))?;

    println!("✅ Signed successfully!");
    println!("Signature: {}", output_path.display());
    println!();
    println!("🔐 Signature details:");
    println!("  Algorithm: Ed25519");
    println!("  Size: 64 bytes");
    println!("  Hex: {}", hex::encode(signature_bytes));

    Ok(())
}

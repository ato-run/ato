//! Pack command - build and sign a deployable .capsule archive
//!
//! UARC V1.1.0 compliant packing workflow:
//! 1. Parse capsule.toml into CapsuleManifestV1
//! 2. Scan source directory (respecting .gitignore)
//! 3. Calculate SHA256 hash of source tree (CAS digest)
//! 4. Update manifest.targets.source.digest field
//! 5. Serialize to .capsule file
//! 6. Optionally sign with Ed25519 key

use anyhow::{Context, Result};
use capsuled::capsule_types::capsule_v1::CapsuleManifestV1;
use capsuled::schema::converter::manifest_to_capnp_bytes;
use ed25519_dalek::{Signature, Signer, SigningKey};
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Arguments for the pack command
pub struct PackArgs {
    pub manifest_path: PathBuf,
    pub output: Option<PathBuf>,
    /// Path to signing key (.secret file)
    pub key: Option<PathBuf>,
}

/// Pack result for programmatic use
pub struct PackResult {
    pub manifest: CapsuleManifestV1,
    pub source_dir: PathBuf,
    pub source_digest: Option<String>,
}

/// Pack a capsule in-memory (without writing to disk)
/// Used by `dev` and `run` commands
pub fn pack_in_memory(manifest_path: &Path) -> Result<PackResult> {
    let manifest_content =
        fs::read_to_string(manifest_path).context("Failed to read manifest file")?;
    let mut manifest: CapsuleManifestV1 =
        toml::from_str(&manifest_content).context("Failed to parse TOML manifest")?;

    let source_dir = manifest_path
        .parent()
        .context("Failed to determine source directory")?
        .to_path_buf();

    let mut source_digest = None;

    // Calculate CAS digest if source target is specified
    if let Some(ref mut targets) = manifest.targets {
        if targets.source.is_some() {
            let digest = calculate_source_digest(&source_dir, manifest_path)?;
            let digest_str = format!("sha256:{}", hex::encode(digest));
            targets.source_digest = Some(digest_str.clone());
            source_digest = Some(digest_str);
        }
    }

    Ok(PackResult {
        manifest,
        source_dir,
        source_digest,
    })
}

/// Pack a capsule manifest into a deployable .capsule file
pub fn execute(args: PackArgs) -> Result<()> {
    println!("📦 Packing capsule...");
    println!("Manifest: {}", args.manifest_path.display());

    let result = pack_in_memory(&args.manifest_path)?;
    
    println!("✓ Parsed manifest: {} v{}", result.manifest.name, result.manifest.version);
    
    if let Some(ref digest) = result.source_digest {
        println!("✓ Source target detected, calculating CAS digest...");
        println!("✓ Source digest: {}", digest);
    }

    // Determine output path
    let output_path = args.output.unwrap_or_else(|| {
        let mut path = result.source_dir.join(&result.manifest.name);
        path.set_extension("capsule");
        path
    });

    // Serialize to .capsule file (JSON format for machine processing)
    let output_json = serde_json::to_string_pretty(&result.manifest)
        .context("Failed to serialize manifest to JSON")?;
    fs::write(&output_path, &output_json).context("Failed to write .capsule file")?;

    println!("✅ Packed successfully!");
    println!("Output: {}", output_path.display());

    // Sign if key provided
    if let Some(key_path) = args.key {
        sign_capsule(&result.manifest, &key_path, &output_path)?;
    } else {
        println!("\n💡 Tip: Use 'capsule pack --key <path>' to sign");
    }

    Ok(())
}

/// Sign a capsule with Ed25519 over canonical bytes
fn sign_capsule(manifest: &CapsuleManifestV1, key_path: &Path, capsule_path: &Path) -> Result<()> {
    println!("\n✍️  Signing with: {}", key_path.display());

    // Generate Cap'n Proto canonical bytes
    let canonical_bytes = manifest_to_capnp_bytes(manifest)
        .context("Failed to generate canonical bytes")?;
    println!("   ✓ Generated canonical bytes ({} bytes)", canonical_bytes.len());

    // Load Ed25519 private key
    let secret_bytes = fs::read(key_path)
        .with_context(|| format!("Failed to read private key: {}", key_path.display()))?;

    if secret_bytes.len() != 32 {
        anyhow::bail!(
            "Invalid private key length: expected 32 bytes, got {}",
            secret_bytes.len()
        );
    }

    let signing_key = SigningKey::from_bytes(
        &secret_bytes.try_into().expect("Already verified length is 32"),
    );

    // Sign canonical bytes
    let signature: Signature = signing_key.sign(&canonical_bytes);
    let signature_bytes = signature.to_bytes();

    // Write .sig file
    let sig_path = capsule_path.with_extension("sig");
    fs::write(&sig_path, &signature_bytes)
        .with_context(|| format!("Failed to write signature: {}", sig_path.display()))?;

    println!("   ✓ Signed: {}", sig_path.display());
    println!("   Signature: {}...", hex::encode(&signature_bytes[..16]));

    Ok(())
}

/// Pack and sign in one step (for programmatic use)
pub fn pack_and_sign(manifest_path: &Path, key_path: &Path) -> Result<(PackResult, Vec<u8>)> {
    let result = pack_in_memory(manifest_path)?;
    
    // Generate canonical bytes and sign
    let canonical_bytes = manifest_to_capnp_bytes(&result.manifest)
        .context("Failed to generate canonical bytes")?;
    
    let secret_bytes = fs::read(key_path)?;
    if secret_bytes.len() != 32 {
        anyhow::bail!("Invalid key length");
    }
    
    let signing_key = SigningKey::from_bytes(
        &secret_bytes.try_into().expect("Length verified"),
    );
    let signature: Signature = signing_key.sign(&canonical_bytes);
    
    Ok((result, signature.to_bytes().to_vec()))
}

/// Calculate SHA256 digest of source directory
///
/// Respects .gitignore rules and excludes:
/// - .git directory
/// - .capsule files
/// - capsule.toml manifest itself
pub fn calculate_source_digest(source_dir: &Path, manifest_path: &Path) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut file_count = 0usize;

    // Use ignore crate for .gitignore support
    let walker = WalkBuilder::new(source_dir)
        .hidden(false) // Include hidden files (they might be important)
        .git_ignore(true) // Respect .gitignore
        .git_exclude(true) // Respect .git/info/exclude
        .git_global(false) // Don't use global gitignore
        .build();

    for entry in walker {
        let entry = entry.context("Failed to read directory entry")?;
        let path = entry.path();

        // Skip directories
        if path.is_dir() {
            continue;
        }

        // Skip .git directory contents (should already be filtered, but double-check)
        if path
            .components()
            .any(|c| c.as_os_str().to_string_lossy() == ".git")
        {
            continue;
        }

        // Skip .capsule output files
        if path.extension().and_then(|e| e.to_str()) == Some("capsule")
            || path.extension().and_then(|e| e.to_str()) == Some("sig")
        {
            continue;
        }

        // Skip the manifest file itself
        if path == manifest_path {
            continue;
        }

        // Read file and update hash
        let relative_path = path
            .strip_prefix(source_dir)
            .context("Failed to compute relative path")?;
        let mut file = fs::File::open(path)
            .with_context(|| format!("Failed to open file: {}", path.display()))?;

        // Hash: path length (8 bytes) + path bytes + file content
        let path_str = relative_path.to_string_lossy();
        let path_bytes = path_str.as_bytes();
        hasher.update(&(path_bytes.len() as u64).to_le_bytes());
        hasher.update(path_bytes);

        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;
        hasher.update(&buffer);

        file_count += 1;
    }

    println!("✓ Scanned {} files", file_count);

    let result = hasher.finalize();
    Ok(result.into())
}

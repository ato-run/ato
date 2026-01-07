//! Pack command - build and sign a deployable .capsule archive
//!
//! UARC V1.1.0 compliant packing workflow:
//! 1. Parse capsule.toml into CapsuleManifestV1
//! 2. Scan source directory (respecting .gitignore)
//! 3. Create tar.gz archive of source files
//! 4. Calculate SHA256 hash of archive (CAS digest)
//! 5. Store archive in CAS (~/.capsule/cas/blobs/)
//! 6. Update manifest.targets.source.digest field
//! 7. Serialize to .capsule file
//! 8. Optionally sign with Ed25519 key

use anyhow::{Context, Result};
use capsuled::capsule_types::capsule_v1::CapsuleManifestV1;
use capsuled::schema::converter::manifest_to_capnp_bytes;
use ed25519_dalek::{Signature, Signer, SigningKey};
use flate2::write::GzEncoder;
use flate2::Compression;
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tar::Builder;

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
    /// Path to the archived source in CAS
    pub cas_blob_path: Option<PathBuf>,
}

/// Get the CAS blobs directory
fn get_cas_blobs_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to get home directory")?;
    let cas_dir = home.join(".capsule").join("cas").join("blobs");
    fs::create_dir_all(&cas_dir).context("Failed to create CAS blobs directory")?;
    Ok(cas_dir)
}

/// Pack a capsule in-memory (without writing to disk)
/// Used by `dev` and `run` commands
pub fn pack_in_memory(manifest_path: &Path) -> Result<PackResult> {
    // Canonicalize path to handle relative paths correctly
    let manifest_path = manifest_path.canonicalize()
        .with_context(|| format!("Failed to canonicalize manifest path: {}", manifest_path.display()))?;
    
    let manifest_content =
        fs::read_to_string(&manifest_path).context("Failed to read manifest file")?;
    let mut manifest: CapsuleManifestV1 =
        toml::from_str(&manifest_content).context("Failed to parse TOML manifest")?;

    let source_dir = manifest_path
        .parent()
        .context("Failed to determine source directory")?
        .to_path_buf();

    let mut source_digest = None;
    let mut cas_blob_path = None;

    // Create archive and store in CAS if source target is specified
    if let Some(ref mut targets) = manifest.targets {
        if targets.source.is_some() {
            // Create tar.gz archive of source files
            let (archive_bytes, file_count) = create_source_archive(&source_dir, &manifest_path)?;
            println!("✓ Archived {} files ({} bytes)", file_count, archive_bytes.len());
            
            // Calculate SHA256 hash of archive
            let mut hasher = Sha256::new();
            hasher.update(&archive_bytes);
            let digest_bytes: [u8; 32] = hasher.finalize().into();
            let hash_hex = hex::encode(digest_bytes);
            let digest_str = format!("sha256:{}", hash_hex);
            
            // Store archive in CAS with format: sha256-<hash> (matches LocalCasClient.blob_path)
            let cas_dir = get_cas_blobs_dir()?;
            let blob_filename = format!("sha256-{}", hash_hex);
            let blob_path = cas_dir.join(&blob_filename);
            fs::write(&blob_path, &archive_bytes)
                .with_context(|| format!("Failed to write CAS blob: {}", blob_path.display()))?;
            println!("✓ Stored in CAS: {}", blob_path.display());
            
            targets.source_digest = Some(digest_str.clone());
            source_digest = Some(digest_str);
            cas_blob_path = Some(blob_path);
        }
    }

    Ok(PackResult {
        manifest,
        source_dir,
        source_digest,
        cas_blob_path,
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

    // Get public key for signature file
    let public_key = signing_key.verifying_key();
    let public_key_bytes = public_key.to_bytes();

    // Sign canonical bytes
    let signature: Signature = signing_key.sign(&canonical_bytes);
    let signature_bytes = signature.to_bytes();

    // Build full signature file:
    // [version: 1 byte][key_type: 1 byte][public_key: 32 bytes][signature: 64 bytes][metadata_len: 2 bytes][metadata: N bytes]
    let metadata = serde_json::json!({
        "capsule_name": manifest.name,
        "capsule_version": manifest.version,
        "signed_at": chrono::Utc::now().to_rfc3339()
    });
    let metadata_bytes = serde_json::to_vec(&metadata)?;
    let metadata_len = metadata_bytes.len() as u16;

    let mut sig_file_bytes = Vec::with_capacity(1 + 1 + 32 + 64 + 2 + metadata_bytes.len());
    sig_file_bytes.push(1u8);  // version
    sig_file_bytes.push(1u8);  // key_type: 1 = Ed25519
    sig_file_bytes.extend_from_slice(&public_key_bytes);
    sig_file_bytes.extend_from_slice(&signature_bytes);
    sig_file_bytes.extend_from_slice(&metadata_len.to_be_bytes());
    sig_file_bytes.extend_from_slice(&metadata_bytes);

    // Write .sig file
    let sig_path = capsule_path.with_extension("sig");
    fs::write(&sig_path, &sig_file_bytes)
        .with_context(|| format!("Failed to write signature: {}", sig_path.display()))?;

    println!("   ✓ Signed: {}", sig_path.display());
    println!("   Signature: {}...", hex::encode(&signature_bytes[..16]));
    println!("   Public key: {}...", hex::encode(&public_key_bytes[..8]));

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

/// Create a tar.gz archive of source files
///
/// Respects .gitignore rules and excludes:
/// - .git directory
/// - .capsule files
/// - capsule.toml manifest itself
///
/// Returns (archive_bytes, file_count)
pub fn create_source_archive(source_dir: &Path, manifest_path: &Path) -> Result<(Vec<u8>, usize)> {
    let mut file_count = 0usize;
    
    // Collect files first (sorted for reproducibility)
    let mut files: Vec<PathBuf> = Vec::new();
    
    // Use ignore crate for .gitignore support
    let walker = WalkBuilder::new(source_dir)
        .hidden(false) // Include hidden files (they might be important)
        .git_ignore(true) // Respect .gitignore if present
        .git_exclude(true) // Respect .git/info/exclude if present
        .git_global(false) // Don't use global gitignore
        .require_git(false) // Don't require a git repository
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

        // Skip the manifest file itself (capsule.toml is included in .capsule, not in archive)
        if path == manifest_path {
            continue;
        }

        files.push(path.to_path_buf());
    }
    
    // Sort for reproducibility
    files.sort();
    
    // Create tar.gz archive in memory
    let mut archive_buffer = Vec::new();
    {
        let encoder = GzEncoder::new(&mut archive_buffer, Compression::default());
        let mut tar_builder = Builder::new(encoder);
        
        for path in &files {
            let relative_path = path
                .strip_prefix(source_dir)
                .context("Failed to compute relative path")?;
            
            let mut file = fs::File::open(path)
                .with_context(|| format!("Failed to open file: {}", path.display()))?;
            
            // Add file to tar with relative path
            tar_builder.append_file(relative_path, &mut file)
                .with_context(|| format!("Failed to add file to archive: {}", path.display()))?;
            
            file_count += 1;
        }
        
        // Finish tar and gzip
        let encoder = tar_builder.into_inner()
            .context("Failed to finalize tar archive")?;
        encoder.finish()
            .context("Failed to finalize gzip compression")?;
    }
    
    Ok((archive_buffer, file_count))}

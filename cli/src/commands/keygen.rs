//! Key generation command implementation
//!
//! Generates Ed25519 keypairs for signing UARC capsules.
//! Keys are stored in ~/.capsule/keys/ with 0600 permissions.

use anyhow::{Context, Result};
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Arguments for the keygen command
pub struct KeygenArgs {
    pub name: Option<String>,
}

/// Generate a new Ed25519 keypair and save to ~/.capsule/keys/
pub fn execute(args: KeygenArgs) -> Result<()> {
    // Determine key name
    let key_name = args.name.unwrap_or_else(|| {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        format!("capsule-key-{}", timestamp)
    });

    // Get keys directory
    let keys_dir = get_keys_directory()?;
    fs::create_dir_all(&keys_dir)
        .with_context(|| format!("Failed to create keys directory: {:?}", keys_dir))?;

    // Generate Ed25519 keypair
    let mut csprng = OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let verifying_key: VerifyingKey = (&signing_key).into();

    // Save private key (secret key bytes)
    let secret_key_path = keys_dir.join(format!("{}.secret", key_name));
    let secret_bytes = signing_key.to_bytes();
    fs::write(&secret_key_path, secret_bytes)
        .with_context(|| format!("Failed to write secret key: {:?}", secret_key_path))?;

    // Set 0600 permissions (owner read/write only)
    let mut perms = fs::metadata(&secret_key_path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&secret_key_path, perms)
        .with_context(|| format!("Failed to set permissions on: {:?}", secret_key_path))?;

    // Save public key
    let public_key_path = keys_dir.join(format!("{}.public", key_name));
    let public_bytes = verifying_key.to_bytes();
    fs::write(&public_key_path, public_bytes)
        .with_context(|| format!("Failed to write public key: {:?}", public_key_path))?;

    // Calculate public key fingerprint (SHA256)
    let mut hasher = Sha256::new();
    hasher.update(&public_bytes);
    let fingerprint = hasher.finalize();
    let fingerprint_hex: String = fingerprint.iter().map(|b| format!("{:02x}", b)).collect();

    // Output success message
    println!("✅ Key generated successfully!");
    println!();
    println!("Key name:      {}", key_name);
    println!("Private key:   {}", secret_key_path.display());
    println!("Public key:    {}", public_key_path.display());
    println!();
    println!("Public key (hex):");
    println!("{}", hex::encode(public_bytes));
    println!();
    println!("Fingerprint (SHA256):");
    println!("{}", fingerprint_hex);
    println!();
    println!("⚠️  Keep your private key secure! (stored with 0600 permissions)");

    Ok(())
}

/// Get the keys directory path (~/.capsule/keys)
fn get_keys_directory() -> Result<PathBuf> {
    let home_dir = dirs::home_dir().context("Failed to determine home directory")?;
    Ok(home_dir.join(".capsule").join("keys"))
}

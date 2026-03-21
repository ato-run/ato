//! Profile command implementation
//!
//! Creates and shows profile.sync capsules.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use blake3::Hasher;
use ed25519_dalek::Signer;
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use capsule_core::types::profile::{
    ProfileInfo, ProfileManifest, ProfileMeta, ProfilePermissions, ProfilePolicy, ProfileSignature,
    ProfileSync,
};
use capsule_core::types::signing::StoredKey;
use capsule_core::CapsuleReporter;

#[derive(Debug)]
pub struct CreateArgs {
    pub name: String,
    pub bio: Option<String>,
    pub avatar: Option<PathBuf>,
    pub key: PathBuf,
    pub output: Option<PathBuf>,
    pub website: Option<String>,
    pub github: Option<String>,
    pub twitter: Option<String>,
}

#[derive(Debug)]
pub struct ShowArgs {
    pub path: PathBuf,
    pub json: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileOutput {
    pub did: String,
    pub display_name: String,
    pub bio: Option<String>,
    pub links: Option<HashMap<String, String>>,
    pub created_at: String,
    pub is_signed: bool,
    pub avatar_present: bool,
}

pub fn execute_create(
    args: CreateArgs,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    // Load signing key
    let stored_key = StoredKey::read(&args.key)
        .with_context(|| format!("Failed to read key: {}", args.key.display()))?;
    let signing_key = stored_key.to_signing_key()?;
    let did = stored_key.did()?;

    // Build profile manifest
    let now = chrono::Utc::now();
    let mut links = HashMap::new();
    if let Some(ref w) = args.website {
        links.insert("website".to_string(), w.clone());
    }
    if let Some(ref g) = args.github {
        links.insert("github".to_string(), format!("https://github.com/{}", g));
    }
    if let Some(ref t) = args.twitter {
        links.insert("twitter".to_string(), format!("https://twitter.com/{}", t));
    }

    let mut manifest = ProfileManifest {
        sync: ProfileSync::default(),
        meta: ProfileMeta {
            created_by: did.clone(),
            created_at: now.to_rfc3339(),
            updated_at: None,
        },
        profile: ProfileInfo {
            display_name: args.name.clone(),
            bio: args.bio.clone(),
            avatar_hash: None,
            links: if links.is_empty() { None } else { Some(links) },
        },
        policy: ProfilePolicy::default(),
        permissions: ProfilePermissions::default(),
        signature: None,
    };

    // Create output directory
    let output_path = args.output.unwrap_or_else(|| PathBuf::from("profile.sync"));
    let default_dir = PathBuf::from(".");
    let output_dir = output_path.parent().unwrap_or(&default_dir);
    std::fs::create_dir_all(output_dir)?;

    // Create temp directory for building archive
    let temp_dir = tempfile::tempdir()?;
    let bundle_dir = temp_dir.path();

    // Copy avatar if provided
    if let Some(ref avatar_path) = args.avatar {
        let avatar_data = std::fs::read(avatar_path)
            .with_context(|| format!("Failed to read avatar: {}", avatar_path.display()))?;

        // Compute BLAKE3 hash
        let mut hasher = Hasher::new();
        hasher.update(&avatar_data);
        let hash = hasher.finalize();
        let hash_hex = hex::encode(hash.as_bytes());

        manifest.profile.avatar_hash = Some(format!("blake3:{}", hash_hex));

        // Determine extension
        let ext = avatar_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png");

        // Create payload directory and write avatar
        let payload_dir = bundle_dir.join("payload");
        std::fs::create_dir_all(&payload_dir)?;
        std::fs::write(payload_dir.join(format!("avatar.{}", ext)), &avatar_data)?;
    }

    // Compute manifest hash (without signature)
    let manifest_toml = manifest.to_toml()?;
    let mut manifest_hasher = Hasher::new();
    manifest_hasher.update(manifest_toml.as_bytes());
    let manifest_hash = manifest_hasher.finalize();
    let manifest_hash_hex = format!("blake3:{}", hex::encode(manifest_hash.as_bytes()));

    // Compute payload hash if present
    let payload_hash = if manifest.profile.avatar_hash.is_some() {
        let payload_dir = bundle_dir.join("payload");
        let mut hasher = Hasher::new();
        for entry in walkdir::WalkDir::new(&payload_dir)
            .sort_by_file_name()
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let data = std::fs::read(entry.path())?;
            hasher.update(&data);
        }
        Some(format!(
            "blake3:{}",
            hex::encode(hasher.finalize().as_bytes())
        ))
    } else {
        None
    };

    // Create signing payload
    let signing_payload = if let Some(ref ph) = payload_hash {
        format!("{}|{}", manifest_hash_hex, ph)
    } else {
        manifest_hash_hex.clone()
    };

    // Sign
    let signature = signing_key.sign(signing_payload.as_bytes());
    let sig_base64 = BASE64.encode(signature.to_bytes());

    // Add signature to manifest
    manifest.signature = Some(ProfileSignature {
        algo: "Ed25519".to_string(),
        manifest_hash: manifest_hash_hex,
        payload_hash,
        timestamp: now.to_rfc3339(),
        value: sig_base64,
    });

    // Write final manifest
    let final_manifest = manifest.to_toml()?;
    std::fs::write(bundle_dir.join("manifest.toml"), &final_manifest)?;

    // Create .sync archive (zip)
    create_sync_archive(bundle_dir, &output_path)?;

    futures::executor::block_on(reporter.notify("".to_string()))?;
    futures::executor::block_on(reporter.notify("✅ Profile created successfully!".to_string()))?;
    futures::executor::block_on(reporter.notify("".to_string()))?;
    futures::executor::block_on(
        reporter.notify(format!("   Output:       {}", output_path.display())),
    )?;
    futures::executor::block_on(reporter.notify(format!("   DID:          {}", did)))?;
    futures::executor::block_on(reporter.notify(format!("   Display Name: {}", args.name)))?;
    if let Some(ref bio) = args.bio {
        futures::executor::block_on(reporter.notify(format!("   Bio:          {}", bio)))?;
    }
    if args.avatar.is_some() {
        futures::executor::block_on(reporter.notify("   Avatar:       ✓".to_string()))?;
    }
    futures::executor::block_on(reporter.notify("".to_string()))?;

    Ok(())
}

pub fn execute_show(
    args: ShowArgs,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let manifest = read_profile_manifest(&args.path)?;

    if args.json {
        let output = ProfileOutput {
            did: manifest.meta.created_by.clone(),
            display_name: manifest.profile.display_name.clone(),
            bio: manifest.profile.bio.clone(),
            links: manifest.profile.links.clone(),
            created_at: manifest.meta.created_at.clone(),
            is_signed: manifest.signature.is_some(),
            avatar_present: manifest.profile.avatar_hash.is_some(),
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    futures::executor::block_on(reporter.notify("".to_string()))?;
    futures::executor::block_on(reporter.notify("👤 Profile".to_string()))?;
    futures::executor::block_on(reporter.notify("".to_string()))?;
    futures::executor::block_on(
        reporter.notify(format!("   DID:          {}", manifest.meta.created_by)),
    )?;
    futures::executor::block_on(reporter.notify(format!(
        "   Display Name: {}",
        manifest.profile.display_name
    )))?;
    if let Some(ref bio) = manifest.profile.bio {
        futures::executor::block_on(reporter.notify(format!("   Bio:          {}", bio)))?;
    }
    if manifest.profile.avatar_hash.is_some() {
        futures::executor::block_on(reporter.notify("   Avatar:       ✓".to_string()))?;
    }
    if let Some(ref links) = manifest.profile.links {
        futures::executor::block_on(reporter.notify("   Links:".to_string()))?;
        for (k, v) in links {
            futures::executor::block_on(reporter.notify(format!("     {}: {}", k, v)))?;
        }
    }
    futures::executor::block_on(
        reporter.notify(format!("   Created:      {}", manifest.meta.created_at)),
    )?;
    if manifest.signature.is_some() {
        futures::executor::block_on(reporter.notify("   Signed:       ✅".to_string()))?;
    } else {
        futures::executor::block_on(reporter.notify("   Signed:       ❌".to_string()))?;
    }
    futures::executor::block_on(reporter.notify("".to_string()))?;

    Ok(())
}

fn create_sync_archive(source_dir: &std::path::Path, output: &PathBuf) -> Result<()> {
    let file = std::fs::File::create(output)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for entry in walkdir::WalkDir::new(source_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let rel_path = path.strip_prefix(source_dir)?;

        if path.is_file() {
            zip.start_file(rel_path.to_string_lossy(), options)?;
            let mut f = std::fs::File::open(path)?;
            let mut buffer = Vec::new();
            f.read_to_end(&mut buffer)?;
            zip.write_all(&buffer)?;
        } else if path.is_dir() && rel_path.to_string_lossy() != "" {
            zip.add_directory(rel_path.to_string_lossy(), options)?;
        }
    }

    zip.finish()?;
    Ok(())
}

fn read_profile_manifest(path: &PathBuf) -> Result<ProfileManifest> {
    let file =
        std::fs::File::open(path).with_context(|| format!("Failed to open: {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)?;

    let mut manifest_file = archive
        .by_name("manifest.toml")
        .with_context(|| "manifest.toml not found in archive")?;

    let mut content = String::new();
    manifest_file.read_to_string(&mut content)?;

    ProfileManifest::from_toml(&content).with_context(|| "Failed to parse manifest.toml")
}

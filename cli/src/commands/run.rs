//! Run command - deploy signed capsule in production mode

use anyhow::{Context, Result};
use capsuled::capsule_types::capsule_v1::CapsuleManifestV1;
use std::fs;
use std::path::PathBuf;

use crate::engine_client::{resolve_engine_url, CapsuleEngineClient};

/// Arguments for the run command
pub struct RunArgs {
    pub capsule_path: PathBuf,
    pub engine_url: Option<String>,
}

/// Deploy a signed capsule to the engine
pub async fn execute(args: RunArgs) -> Result<()> {
    println!("▶️  Running capsule...\n");

    // 1. Load .capsule file
    if !args.capsule_path.exists() {
        anyhow::bail!("Capsule file not found: {}", args.capsule_path.display());
    }

    println!("📄 Loading: {}", args.capsule_path.display());
    let capsule_content =
        fs::read_to_string(&args.capsule_path).context("Failed to read capsule file")?;
    let manifest: CapsuleManifestV1 =
        serde_json::from_str(&capsule_content).context("Failed to parse capsule manifest")?;

    println!("   Name: {} v{}", manifest.name, manifest.version);

    // 2. Look for signature file
    let sig_path = args.capsule_path.with_extension("sig");
    let signature = if sig_path.exists() {
        let sig_bytes = fs::read(&sig_path).context("Failed to read signature file")?;
        if sig_bytes.len() != 64 {
            anyhow::bail!(
                "Invalid signature file: expected 64 bytes, got {}",
                sig_bytes.len()
            );
        }
        println!("   ✓ Signature found: {}", sig_path.display());
        Some(sig_bytes)
    } else {
        println!("   ⚠ No signature file (running unsigned)");
        None
    };

    // 3. Connect to engine
    let engine_url = resolve_engine_url(args.engine_url.as_deref());
    println!("\n🔌 Connecting to engine: {}", engine_url);

    let mut client = CapsuleEngineClient::connect(&engine_url)
        .await
        .context("Failed to connect to engine. Is capsuled running?")?;
    println!("   ✓ Connected");

    // 4. Deploy
    let capsule_id = manifest.name.clone();
    println!("\n🚀 Deploying...");

    let response = client
        .deploy_capsule(&capsule_id, &manifest, signature.as_deref())
        .await
        .context("Deploy failed")?;

    if !response.failure_codes.is_empty() || !response.failure_message.is_empty() {
        println!("   ❌ Deploy failed: {}", response.failure_message);
        for code in &response.failure_codes {
            println!("      - {}", code);
        }
        anyhow::bail!("Deployment failed");
    }

    println!("   ✓ Deployed: {}", response.capsule_id);
    println!("   Status: {}", response.status);
    if !response.local_url.is_empty() {
        println!("   🌐 URL: {}", response.local_url);
    }

    println!("\n✅ Capsule is running.");
    println!("   Use 'capsule logs {}' to view logs", response.capsule_id);
    println!("   Use 'capsule stop {}' to stop", response.capsule_id);

    Ok(())
}

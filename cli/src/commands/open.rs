//! Open command - unified entry point for running Capsules
//!
//! Replaces both `dev` and `run` commands:
//! - `capsule open` - Open in production mode (requires signed .capsule)
//! - `capsule open --dev` - Open in development mode (auto-pack, hot reload)

use anyhow::{Context, Result};
use capsuled::capsule_types::capsule_v1::{SourceTarget as ManifestSourceTarget, TargetsConfig};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tokio::time::{sleep, Duration};

use crate::engine_client::{resolve_engine_url, CapsuleEngineClient};
use super::pack::pack_in_memory;

/// Arguments for the open command
pub struct OpenArgs {
    /// Path to capsule.toml or .capsule file
    pub path: Option<PathBuf>,
    /// Development mode (hot reload, loose security)
    pub dev: bool,
    /// Engine gRPC URL
    pub engine_url: Option<String>,
}

/// Open and run a Capsule
pub async fn execute(args: OpenArgs) -> Result<()> {
    if args.dev {
        execute_dev_mode(args).await
    } else {
        execute_prod_mode(args).await
    }
}

/// Development mode: auto-pack and run with dev_mode: true
async fn execute_dev_mode(args: OpenArgs) -> Result<()> {
    println!("🚀 Opening in development mode...\n");

    // 1. Locate capsule.toml
    let manifest_path = args.path.unwrap_or_else(|| PathBuf::from("capsule.toml"));
    if !manifest_path.exists() {
        anyhow::bail!(
            "Manifest not found: {}\n\
            Run 'capsule init' to create one, or specify a path with 'capsule open --dev <path>'",
            manifest_path.display()
        );
    }

    let manifest_path = manifest_path.canonicalize()
        .context("Failed to resolve manifest path")?;
    println!("📄 Manifest: {}", manifest_path.display());

    // 2. Pack capsule in-memory
    println!("📦 Packing...");
    let pack_result = pack_in_memory(&manifest_path)?;
    let capsule_id = format!("{}-dev", pack_result.manifest.name);
    
    // Modify manifest for dev mode: set targets.source.dev_mode = true
    // If targets.source doesn't exist, create it from execution section
    let mut manifest = pack_result.manifest.clone();
    
    if manifest.targets.is_none() {
        // Create targets from legacy execution section
        // Infer language from entrypoint extension
        let entrypoint = &manifest.execution.entrypoint;
        let language = if entrypoint.ends_with(".py") {
            "python"
        } else if entrypoint.ends_with(".js") || entrypoint.ends_with(".ts") {
            "node"
        } else if entrypoint.ends_with(".rb") {
            "ruby"
        } else {
            "python" // Default
        };
        
        let source_target = ManifestSourceTarget {
            language: language.to_string(),
            version: None,
            entrypoint: entrypoint.clone(),
            dependencies: None,
            args: vec![],
            dev_mode: true, // Dev mode enabled
        };
        manifest.targets = Some(TargetsConfig {
            preference: vec!["source".to_string()],
            source: Some(source_target),
            ..Default::default()
        });
    } else if let Some(ref mut targets) = manifest.targets {
        if let Some(ref mut source) = targets.source {
            source.dev_mode = true;
        }
    }
    
    println!("   Name: {} v{}", manifest.name, manifest.version);
    if let Some(ref digest) = pack_result.source_digest {
        println!("   Digest: {}", &digest[..32.min(digest.len())]);
    }

    // 3. Resolve engine URL
    let engine_url = resolve_engine_url(args.engine_url.as_deref());
    println!("\n🔌 Connecting to engine: {}", engine_url);

    // 4. Try to connect, auto-start daemon if needed
    let mut client = match CapsuleEngineClient::try_connect(&engine_url).await {
        Ok(c) => {
            println!("   ✓ Engine connected");
            c
        }
        Err(_) => {
            println!("   ⚠ Engine not running, attempting auto-start...");
            
            if try_start_daemon().await? {
                sleep(Duration::from_secs(2)).await;
                
                CapsuleEngineClient::connect(&engine_url)
                    .await
                    .context("Failed to connect after starting daemon")?
            } else {
                anyhow::bail!(
                    "Could not start capsuled daemon.\n\
                    Please start it manually: capsuled"
                );
            }
        }
    };

    // 5. Deploy capsule (dev mode)
    println!("\n🚀 Opening capsule...");
    
    let source_dir = pack_result.source_dir.to_string_lossy().to_string();
    let response = client
        .deploy_capsule_with_source(
            &capsule_id,
            &manifest, // Use modified manifest with dev_mode=true
            None, // No signature for dev mode
            &source_dir,
        )
        .await
        .context("Failed to open capsule")?;

    if !response.failure_codes.is_empty() || !response.failure_message.is_empty() {
        println!("   ❌ Open failed: {}", response.failure_message);
        for code in &response.failure_codes {
            println!("      - {}", code);
        }
        anyhow::bail!("Failed to open capsule");
    }

    println!("   ✓ Opened: {}", response.capsule_id);
    println!("   Status: {}", response.status);
    if !response.local_url.is_empty() {
        println!("   🌐 URL: {}", response.local_url);
    }

    println!("\n✅ Capsule is running in development mode.");
    println!("   Use 'capsule logs {}' to view logs", response.capsule_id);
    println!("   Use 'capsule close {}' to stop", response.capsule_id);

    Ok(())
}

/// Production mode: run signed .capsule file
async fn execute_prod_mode(args: OpenArgs) -> Result<()> {
    println!("▶️  Opening capsule...\n");

    // 1. Locate .capsule file or capsule.toml
    let path = args.path.unwrap_or_else(|| PathBuf::from("capsule.toml"));
    
    if !path.exists() {
        anyhow::bail!(
            "File not found: {}\n\
            Specify a .capsule file or capsule.toml",
            path.display()
        );
    }

    let is_manifest = path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e == "toml")
        .unwrap_or(false);

    let (manifest, signature) = if is_manifest {
        // Pack and look for signature
        println!("📦 Packing from manifest: {}", path.display());
        let result = pack_in_memory(&path)?;
        
        // Look for existing .sig file
        let sig_path = path.with_extension("sig");
        let sig = if sig_path.exists() {
            let sig_bytes = std::fs::read(&sig_path).context("Failed to read signature")?;
            if sig_bytes.len() != 64 {
                anyhow::bail!("Invalid signature file");
            }
            println!("   ✓ Using signature: {}", sig_path.display());
            Some(sig_bytes)
        } else {
            println!("   ⚠ No signature found (running unsigned)");
            None
        };
        
        (result.manifest, sig)
    } else {
        // Load .capsule file directly
        println!("📄 Loading: {}", path.display());
        let content = std::fs::read_to_string(&path)
            .context("Failed to read capsule file")?;
        let manifest = serde_json::from_str(&content)
            .context("Failed to parse capsule manifest")?;
        
        // Look for .sig file
        let sig_path = path.with_extension("sig");
        let sig = if sig_path.exists() {
            let sig_bytes = std::fs::read(&sig_path).context("Failed to read signature")?;
            if sig_bytes.len() != 64 {
                anyhow::bail!("Invalid signature file");
            }
            println!("   ✓ Signature found: {}", sig_path.display());
            Some(sig_bytes)
        } else {
            println!("   ⚠ No signature file (running unsigned)");
            None
        };
        
        (manifest, sig)
    };

    println!("   Name: {} v{}", manifest.name, manifest.version);

    // 2. Connect to engine
    let engine_url = resolve_engine_url(args.engine_url.as_deref());
    println!("\n🔌 Connecting to engine: {}", engine_url);

    let mut client = CapsuleEngineClient::connect(&engine_url)
        .await
        .context("Failed to connect to engine. Is capsuled running?")?;
    println!("   ✓ Connected");

    // 3. Deploy
    let capsule_id = manifest.name.clone();
    println!("\n🚀 Opening...");

    let response = client
        .deploy_capsule(&capsule_id, &manifest, signature.as_deref())
        .await
        .context("Deploy failed")?;

    if !response.failure_codes.is_empty() || !response.failure_message.is_empty() {
        println!("   ❌ Open failed: {}", response.failure_message);
        for code in &response.failure_codes {
            println!("      - {}", code);
        }
        anyhow::bail!("Failed to open capsule");
    }

    println!("   ✓ Opened: {}", response.capsule_id);
    println!("   Status: {}", response.status);
    if !response.local_url.is_empty() {
        println!("   🌐 URL: {}", response.local_url);
    }

    println!("\n✅ Capsule is running.");
    println!("   Use 'capsule logs {}' to view logs", response.capsule_id);
    println!("   Use 'capsule close {}' to stop", response.capsule_id);

    Ok(())
}

/// Try to start the capsuled daemon
async fn try_start_daemon() -> Result<bool> {
    // Check if capsuled binary exists
    let capsuled_path = which::which("capsuled");
    
    match capsuled_path {
        Ok(path) => {
            println!("   Starting daemon: {}", path.display());
            
            Command::new(&path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("Failed to spawn capsuled")?;
            
            Ok(true)
        }
        Err(_) => {
            // Try relative path
            let local_path = PathBuf::from("./target/release/capsuled");
            if local_path.exists() {
                println!("   Starting daemon: {}", local_path.display());
                
                Command::new(&local_path)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .context("Failed to spawn capsuled")?;
                
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }
}

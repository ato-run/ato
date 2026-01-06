//! Dev command - run capsule in development mode with live reload
//!
//! Launcher Mode workflow:
//! 1. Check if capsuled daemon is running (gRPC health check)
//! 2. Auto-start daemon if not running
//! 3. Pack capsule in-memory (with dev_mode: true)
//! 4. Deploy via gRPC
//! 5. Stream logs to terminal

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tokio::time::{sleep, Duration};

use crate::engine_client::{resolve_engine_url, CapsuleEngineClient};
use super::pack::pack_in_memory;

/// Arguments for the dev command
pub struct DevArgs {
    pub manifest_path: Option<PathBuf>,
    pub engine_url: Option<String>,
}

/// Run capsule in development mode
pub async fn execute(args: DevArgs) -> Result<()> {
    println!("🚀 Starting development mode...\n");

    // 1. Locate capsule.toml
    let manifest_path = args.manifest_path.unwrap_or_else(|| PathBuf::from("capsule.toml"));
    if !manifest_path.exists() {
        anyhow::bail!(
            "Manifest not found: {}\nRun 'capsule dev' from a directory with capsule.toml",
            manifest_path.display()
        );
    }

    let manifest_path = manifest_path.canonicalize()
        .context("Failed to resolve manifest path")?;
    println!("📄 Manifest: {}", manifest_path.display());

    // 2. Pack capsule in-memory
    println!("📦 Packing capsule...");
    let pack_result = pack_in_memory(&manifest_path)?;
    let capsule_id = format!("{}-dev", pack_result.manifest.name);
    
    println!("   Name: {} v{}", pack_result.manifest.name, pack_result.manifest.version);
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
            
            // Try to start capsuled daemon
            if try_start_daemon().await? {
                // Wait for daemon to be ready
                sleep(Duration::from_secs(2)).await;
                
                CapsuleEngineClient::connect(&engine_url)
                    .await
                    .context("Failed to connect after starting daemon")?
            } else {
                anyhow::bail!(
                    "Could not start capsuled daemon.\n\
                    Please start it manually: capsuled --daemon"
                );
            }
        }
    };

    // 5. Deploy capsule (dev mode)
    println!("\n🚀 Deploying capsule...");
    
    // Note: For dev mode, we pass source_working_dir so engine can access files
    let source_dir = pack_result.source_dir.to_string_lossy().to_string();
    let response = client
        .deploy_capsule_with_source(
            &capsule_id,
            &pack_result.manifest,
            None, // No signature for dev mode
            &source_dir,
        )
        .await
        .context("Failed to deploy capsule")?;

    if !response.failure_codes.is_empty() || !response.failure_message.is_empty() {
        println!("   ❌ Deploy failed: {}", response.failure_message);
        for code in &response.failure_codes {
            println!("      - {}", code);
        }
        anyhow::bail!("Deployment failed");
    }

    println!("   ✓ Deployed: {}", response.capsule_id);
    if !response.local_url.is_empty() {
        println!("   🌐 URL: {}", response.local_url);
    }

    // 6. Stream logs
    println!("\n📋 Streaming logs (Ctrl+C to stop)...\n");
    println!("────────────────────────────────────────");

    let mut log_stream = client
        .stream_logs(&capsule_id, true, 50)
        .await
        .context("Failed to start log stream")?;

    // Handle Ctrl+C gracefully
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                println!("\n────────────────────────────────────────");
                println!("⏹ Stopping capsule...");
                
                match client.stop_capsule(&capsule_id).await {
                    Ok(stop_resp) => {
                        println!("   ✓ Stopped: {} ({})", stop_resp.capsule_id, stop_resp.status);
                    }
                    Err(e) => {
                        println!("   ⚠ Stop failed: {}", e);
                    }
                }
                break;
            }
            msg = log_stream.message() => {
                match msg {
                    Ok(Some(entry)) => {
                        let prefix = match entry.source.as_str() {
                            "stderr" => "ERR",
                            _ => "OUT",
                        };
                        println!("[{}] {}", prefix, entry.line);
                    }
                    Ok(None) => {
                        println!("\n(Log stream ended)");
                        break;
                    }
                    Err(e) => {
                        println!("\n(Log stream error: {})", e);
                        break;
                    }
                }
            }
        }
    }

    println!("\n✅ Development session ended.");
    Ok(())
}

/// Try to start capsuled daemon as a background process
async fn try_start_daemon() -> Result<bool> {
    // Look for capsuled binary
    let capsuled_path = which_capsuled();
    
    if capsuled_path.is_none() {
        println!("   ⚠ capsuled binary not found in PATH");
        return Ok(false);
    }

    let path = capsuled_path.unwrap();
    println!("   Starting: {}", path.display());

    // Spawn capsuled as background process
    let result = Command::new(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match result {
        Ok(_child) => {
            println!("   ✓ Daemon started");
            Ok(true)
        }
        Err(e) => {
            println!("   ✗ Failed to start daemon: {}", e);
            Ok(false)
        }
    }
}

/// Find capsuled binary
fn which_capsuled() -> Option<PathBuf> {
    // Check common locations
    let candidates = [
        PathBuf::from("capsuled"),
        PathBuf::from("./target/debug/capsuled"),
        PathBuf::from("./target/release/capsuled"),
        PathBuf::from("../target/debug/capsuled"),
        PathBuf::from("../target/release/capsuled"),
    ];

    for path in candidates {
        if path.exists() {
            return Some(path);
        }
    }

    // Try which command
    if let Ok(output) = std::process::Command::new("which")
        .arg("capsuled")
        .output()
    {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout);
            let path = PathBuf::from(path_str.trim());
            if path.exists() {
                return Some(path);
            }
        }
    }

    None
}

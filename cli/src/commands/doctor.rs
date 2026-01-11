//! Doctor command - diagnostic tool for system health checks

use anyhow::Result;
use std::process::Command;

use crate::engine_client::{resolve_engine_url, CapsuleEngineClient};

/// Arguments for the doctor command
pub struct DoctorArgs {
    pub verbose: bool,
}

/// Run system diagnostics (async version)
pub async fn execute(args: DoctorArgs) -> Result<()> {
    println!("🔍 Running system diagnostics...\n");

    let mut all_ok = true;

    // 1. Check capsuled daemon
    println!("🔌 Engine Connection:");
    let engine_url = resolve_engine_url(None);
    match CapsuleEngineClient::try_connect(&engine_url).await {
        Ok(mut client) => match client.get_system_status().await {
            Ok(status) => {
                println!("   ✅ Connected to {}", engine_url);
                println!("      Backend: {}", status.backend_mode);
                if !status.vpn_ip.is_empty() {
                    println!("      VPN IP: {}", status.vpn_ip);
                }
                println!("      Running capsules: {}", status.capsules.len());
                if args.verbose {
                    for capsule in &status.capsules {
                        println!("        - {} ({})", capsule.name, capsule.status);
                    }
                }
            }
            Err(e) => {
                println!("   ⚠️  Connected but status check failed: {}", e);
                all_ok = false;
            }
        },
        Err(_) => {
            println!("   ❌ Engine not reachable at {}", engine_url);
            println!("      Note: Engine mode is legacy. 'nacelle dev' does not require it.");
            all_ok = false;
        }
    }

    // 2. Check keys directory
    println!("\n🔑 Key Storage:");
    let keys_dir = dirs::home_dir()
        .map(|h| h.join(".capsule").join("keys"))
        .unwrap_or_default();
    if keys_dir.exists() {
        let key_count = std::fs::read_dir(&keys_dir)
            .map(|entries| entries.filter(|e| e.is_ok()).count() / 2) // .secret + .public pairs
            .unwrap_or(0);
        println!("   ✅ Keys directory: {}", keys_dir.display());
        println!("      Keys: {}", key_count);
    } else {
        println!("   ⚠️  Keys directory not found: {}", keys_dir.display());
        println!("      Create keys with: nacelle keygen");
    }

    // 3. Check Python
    println!("\n🐍 Python:");
    check_command("python3", &["--version"], args.verbose);

    // 4. Check Node.js
    println!("\n📦 Node.js:");
    check_command("node", &["--version"], args.verbose);

    // 5. Check Docker
    println!("\n🐳 Docker:");
    check_command("docker", &["--version"], args.verbose);

    // 6. Check GPU (macOS MPS or CUDA)
    println!("\n🎮 GPU:");
    #[cfg(target_os = "macos")]
    {
        // Check for Apple Silicon MPS
        let output = Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output();
        match output {
            Ok(o) if o.status.success() => {
                let cpu = String::from_utf8_lossy(&o.stdout);
                if cpu.contains("Apple") {
                    println!("   ✅ Apple Silicon detected (MPS available)");
                } else {
                    println!("   ⚠️  Intel Mac (no GPU acceleration)");
                }
            }
            _ => println!("   ❓ Could not detect GPU"),
        }
    }
    #[cfg(target_os = "linux")]
    {
        // Check for NVIDIA CUDA
        check_command(
            "nvidia-smi",
            &["--query-gpu=name", "--format=csv,noheader"],
            args.verbose,
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        println!("   ⚠️  GPU detection not implemented for this platform");
    }

    // Summary
    println!("\n────────────────────────────────────────");
    if all_ok {
        println!("✅ All systems operational!");
    } else {
        println!("⚠️  Some issues detected. See above for details.");
    }

    Ok(())
}

fn check_command(cmd: &str, args: &[&str], verbose: bool) {
    match Command::new(cmd).args(args).output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            let version = version.trim();
            println!("   ✅ {}", version);
            if verbose {
                println!(
                    "      Path: {}",
                    which(cmd).unwrap_or_else(|| "unknown".to_string())
                );
            }
        }
        Ok(_) => {
            println!("   ❌ {} found but returned error", cmd);
        }
        Err(_) => {
            println!("   ❌ {} not found", cmd);
        }
    }
}

fn which(cmd: &str) -> Option<String> {
    Command::new("which")
        .arg(cmd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

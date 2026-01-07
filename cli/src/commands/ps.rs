//! Ps command - list running Capsules
//!
//! Shows all currently open Capsules with their status.

use anyhow::{Context, Result};

use crate::engine_client::{resolve_engine_url, CapsuleEngineClient};

/// Arguments for the ps command
pub struct PsArgs {
    /// Show all (including stopped) Capsules
    pub all: bool,
    /// Engine gRPC URL
    pub engine_url: Option<String>,
}

/// List running Capsules
pub async fn execute(args: PsArgs) -> Result<()> {
    // Connect to engine
    let engine_url = resolve_engine_url(args.engine_url.as_deref());
    let mut client = CapsuleEngineClient::connect(&engine_url)
        .await
        .context("Failed to connect to engine. Is capsuled running?")?;

    // Get system status (includes capsule list)
    let status = client
        .get_system_status()
        .await
        .context("Failed to get system status")?;

    if status.capsules.is_empty() {
        println!("No capsules running.");
        println!("\nRun 'capsule open --dev' to start a capsule.");
        return Ok(());
    }

    // Print header
    println!("{:<24} {:<12} {}", "ID", "STATUS", "URL");
    println!("{}", "-".repeat(50));

    // Print each capsule
    for capsule in &status.capsules {
        // Filter stopped if not --all
        if !args.all && capsule.status.to_lowercase() == "stopped" {
            continue;
        }

        let url = if capsule.local_url.is_empty() {
            "-".to_string()
        } else {
            capsule.local_url.clone()
        };

        let status_display = match capsule.status.to_lowercase().as_str() {
            "running" => "✅ running",
            "starting" => "🔄 starting",
            "stopped" => "⏹️  stopped",
            "failed" => "❌ failed",
            _ => &capsule.status,
        };

        println!(
            "{:<24} {:<12} {}",
            truncate(&capsule.name, 24),
            status_display,
            url
        );
    }

    let running_count = status
        .capsules
        .iter()
        .filter(|c| c.status.to_lowercase() == "running")
        .count();

    println!("\n{} capsule(s) running", running_count);

    Ok(())
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len - 1])
    }
}

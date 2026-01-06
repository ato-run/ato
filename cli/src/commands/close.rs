//! Close command - stop a running Capsule
//!
//! Replaces the `stop` command with cleaner naming.

use anyhow::{Context, Result};

use crate::engine_client::{resolve_engine_url, CapsuleEngineClient};

/// Arguments for the close command
pub struct CloseArgs {
    /// Capsule ID to close
    pub capsule_id: String,
    /// Engine gRPC URL
    pub engine_url: Option<String>,
}

/// Close a running Capsule
pub async fn execute(args: CloseArgs) -> Result<()> {
    println!("⏹️  Closing capsule: {}\n", args.capsule_id);

    // Connect to engine
    let engine_url = resolve_engine_url(args.engine_url.as_deref());
    let mut client = CapsuleEngineClient::connect(&engine_url)
        .await
        .context("Failed to connect to engine")?;

    // Send stop request
    let response = client
        .stop_capsule(&args.capsule_id)
        .await
        .context("Close request failed")?;

    println!("✅ Closed: {} ({})", response.capsule_id, response.status);

    Ok(())
}

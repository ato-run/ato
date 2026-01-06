//! Stop command - stop a running capsule

use anyhow::{Context, Result};

use crate::engine_client::{resolve_engine_url, CapsuleEngineClient};

/// Arguments for the stop command
pub struct StopArgs {
    pub capsule_id: String,
    pub engine_url: Option<String>,
}

/// Stop a running capsule
pub async fn execute(args: StopArgs) -> Result<()> {
    println!("⏹️  Stopping capsule: {}\n", args.capsule_id);

    // Connect to engine
    let engine_url = resolve_engine_url(args.engine_url.as_deref());
    let mut client = CapsuleEngineClient::connect(&engine_url)
        .await
        .context("Failed to connect to engine")?;

    // Send stop request
    let response = client
        .stop_capsule(&args.capsule_id)
        .await
        .context("Stop request failed")?;

    println!("✅ Stopped: {} ({})", response.capsule_id, response.status);

    Ok(())
}

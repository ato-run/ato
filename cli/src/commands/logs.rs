//! Logs command - stream logs from a running capsule

use anyhow::{Context, Result};

use crate::engine_client::{resolve_engine_url, CapsuleEngineClient};

/// Arguments for the logs command
pub struct LogsArgs {
    pub capsule_id: String,
    pub follow: bool,
    pub engine_url: Option<String>,
}

/// Stream logs from a running capsule
pub async fn execute(args: LogsArgs) -> Result<()> {
    println!("📋 Fetching logs for: {}\n", args.capsule_id);

    // Connect to engine
    let engine_url = resolve_engine_url(args.engine_url.as_deref());
    let mut client = CapsuleEngineClient::connect(&engine_url)
        .await
        .context("Failed to connect to engine")?;

    // Stream logs
    let tail_lines = if args.follow { 50 } else { 100 };
    let mut stream = client
        .stream_logs(&args.capsule_id, args.follow, tail_lines)
        .await
        .context("Failed to stream logs")?;

    println!("────────────────────────────────────────");

    if args.follow {
        // Handle Ctrl+C for follow mode
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::pin!(ctrl_c);

        loop {
            tokio::select! {
                _ = &mut ctrl_c => {
                    println!("\n────────────────────────────────────────");
                    println!("(Stopped following)");
                    break;
                }
                msg = stream.message() => {
                    match msg {
                        Ok(Some(entry)) => {
                            let prefix = match entry.source.as_str() {
                                "stderr" => "ERR",
                                _ => "OUT",
                            };
                            println!("[{}] {}", prefix, entry.line);
                        }
                        Ok(None) => {
                            println!("\n(Stream ended)");
                            break;
                        }
                        Err(e) => {
                            println!("\n(Stream error: {})", e);
                            break;
                        }
                    }
                }
            }
        }
    } else {
        // One-shot mode: just print what we get
        while let Some(entry) = stream.message().await? {
            let prefix = match entry.source.as_str() {
                "stderr" => "ERR",
                _ => "OUT",
            };
            println!("[{}] {}", prefix, entry.line);
        }
        println!("────────────────────────────────────────");
    }

    Ok(())
}

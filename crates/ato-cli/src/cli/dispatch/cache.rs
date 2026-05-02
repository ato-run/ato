use std::io::{self, IsTerminal, Write};

use anyhow::{Context, Result};

use crate::application::cache_admin;
use crate::cli::cache::CacheCommands;

pub(crate) fn execute_cache_command(command: CacheCommands) -> Result<()> {
    match command {
        CacheCommands::Stats { pretty } => stats_command(pretty),
        CacheCommands::Clear { yes, derivation } => clear_command(yes, derivation),
    }
}

fn stats_command(pretty: bool) -> Result<()> {
    let stats = cache_admin::collect_cache_stats().context("failed to collect cache stats")?;
    let rendered = if pretty {
        serde_json::to_string_pretty(&stats)?
    } else {
        serde_json::to_string(&stats)?
    };
    println!("{rendered}");
    Ok(())
}

fn clear_command(yes: bool, derivation: Option<String>) -> Result<()> {
    if let Some(hash) = derivation {
        let outcome = cache_admin::clear_derivation(&hash)
            .with_context(|| format!("failed to clear derivation {hash}"))?;
        let payload = serde_json::json!({
            "scope": "derivation",
            "derivation_hash": hash,
            "blobs_removed": outcome.blobs_removed,
            "refs_removed": outcome.refs_removed,
            "meta_files_removed": outcome.meta_files_removed,
            "bytes_freed": outcome.bytes_freed,
            "skipped_referenced": outcome.skipped_referenced,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if !yes {
        let stats = cache_admin::collect_cache_stats()?;
        eprintln!(
            "About to clear A1 dependency cache: {} blobs ({} bytes) and {} refs.",
            stats.blob_count, stats.total_bytes, stats.ref_count
        );
        if !confirm("Proceed? [y/N] ")? {
            anyhow::bail!("aborted by user");
        }
    }

    let outcome = cache_admin::clear_all().context("failed to clear cache")?;
    let payload = serde_json::json!({
        "scope": "all",
        "blobs_removed": outcome.blobs_removed,
        "refs_removed": outcome.refs_removed,
        "meta_files_removed": outcome.meta_files_removed,
        "bytes_freed": outcome.bytes_freed,
        "skipped_referenced": outcome.skipped_referenced,
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    if !io::stdin().is_terminal() {
        anyhow::bail!(
            "stdin is not a terminal; pass --yes to acknowledge the destructive operation"
        );
    }
    eprint!("{prompt}");
    io::stderr().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(matches!(buf.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

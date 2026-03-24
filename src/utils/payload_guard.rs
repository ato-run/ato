use std::path::Path;

use anyhow::{bail, Context, Result};

pub const DEFAULT_MAX_PAYLOAD_BYTES: u64 = 200 * 1024 * 1024;

pub fn ensure_payload_size(path: &Path, force_large_payload: bool, hint_flag: &str) -> Result<()> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("Failed to stat payload: {}", path.display()))?
        .len();
    ensure_payload_bytes_size(size, force_large_payload, hint_flag)
}

pub fn ensure_payload_bytes_size(
    size: u64,
    force_large_payload: bool,
    hint_flag: &str,
) -> Result<()> {
    if size <= DEFAULT_MAX_PAYLOAD_BYTES {
        return Ok(());
    }

    if force_large_payload {
        eprintln!(
            "⚠️  Large payload allowed by {}: {} bytes ({:.1} MB) > {:.1} MB",
            hint_flag,
            size,
            size as f64 / (1024.0 * 1024.0),
            DEFAULT_MAX_PAYLOAD_BYTES as f64 / (1024.0 * 1024.0)
        );
        return Ok(());
    }

    bail!(
        "Capsule payload is unusually large: {} bytes ({:.1} MB), threshold is {:.1} MB. Use {} to override intentionally.",
        size,
        size as f64 / (1024.0 * 1024.0),
        DEFAULT_MAX_PAYLOAD_BYTES as f64 / (1024.0 * 1024.0),
        hint_flag
    )
}

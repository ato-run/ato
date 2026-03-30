use std::path::Path;

use anyhow::{bail, Context, Result};

pub const DEFAULT_MAX_PAYLOAD_BYTES: u64 = 200 * 1024 * 1024;
pub const PAID_MAX_PAYLOAD_BYTES: u64 = 1024 * 1024 * 1024;

pub fn ensure_payload_size(
    path: &Path,
    force_large_payload: bool,
    paid_large_payload: bool,
    hint_flag: &str,
) -> Result<()> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("Failed to stat payload: {}", path.display()))?
        .len();
    ensure_payload_bytes_size(size, force_large_payload, paid_large_payload, hint_flag)
}

pub fn ensure_payload_bytes_size(
    size: u64,
    force_large_payload: bool,
    paid_large_payload: bool,
    hint_flag: &str,
) -> Result<()> {
    if size <= DEFAULT_MAX_PAYLOAD_BYTES {
        return Ok(());
    }

    if size <= PAID_MAX_PAYLOAD_BYTES && paid_large_payload {
        eprintln!(
            "⚠️  Paid-plan large payload allowed by --paid-large-payload: {} bytes ({:.1} MB) > {:.1} MB and <= {:.1} MB",
            size,
            size as f64 / (1024.0 * 1024.0),
            DEFAULT_MAX_PAYLOAD_BYTES as f64 / (1024.0 * 1024.0),
            PAID_MAX_PAYLOAD_BYTES as f64 / (1024.0 * 1024.0)
        );
        return Ok(());
    }

    if force_large_payload {
        eprintln!(
            "⚠️  Large payload allowed by {}: {} bytes ({:.1} MB) > {:.1} MB",
            hint_flag,
            size,
            size as f64 / (1024.0 * 1024.0),
            if paid_large_payload {
                PAID_MAX_PAYLOAD_BYTES as f64 / (1024.0 * 1024.0)
            } else {
                DEFAULT_MAX_PAYLOAD_BYTES as f64 / (1024.0 * 1024.0)
            }
        );
        return Ok(());
    }

    if size <= PAID_MAX_PAYLOAD_BYTES {
        bail!(
            "Capsule payload is unusually large: {} bytes ({:.1} MB), threshold is {:.1} MB. Use --paid-large-payload for paid-plan payloads up to {:.1} MB, or {} to override intentionally.",
            size,
            size as f64 / (1024.0 * 1024.0),
            DEFAULT_MAX_PAYLOAD_BYTES as f64 / (1024.0 * 1024.0),
            PAID_MAX_PAYLOAD_BYTES as f64 / (1024.0 * 1024.0),
            hint_flag
        )
    }

    bail!(
        "Capsule payload is unusually large: {} bytes ({:.1} MB), paid-plan threshold is {:.1} MB. Use {} to override intentionally.",
        size,
        size as f64 / (1024.0 * 1024.0),
        PAID_MAX_PAYLOAD_BYTES as f64 / (1024.0 * 1024.0),
        hint_flag
    )
}

#[cfg(test)]
mod tests {
    use super::{ensure_payload_bytes_size, DEFAULT_MAX_PAYLOAD_BYTES, PAID_MAX_PAYLOAD_BYTES};

    #[test]
    fn payload_guard_rejects_paid_sized_payload_without_paid_flag() {
        let err =
            ensure_payload_bytes_size(750 * 1024 * 1024, false, false, "--force-large-payload")
                .expect_err("must require paid flag");

        assert!(err.to_string().contains("--paid-large-payload"));
        assert!(err.to_string().contains("--force-large-payload"));
    }

    #[test]
    fn payload_guard_allows_paid_sized_payload_with_paid_flag() {
        ensure_payload_bytes_size(750 * 1024 * 1024, false, true, "--force-large-payload")
            .expect("paid-sized payload should pass");
    }

    #[test]
    fn payload_guard_rejects_over_paid_threshold_without_force() {
        let err = ensure_payload_bytes_size(
            PAID_MAX_PAYLOAD_BYTES + 1,
            false,
            true,
            "--force-large-payload",
        )
        .expect_err("must require force override");

        assert!(err
            .to_string()
            .contains(&(PAID_MAX_PAYLOAD_BYTES as f64 / (1024.0 * 1024.0)).to_string()));
    }

    #[test]
    fn payload_guard_keeps_default_threshold_without_flags() {
        ensure_payload_bytes_size(
            DEFAULT_MAX_PAYLOAD_BYTES,
            false,
            false,
            "--force-large-payload",
        )
        .expect("default-sized payload should pass");
    }
}

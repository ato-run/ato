use std::path::Path;

use anyhow::{bail, Context, Result};

pub const DEFAULT_MAX_PAYLOAD_BYTES: u64 = 200 * 1024 * 1024;

pub fn ensure_payload_size(path: &Path, force_large_payload: bool, hint_flag: &str) -> Result<()> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("Failed to stat payload: {}", path.display()))?
        .len();
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
        "Capsule payload is unusually large: {} bytes ({:.1} MB), threshold is {:.1} MB. \
Use {} to override intentionally.",
        size,
        size as f64 / (1024.0 * 1024.0),
        DEFAULT_MAX_PAYLOAD_BYTES as f64 / (1024.0 * 1024.0),
        hint_flag
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn accepts_small_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("small.capsule");
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(b"small").expect("write");
        ensure_payload_size(&path, false, "--force-large-payload").expect("should pass");
    }

    #[test]
    fn rejects_large_payload_without_force() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("large.capsule");
        let f = std::fs::File::create(&path).expect("create");
        f.set_len(DEFAULT_MAX_PAYLOAD_BYTES + 1).expect("set_len");
        let err =
            ensure_payload_size(&path, false, "--force-large-payload").expect_err("must fail");
        assert!(err
            .to_string()
            .contains("Capsule payload is unusually large"));
    }

    #[test]
    fn allows_large_payload_with_force() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("large.capsule");
        let f = std::fs::File::create(&path).expect("create");
        f.set_len(DEFAULT_MAX_PAYLOAD_BYTES + 1).expect("set_len");
        ensure_payload_size(&path, true, "--force-large-payload").expect("should pass");
    }
}

//! Spawn-time size guard + rotation for `ExecuteMode::Logged` guest logs.
//!
//! Both the source executor and the shell executor wire a child's
//! stdout+stderr straight into a log file at `Command::spawn` time via
//! `Stdio::from(File)` (so the redirection survives the parent's exit). That
//! file is opened with `create(true).append(true)` and grew unbounded across
//! (re)spawns — #767.
//!
//! Before opening the log for append we check its current size. If it already
//! exceeds [`LOG_ROTATE_THRESHOLD_BYTES`], we rotate: the current file becomes
//! `<log>.1`, the previous `<log>.1` becomes `<log>.2`, and so on up to
//! [`LOG_ROTATE_MAX_GENERATIONS`]; the oldest generation beyond that is
//! deleted. A fresh empty file is then opened for the new (re)spawn.
//!
//! ## Known limitation (#767 residual)
//! This bounds the log *across (re)spawns* only. A single long-running, chatty
//! process holds the kernel file descriptor for its whole lifetime and keeps
//! appending to the same inode, so its log can still grow *within one run* —
//! spawn-time rotation cannot interpose on an already-open fd without a proxy
//! writer thread, which the older `Piped` pattern proved drops output once the
//! parent exits. The TTL/size sweep (see `capsule::state::session::sweep`)
//! reclaims old generations after the fact; bounding within a single run is a
//! deliberate follow-up, not stubbed here.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::debug;

/// Rotate a guest log once it reaches this many bytes (50 MiB).
pub const LOG_ROTATE_THRESHOLD_BYTES: u64 = 50 * 1024 * 1024;

/// How many rotated generations (`<log>.1` .. `<log>.N`) to keep. The live
/// file plus this many archives bound the total retained per log to roughly
/// `(N + 1) * LOG_ROTATE_THRESHOLD_BYTES`.
pub const LOG_ROTATE_MAX_GENERATIONS: u32 = 3;

/// `true` if `log_path` exists and is at least `threshold` bytes, i.e. it
/// should be rotated before the next append. A missing file, or any metadata
/// error, returns `false` (nothing to rotate).
pub fn should_rotate(log_path: &Path, threshold: u64) -> bool {
    match log_path.metadata() {
        Ok(metadata) => metadata.is_file() && metadata.len() >= threshold,
        Err(_) => false,
    }
}

/// The rotated path for generation `n` (1-based): `<log>.1`, `<log>.2`, ...
fn generation_path(log_path: &Path, n: u32) -> PathBuf {
    let mut name = log_path.as_os_str().to_owned();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// Rotate `log_path` if it has grown to or beyond `threshold`, keeping at most
/// `max_generations` archives. Shifts `<log>.(N-1)` -> `<log>.N` (deleting the
/// oldest), then `<log>` -> `<log>.1`, leaving `<log>` absent so the caller's
/// `create(true).append(true)` open starts a fresh file.
///
/// Best-effort and idempotent: if `log_path` is below threshold or missing this
/// is a no-op. Returns whether a rotation occurred.
pub fn rotate_if_needed(log_path: &Path, threshold: u64, max_generations: u32) -> Result<bool> {
    if max_generations == 0 || !should_rotate(log_path, threshold) {
        return Ok(false);
    }

    // Drop the oldest generation so the shift below doesn't exceed the cap.
    let oldest = generation_path(log_path, max_generations);
    if oldest.exists() {
        if let Err(error) = std::fs::remove_file(&oldest) {
            if error.kind() != std::io::ErrorKind::NotFound {
                debug!(path = %oldest.display(), error = %error, "failed to remove oldest rotated log");
            }
        }
    }

    // Shift remaining generations up: .（N-1) -> .N, ..., .1 -> .2.
    for n in (1..max_generations).rev() {
        let from = generation_path(log_path, n);
        let to = generation_path(log_path, n + 1);
        if from.exists() {
            if let Err(error) = std::fs::rename(&from, &to) {
                debug!(from = %from.display(), to = %to.display(), error = %error, "failed to shift rotated log generation");
            }
        }
    }

    // Move the live log to generation .1, freeing `log_path` for a fresh file.
    let first = generation_path(log_path, 1);
    std::fs::rename(log_path, &first).with_context(|| {
        format!(
            "failed to rotate log {} -> {}",
            log_path.display(),
            first.display()
        )
    })?;
    Ok(true)
}

/// Rotate `log_path` using the crate defaults before it is (re)opened for
/// append at spawn time. Best-effort: a rotation failure is logged and the
/// caller proceeds to append to the existing file rather than failing the
/// launch outright.
pub fn rotate_before_append(log_path: &Path) {
    match rotate_if_needed(
        log_path,
        LOG_ROTATE_THRESHOLD_BYTES,
        LOG_ROTATE_MAX_GENERATIONS,
    ) {
        Ok(true) => {
            debug!(path = %log_path.display(), "rotated oversized guest log before append");
        }
        Ok(false) => {}
        Err(error) => {
            debug!(path = %log_path.display(), error = %error, "guest log rotation failed; appending to existing file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_sized(path: &Path, len: usize) {
        fs::write(path, vec![b'x'; len]).expect("write log");
    }

    #[test]
    fn should_rotate_respects_threshold() {
        let temp = tempdir().expect("tempdir");
        let log = temp.path().join("app.log");
        // Missing file: nothing to rotate.
        assert!(!should_rotate(&log, 100));
        write_sized(&log, 50);
        assert!(!should_rotate(&log, 100));
        write_sized(&log, 100);
        assert!(should_rotate(&log, 100));
        write_sized(&log, 250);
        assert!(should_rotate(&log, 100));
    }

    #[test]
    fn rotate_below_threshold_is_noop() {
        let temp = tempdir().expect("tempdir");
        let log = temp.path().join("app.log");
        write_sized(&log, 10);
        assert!(!rotate_if_needed(&log, 100, 3).expect("rotate"));
        assert!(log.exists());
        assert!(!generation_path(&log, 1).exists());
    }

    #[test]
    fn rotate_moves_live_to_generation_one() {
        let temp = tempdir().expect("tempdir");
        let log = temp.path().join("app.log");
        write_sized(&log, 200);
        assert!(rotate_if_needed(&log, 100, 3).expect("rotate"));
        // Live file is gone (freed for a fresh open) and content is in .1.
        assert!(!log.exists());
        let first = generation_path(&log, 1);
        assert!(first.exists());
        assert_eq!(fs::metadata(&first).expect("meta").len(), 200);
    }

    #[test]
    fn rotate_prunes_oldest_generation() {
        let temp = tempdir().expect("tempdir");
        let log = temp.path().join("app.log");
        let max = 2u32;
        // Seed existing generations so we can observe the shift + prune.
        write_sized(&log, 200); // live, will become .1
        write_sized(&generation_path(&log, 1), 11); // -> .2
        write_sized(&generation_path(&log, 2), 22); // oldest, must be deleted

        assert!(rotate_if_needed(&log, 100, max).expect("rotate"));

        // Cap honored: no generation beyond `max`.
        assert!(!generation_path(&log, max + 1).exists());
        // .1 is the former live file (200 bytes).
        assert_eq!(
            fs::metadata(generation_path(&log, 1))
                .expect("meta .1")
                .len(),
            200
        );
        // .2 is the former .1 (11 bytes); the former .2 (22) was pruned.
        assert_eq!(
            fs::metadata(generation_path(&log, 2))
                .expect("meta .2")
                .len(),
            11
        );
        assert!(!log.exists());
    }

    #[test]
    fn rotate_with_zero_generations_is_noop() {
        let temp = tempdir().expect("tempdir");
        let log = temp.path().join("app.log");
        write_sized(&log, 200);
        assert!(!rotate_if_needed(&log, 100, 0).expect("rotate"));
        assert!(log.exists());
    }
}

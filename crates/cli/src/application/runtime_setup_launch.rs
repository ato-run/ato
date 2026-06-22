//! Runtime Setup launch-intent marker (#460 PR3).
//!
//! When a capsule launch is interrupted because the host runtime needs setup
//! (or a reboot), the launching surface records *what the user was trying to
//! open* here, so that once Runtime Setup reaches `ready` the Desktop can return
//! them to that launch instead of stranding them on the setup screen.
//!
//! Layout: `~/.ato/runtime-setup/launch-intent.json`.
//!
//! Like the reboot-resume marker, this is advisory and self-healing: a missing,
//! corrupt, or stale intent is treated as "nothing to resume" and never errors.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule::runtime_setup::RuntimeSetupLaunchIntent;

/// Launch intents older than this are ignored (the user likely moved on).
pub(crate) const LAUNCH_INTENT_TTL_MS: u64 = 24 * 60 * 60 * 1000; // 24h

/// Default on-disk path for the launch-intent marker. Never falls back to /tmp.
pub(crate) fn launch_intent_path() -> PathBuf {
    capsule::common::paths::ato_path_or_workspace_tmp("runtime-setup/launch-intent.json")
}

/// Write a launch intent to `path`, creating parent directories as needed.
///
/// The write/clear/consume side of this marker API is consumed by the Desktop
/// launch-interruption path in PR3b (#460); PR3a wires only the read + decide
/// side (via `resume-after-reboot`) and exercises write/consume from tests.
#[allow(dead_code)]
pub(crate) fn write_launch_intent_at(path: &Path, intent: &RuntimeSetupLaunchIntent) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(intent).context("failed to serialize launch intent")?;
    std::fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Read a launch intent from `path`. Returns `None` when absent or corrupt.
pub(crate) fn read_launch_intent_at(path: &Path) -> Option<RuntimeSetupLaunchIntent> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remove the launch-intent marker at `path`. A missing file is success.
#[allow(dead_code)] // write/clear/consume side consumed by PR3b (#460)
pub(crate) fn clear_launch_intent_at(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

/// Whether `intent` is older than [`LAUNCH_INTENT_TTL_MS`] relative to
/// `now_unix_ms`. An intent stamped in the future is not stale.
pub(crate) fn is_intent_stale(intent: &RuntimeSetupLaunchIntent, now_unix_ms: u64) -> bool {
    now_unix_ms.saturating_sub(intent.created_at_unix_ms) > LAUNCH_INTENT_TTL_MS
}

/// Read and remove the intent at `path` in one step (consume). Returns the
/// intent only when present and not stale; clears the marker either way (a stale
/// intent is discarded). Idempotent: a second call returns `None`.
#[allow(dead_code)] // consumed by PR3b (#460)
pub(crate) fn consume_launch_intent_at(
    path: &Path,
    now_unix_ms: u64,
) -> Option<RuntimeSetupLaunchIntent> {
    let intent = read_launch_intent_at(path)?;
    let _ = clear_launch_intent_at(path);
    if is_intent_stale(&intent, now_unix_ms) {
        None
    } else {
        Some(intent)
    }
}

/// Outcome of evaluating a launch intent against current substrate readiness
/// (#460 PR3). Pure decision so the policy is unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchContinuation {
    /// No intent recorded — nothing to resume.
    None,
    /// Intent is too old; it should be discarded without launching.
    Discard,
    /// Substrate not ready yet — keep the intent and wait.
    Pending,
    /// Substrate ready — resume this launch.
    Continue(Box<RuntimeSetupLaunchIntent>),
}

/// Decide what to do with a launch intent given whether the substrate is ready.
/// Pure. See #460 PR3.
pub(crate) fn decide_launch_continuation(
    intent: Option<RuntimeSetupLaunchIntent>,
    substrate_ready: bool,
    now_unix_ms: u64,
) -> LaunchContinuation {
    let Some(intent) = intent else {
        return LaunchContinuation::None;
    };
    if is_intent_stale(&intent, now_unix_ms) {
        return LaunchContinuation::Discard;
    }
    if substrate_ready {
        LaunchContinuation::Continue(Box::new(intent))
    } else {
        LaunchContinuation::Pending
    }
}

// ── default-path convenience wrappers ─────────────────────────────────────────

/// Read the launch intent at the default path (`None` if absent/corrupt).
pub(crate) fn read_launch_intent() -> Option<RuntimeSetupLaunchIntent> {
    read_launch_intent_at(&launch_intent_path())
}

/// Consume the launch intent at the default path.
#[allow(dead_code)] // consumed by PR3b (#460)
pub(crate) fn consume_launch_intent(now_unix_ms: u64) -> Option<RuntimeSetupLaunchIntent> {
    consume_launch_intent_at(&launch_intent_path(), now_unix_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::runtime_setup::{LaunchIntentKind, LaunchIntentNextStep};

    fn sample_intent(created_at_unix_ms: u64) -> RuntimeSetupLaunchIntent {
        RuntimeSetupLaunchIntent {
            schema_version: capsule::runtime_setup::RUNTIME_SETUP_LAUNCH_INTENT_SCHEMA_VERSION,
            created_at_unix_ms,
            source_surface: "launch_flow".to_string(),
            intent_kind: LaunchIntentKind::CapsuleUrl,
            launch_input: "capsule://github.com/sosedoff/pgweb".to_string(),
            expected_next_step: LaunchIntentNextStep::ContinueLaunch,
            request_id: Some("req-1".to_string()),
            display_label: Some("pgweb".to_string()),
            requested_client: None,
        }
    }

    #[test]
    fn write_read_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("runtime-setup/launch-intent.json");
        let intent = sample_intent(1_000);
        write_launch_intent_at(&path, &intent).expect("write");
        assert_eq!(read_launch_intent_at(&path), Some(intent));
    }

    #[test]
    fn read_absent_and_corrupt_are_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope.json");
        assert_eq!(read_launch_intent_at(&missing), None);
        let corrupt = dir.path().join("c.json");
        std::fs::write(&corrupt, "{ bad").expect("write");
        assert_eq!(read_launch_intent_at(&corrupt), None);
    }

    #[test]
    fn clear_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("launch-intent.json");
        write_launch_intent_at(&path, &sample_intent(1_000)).expect("write");
        clear_launch_intent_at(&path).expect("clear");
        assert!(!path.exists());
        clear_launch_intent_at(&path).expect("clear again");
    }

    #[test]
    fn consume_returns_then_clears() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("launch-intent.json");
        write_launch_intent_at(&path, &sample_intent(1_000)).expect("write");
        let first = consume_launch_intent_at(&path, 2_000);
        assert!(first.is_some());
        assert!(!path.exists(), "consume clears the marker");
        // Idempotent: a second consume finds nothing.
        assert_eq!(consume_launch_intent_at(&path, 2_000), None);
    }

    #[test]
    fn consume_discards_stale_intent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("launch-intent.json");
        write_launch_intent_at(&path, &sample_intent(1_000)).expect("write");
        // now is past the TTL → stale → returns None and clears the file.
        let got = consume_launch_intent_at(&path, 1_000 + LAUNCH_INTENT_TTL_MS + 1);
        assert_eq!(got, None);
        assert!(!path.exists());
    }

    #[test]
    fn decide_no_intent_is_none() {
        assert_eq!(
            decide_launch_continuation(None, true, 2_000),
            LaunchContinuation::None
        );
    }

    #[test]
    fn decide_stale_is_discard_even_if_ready() {
        let intent = sample_intent(1_000);
        let now = 1_000 + LAUNCH_INTENT_TTL_MS + 1;
        assert_eq!(
            decide_launch_continuation(Some(intent), true, now),
            LaunchContinuation::Discard
        );
    }

    #[test]
    fn decide_ready_continues() {
        let intent = sample_intent(1_000);
        match decide_launch_continuation(Some(intent.clone()), true, 2_000) {
            LaunchContinuation::Continue(got) => assert_eq!(*got, intent),
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    #[test]
    fn decide_not_ready_is_pending() {
        let intent = sample_intent(1_000);
        assert_eq!(
            decide_launch_continuation(Some(intent), false, 2_000),
            LaunchContinuation::Pending
        );
    }
}

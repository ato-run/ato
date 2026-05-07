//! Session record store: where the JSON files live, how to find them,
//! how to read all of them, and how to write one **atomically** so the
//! Desktop direct-read fast path can never observe a partial record.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::common::paths::ato_path;
use tracing::{debug, warn};

use crate::record::StoredSessionInfo;

/// Environment variable that overrides the default session root. Honored
/// by both `ato-cli` (which writes records here) and `ato-desktop`
/// (which reads them) — keeping a single env name avoids the two ends
/// drifting onto different roots.
const SESSION_ROOT_ENV: &str = "ATO_DESKTOP_SESSION_ROOT";

/// Returns the directory holding all `<session_id>.json` records for
/// `ato-desktop`. Honors `ATO_DESKTOP_SESSION_ROOT`; otherwise resolves
/// to `${ATO_HOME:-~/.ato}/apps/ato-desktop/sessions/`.
pub fn session_root() -> Result<PathBuf> {
    if let Ok(path) = std::env::var(SESSION_ROOT_ENV) {
        return Ok(PathBuf::from(path));
    }
    ato_path("apps/ato-desktop/sessions").context("failed to resolve ato home for session root")
}

/// Path of a single session record file inside `root`.
pub fn session_record_path(root: &Path, session_id: &str) -> PathBuf {
    root.join(format!("{session_id}.json"))
}

/// Read every `<session_id>.json` under `root` and return the parsed
/// records. Files that fail to parse are skipped with a warn-level
/// trace — a single corrupted record must never prevent the rest of
/// the fast path from working.
pub fn read_session_records(root: &Path) -> Result<Vec<StoredSessionInfo>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    let entries = fs::read_dir(root)
        .with_context(|| format!("failed to read session root {}", root.display()))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                debug!(error = %err, "skipping unreadable session entry");
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) => {
                debug!(path = %path.display(), error = %err, "skipping unreadable session record");
                continue;
            }
        };
        match serde_json::from_str::<StoredSessionInfo>(&raw) {
            Ok(record) => records.push(record),
            Err(err) => {
                warn!(
                    path = %path.display(),
                    error = %err,
                    "skipping malformed session record"
                );
            }
        }
    }
    Ok(records)
}

/// Write `session` to `root/<session_id>.json` **atomically**: serialize
/// to a temp file in the same directory, then rename over the final
/// path. Same-filesystem rename is atomic on macOS / Linux, so a reader
/// (e.g. the Desktop fast path) can never observe a half-written
/// record.
///
/// Replaces the legacy `fs::write` write path that the App Session
/// Materialization v0 RFC §9.4 listed as a Phase 1 prerequisite.
pub fn write_session_record_atomic(root: &Path, session: &StoredSessionInfo) -> Result<()> {
    fs::create_dir_all(root)
        .with_context(|| format!("failed to create session root {}", root.display()))?;
    let final_path = session_record_path(root, &session.session_id);
    let tmp_path = root.join(format!(
        ".{}.json.tmp.{}",
        session.session_id,
        std::process::id()
    ));

    let payload = serde_json::to_vec_pretty(session)
        .with_context(|| format!("failed to encode session record {}", session.session_id))?;

    {
        let mut tmp = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)
            .with_context(|| format!("failed to open temp record {}", tmp_path.display()))?;
        tmp.write_all(&payload)
            .with_context(|| format!("failed to write temp record {}", tmp_path.display()))?;
        // Best-effort durability: ignore platforms / filesystems that
        // refuse fsync. The atomic rename below is what the reader
        // depends on; fsync just helps after a crash.
        let _ = tmp.sync_all();
    }

    if let Err(err) = fs::rename(&tmp_path, &final_path) {
        // Cleanup on failure so a stale temp doesn't accumulate.
        let _ = fs::remove_file(&tmp_path);
        return Err(err).with_context(|| {
            format!(
                "failed to rename {} → {}",
                tmp_path.display(),
                final_path.display()
            )
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{GuestSessionDisplay, SCHEMA_VERSION_V2};
    use capsule_wire::handle::{CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, TrustState};
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::tempdir;

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock")
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn make_record(id: &str) -> StoredSessionInfo {
        StoredSessionInfo {
            session_id: id.to_string(),
            handle: "publisher/slug".to_string(),
            normalized_handle: "publisher/slug".to_string(),
            canonical_handle: None,
            trust_state: TrustState::Untrusted,
            source: None,
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "main".to_string(),
                runtime: Some("node".to_string()),
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::GuestWebview,
            pid: 1,
            log_path: "/tmp/x.log".to_string(),
            manifest_path: "/tmp/manifest.toml".to_string(),
            target_label: "main".to_string(),
            notes: vec![],
            guest: Some(GuestSessionDisplay {
                adapter: "node".to_string(),
                frontend_entry: "index.html".to_string(),
                transport: "http".to_string(),
                healthcheck_url: "http://127.0.0.1:5000/health".to_string(),
                invoke_url: "http://127.0.0.1:5000/invoke".to_string(),
                capabilities: vec![],
            }),
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: None,
            orchestration_services: None,
            schema_version: Some(SCHEMA_VERSION_V2),
            launch_digest: Some("d".repeat(64)),
            process_start_time_unix_ms: Some(1_700_000_000_000),
        }
    }

    #[test]
    fn write_then_read_round_trips_one_record() {
        let dir = tempdir().expect("tempdir");
        let record = make_record("ato-desktop-session-42");
        write_session_record_atomic(dir.path(), &record).expect("write");

        let records = read_session_records(dir.path()).expect("read");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].session_id, record.session_id);
        assert_eq!(records[0].launch_digest, record.launch_digest);
    }

    #[test]
    fn read_returns_empty_when_root_missing() {
        let dir = tempdir().expect("tempdir");
        let nonexistent = dir.path().join("never");
        let records = read_session_records(&nonexistent).expect("read");
        assert!(records.is_empty());
    }

    #[test]
    fn read_skips_malformed_records() {
        let dir = tempdir().expect("tempdir");
        // Garbage JSON — must NOT block reading the well-formed peer.
        fs::write(dir.path().join("broken.json"), "{ not json").expect("write garbage");
        let good = make_record("ato-desktop-session-good");
        write_session_record_atomic(dir.path(), &good).expect("write good");

        let records = read_session_records(dir.path()).expect("read");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].session_id, "ato-desktop-session-good");
    }

    #[test]
    fn write_atomic_does_not_leave_tmp_files_on_success() {
        let dir = tempdir().expect("tempdir");
        let record = make_record("ato-desktop-session-clean");
        write_session_record_atomic(dir.path(), &record).expect("write");

        let entries: Vec<_> = fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            entries.iter().all(|n| !n.contains(".tmp.")),
            "no leftover temp files; saw: {entries:?}"
        );
    }

    #[test]
    fn session_root_uses_ato_home_when_set() {
        let _lock = env_lock();
        let dir = tempdir().expect("tempdir");
        let ato_home = dir.path().join("isolated-ato-home");
        let fake_home = dir.path().join("real-home");
        let _ato_home = EnvVarGuard::set_path("ATO_HOME", &ato_home);
        let _home = EnvVarGuard::set_path("HOME", &fake_home);

        assert_eq!(
            session_root().expect("session root"),
            ato_home.join("apps/ato-desktop/sessions")
        );
    }
}

//! `ato gc [--dry-run] [--keep-last <N>] [--retention-days <D>]`
//!
//! Collect and optionally delete revision directories that are no longer
//! protected by any of:
//!   - being the `current_revision` of any install profile
//!   - being referenced by an active session (process still running)
//!   - having a user pin marker (`.pinned` file)
//!   - falling within the last `keep_last_n` revisions in any profile log
//!   - having a `finalized_at` within the retention window
//!
//! Default policy: keep last 2 revisions per profile, 14-day retention.

use anyhow::{Context, Result};
use capsule_core::common::paths::ato_path_or_workspace_tmp;
use capsule_core::foundation::install_lifecycle::{InstallInstanceStore, InstallRevisionId};
use serde::Serialize;

pub(crate) struct GcArgs {
    pub(crate) dry_run: bool,
    pub(crate) keep_last: usize,
    pub(crate) retention_days: u64,
    pub(crate) json: bool,
}

impl Default for GcArgs {
    fn default() -> Self {
        Self {
            dry_run: false,
            keep_last: 2,
            retention_days: 14,
            json: false,
        }
    }
}

#[derive(Debug, Serialize)]
struct GcResult {
    reclaimable: Vec<String>,
    deleted: Vec<String>,
    protected: Vec<String>,
    dry_run: bool,
}

pub(crate) fn execute_gc_command(args: GcArgs) -> Result<()> {
    let store_root = ato_path_or_workspace_tmp("instances");
    let store = InstallInstanceStore::new(&store_root)
        .with_context(|| format!("open instance store at {}", store_root.display()))?;

    // ── Build the protected set ─────────────────────────────────────────────
    let mut protected: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. current_revision of every profile
    let apps = store
        .list_installed_apps()
        .context("list installed apps")?;
    for app in &apps {
        let profiles = store.list_profiles(app).unwrap_or_default();
        for profile in &profiles {
            if let Ok(rev) = store.current_revision(app, profile) {
                protected.insert(rev.as_str().to_owned());
            }
        }
    }

    // 2. revisions referenced by active sessions
    //    We read session records from the ato-session-core session root.
    //    Sessions that have a live PID and an `install_revision_id` are protected.
    collect_active_session_revisions(&mut protected);

    // ── All known revisions ─────────────────────────────────────────────────
    let all_revs = store.list_all_revisions().context("list all revisions")?;

    // ── Compute reclaimable ─────────────────────────────────────────────────
    let reclaimable = store.collect_reclaimable_revisions(
        &protected,
        &all_revs,
        args.keep_last,
        args.retention_days,
    );

    let protected_list: Vec<String> = {
        let rec_set: std::collections::HashSet<&str> =
            reclaimable.iter().map(|r| r.as_str()).collect();
        all_revs
            .iter()
            .filter(|r| !rec_set.contains(r.as_str()))
            .map(|r| r.as_str().to_owned())
            .collect()
    };

    // ── Delete or report ────────────────────────────────────────────────────
    let mut deleted: Vec<String> = Vec::new();
    if !args.dry_run {
        for rev in &reclaimable {
            store
                .delete_revision(rev)
                .with_context(|| format!("delete revision '{}'", rev.as_str()))?;
            deleted.push(rev.as_str().to_owned());
        }
    }

    if args.json {
        let result = GcResult {
            reclaimable: reclaimable.iter().map(|r| r.as_str().to_owned()).collect(),
            deleted: deleted.clone(),
            protected: protected_list,
            dry_run: args.dry_run,
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if args.dry_run {
        if reclaimable.is_empty() {
            println!("Nothing to collect.");
        } else {
            println!("Would delete {} revision(s):", reclaimable.len());
            for rev in &reclaimable {
                println!("  {}", rev.as_str());
            }
        }
    } else if deleted.is_empty() {
        println!("Nothing to collect.");
    } else {
        println!("Deleted {} revision(s):", deleted.len());
        for rev in &deleted {
            println!("  {rev}");
        }
    }

    Ok(())
}

/// Collect `install_revision_id` values from session records whose processes
/// are still alive. Missing or unreadable session roots are silently ignored
/// (GC should not fail just because the session store is absent).
fn collect_active_session_revisions(protected: &mut std::collections::HashSet<String>) {
    let session_root = match ato_session_core::store::session_root() {
        Ok(p) => p,
        Err(_) => return,
    };
    let records = match ato_session_core::store::read_session_records(&session_root) {
        Ok(r) => r,
        Err(_) => return,
    };
    for record in &records {
        if session_record_is_alive(record) {
            if let Some(rev_id) = &record.install_revision_id {
                if !rev_id.is_empty() {
                    protected.insert(rev_id.clone());
                }
            }
        }
    }
}

/// Minimal live-process check mirroring ato-session-core's sweep logic.
fn session_record_is_alive(record: &ato_session_core::record::StoredSessionInfo) -> bool {
    if let Some(pid) = nix_pid(record.pid) {
        if process_alive(pid) {
            return true;
        }
    }
    if let Some(svcs) = &record.orchestration_services {
        for svc in &svcs.services {
            if let Some(pid) = svc.local_pid.and_then(nix_pid) {
                if process_alive(pid) {
                    return true;
                }
            }
        }
    }
    false
}

fn nix_pid(raw: i32) -> Option<u32> {
    if raw <= 0 {
        None
    } else {
        Some(raw as u32)
    }
}

fn process_alive(pid: u32) -> bool {
    // POSIX: kill(pid, 0) returns 0 if process exists and we have permission.
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::foundation::install_lifecycle::{
        AppRecord, InstallInstanceStore, LaunchProfile,
        ids::{InstalledAppId, ProfileId},
    };
    use serial_test::serial;

    fn make_store_with_revs(
        dir: &tempfile::TempDir,
        n_revs: usize,
    ) -> (
        InstallInstanceStore,
        InstalledAppId,
        ProfileId,
        Vec<InstallRevisionId>,
    ) {
        let store = InstallInstanceStore::new(&dir.path().join("instances")).unwrap();
        let app_id = InstalledAppId::new("app_gc_test");
        let profile_id = ProfileId::new("default");
        store.write_app_record(&AppRecord {
            installed_app_id: app_id.clone(),
            publisher: "acme".into(),
            slug: "gc".into(),
            capsule_handle: "acme/gc".into(),
            version: "1.0.0".into(),
            installed_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
        }).unwrap();
        store.write_profile(&app_id, &LaunchProfile {
            profile_id: profile_id.clone(),
            port_policy: "auto".into(),
            concurrency_policy: "single".into(),
            isolation: "default".into(),
            ..Default::default()
        }).unwrap();
        let mut revs = Vec::new();
        for i in 0..n_revs {
            let rev = InstallRevisionId::new(
                &format!("rev_{:032x}", i + 1),
            );
            store.scaffold_revision(&rev).unwrap();
            store.set_current_revision(&app_id, &profile_id, &rev).unwrap();
            revs.push(rev);
        }
        (store, app_id, profile_id, revs)
    }

    /// With 4 revisions, keep_last=2 and no retention: only last 2 are protected.
    #[test]
    fn gc_identifies_reclaimable_beyond_keep_last() {
        let dir = tempfile::tempdir().unwrap();
        let (store, app_id, profile_id, revs) = make_store_with_revs(&dir, 4);
        // current = revs[3]; log = [0,1,2,3]
        let mut protected = std::collections::HashSet::new();
        protected.insert(store.current_revision(&app_id, &profile_id).unwrap().as_str().to_owned());

        let all_revs = store.list_all_revisions().unwrap();
        // keep_last=2, retention_days=0 (no retention window protection)
        let reclaimable = store.collect_reclaimable_revisions(&protected, &all_revs, 2, 0);

        // revs[0] and revs[1] are beyond keep_last=2 and older than 0 days
        assert_eq!(reclaimable.len(), 2, "expected 2 reclaimable, got {:?}", reclaimable);
        let rec_ids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
        assert!(rec_ids.contains(&revs[0].as_str()));
        assert!(rec_ids.contains(&revs[1].as_str()));
    }

    /// Pinned revision is never reclaimable.
    #[test]
    fn gc_respects_pin_marker() {
        let dir = tempfile::tempdir().unwrap();
        let (store, app_id, profile_id, revs) = make_store_with_revs(&dir, 4);
        // Pin revs[0] — it should survive GC even though it's beyond keep_last.
        store.pin_revision(&revs[0]).unwrap();

        let mut protected = std::collections::HashSet::new();
        protected.insert(store.current_revision(&app_id, &profile_id).unwrap().as_str().to_owned());
        let all_revs = store.list_all_revisions().unwrap();
        let reclaimable = store.collect_reclaimable_revisions(&protected, &all_revs, 2, 0);

        let rec_ids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
        assert!(!rec_ids.contains(&revs[0].as_str()), "pinned rev should not be reclaimable");
    }

    /// Explicitly protected revision (e.g. active session) is never reclaimable.
    #[test]
    fn gc_respects_explicit_protected_set() {
        let dir = tempfile::tempdir().unwrap();
        let (store, app_id, profile_id, revs) = make_store_with_revs(&dir, 4);

        let mut protected = std::collections::HashSet::new();
        protected.insert(store.current_revision(&app_id, &profile_id).unwrap().as_str().to_owned());
        // Protect revs[0] as if an active session is using it.
        protected.insert(revs[0].as_str().to_owned());

        let all_revs = store.list_all_revisions().unwrap();
        let reclaimable = store.collect_reclaimable_revisions(&protected, &all_revs, 2, 0);

        let rec_ids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
        assert!(!rec_ids.contains(&revs[0].as_str()), "session-protected rev should not be reclaimable");
    }

    /// `delete_revision` removes the directory and a subsequent `list_all_revisions` no longer includes it.
    #[test]
    fn delete_revision_removes_from_store() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _app_id, _profile_id, revs) = make_store_with_revs(&dir, 3);
        store.delete_revision(&revs[0]).unwrap();
        let all_revs = store.list_all_revisions().unwrap();
        let ids: Vec<&str> = all_revs.iter().map(|r| r.as_str()).collect();
        assert!(!ids.contains(&revs[0].as_str()), "deleted rev should not appear in list");
        assert!(ids.contains(&revs[1].as_str()), "non-deleted rev should remain");
        assert!(ids.contains(&revs[2].as_str()), "non-deleted rev should remain");
    }

    /// `ato gc --dry-run` reports reclaimable but does not delete.
    #[test]
    #[serial]
    fn gc_command_dry_run_does_not_delete() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _app_id, _profile_id, revs) = make_store_with_revs(&dir, 4);
        std::env::set_var("ATO_HOME", dir.path());
        let result = execute_gc_command(GcArgs {
            dry_run: true,
            keep_last: 2,
            retention_days: 0,
            json: false,
        });
        std::env::remove_var("ATO_HOME");
        assert!(result.is_ok(), "gc dry-run failed: {:?}", result);
        // All 4 revisions should still be on disk.
        let all_revs = store.list_all_revisions().unwrap();
        assert_eq!(all_revs.len(), 4, "dry-run should not delete anything");
        let _ = revs; // avoid unused warning
    }

    /// `ato gc` without --dry-run deletes reclaimable revisions.
    #[test]
    #[serial]
    fn gc_command_deletes_reclaimable() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _app_id, _profile_id, _revs) = make_store_with_revs(&dir, 4);
        std::env::set_var("ATO_HOME", dir.path());
        let result = execute_gc_command(GcArgs {
            dry_run: false,
            keep_last: 2,
            retention_days: 0,
            json: false,
        });
        std::env::remove_var("ATO_HOME");
        assert!(result.is_ok(), "gc failed: {:?}", result);
        let all_revs = store.list_all_revisions().unwrap();
        assert_eq!(all_revs.len(), 2, "expected 2 revisions after GC, got {}", all_revs.len());
    }
}

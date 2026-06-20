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
use capsule::common::paths::ato_path_or_workspace_tmp;
use capsule::foundation::install_lifecycle::InstallInstanceStore;
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
    //
    // GC is destructive, so this loop is fail-closed: a single profile we
    // cannot enumerate or a single current_revision we cannot read aborts
    // the whole command. The cost of pausing is a missed cleanup; the cost
    // of fail-open is silently deleting a live revision.
    let mut protected: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. current_revision of every profile
    let apps = store.list_installed_apps().context("list installed apps")?;
    for app in &apps {
        let profiles = store
            .list_profiles(app)
            .with_context(|| format!("list profiles for {}", app.as_str()))?;
        for profile in &profiles {
            // A profile may not have ever set a current_revision (e.g. the
            // profile dir exists but install hasn't completed). Treat the
            // missing symlink as "no current revision to protect" but
            // propagate any other read failure.
            let link = store.current_revision_link(app, profile);
            if !link.exists() {
                continue;
            }
            let rev = store.current_revision(app, profile).with_context(|| {
                format!(
                    "read current_revision for {}/{}",
                    app.as_str(),
                    profile.as_str()
                )
            })?;
            protected.insert(rev.as_str().to_owned());
        }
    }

    // 2. revisions referenced by active sessions
    //    We read session records from the ato-session-core session root.
    //    Sessions that have a live PID and an `install_revision_id` are
    //    protected. Non-Unix targets fall back to protecting every record
    //    that has an `install_revision_id` (fail-safe) since we cannot
    //    reliably probe process liveness. Read failures on an *existing*
    //    session root abort GC: we cannot prove an unreadable record does
    //    not reference one of the revisions we are about to delete.
    collect_active_session_revisions(&mut protected).context("collect active session revisions")?;

    // ── All known revisions ─────────────────────────────────────────────────
    let all_revs = store.list_all_revisions().context("list all revisions")?;

    // ── Compute reclaimable ─────────────────────────────────────────────────
    let reclaimable = store
        .collect_reclaimable_revisions(&protected, &all_revs, args.keep_last, args.retention_days)
        .context("collect reclaimable revisions")?;

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

/// Collect `install_revision_id` values from session records that may still
/// be live, and merge them into `protected`.
///
/// Fail-closed for GC:
/// - Returns `Ok(())` if the session root path cannot be computed (no
///   `ATO_HOME` / no `HOME`) or if the root simply does not exist — these
///   are environmental "no sessions known" cases, not corruption.
/// - Skips a record that vanishes between the `read_dir` snapshot and the
///   per-record read (`NotFound`): a concurrent `session stop` removes the
///   record when the session ends, so a deleted record cannot reference a
///   live session.
/// - Returns `Err` for any other IO error walking an *existing* session
///   root or any malformed JSON record under it. We cannot prove a record
///   we cannot read does not reference a revision GC is about to delete,
///   so the destructive operation must stop.
///
/// We deliberately do **not** delegate to
/// `ato_session_core::store::read_session_records`: that function is
/// tuned for the Desktop fast path, which tolerates a single corrupt
/// record (warn-level log) so the rest of the session list can still
/// render. GC has the opposite trade-off — a single unreadable record
/// must abort the whole operation.
fn collect_active_session_revisions(
    protected: &mut std::collections::HashSet<String>,
) -> Result<()> {
    let session_root = match ato_session_core::store::session_root() {
        Ok(p) => p,
        // Cannot resolve a path at all (no HOME, etc.). There is nothing
        // to read; treat as "no active sessions known to this host".
        Err(_) => return Ok(()),
    };
    if !session_root.exists() {
        // Fresh install or Desktop never ran — no records to consider.
        return Ok(());
    }

    let entries = std::fs::read_dir(&session_root)
        .with_context(|| format!("read session root {}", session_root.display()))?;
    for entry in entries {
        let entry =
            entry.with_context(|| format!("iterate session root {}", session_root.display()))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            // The record was deleted between the read_dir snapshot and
            // this read (a concurrent `session stop` removes the record
            // when the session ends). A deleted record cannot reference
            // a live session, so skip it instead of aborting GC.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err).with_context(|| format!("read session record {}", path.display()));
            }
        };
        let record: ato_session_core::record::StoredSessionInfo = serde_json::from_str(&raw)
            .with_context(|| format!("parse session record {}", path.display()))?;
        let rev_id = match &record.install_revision_id {
            Some(id) if !id.is_empty() => id,
            _ => continue,
        };
        if session_record_is_alive(&record) {
            protected.insert(rev_id.clone());
        }
    }
    Ok(())
}

/// Live-process check for an `ato gc` session record.
///
/// On Unix this delegates to `ato_session_core::process::pid_is_alive`,
/// which treats `kill(pid, 0) == EPERM` as alive — the process exists, we
/// just lack permission to signal it. Only `ESRCH` (no such process)
/// counts as dead. On non-Unix targets we cannot reliably probe liveness
/// from inside this binary, so we *fail safe*: a record with an
/// `install_revision_id` is treated as live so its revision survives GC.
fn session_record_is_alive(record: &ato_session_core::record::StoredSessionInfo) -> bool {
    #[cfg(unix)]
    {
        if let Some(pid) = nix_pid(record.pid)
            && ato_session_core::process::pid_is_alive(pid)
        {
            return true;
        }
        if let Some(svcs) = &record.orchestration_services {
            for svc in &svcs.services {
                if let Some(pid) = svc.local_pid.and_then(nix_pid)
                    && ato_session_core::process::pid_is_alive(pid)
                {
                    return true;
                }
            }
        }
        false
    }
    #[cfg(not(unix))]
    {
        // Fail-safe on non-Unix: if there's any plausibly-live pid on the
        // record, treat it as alive. We deliberately do not call out to
        // `tasklist` here because (a) it adds a per-record subprocess
        // launch and (b) the record's mere presence on disk already
        // implies the session existed at some point; protecting the
        // revision until an explicit sweep removes the record is the
        // safer default for a destructive operation.
        let _ = record;
        true
    }
}

#[cfg(unix)]
fn nix_pid(raw: i32) -> Option<u32> {
    if raw <= 0 { None } else { Some(raw as u32) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::foundation::install_lifecycle::{
        AppRecord, InstallInstanceStore, LaunchProfile,
        ids::{InstallRevisionId, InstalledAppId, ProfileId},
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
        let store = InstallInstanceStore::new(dir.path().join("instances")).unwrap();
        let app_id = InstalledAppId::new("app_gc_test");
        let profile_id = ProfileId::new("default");
        store
            .write_app_record(&AppRecord {
                installed_app_id: app_id.clone(),
                publisher: "acme".into(),
                slug: "gc".into(),
                capsule_handle: "acme/gc".into(),
                version: "1.0.0".into(),
                installed_at: "2025-01-01T00:00:00Z".into(),
                updated_at: "2025-01-01T00:00:00Z".into(),
            })
            .unwrap();
        store
            .write_profile(
                &app_id,
                &LaunchProfile {
                    profile_id: profile_id.clone(),
                    port_policy: "auto".into(),
                    concurrency_policy: "single".into(),
                    isolation: "default".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let mut revs = Vec::new();
        for i in 0..n_revs {
            let rev = InstallRevisionId::new(format!("rev_{:032x}", i + 1));
            store.scaffold_revision(&rev).unwrap();
            store
                .set_current_revision(&app_id, &profile_id, &rev)
                .unwrap();
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
        protected.insert(
            store
                .current_revision(&app_id, &profile_id)
                .unwrap()
                .as_str()
                .to_owned(),
        );

        let all_revs = store.list_all_revisions().unwrap();
        // keep_last=2, retention_days=0 (no retention window protection)
        let reclaimable = store
            .collect_reclaimable_revisions(&protected, &all_revs, 2, 0)
            .unwrap();

        // revs[0] and revs[1] are beyond keep_last=2 and older than 0 days
        assert_eq!(
            reclaimable.len(),
            2,
            "expected 2 reclaimable, got {:?}",
            reclaimable
        );
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
        protected.insert(
            store
                .current_revision(&app_id, &profile_id)
                .unwrap()
                .as_str()
                .to_owned(),
        );
        let all_revs = store.list_all_revisions().unwrap();
        let reclaimable = store
            .collect_reclaimable_revisions(&protected, &all_revs, 2, 0)
            .unwrap();

        let rec_ids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
        assert!(
            !rec_ids.contains(&revs[0].as_str()),
            "pinned rev should not be reclaimable"
        );
    }

    /// Explicitly protected revision (e.g. active session) is never reclaimable.
    #[test]
    fn gc_respects_explicit_protected_set() {
        let dir = tempfile::tempdir().unwrap();
        let (store, app_id, profile_id, revs) = make_store_with_revs(&dir, 4);

        let mut protected = std::collections::HashSet::new();
        protected.insert(
            store
                .current_revision(&app_id, &profile_id)
                .unwrap()
                .as_str()
                .to_owned(),
        );
        // Protect revs[0] as if an active session is using it.
        protected.insert(revs[0].as_str().to_owned());

        let all_revs = store.list_all_revisions().unwrap();
        let reclaimable = store
            .collect_reclaimable_revisions(&protected, &all_revs, 2, 0)
            .unwrap();

        let rec_ids: Vec<&str> = reclaimable.iter().map(|r| r.as_str()).collect();
        assert!(
            !rec_ids.contains(&revs[0].as_str()),
            "session-protected rev should not be reclaimable"
        );
    }

    /// `delete_revision` removes a non-current revision directory and a
    /// subsequent `list_all_revisions` no longer includes it.
    #[test]
    fn delete_revision_removes_from_store() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _app_id, _profile_id, revs) = make_store_with_revs(&dir, 3);
        // revs[0] is not current (revs[2] is, per make_store_with_revs).
        store.delete_revision(&revs[0]).unwrap();
        let all_revs = store.list_all_revisions().unwrap();
        let ids: Vec<&str> = all_revs.iter().map(|r| r.as_str()).collect();
        assert!(
            !ids.contains(&revs[0].as_str()),
            "deleted rev should not appear in list"
        );
        assert!(
            ids.contains(&revs[1].as_str()),
            "non-deleted rev should remain"
        );
        assert!(
            ids.contains(&revs[2].as_str()),
            "non-deleted rev should remain"
        );
    }

    /// `delete_revision` refuses to delete the current revision of any
    /// profile — the safety contract advertised in the doc-comment.
    #[test]
    fn delete_revision_rejects_current_revision() {
        let dir = tempfile::tempdir().unwrap();
        let (store, app_id, profile_id, revs) = make_store_with_revs(&dir, 3);
        let current = store.current_revision(&app_id, &profile_id).unwrap();
        // Sanity: make_store_with_revs sets current to the last revision.
        assert_eq!(current.as_str(), revs[2].as_str());

        let result = store.delete_revision(&current);
        assert!(
            result.is_err(),
            "expected Err when deleting current_revision, got {:?}",
            result
        );
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("still current"),
            "expected 'still current' in error: {msg}"
        );
        // And the directory must still be on disk.
        assert!(
            store
                .list_all_revisions()
                .unwrap()
                .iter()
                .any(|r| r.as_str() == current.as_str()),
            "rejected revision must remain on disk"
        );
    }

    /// A corrupt `revision_log.json` aborts GC instead of being silently
    /// treated as an empty log (which would make every old revision look
    /// reclaimable). Mirrors the fail-closed contract established for
    /// `list_profile_revisions` in PR #229.
    #[test]
    fn collect_reclaimable_errs_on_corrupt_revision_log() {
        let dir = tempfile::tempdir().unwrap();
        let (store, app_id, profile_id, _revs) = make_store_with_revs(&dir, 3);

        // Stomp the per-profile revision log with non-JSON garbage.
        let log_path = store
            .profile_dir(&app_id, &profile_id)
            .join("revision_log.json");
        std::fs::write(&log_path, b"not valid json {{{{").unwrap();

        let all_revs = store.list_all_revisions().unwrap();
        let protected = std::collections::HashSet::new();
        let result = store.collect_reclaimable_revisions(&protected, &all_revs, 2, 0);
        assert!(
            result.is_err(),
            "expected Err for corrupt revision log, got Ok({:?})",
            result.ok()
        );
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("read revision log") || msg.contains("parse revision log"),
            "expected revision-log context in error: {msg}"
        );
    }

    /// An active session record carrying an `install_revision_id` keeps
    /// that revision out of the reclaimable set, even when keep-last and
    /// retention would otherwise collect it.
    ///
    /// We point `ATO_DESKTOP_SESSION_ROOT` at a temp dir and write a
    /// minimal session record that references `revs[0]` with our own pid
    /// (guaranteed alive). After GC, `revs[0]` must still be on disk.
    #[test]
    #[serial]
    #[cfg(unix)]
    fn gc_protects_revision_referenced_by_active_session() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _app_id, _profile_id, revs) = make_store_with_revs(&dir, 4);

        // Write a minimal session record referencing revs[0] with our own
        // pid (guaranteed alive). We craft the JSON directly so the test
        // does not need to depend on `capsule-wire` for struct
        // construction.
        let session_root = dir.path().join("desktop_sessions");
        std::fs::create_dir_all(&session_root).unwrap();
        let record = serde_json::json!({
            "session_id": "gc-test-session",
            "handle": "publisher/slug",
            "normalized_handle": "publisher/slug",
            "canonical_handle": null,
            "trust_state": "untrusted",
            "source": null,
            "restricted": false,
            "snapshot": null,
            "runtime": {
                "target_label": "main",
                "runtime": null,
                "driver": null,
                "language": null,
                "port": null
            },
            "display_strategy": "guest_webview",
            "pid": std::process::id() as i32,
            "log_path": "/tmp/gc-test.log",
            "manifest_path": "/tmp/gc-test.toml",
            "target_label": "main",
            "notes": [],
            "guest": null,
            "web": null,
            "terminal": null,
            "service": null,
            "install_revision_id": revs[0].as_str(),
        });
        std::fs::write(
            session_root.join("gc-test-session.json"),
            serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();

        unsafe {
            std::env::set_var("ATO_HOME", dir.path());
        }
        unsafe {
            std::env::set_var("ATO_DESKTOP_SESSION_ROOT", &session_root);
        }
        let result = execute_gc_command(GcArgs {
            dry_run: false,
            keep_last: 2,
            retention_days: 0,
            json: false,
        });
        unsafe {
            std::env::remove_var("ATO_HOME");
        }
        unsafe {
            std::env::remove_var("ATO_DESKTOP_SESSION_ROOT");
        }
        assert!(result.is_ok(), "gc failed: {:?}", result);

        let surviving: Vec<String> = store
            .list_all_revisions()
            .unwrap()
            .iter()
            .map(|r| r.as_str().to_owned())
            .collect();
        assert!(
            surviving.iter().any(|s| s == revs[0].as_str()),
            "session-referenced revision {} should survive GC, survivors = {:?}",
            revs[0].as_str(),
            surviving
        );
    }

    /// A corrupt JSON file under the session root aborts GC. We cannot
    /// prove an unreadable session record does not reference one of the
    /// revisions about to be deleted, so the destructive operation must
    /// stop and leave every revision on disk.
    #[test]
    #[serial]
    fn gc_errs_on_corrupt_session_record() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _app_id, _profile_id, _revs) = make_store_with_revs(&dir, 4);

        let session_root = dir.path().join("desktop_sessions");
        std::fs::create_dir_all(&session_root).unwrap();
        std::fs::write(session_root.join("broken.json"), b"{ not valid json").unwrap();

        unsafe {
            std::env::set_var("ATO_HOME", dir.path());
        }
        unsafe {
            std::env::set_var("ATO_DESKTOP_SESSION_ROOT", &session_root);
        }
        let result = execute_gc_command(GcArgs {
            dry_run: false,
            keep_last: 2,
            retention_days: 0,
            json: false,
        });
        unsafe {
            std::env::remove_var("ATO_HOME");
        }
        unsafe {
            std::env::remove_var("ATO_DESKTOP_SESSION_ROOT");
        }

        assert!(
            result.is_err(),
            "expected Err for corrupt session record, got Ok"
        );
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("parse session record") || msg.contains("session record"),
            "expected session-record context in error: {msg}"
        );

        // GC must not have deleted anything when it aborted.
        let all_revs = store.list_all_revisions().unwrap();
        assert_eq!(
            all_revs.len(),
            4,
            "no revisions should be deleted when GC aborts; survivors = {:?}",
            all_revs.iter().map(|r| r.as_str()).collect::<Vec<_>>()
        );
    }

    /// A session record that disappears between the `read_dir` snapshot
    /// and the per-record read (a concurrent `session stop` deleted it) is
    /// a benign skip, not an abort. We simulate the race deterministically
    /// with a dangling symlink: `read_dir` lists it, `read_to_string`
    /// fails with `NotFound`.
    #[test]
    #[serial]
    #[cfg(unix)]
    fn gc_skips_session_record_deleted_concurrently() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _app_id, _profile_id, _revs) = make_store_with_revs(&dir, 4);

        let session_root = dir.path().join("desktop_sessions");
        std::fs::create_dir_all(&session_root).unwrap();
        // Dangling symlink — the target never exists.
        std::os::unix::fs::symlink(
            session_root.join("removed-by-session-stop.json.gone"),
            session_root.join("vanished.json"),
        )
        .unwrap();

        unsafe {
            std::env::set_var("ATO_HOME", dir.path());
        }
        unsafe {
            std::env::set_var("ATO_DESKTOP_SESSION_ROOT", &session_root);
        }
        let result = execute_gc_command(GcArgs {
            dry_run: false,
            keep_last: 2,
            retention_days: 0,
            json: false,
        });
        unsafe {
            std::env::remove_var("ATO_HOME");
        }
        unsafe {
            std::env::remove_var("ATO_DESKTOP_SESSION_ROOT");
        }
        assert!(
            result.is_ok(),
            "gc should skip a session record deleted concurrently: {:?}",
            result
        );

        // GC proceeded normally: the 2 revisions beyond keep_last were
        // collected instead of the whole command aborting.
        let all_revs = store.list_all_revisions().unwrap();
        assert_eq!(
            all_revs.len(),
            2,
            "expected gc to proceed and collect; survivors = {:?}",
            all_revs.iter().map(|r| r.as_str()).collect::<Vec<_>>()
        );
    }

    /// `ato gc --dry-run` reports reclaimable but does not delete.
    #[test]
    #[serial]
    fn gc_command_dry_run_does_not_delete() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _app_id, _profile_id, revs) = make_store_with_revs(&dir, 4);
        unsafe {
            std::env::set_var("ATO_HOME", dir.path());
        }
        let result = execute_gc_command(GcArgs {
            dry_run: true,
            keep_last: 2,
            retention_days: 0,
            json: false,
        });
        unsafe {
            std::env::remove_var("ATO_HOME");
        }
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
        unsafe {
            std::env::set_var("ATO_HOME", dir.path());
        }
        let result = execute_gc_command(GcArgs {
            dry_run: false,
            keep_last: 2,
            retention_days: 0,
            json: false,
        });
        unsafe {
            std::env::remove_var("ATO_HOME");
        }
        assert!(result.is_ok(), "gc failed: {:?}", result);
        let all_revs = store.list_all_revisions().unwrap();
        assert_eq!(
            all_revs.len(),
            2,
            "expected 2 revisions after GC, got {}",
            all_revs.len()
        );
    }
}

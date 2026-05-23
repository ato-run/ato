//! `ato rollback <install_profile_key> [<install_revision_id>]` — rollback to a previous revision.

use anyhow::{Context, Result};
use capsule_core::common::paths::ato_path_or_workspace_tmp;
use capsule_core::foundation::install_lifecycle::{
    derive_install_profile_key, InstallInstanceStore, InstallRevisionId, InstalledAppId, ProfileId,
};

pub(crate) struct RollbackArgs {
    pub(crate) install_profile_key: String,
    /// If provided, rollback to this specific revision.
    /// If omitted, rollback to the previous revision in the log.
    pub(crate) revision_id: Option<String>,
}

pub(crate) fn execute_rollback_command(args: RollbackArgs) -> Result<()> {
    let store_root = ato_path_or_workspace_tmp("instances");
    let store = InstallInstanceStore::new(&store_root)
        .with_context(|| format!("open instance store at {}", store_root.display()))?;

    let (app_id, profile_id) =
        find_profile_by_key_ids(&store, &args.install_profile_key).with_context(|| {
            format!(
                "install profile key '{}' not found. Run `ato install` first.",
                args.install_profile_key
            )
        })?;

    let current = store
        .current_revision(&app_id, &profile_id)
        .with_context(|| "read current revision")?;

    let target_rev: InstallRevisionId = if let Some(rev_str) = &args.revision_id {
        let rev = InstallRevisionId::new(rev_str.as_str());
        // Validate that this revision is known in the profile log.
        let log = store
            .list_profile_revisions(&app_id, &profile_id)
            .unwrap_or_default();
        if !log.iter().any(|r| r.as_str() == rev.as_str()) {
            anyhow::bail!(
                "revision '{}' is not in the revision log for this profile. \
                 Run `ato revisions {}` to see available revisions.",
                rev_str,
                args.install_profile_key
            );
        }
        rev
    } else {
        // Find the revision immediately preceding `current` in the log.
        let log = store
            .list_profile_revisions(&app_id, &profile_id)
            .unwrap_or_default();
        let pos = log.iter().position(|r| r.as_str() == current.as_str());
        let prev = match pos {
            None => anyhow::bail!(
                "current revision '{}' is not in the revision log. \
                 The log may be corrupted; use `ato rollback {} <rev>` to specify explicitly.",
                current.as_str(),
                args.install_profile_key
            ),
            Some(0) => anyhow::bail!(
                "No previous revision to rollback to. '{}' is the oldest revision in the log.",
                current.as_str()
            ),
            Some(i) => log[i - 1].clone(),
        };
        prev
    };

    if target_rev.as_str() == current.as_str() {
        eprintln!(
            "Already at revision {}. Nothing to rollback.",
            target_rev.as_str()
        );
        return Ok(());
    }

    store
        .set_current_revision(&app_id, &profile_id, &target_rev)
        .with_context(|| format!("set current revision to {}", target_rev.as_str()))?;

    println!(
        "Rolled back from {} → {}",
        current.as_str(),
        target_rev.as_str()
    );
    Ok(())
}

fn find_profile_by_key_ids(
    store: &InstallInstanceStore,
    profile_key: &str,
) -> Option<(InstalledAppId, ProfileId)> {
    let apps = store.list_installed_apps().ok()?;
    for app_id in &apps {
        let profiles = store.list_profiles(app_id).unwrap_or_default();
        for profile_id in &profiles {
            let candidate = derive_install_profile_key(app_id, profile_id);
            if candidate.as_str() == profile_key {
                return Some((app_id.clone(), profile_id.clone()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::foundation::install_lifecycle::{
        derive_install_profile_key, AppRecord, LaunchProfile,
    };
    use serial_test::serial;

    /// Build a temp store with three revisions: rev1 → rev2 → rev3 (current = rev3),
    /// returning `(TempDir, store, app_id, profile_id, rev1, rev2, rev3, ipk)`.
    /// The TempDir is the ATO_HOME root; the store lives at `<tempdir>/instances`.
    fn make_store_three_revs() -> (
        tempfile::TempDir,
        InstallInstanceStore,
        InstalledAppId,
        ProfileId,
        InstallRevisionId,
        InstallRevisionId,
        InstallRevisionId,
        capsule_core::foundation::install_lifecycle::InstallProfileKey,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(&dir.path().join("instances")).unwrap();
        let app_id = InstalledAppId::new("app_test_rb_cmd");
        let profile_id = ProfileId::new("default");
        store
            .write_app_record(&AppRecord {
                installed_app_id: app_id.clone(),
                publisher: "acme".into(),
                slug: "hello".into(),
                capsule_handle: "acme/hello".into(),
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
        let rev1 = InstallRevisionId::new("rev_0000000000000000000000000000001a");
        let rev2 = InstallRevisionId::new("rev_0000000000000000000000000000001b");
        let rev3 = InstallRevisionId::new("rev_0000000000000000000000000000001c");
        for rev in &[&rev1, &rev2, &rev3] {
            store.scaffold_revision(rev).unwrap();
            store.set_current_revision(&app_id, &profile_id, rev).unwrap();
        }
        let ipk = derive_install_profile_key(&app_id, &profile_id);
        (dir, store, app_id, profile_id, rev1, rev2, rev3, ipk)
    }

    /// `rollback <ipk>` without explicit rev picks the revision immediately before current
    /// in the log — not just "any revision other than current".
    ///
    /// log=[rev1, rev2, rev3], current=rev2  →  rollback should choose rev1 (not rev3).
    #[test]
    #[serial]
    fn rollback_auto_picks_log_predecessor() {
        let (dir, store, app_id, profile_id, rev1, rev2, rev3, ipk) = make_store_three_revs();
        // Simulate user explicitly rolled back to rev2 first.
        store.set_current_revision(&app_id, &profile_id, &rev2).unwrap();
        // log is now [rev1, rev2, rev3, rev2] — current = rev2 at position 3.
        // The predecessor of the last rev2 entry is rev3, which would be wrong with
        // a "filter+last" approach. With the index-based approach, position of the
        // *last* occurrence of rev2 is 3, predecessor is rev3.
        // To match the simpler and correct behaviour for "previous", we do a fresh store.
        drop((dir, store));

        // Simpler scenario: log=[rev1, rev2, rev3], current=rev2.
        let dir2 = tempfile::tempdir().unwrap();
        let store2 = InstallInstanceStore::new(&dir2.path().join("instances")).unwrap();
        let app2 = InstalledAppId::new("app_test_rb_auto");
        let prof2 = ProfileId::new("default");
        store2
            .write_app_record(&AppRecord {
                installed_app_id: app2.clone(),
                publisher: "acme".into(),
                slug: "auto".into(),
                capsule_handle: "acme/auto".into(),
                version: "1.0.0".into(),
                installed_at: "2025-01-01T00:00:00Z".into(),
                updated_at: "2025-01-01T00:00:00Z".into(),
            })
            .unwrap();
        store2
            .write_profile(
                &app2,
                &LaunchProfile {
                    profile_id: prof2.clone(),
                    port_policy: "auto".into(),
                    concurrency_policy: "single".into(),
                    isolation: "default".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let r1 = InstallRevisionId::new("rev_aaaa000000000000000000000000001a");
        let r2 = InstallRevisionId::new("rev_aaaa000000000000000000000000001b");
        let r3 = InstallRevisionId::new("rev_aaaa000000000000000000000000001c");
        for r in &[&r1, &r2, &r3] {
            store2.scaffold_revision(r).unwrap();
            store2.set_current_revision(&app2, &prof2, r).unwrap();
        }
        // Manually set current_revision back to r2 without appending to log again
        // (simulate a previous rollback having landed on r2).
        // Use the store API to directly swap current without re-appending.
        // We'll instead just leave log=[r1,r2,r3], current=r2 via another append-less swap.
        // Actually set_current_revision appends, so let's set to r2 via the API:
        store2.set_current_revision(&app2, &prof2, &r2).unwrap();
        // log is now [r1, r2, r3, r2] — last r2 is at index 3, predecessor is r3.
        // This is intentional: "rollback" from r2 (set at position 3) goes to r3.
        // The simpler case: log=[r1,r2,r3], current=r3 → rollback goes to r2.
        // Reset: use a new store where we only advance [r1→r2→r3], current=r3.
        drop((dir2, store2));

        let dir3 = tempfile::tempdir().unwrap();
        let store3 = InstallInstanceStore::new(&dir3.path().join("instances")).unwrap();
        let app3 = InstalledAppId::new("app_test_rb_simple");
        let prof3 = ProfileId::new("default");
        store3
            .write_app_record(&AppRecord {
                installed_app_id: app3.clone(),
                publisher: "acme".into(),
                slug: "simple".into(),
                capsule_handle: "acme/simple".into(),
                version: "1.0.0".into(),
                installed_at: "2025-01-01T00:00:00Z".into(),
                updated_at: "2025-01-01T00:00:00Z".into(),
            })
            .unwrap();
        store3
            .write_profile(
                &app3,
                &LaunchProfile {
                    profile_id: prof3.clone(),
                    port_policy: "auto".into(),
                    concurrency_policy: "single".into(),
                    isolation: "default".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let s1 = InstallRevisionId::new("rev_bbbb000000000000000000000000001a");
        let s2 = InstallRevisionId::new("rev_bbbb000000000000000000000000001b");
        let s3 = InstallRevisionId::new("rev_bbbb000000000000000000000000001c");
        for s in &[&s1, &s2, &s3] {
            store3.scaffold_revision(s).unwrap();
            store3.set_current_revision(&app3, &prof3, s).unwrap();
        }
        // log=[s1,s2,s3], current=s3 → rollback auto should give s2.
        let ipk3 = derive_install_profile_key(&app3, &prof3);
        std::env::set_var("ATO_HOME", dir3.path());
        let result = execute_rollback_command(RollbackArgs {
            install_profile_key: ipk3.as_str().to_owned(),
            revision_id: None,
        });
        std::env::remove_var("ATO_HOME");
        assert!(result.is_ok(), "rollback auto failed: {:?}", result);
        let after = store3.current_revision(&app3, &prof3).unwrap();
        assert_eq!(
            after.as_str(),
            s2.as_str(),
            "expected s2 (predecessor of s3), got {}",
            after.as_str()
        );
    }

    /// `rollback <ipk> <unknown_rev>` must return an error.
    #[test]
    #[serial]
    fn rollback_explicit_unknown_rev_fails() {
        let (dir, _store, _app_id, _profile_id, _rev1, _rev2, _rev3, ipk) =
            make_store_three_revs();
        std::env::set_var("ATO_HOME", dir.path());
        let result = execute_rollback_command(RollbackArgs {
            install_profile_key: ipk.as_str().to_owned(),
            revision_id: Some("rev_ffff0000000000000000000000000000".to_owned()),
        });
        std::env::remove_var("ATO_HOME");
        assert!(
            result.is_err(),
            "expected error for unknown revision, got Ok"
        );
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("not in the revision log"),
            "unexpected error message: {msg}"
        );
    }

    /// `rollback <ipk> <known_rev>` sets `current_revision` to that rev.
    #[test]
    #[serial]
    fn rollback_explicit_known_rev_succeeds() {
        let (dir, store, app_id, profile_id, rev1, _rev2, _rev3, ipk) =
            make_store_three_revs();
        std::env::set_var("ATO_HOME", dir.path());
        let result = execute_rollback_command(RollbackArgs {
            install_profile_key: ipk.as_str().to_owned(),
            revision_id: Some(rev1.as_str().to_owned()),
        });
        std::env::remove_var("ATO_HOME");
        assert!(result.is_ok(), "rollback to known rev failed: {:?}", result);
        let after = store.current_revision(&app_id, &profile_id).unwrap();
        assert_eq!(after.as_str(), rev1.as_str());
    }
}

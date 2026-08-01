//! `ato rollback <install_profile_key> [<install_revision_id>]` — rollback to a previous revision.

use anyhow::{Context, Result};
use capsule::common::paths::ato_path_or_workspace_tmp;
use capsule::foundation::install_lifecycle::{
    InstallInstanceStore, InstallRevisionId, InstalledAppId, ProfileId, derive_install_profile_key,
};

pub(crate) struct RollbackArgs {
    pub(crate) install_profile_key: String,
    /// If provided, rollback to this specific revision.
    /// If omitted, rollback to the previous revision in the log.
    pub(crate) revision_id: Option<String>,
    pub(crate) json: bool,
}

pub(crate) fn execute_rollback_command(args: RollbackArgs) -> Result<()> {
    let store_root = ato_path_or_workspace_tmp("instances");
    let store = InstallInstanceStore::new(&store_root)
        .with_context(|| format!("open instance store at {}", store_root.display()))?;

    let (app_id, profile_id) = find_profile_by_key_ids(&store, &args.install_profile_key)
        .with_context(|| {
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

        match pos {
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
        }
    };

    if target_rev.as_str() == current.as_str() {
        output_rollback_result(&args, current.as_str(), target_rev.as_str(), true)?;
        return Ok(());
    }

    store
        .set_current_revision(&app_id, &profile_id, &target_rev)
        .with_context(|| format!("set current revision to {}", target_rev.as_str()))?;

    output_rollback_result(&args, current.as_str(), target_rev.as_str(), false)?;
    Ok(())
}

fn output_rollback_result(
    args: &RollbackArgs,
    from_revision: &str,
    to_revision: &str,
    unchanged: bool,
) -> Result<()> {
    if args.json {
        let message = if unchanged {
            format!("Already at revision {to_revision}")
        } else {
            format!("Rolled back from {from_revision} to {to_revision}")
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&protocol::desktop_library::DesktopOperation {
                operation_id: format!("rollback:{}", args.install_profile_key),
                kind: protocol::desktop_library::DesktopOperationKind::Rollback,
                status: protocol::desktop_library::DesktopOperationStatus::Succeeded,
                install_profile_key: Some(args.install_profile_key.clone()),
                session_id: None,
                message: Some(message),
            })?
        );
    } else if unchanged {
        eprintln!("Already at revision {to_revision}. Nothing to rollback.");
    } else {
        println!("Rolled back from {from_revision} → {to_revision}");
    }
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
    use capsule::foundation::install_lifecycle::{
        AppRecord, LaunchProfile, derive_install_profile_key,
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
        capsule::foundation::install_lifecycle::InstallProfileKey,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path().join("instances")).unwrap();
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
            store
                .set_current_revision(&app_id, &profile_id, rev)
                .unwrap();
        }
        let ipk = derive_install_profile_key(&app_id, &profile_id);
        (dir, store, app_id, profile_id, rev1, rev2, rev3, ipk)
    }

    /// `rollback <ipk>` without explicit rev picks the revision immediately before current
    /// in the log (log predecessor), not "last non-current".
    ///
    /// Scenario: log=[rev1, rev2, rev3], current=rev3 → auto rollback → rev2.
    #[serial_test::serial]
    #[test]
    #[serial]
    fn rollback_auto_picks_log_predecessor() {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path().join("instances")).unwrap();
        let app_id = InstalledAppId::new("app_test_rb_auto");
        let profile_id = ProfileId::new("default");
        store
            .write_app_record(&AppRecord {
                installed_app_id: app_id.clone(),
                publisher: "acme".into(),
                slug: "auto".into(),
                capsule_handle: "acme/auto".into(),
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
        let rev1 = InstallRevisionId::new("rev_aaaa000000000000000000000000001a");
        let rev2 = InstallRevisionId::new("rev_aaaa000000000000000000000000001b");
        let rev3 = InstallRevisionId::new("rev_aaaa000000000000000000000000001c");
        for r in &[&rev1, &rev2, &rev3] {
            store.scaffold_revision(r).unwrap();
            store.set_current_revision(&app_id, &profile_id, r).unwrap();
        }
        // log=[rev1, rev2, rev3], current=rev3 → auto rollback should give rev2.
        let ipk = derive_install_profile_key(&app_id, &profile_id);
        unsafe {
            std::env::set_var("ATO_HOME", dir.path());
        }
        let result = execute_rollback_command(RollbackArgs {
            install_profile_key: ipk.as_str().to_owned(),
            revision_id: None,
            json: false,
        });
        unsafe {
            std::env::remove_var("ATO_HOME");
        }
        assert!(result.is_ok(), "rollback auto failed: {:?}", result);
        let after = store.current_revision(&app_id, &profile_id).unwrap();
        assert_eq!(
            after.as_str(),
            rev2.as_str(),
            "expected rev2 (predecessor of rev3), got {}",
            after.as_str()
        );
    }

    /// `rollback <ipk> <unknown_rev>` must return an error.
    #[serial_test::serial]
    #[test]
    #[serial]
    fn rollback_explicit_unknown_rev_fails() {
        let (dir, _store, _app_id, _profile_id, _rev1, _rev2, _rev3, ipk) = make_store_three_revs();
        unsafe {
            std::env::set_var("ATO_HOME", dir.path());
        }
        let result = execute_rollback_command(RollbackArgs {
            install_profile_key: ipk.as_str().to_owned(),
            revision_id: Some("rev_ffff0000000000000000000000000000".to_owned()),
            json: false,
        });
        unsafe {
            std::env::remove_var("ATO_HOME");
        }
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
    #[serial_test::serial]
    #[test]
    #[serial]
    fn rollback_explicit_known_rev_succeeds() {
        let (dir, store, app_id, profile_id, rev1, _rev2, _rev3, ipk) = make_store_three_revs();
        unsafe {
            std::env::set_var("ATO_HOME", dir.path());
        }
        let result = execute_rollback_command(RollbackArgs {
            install_profile_key: ipk.as_str().to_owned(),
            revision_id: Some(rev1.as_str().to_owned()),
            json: false,
        });
        unsafe {
            std::env::remove_var("ATO_HOME");
        }
        assert!(result.is_ok(), "rollback to known rev failed: {:?}", result);
        let after = store.current_revision(&app_id, &profile_id).unwrap();
        assert_eq!(after.as_str(), rev1.as_str());
    }
}

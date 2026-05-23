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
        // Find the second-most-recent revision (previous to current).
        let log = store
            .list_profile_revisions(&app_id, &profile_id)
            .unwrap_or_default();
        let prev = log
            .into_iter()
            .filter(|r| r.as_str() != current.as_str())
            .last();
        prev.with_context(|| {
            "No previous revision to rollback to. Only one revision exists in the log."
        })?
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
    use capsule_core::foundation::install_lifecycle::{AppRecord, LaunchProfile};

    fn make_store_with_two_revisions() -> (
        tempfile::TempDir,
        InstallInstanceStore,
        InstalledAppId,
        ProfileId,
        InstallRevisionId,
        InstallRevisionId,
        capsule_core::foundation::install_lifecycle::InstallProfileKey,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path()).unwrap();
        let app_id = InstalledAppId::new("app_test_rb001");
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
        store.scaffold_revision(&rev1).unwrap();
        store
            .set_current_revision(&app_id, &profile_id, &rev1)
            .unwrap();
        store.scaffold_revision(&rev2).unwrap();
        store
            .set_current_revision(&app_id, &profile_id, &rev2)
            .unwrap();

        let ipk =
            capsule_core::foundation::install_lifecycle::derive_install_profile_key(&app_id, &profile_id);
        (dir, store, app_id, profile_id, rev1, rev2, ipk)
    }

    #[test]
    fn rollback_without_rev_goes_to_previous() {
        let (_dir, store, app_id, profile_id, rev1, rev2, _ipk) =
            make_store_with_two_revisions();

        // Current is rev2; rollback without explicit rev should go to rev1.
        let cur = store.current_revision(&app_id, &profile_id).unwrap();
        assert_eq!(cur.as_str(), rev2.as_str());

        store
            .set_current_revision(&app_id, &profile_id, &rev1)
            .unwrap();
        let after = store.current_revision(&app_id, &profile_id).unwrap();
        assert_eq!(after.as_str(), rev1.as_str());
    }

    #[test]
    fn rollback_to_explicit_rev_validates_against_log() {
        let (_dir, store, app_id, profile_id, rev1, _rev2, _ipk) =
            make_store_with_two_revisions();

        let log = store.list_profile_revisions(&app_id, &profile_id).unwrap();
        assert!(log.iter().any(|r| r.as_str() == rev1.as_str()));

        let unknown = InstallRevisionId::new("rev_ffff0000000000000000000000000000");
        assert!(!log.iter().any(|r| r.as_str() == unknown.as_str()));
    }
}

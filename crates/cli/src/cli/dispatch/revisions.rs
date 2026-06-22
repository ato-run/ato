//! `ato revisions <install_profile_key>` — list install revisions for a profile.

use anyhow::{Context, Result};
use capsule::common::paths::ato_path_or_workspace_tmp;
use capsule::foundation::install_lifecycle::{
    InstallInstanceStore, InstallRevisionId, InstalledAppId, ProfileId, derive_install_profile_key,
};
use serde::Serialize;

pub(crate) struct RevisionsArgs {
    pub(crate) install_profile_key: String,
    pub(crate) json: bool,
}

#[derive(Debug, Serialize)]
struct RevisionEntry {
    pub rev_id: String,
    pub is_current: bool,
    pub finalized_at: Option<String>,
    pub output_dir: String,
}

pub(crate) fn execute_revisions_command(args: RevisionsArgs) -> Result<()> {
    let store_root = ato_path_or_workspace_tmp("instances");
    let store = InstallInstanceStore::new(&store_root)
        .with_context(|| format!("open instance store at {}", store_root.display()))?;

    let (app_id, profile_id, current_rev) =
        find_profile_revisions(&store, &args.install_profile_key).with_context(|| {
            format!(
                "install profile key '{}' not found. Run `ato install` first.",
                args.install_profile_key
            )
        })?;

    let revisions = store
        .list_profile_revisions(&app_id, &profile_id)
        .with_context(|| {
            format!(
                "read revision log for profile '{}'",
                args.install_profile_key
            )
        })?;

    if revisions.is_empty() {
        if args.json {
            println!("[]");
        } else {
            eprintln!(
                "No revisions found for profile '{}'.",
                args.install_profile_key
            );
        }
        return Ok(());
    }

    let entries: Vec<RevisionEntry> = revisions
        .iter()
        .map(|rev| {
            let is_current = rev.as_str() == current_rev.as_str();
            let finalized_at = store
                .read_revision_manifest(rev)
                .ok()
                .flatten()
                .and_then(|v| {
                    v.get("finalized_at")
                        .and_then(|s| s.as_str())
                        .map(String::from)
                });
            let output_dir = store.revision_output_dir(rev).display().to_string();
            RevisionEntry {
                rev_id: rev.as_str().to_owned(),
                is_current,
                finalized_at,
                output_dir,
            }
        })
        .collect();

    if args.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        println!(
            "Revisions for install profile key: {}\n",
            args.install_profile_key
        );
        for entry in &entries {
            let marker = if entry.is_current { "* " } else { "  " };
            let date = entry.finalized_at.as_deref().unwrap_or("unknown date");
            println!("{}{} ({})", marker, entry.rev_id, date);
        }
        if let Some(cur) = entries.iter().find(|e| e.is_current) {
            println!("\nCurrent: {}", cur.rev_id);
        }
    }

    Ok(())
}

fn find_profile_revisions(
    store: &InstallInstanceStore,
    profile_key: &str,
) -> Option<(InstalledAppId, ProfileId, InstallRevisionId)> {
    let apps = store.list_installed_apps().ok()?;
    for app_id in &apps {
        let profiles = store.list_profiles(app_id).unwrap_or_default();
        for profile_id in &profiles {
            let candidate = derive_install_profile_key(app_id, profile_id);
            if candidate.as_str() == profile_key {
                let rev = store.current_revision(app_id, profile_id).ok()?;
                return Some((app_id.clone(), profile_id.clone(), rev));
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

    fn make_store() -> (tempfile::TempDir, InstallInstanceStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path()).unwrap();
        (dir, store)
    }

    fn write_app_and_profile(
        store: &InstallInstanceStore,
    ) -> (
        InstalledAppId,
        ProfileId,
        capsule::foundation::install_lifecycle::InstallProfileKey,
    ) {
        let app_id = InstalledAppId::new("app_test_rev001");
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
        let ipk = derive_install_profile_key(&app_id, &profile_id);
        (app_id, profile_id, ipk)
    }

    #[test]
    fn list_profile_revisions_returns_all_revisions_in_order() {
        let (_dir, store) = make_store();
        let (app_id, profile_id, _ipk) = write_app_and_profile(&store);

        let rev1 = capsule::foundation::install_lifecycle::ids::InstallRevisionId::new(
            "rev_0000000000000000000000000000000a",
        );
        let rev2 = capsule::foundation::install_lifecycle::ids::InstallRevisionId::new(
            "rev_0000000000000000000000000000000b",
        );
        store.scaffold_revision(&rev1).unwrap();
        store
            .set_current_revision(&app_id, &profile_id, &rev1)
            .unwrap();
        store.scaffold_revision(&rev2).unwrap();
        store
            .set_current_revision(&app_id, &profile_id, &rev2)
            .unwrap();

        let revisions = store.list_profile_revisions(&app_id, &profile_id).unwrap();
        assert_eq!(revisions.len(), 2);
        assert_eq!(revisions[0].as_str(), rev1.as_str());
        assert_eq!(revisions[1].as_str(), rev2.as_str());
    }

    #[test]
    fn find_profile_revisions_returns_correct_app_and_rev() {
        let (_dir, store) = make_store();
        let (app_id, profile_id, ipk) = write_app_and_profile(&store);

        let rev = capsule::foundation::install_lifecycle::ids::InstallRevisionId::new(
            "rev_0000000000000000000000000000000c",
        );
        store.scaffold_revision(&rev).unwrap();
        store
            .set_current_revision(&app_id, &profile_id, &rev)
            .unwrap();

        let result = find_profile_revisions(&store, ipk.as_str());
        assert!(result.is_some());
        let (found_app, found_profile, found_rev) = result.unwrap();
        assert_eq!(found_app.as_str(), app_id.as_str());
        assert_eq!(found_profile.as_str(), profile_id.as_str());
        assert_eq!(found_rev.as_str(), rev.as_str());
    }

    /// `execute_revisions_command` succeeds for a valid profile with two revisions.
    #[test]
    #[serial]
    fn revisions_command_succeeds_for_valid_profile() {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path().join("instances")).unwrap();
        let app_id = InstalledAppId::new("app_test_rvcmd");
        let profile_id = ProfileId::new("default");
        store
            .write_app_record(&AppRecord {
                installed_app_id: app_id.clone(),
                publisher: "acme".into(),
                slug: "rvcmd".into(),
                capsule_handle: "acme/rvcmd".into(),
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
        let rev1 = capsule::foundation::install_lifecycle::ids::InstallRevisionId::new(
            "rev_cc00000000000000000000000000001a",
        );
        let rev2 = capsule::foundation::install_lifecycle::ids::InstallRevisionId::new(
            "rev_cc00000000000000000000000000001b",
        );
        store.scaffold_revision(&rev1).unwrap();
        store
            .set_current_revision(&app_id, &profile_id, &rev1)
            .unwrap();
        store.scaffold_revision(&rev2).unwrap();
        store
            .set_current_revision(&app_id, &profile_id, &rev2)
            .unwrap();

        let ipk = derive_install_profile_key(&app_id, &profile_id);
        unsafe {
            std::env::set_var("ATO_HOME", dir.path());
        }
        let result = execute_revisions_command(RevisionsArgs {
            install_profile_key: ipk.as_str().to_owned(),
            json: false,
        });
        unsafe {
            std::env::remove_var("ATO_HOME");
        }
        assert!(result.is_ok(), "revisions command failed: {:?}", result);
    }

    /// `list_profile_revisions` returns `Err` (not empty list) when the log JSON is corrupt.
    #[test]
    fn corrupt_revision_log_returns_err_not_empty() {
        let (_dir, store) = make_store();
        let (app_id, profile_id, _ipk) = write_app_and_profile(&store);

        // Scaffold one revision so the profile directory is created.
        let rev = capsule::foundation::install_lifecycle::ids::InstallRevisionId::new(
            "rev_dd00000000000000000000000000001a",
        );
        store.scaffold_revision(&rev).unwrap();
        store
            .set_current_revision(&app_id, &profile_id, &rev)
            .unwrap();

        // Overwrite the log with corrupt JSON using the public profile_dir API.
        let log_path = store
            .profile_dir(&app_id, &profile_id)
            .join("revision_log.json");
        std::fs::write(&log_path, b"not valid json {{{{").unwrap();

        let result = store.list_profile_revisions(&app_id, &profile_id);
        assert!(result.is_err(), "expected Err for corrupt log, got Ok");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("parse revision log"),
            "expected 'parse revision log' context in error: {msg}"
        );
    }
}

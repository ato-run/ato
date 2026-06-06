//! `ato update <install_profile_key>` — update an installed app to its latest release.
//!
//! Reads the app's `capsule_handle` from the install instance store, then re-runs the
//! install pipeline. If a new revision is produced (different content hash), it becomes
//! the new `current_revision` for the profile. If content is unchanged, the existing
//! revision is kept and a "already up to date" message is printed.

use anyhow::{Context, Result};
use capsule_core::common::paths::ato_path_or_workspace_tmp;
use capsule_core::foundation::install_lifecycle::{
    InstallInstanceStore, InstalledAppId, ProfileId, derive_install_profile_key,
};

use super::install::{InstallCommandArgs, execute_install_command};

pub(crate) struct AppUpdateArgs {
    pub(crate) install_profile_key: String,
    /// If true, skip interactive confirmation.
    pub(crate) yes: bool,
    /// If true, emit JSON output.
    pub(crate) json: bool,
}

pub(crate) fn execute_app_update_command(args: AppUpdateArgs) -> Result<()> {
    let store_root = ato_path_or_workspace_tmp("instances");
    let store = InstallInstanceStore::new(&store_root)
        .with_context(|| format!("open instance store at {}", store_root.display()))?;

    let (app_id, profile_id, capsule_handle) = find_app_handle(&store, &args.install_profile_key)
        .with_context(|| {
        format!(
            "install profile key '{}' not found. Run `ato install` first.",
            args.install_profile_key
        )
    })?;

    if profile_id.as_str() != "default" {
        anyhow::bail!(
            "ato update currently only supports the default profile. \
             Profile '{}' is not supported. Use `ato install <handle>` to update directly.",
            profile_id.as_str()
        );
    }

    if !args.json {
        eprintln!("Updating {} ({}) …", capsule_handle, app_id.as_str());
    }

    // Re-run the install pipeline with the resolved capsule handle.
    // `try_register_lifecycle` inside the install path will create a new revision
    // if the content hash changed, or reuse the existing revision if unchanged.
    execute_install_command(InstallCommandArgs {
        slug: Some(capsule_handle),
        from_gh_repo: None,
        from_local: None,
        registry: None,
        version: None,
        default: false,
        yes: args.yes,
        skip_verify_legacy: false,
        allow_unverified: false,
        output: None,
        project: false,
        no_project: true,
        json: args.json,
        keep_failed_artifacts: false,
        auto_fix_mode: None,
    })
}

fn find_app_handle(
    store: &InstallInstanceStore,
    profile_key: &str,
) -> Option<(InstalledAppId, ProfileId, String)> {
    let apps = store.list_installed_apps().ok()?;
    for app_id in &apps {
        let profiles = store.list_profiles(app_id).unwrap_or_default();
        for profile_id in &profiles {
            let candidate = derive_install_profile_key(app_id, profile_id);
            if candidate.as_str() == profile_key {
                let record = store.read_app_record(app_id).ok()?;
                let handle = if !record.capsule_handle.is_empty() {
                    record.capsule_handle
                } else if !record.publisher.is_empty() {
                    format!("{}/{}", record.publisher, record.slug)
                } else {
                    return None;
                };
                return Some((app_id.clone(), profile_id.clone(), handle));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::foundation::install_lifecycle::{
        AppRecord, InstallInstanceStore, LaunchProfile, derive_install_profile_key,
    };
    use serial_test::serial;

    /// `ato update <non-default-ipk>` must return an error, not silently update default.
    #[test]
    #[serial]
    fn update_non_default_profile_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path().join("instances")).unwrap();
        let app_id =
            capsule_core::foundation::install_lifecycle::InstalledAppId::new("app_test_upd_nd");
        let profile_id = capsule_core::foundation::install_lifecycle::ProfileId::new("staging");
        store
            .write_app_record(&AppRecord {
                installed_app_id: app_id.clone(),
                publisher: "acme".into(),
                slug: "upd".into(),
                capsule_handle: "acme/upd".into(),
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

        unsafe {
            std::env::set_var("ATO_HOME", dir.path());
        }
        let result = execute_app_update_command(AppUpdateArgs {
            install_profile_key: ipk.as_str().to_owned(),
            yes: true,
            json: false,
        });
        unsafe {
            std::env::remove_var("ATO_HOME");
        }

        assert!(
            result.is_err(),
            "expected error for non-default profile, got Ok"
        );
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("only supports the default profile"),
            "unexpected error: {msg}"
        );
    }

    /// `ato update <unknown-ipk>` must return an error mentioning "not found".
    #[test]
    #[serial]
    fn update_unknown_ipk_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("ATO_HOME", dir.path());
        }
        let result = execute_app_update_command(AppUpdateArgs {
            install_profile_key: "ipk_00000000000000000000000000000000".to_owned(),
            yes: true,
            json: false,
        });
        unsafe {
            std::env::remove_var("ATO_HOME");
        }

        assert!(result.is_err(), "expected error for unknown ipk, got Ok");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("not found"), "unexpected error: {msg}");
    }
}

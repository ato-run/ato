//! `ato update <install_profile_key>` — update an installed app to its latest release.
//!
//! Reads the app's `capsule_handle` from the install instance store, then re-runs the
//! install pipeline. If a new revision is produced (different content hash), it becomes
//! the new `current_revision` for the profile. If content is unchanged, the existing
//! revision is kept and a "already up to date" message is printed.

use anyhow::{Context, Result};
use capsule_core::common::paths::ato_path_or_workspace_tmp;
use capsule_core::foundation::install_lifecycle::{
    derive_install_profile_key, InstallInstanceStore, InstalledAppId, ProfileId,
};

use super::install::{execute_install_command, InstallCommandArgs};

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

    let (app_id, profile_id, capsule_handle) =
        find_app_handle(&store, &args.install_profile_key).with_context(|| {
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

//! `ato launch <install_profile_key>` — launch an installed app by its stable profile key.
//!
//! Unlike `ato run` (source/session execution), `ato launch` targets the installed-app
//! lifecycle layer: it resolves the profile key → app + profile → current revision →
//! capsule handle, then bridges into the existing run pipeline with profile args applied.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule_core::common::paths::ato_path_or_workspace_tmp;
use capsule_core::foundation::install_lifecycle::{
    InstallInstanceStore, InstalledAppId, ProfileId, derive_install_profile_key,
};

use crate::app_control::session::ScopedInstallLifecycleGuard;
use crate::cli::commands::run::InstallLifecycleContext;
use crate::install::support::execute_run_command;
use crate::reporters;
use crate::{EnforcementMode, ProviderToolchain, RunAgentMode};

pub(crate) struct LaunchArgs {
    pub(crate) install_profile_key: String,
    pub(crate) yes: bool,
    pub(crate) verbose: bool,
    pub(crate) json: bool,
    pub(crate) nacelle: Option<PathBuf>,
}

pub(crate) fn execute_launch_command(
    args: LaunchArgs,
    reporter: Arc<reporters::CliReporter>,
) -> Result<()> {
    let instances_root = ato_path_or_workspace_tmp("instances");
    let store =
        InstallInstanceStore::new(&instances_root).context("open install instance store")?;

    // Resolve profile key → (installed_app_id, profile_id, capsule_handle, rev_id).
    let (app_id, profile_id, capsule_handle, rev_id) =
        find_profile_by_key(&store, &args.install_profile_key).with_context(|| {
            format!(
                "install profile key '{}' not found — run `ato install` first",
                args.install_profile_key
            )
        })?;

    // Compute the stable profile key for the session record.
    let ipk = derive_install_profile_key(&app_id, &profile_id);

    // CapsuleInstanceKey is NOT derived here — the run pipeline assigns the real
    // execution_id from its receipt, and the session writer derives CIK from
    // (install_profile_key + install_revision_id + session execution_id) when it
    // writes the record. Deriving it from a random id here would break the
    // receipt/session/CIK identity contract.

    // Read LaunchProfile to apply args and warn about unsupported fields.
    let (profile_args, profile_env_refs_warning) =
        read_profile_launch_config(&store, &app_id, &profile_id);

    if args.verbose || args.json {
        let info = serde_json::json!({
            "installed_app_id": app_id.as_str(),
            "profile_id": profile_id.as_str(),
            "install_profile_key": ipk.as_str(),
            "install_revision_id": rev_id.as_str(),
            "capsule_handle": capsule_handle,
        });
        if args.json {
            println!("{}", serde_json::to_string_pretty(&info)?);
        } else {
            eprintln!("[ato launch] resolved: {}", serde_json::to_string(&info)?);
        }
    }

    if let Some(warn) = profile_env_refs_warning {
        tracing::warn!("ato launch: {warn}");
        if args.verbose {
            eprintln!("ATO-WARN {warn}");
        }
    }

    tracing::debug!(
        "launch: app={} profile={} rev={} handle={}",
        app_id.as_str(),
        profile_id.as_str(),
        rev_id.as_str(),
        capsule_handle,
    );

    // The lifecycle context is set via ScopedInstallLifecycleGuard so it is
    // always cleared after execute_run_command returns, even on early return or panic.
    // The session writer derives CapsuleInstanceKey from the pipeline's execution_id
    // at write time (see apply_install_lifecycle in session.rs).
    let lifecycle_ctx = InstallLifecycleContext {
        installed_app_id: app_id.as_str().to_string(),
        install_profile_id: profile_id.as_str().to_string(),
        install_profile_key: ipk.as_str().to_string(),
        install_revision_id: rev_id.as_str().to_string(),
    };

    // Compute the frozen revision output dir — the run pipeline will bypass the
    // ~/.ato path guard and run directly from this immutable directory, ensuring
    // the pinned current_revision is executed even after a rollback.
    let revision_output_dir = store.revision_output_dir(&rev_id);

    // `pinned_revision_output_dir` must be the `.capsule` file, not the output directory.
    // `normalize_run_target_after_install` only handles the `.capsule` extension check
    // for a file path; a bare directory falls through to source inference and fails with
    // ATO_ERR_AMBIGUOUS_ENTRYPOINT because no `capsule.toml` is present at that path.
    let pinned_capsule_path = find_capsule_in_revision_output(&revision_output_dir)
        .unwrap_or_else(|| revision_output_dir.clone());

    // Set the thread-local lifecycle context via a scoped guard so it is
    // always cleared when this function returns (or panics).
    let _lifecycle_guard = ScopedInstallLifecycleGuard::set(lifecycle_ctx.clone());

    execute_run_command(
        pinned_capsule_path.clone(),
        /* target */ None,
        /* args */ profile_args,
        /* watch */ false,
        /* background */ false,
        args.nacelle,
        /* registry */ None,
        /* enforcement */ EnforcementMode::Strict,
        /* sandbox_mode */ false,
        // Installed capsules are pre-consented artifacts; bypass the sandbox
        // opt-in / execution-plan consent gate that applies to `ato run`.
        /* dangerously_skip_permissions */
        true,
        /* compatibility_fallback */ None,
        /* provider_toolchain */ ProviderToolchain::Auto,
        /* use_existing_toml */ None,
        /* explicit_commit */ None,
        /* assume_yes */ args.yes,
        /* verbose */ args.verbose,
        /* agent_mode */ RunAgentMode::Auto,
        /* agent_local_root */ None,
        /* keep_failed_artifacts */ false,
        /* auto_fix_mode */ None,
        /* allow_unverified */ false,
        /* read */ vec![],
        /* write */ vec![],
        /* read_write */ vec![],
        /* cwd */ None,
        /* state */ vec![],
        /* managed_state_root */ None,
        /* inject */ vec![],
        /* build_policy */ crate::application::build_materialization::BuildPolicy::IfStale,
        /* cache_strategy_arg */ crate::cli::shared::CacheStrategyArg::Auto,
        /* plan_only */ false,
        Some(lifecycle_ctx),
        /* pinned_revision_output_dir */ Some(pinned_capsule_path),
        reporter,
    )
}

/// Find the single `.capsule` file inside a revision output directory.
///
/// `ato launch` stores revision artifacts as `output/{name}.capsule` — a directory
/// containing exactly one capsule file. `normalize_run_target_after_install` routes
/// a path to `prepare_capsule_target` only when the path itself ends in `.capsule`;
/// passing the parent directory falls through to source inference and fails with
/// `ATO_ERR_AMBIGUOUS_ENTRYPOINT`. This helper extracts the file path so the run
/// pipeline sees a proper capsule target.
///
/// Returns `None` if the directory cannot be read or contains no `.capsule` file
/// (caller falls back to the directory and lets the pipeline surface the error).
fn find_capsule_in_revision_output(output_dir: &std::path::Path) -> Option<PathBuf> {
    std::fs::read_dir(output_dir).ok()?.find_map(|entry| {
        let path = entry.ok()?.path();
        if path
            .extension()
            .map(|ext| ext.eq_ignore_ascii_case("capsule"))
            .unwrap_or(false)
        {
            Some(path)
        } else {
            None
        }
    })
}

/// Scan all installed apps and profiles to find the one matching `profile_key`.
/// Returns `(installed_app_id, profile_id, capsule_handle, install_revision_id)`.
fn find_profile_by_key(
    store: &InstallInstanceStore,
    profile_key: &str,
) -> Option<(
    InstalledAppId,
    ProfileId,
    String,
    capsule_core::foundation::install_lifecycle::InstallRevisionId,
)> {
    let apps = store.list_installed_apps().ok()?;
    for app_id in &apps {
        let profiles = store.list_profiles(app_id).unwrap_or_default();
        for profile_id in &profiles {
            let candidate_key = derive_install_profile_key(app_id, profile_id);
            if candidate_key.as_str() == profile_key {
                let rev_id = store.current_revision(app_id, profile_id).ok()?;
                // Read the app record to get the capsule handle.
                let capsule_handle = store.read_app_record(app_id).ok().and_then(|r| {
                    if r.capsule_handle.is_empty() {
                        // Fallback: reconstruct from publisher/slug.
                        if r.publisher.is_empty() {
                            None
                        } else {
                            Some(format!("{}/{}", r.publisher, r.slug))
                        }
                    } else {
                        Some(r.capsule_handle)
                    }
                })?;
                return Some((app_id.clone(), profile_id.clone(), capsule_handle, rev_id));
            }
        }
    }
    None
}

/// Read the `LaunchProfile` and extract launch-time config supported by the run pipeline.
///
/// Returns `(profile_args, warning_msg)` where `warning_msg` is `Some(...)` if unsupported
/// profile fields (env_refs, secret_refs, port_policy, concurrency_policy, isolation) are
/// non-default — indicating the caller should warn the user.
fn read_profile_launch_config(
    store: &InstallInstanceStore,
    app_id: &InstalledAppId,
    profile_id: &ProfileId,
) -> (Vec<String>, Option<String>) {
    let profile = match store.read_profile(app_id, profile_id) {
        Ok(p) => p,
        Err(_) => return (vec![], None),
    };

    let args = profile.args.clone();

    // Warn if unsupported profile fields are set.
    let mut unsupported = vec![];
    if !profile.env_refs.is_empty() {
        unsupported.push("env_refs");
    }
    if !profile.secret_refs.is_empty() {
        unsupported.push("secret_refs");
    }
    if profile.port_policy != "auto" {
        unsupported.push("port_policy");
    }
    if profile.concurrency_policy != "single" {
        unsupported.push("concurrency_policy");
    }
    if profile.isolation != "default" {
        unsupported.push("isolation");
    }

    let warning = if unsupported.is_empty() {
        None
    } else {
        Some(format!(
            "launch profile has fields not yet supported by `ato launch`: [{}] — they will be ignored",
            unsupported.join(", ")
        ))
    };

    (args, warning)
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::foundation::install_lifecycle::{
        AppRecord, InstallInstanceStore, InstallRevisionId, InstalledAppId, LaunchProfile,
        ProfileId,
    };

    fn make_store() -> (tempfile::TempDir, InstallInstanceStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallInstanceStore::new(dir.path()).unwrap();
        (dir, store)
    }

    fn scaffold_app_with_profile(
        store: &InstallInstanceStore,
        app_id: &InstalledAppId,
        profile_id: &ProfileId,
        rev_id: &InstallRevisionId,
    ) {
        let record = AppRecord {
            installed_app_id: app_id.clone(),
            publisher: "acme".into(),
            slug: "hello".into(),
            capsule_handle: "acme/hello".into(),
            version: "1.0.0".into(),
            installed_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
        };
        store.write_app_record(&record).unwrap();
        store
            .write_profile(
                app_id,
                &LaunchProfile {
                    profile_id: profile_id.clone(),
                    port_policy: "auto".into(),
                    concurrency_policy: "single".into(),
                    isolation: "default".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        store.scaffold_revision(rev_id).unwrap();
        store
            .set_current_revision(app_id, profile_id, rev_id)
            .unwrap();
    }

    /// `find_profile_by_key` returns None for an unknown key.
    #[test]
    fn find_profile_by_key_returns_none_for_unknown_key() {
        let (_dir, store) = make_store();
        assert!(find_profile_by_key(&store, "ipk_unknown_key").is_none());
    }

    /// `find_profile_by_key` resolves the correct (app, profile, handle, rev).
    #[cfg(unix)]
    #[test]
    fn find_profile_by_key_resolves_correct_profile() {
        let (_dir, store) = make_store();
        let app_id = InstalledAppId::new("app_abc123def456789012345678901234");
        let profile_id = ProfileId::new("default");
        let rev_id = InstallRevisionId::new("rev_aabbccdd");
        scaffold_app_with_profile(&store, &app_id, &profile_id, &rev_id);

        let ipk = derive_install_profile_key(&app_id, &profile_id);
        let result = find_profile_by_key(&store, ipk.as_str());
        assert!(result.is_some(), "should find the installed profile");
        let (found_app, found_profile, _handle, found_rev) = result.unwrap();
        assert_eq!(found_app, app_id);
        assert_eq!(found_profile, profile_id);
        assert_eq!(found_rev, rev_id);
    }

    /// After rollback (current_revision swapped back to rev1), `find_profile_by_key`
    /// must return the old revision, not the latest one.
    #[cfg(unix)]
    #[test]
    fn find_profile_by_key_uses_current_revision_after_rollback() {
        let (_dir, store) = make_store();
        let app_id = InstalledAppId::new("app_rollback12345678901234567890ab");
        let profile_id = ProfileId::new("default");
        let rev1 = InstallRevisionId::new("rev_old000");
        let rev2 = InstallRevisionId::new("rev_new000");

        scaffold_app_with_profile(&store, &app_id, &profile_id, &rev1);
        // Install rev2 (simulates update).
        store.scaffold_revision(&rev2).unwrap();
        store
            .set_current_revision(&app_id, &profile_id, &rev2)
            .unwrap();

        let ipk = derive_install_profile_key(&app_id, &profile_id);

        // Verify rev2 is current.
        let (_, _, _, rev) = find_profile_by_key(&store, ipk.as_str()).unwrap();
        assert_eq!(
            rev, rev2,
            "before rollback, current revision should be rev_new"
        );

        // Rollback to rev1.
        store
            .set_current_revision(&app_id, &profile_id, &rev1)
            .unwrap();

        let (_, _, _, rev_after_rollback) = find_profile_by_key(&store, ipk.as_str()).unwrap();
        assert_eq!(
            rev_after_rollback, rev1,
            "after rollback, ato launch must use rev_old, not rev_new"
        );
    }

    /// `read_profile_launch_config` returns empty args and no warning for a default profile.
    #[cfg(unix)]
    #[test]
    fn read_profile_launch_config_no_warning_for_default_profile() {
        let (_dir, store) = make_store();
        let app_id = InstalledAppId::new("app_default_profile_test_1234567");
        let profile_id = ProfileId::new("default");
        let rev_id = InstallRevisionId::new("rev_default");
        scaffold_app_with_profile(&store, &app_id, &profile_id, &rev_id);

        let (args, warning) = read_profile_launch_config(&store, &app_id, &profile_id);
        assert!(args.is_empty(), "default profile has no args");
        assert!(
            warning.is_none(),
            "default profile should produce no warning"
        );
    }

    /// `read_profile_launch_config` warns when unsupported profile fields are set.
    #[cfg(unix)]
    #[test]
    fn read_profile_launch_config_warns_for_env_refs() {
        use capsule_core::foundation::install_lifecycle::LaunchProfile;
        let (_dir, store) = make_store();
        let app_id = InstalledAppId::new("app_envref_warning_test_12345678");
        let profile_id = ProfileId::new("default");
        let rev_id = InstallRevisionId::new("rev_envref");

        let record = AppRecord {
            installed_app_id: app_id.clone(),
            publisher: "acme".into(),
            slug: "envref".into(),
            capsule_handle: "acme/envref".into(),
            version: "1.0.0".into(),
            installed_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
        };
        store.write_app_record(&record).unwrap();
        store.scaffold_revision(&rev_id).unwrap();

        let profile = LaunchProfile {
            profile_id: profile_id.clone(),
            env_refs: [("MY_KEY".into(), "${secret:my_key}".into())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        store.write_profile(&app_id, &profile).unwrap();

        let (_, warning) = read_profile_launch_config(&store, &app_id, &profile_id);
        assert!(
            warning.is_some(),
            "should warn when env_refs are present but unsupported"
        );
        assert!(
            warning.unwrap().contains("env_refs"),
            "warning must mention env_refs"
        );
    }

    /// `find_capsule_in_revision_output` returns the `.capsule` file path.
    #[test]
    fn find_capsule_in_output_dir_finds_capsule_file() {
        let dir = tempfile::tempdir().unwrap();
        let capsule_path = dir.path().join("my-app-1.0.0.capsule");
        std::fs::write(&capsule_path, b"fake capsule content").unwrap();

        let found = find_capsule_in_revision_output(dir.path());
        assert_eq!(found, Some(capsule_path), "should return the .capsule file");
    }

    /// `find_capsule_in_revision_output` returns None for an empty directory.
    #[test]
    fn find_capsule_in_output_dir_returns_none_for_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let found = find_capsule_in_revision_output(dir.path());
        assert!(found.is_none(), "empty directory should return None");
    }

    /// `find_capsule_in_revision_output` ignores non-capsule files.
    #[test]
    fn find_capsule_in_output_dir_ignores_non_capsule_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("artifact_manifest.json"), b"{}").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"notes").unwrap();

        let found = find_capsule_in_revision_output(dir.path());
        assert!(
            found.is_none(),
            "directory with only non-capsule files should return None"
        );
    }

    /// `find_capsule_in_revision_output` returns None for a non-existent directory.
    #[test]
    fn find_capsule_in_output_dir_returns_none_for_missing_dir() {
        let found =
            find_capsule_in_revision_output(std::path::Path::new("/nonexistent/path/output"));
        assert!(
            found.is_none(),
            "missing directory should return None (not panic)"
        );
    }
}

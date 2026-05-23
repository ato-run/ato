//! `ato launch <install_profile_key>` — launch an installed app by its stable profile key.
//!
//! Unlike `ato run` (source/session execution), `ato launch` targets the installed-app
//! lifecycle layer: it resolves the profile key → app + profile → current revision →
//! output directory, then bridges into the existing run pipeline.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule_core::common::paths::ato_path_or_workspace_tmp;
use capsule_core::foundation::install_lifecycle::{
    derive_capsule_instance_key, derive_install_profile_key, ExecutionId, InstallInstanceStore,
    InstalledAppId, ProfileId,
};

use crate::cli::commands::run::InstallLifecycleContext;
use crate::install::support::execute_run_command;
use crate::reporters;
use crate::{EnforcementMode, ProviderToolchain, RunAgentMode};

pub(crate) struct LaunchArgs {
    pub(crate) install_profile_key: String,
    pub(crate) env_file: Option<PathBuf>,
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
    let store = InstallInstanceStore::new(&instances_root)
        .context("open install instance store")?;

    // Resolve profile key → (installed_app_id, profile_id, revision output dir).
    let (app_id, profile_id, output_dir) = find_profile_by_key(&store, &args.install_profile_key)
        .with_context(|| {
            format!(
                "install profile key '{}' not found — run `ato install` first",
                args.install_profile_key
            )
        })?;

    // Mint a fresh execution_id and derive the capsule_instance_key.
    let execution_id = ExecutionId::generate();
    let ipk = derive_install_profile_key(&app_id, &profile_id);
    let rev_id = store
        .current_revision(&app_id, &profile_id)
        .with_context(|| {
            format!(
                "could not read current revision for profile key '{}'",
                args.install_profile_key
            )
        })?;
    let cik = derive_capsule_instance_key(&ipk, &rev_id, &execution_id);

    tracing::debug!(
        "launch: app={} profile={} rev={} exec={} cik={}",
        app_id.as_str(),
        profile_id.as_str(),
        rev_id.as_str(),
        execution_id.as_str(),
        cik.as_str(),
    );

    if args.verbose || args.json {
        let info = serde_json::json!({
            "installed_app_id": app_id.as_str(),
            "profile_id": profile_id.as_str(),
            "install_revision_id": rev_id.as_str(),
            "execution_id": execution_id.as_str(),
            "capsule_instance_key": cik.as_str(),
            "output_dir": output_dir.display().to_string(),
        });
        if args.json {
            println!("{}", serde_json::to_string_pretty(&info)?);
        } else {
            eprintln!("[ato launch] resolved: {}", serde_json::to_string(&info)?);
        }
    }

    // Register lifecycle context for session record stamping before entering run pipeline.
    crate::app_control::session::set_install_lifecycle_context(InstallLifecycleContext {
        installed_app_id: app_id.as_str().to_string(),
        install_profile_id: profile_id.as_str().to_string(),
        install_profile_key: ipk.as_str().to_string(),
        install_revision_id: rev_id.as_str().to_string(),
        capsule_instance_key: cik.as_str().to_string(),
    });

    // Bridge into the run pipeline using the materialized output directory.
    execute_run_command(
        output_dir,
        /* target */ None,
        /* args */ vec![],
        /* watch */ false,
        /* background */ false,
        args.nacelle,
        /* registry */ None,
        /* enforcement */ EnforcementMode::Strict,
        /* sandbox_mode */ false,
        /* dangerously_skip_permissions */ false,
        /* compatibility_fallback */ None,
        /* provider_toolchain */ ProviderToolchain::Auto,
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
        /* inject */ vec![],
        /* build_policy */ crate::application::build_materialization::BuildPolicy::IfStale,
        /* cache_strategy_arg */ crate::cli::shared::CacheStrategyArg::Auto,
        /* plan_only */ false,
        /* install_lifecycle_context */
        Some(InstallLifecycleContext {
            installed_app_id: app_id.as_str().to_string(),
            install_profile_id: profile_id.as_str().to_string(),
            install_profile_key: ipk.as_str().to_string(),
            install_revision_id: rev_id.as_str().to_string(),
            capsule_instance_key: cik.as_str().to_string(),
        }),
        reporter,
    )
}

/// Scan all installed apps and profiles to find the one matching `profile_key`.
/// Returns `(installed_app_id, profile_id, output_dir)`.
fn find_profile_by_key(
    store: &InstallInstanceStore,
    profile_key: &str,
) -> Option<(InstalledAppId, ProfileId, PathBuf)> {
    let apps = store.list_installed_apps().ok()?;
    for app_id in &apps {
        let profiles = store.list_profiles(app_id).unwrap_or_default();
        for profile_id in &profiles {
            let candidate_key = derive_install_profile_key(app_id, profile_id);
            if candidate_key.as_str() == profile_key {
                // Found matching profile — get current revision output dir.
                let rev_id = store.current_revision(app_id, profile_id).ok()?;
                let output_dir = store.revision_output_dir(&rev_id);
                if output_dir.exists() {
                    return Some((app_id.clone(), profile_id.clone(), output_dir));
                }
            }
        }
    }
    None
}

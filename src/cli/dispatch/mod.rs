mod config;
mod fetch;
mod inspect;
mod ipc;
mod key;
mod package;
mod profile;
mod project;
mod publish;
mod scaffold;
mod source;

use std::sync::Arc;

use anyhow::Result;

use crate::auth;
use crate::cli::{Cli, Commands};
use crate::commands;
use crate::orchestration::{
    build_validate, catalog_registry, install_command, publish_command, run_install,
    support_command,
};
use crate::project as crate_project;
use crate::reporters;

use self::config::execute_config_command;
use self::fetch::{execute_fetch_command, execute_finalize_command};
use self::inspect::execute_inspect_command;
use self::ipc::execute_ipc_command;
use self::key::execute_key_command;
use self::package::execute_package_command;
use self::profile::execute_profile_command;
use self::project::{execute_project_command, execute_unproject_command};
use self::publish::execute_publish_command;
use self::scaffold::execute_scaffold_command;
use self::source::execute_source_command;

type Reporter = Arc<reporters::CliReporter>;

pub(crate) fn execute(cli: Cli, reporter: Reporter) -> Result<()> {
    let Cli {
        nacelle,
        json,
        command,
    } = cli;

    match command {
        Commands::Run {
            path,
            skill,
            from_skill,
            target,
            watch,
            background,
            nacelle,
            registry,
            state,
            inject,
            enforcement,
            sandbox_mode,
            unsafe_mode_legacy,
            unsafe_bypass_sandbox_legacy,
            dangerously_skip_permissions,
            compatibility_fallback,
            yes,
            keep_failed_artifacts,
            allow_unverified,
        } => run_install::execute_run_like_command(run_install::RunLikeCommandArgs {
            path,
            target,
            watch,
            background,
            nacelle,
            registry,
            state,
            inject,
            enforcement,
            sandbox_mode,
            unsafe_mode_legacy,
            unsafe_bypass_sandbox_legacy,
            dangerously_skip_permissions,
            compatibility_fallback,
            yes,
            keep_failed_artifacts,
            allow_unverified,
            skill,
            from_skill,
            deprecation_warning: None,
            reporter: reporter.clone(),
        }),

        Commands::Engine { command } => {
            support_command::execute_engine_command(command, nacelle, reporter.clone())
        }

        Commands::Registry { command } => catalog_registry::execute_registry_command(command),

        Commands::Setup {
            engine,
            version,
            skip_verify,
        } => support_command::execute_setup_command(engine, version, skip_verify, reporter.clone()),

        Commands::Init => crate_project::init::execute_prompt(
            crate_project::init::PromptArgs { path: None },
            reporter.clone(),
        ),

        Commands::New { name, template } => crate_project::new::execute(
            crate_project::new::NewArgs {
                name,
                template: Some(template),
            },
            reporter.clone(),
        ),

        Commands::Build {
            dir,
            init,
            key,
            standalone,
            force_large_payload,
            enforcement,
            keep_failed_artifacts,
            timings,
            strict_v3,
        } => build_validate::execute_build_like_command(build_validate::BuildLikeCommandArgs {
            dir,
            init,
            key,
            standalone,
            force_large_payload,
            enforcement: enforcement.as_str().to_string(),
            keep_failed_artifacts,
            timings,
            strict_v3,
            json,
            nacelle,
            deprecation_warning: None,
            reporter: reporter.clone(),
        }),

        Commands::Validate {
            path,
            json: command_json,
        } => build_validate::execute_validate_command(path, json || command_json),

        Commands::Update => {
            commands::update::update()?;
            Ok(())
        }

        Commands::Inspect { command } => execute_inspect_command(command, json),

        Commands::Keygen {
            out,
            force,
            json: command_json,
        } => commands::keygen::execute(
            commands::keygen::KeygenArgs {
                out,
                force,
                json: command_json,
            },
            reporter.clone(),
        ),

        Commands::Key { command } => execute_key_command(command, reporter.clone()),

        Commands::Scaffold { command } => execute_scaffold_command(command, reporter.clone()),

        Commands::Sign { target, key, out } => commands::sign::execute(
            commands::sign::SignArgs { target, key, out },
            reporter.clone(),
        ),

        Commands::Verify {
            target,
            sig,
            signer,
            json: command_json,
        } => commands::verify::execute(
            commands::verify::VerifyArgs {
                target,
                sig,
                signer,
                json: command_json,
            },
            reporter.clone(),
        ),

        Commands::Profile { command } => execute_profile_command(command, reporter.clone()),

        Commands::Install {
            slug,
            from_gh_repo,
            registry,
            version,
            default,
            yes,
            skip_verify_legacy,
            allow_unverified,
            output,
            project,
            no_project,
            json,
            keep_failed_artifacts,
        } => install_command::execute_install_command(install_command::InstallCommandArgs {
            slug,
            from_gh_repo,
            registry,
            version,
            default,
            yes,
            skip_verify_legacy,
            allow_unverified,
            output,
            project,
            no_project,
            json,
            keep_failed_artifacts,
        }),

        Commands::Search {
            query,
            category,
            tags,
            limit,
            cursor,
            registry,
            json,
            no_tui,
            show_manifest,
        } => catalog_registry::execute_search_command(catalog_registry::SearchCommandArgs {
            query,
            category,
            tags,
            limit,
            cursor,
            registry,
            json,
            no_tui,
            show_manifest,
        }),

        Commands::Fetch {
            capsule_ref,
            registry,
            version,
            json: command_json,
        } => execute_fetch_command(capsule_ref, registry, version, json || command_json),

        Commands::Finalize {
            fetched_artifact_dir,
            allow_external_finalize,
            output_dir,
            json: command_json,
        } => execute_finalize_command(
            fetched_artifact_dir,
            allow_external_finalize,
            output_dir,
            json || command_json,
        ),

        Commands::Project {
            derived_app_path,
            launcher_dir,
            json: command_json,
            command,
        } => execute_project_command(
            derived_app_path,
            launcher_dir,
            command_json || json,
            command,
        ),

        Commands::Unproject {
            projection_ref,
            json: command_json,
        } => execute_unproject_command(projection_ref, json || command_json),

        Commands::Config { command } => execute_config_command(command, nacelle, reporter.clone()),

        Commands::Publish {
            registry,
            artifact,
            scoped_id,
            allow_existing,
            prepare,
            build,
            deploy,
            legacy_full_publish,
            force_large_payload,
            fix,
            ci,
            dry_run,
            no_tui,
            json,
        } => execute_publish_command(
            publish_command::PublishCommandArgs {
                registry,
                artifact,
                scoped_id,
                allow_existing,
                prepare,
                build,
                deploy,
                legacy_full_publish,
                force_large_payload,
                fix,
                no_tui,
                json,
            },
            ci,
            dry_run,
            force_large_payload,
            json,
            reporter.clone(),
        ),

        Commands::GenCi => commands::gen_ci::execute(reporter.clone()),

        Commands::Package { command } => execute_package_command(command),

        Commands::Source { command } => execute_source_command(command),

        Commands::Ps {
            all,
            json: command_json,
        } => commands::ps::execute(
            commands::ps::PsArgs {
                all,
                json: command_json,
            },
            reporter.clone(),
        ),

        Commands::Stop {
            id,
            name,
            all,
            force,
        } => commands::close::execute(
            commands::close::CloseArgs {
                id,
                name,
                all,
                force,
            },
            reporter.clone(),
        ),

        Commands::Logs {
            id,
            name,
            follow,
            tail,
        } => commands::logs::execute(
            commands::logs::LogsArgs {
                id,
                name,
                follow,
                tail,
            },
            reporter.clone(),
        ),

        Commands::State { command } => support_command::execute_state_command(command),

        Commands::Binding { command } => support_command::execute_binding_command(command),

        Commands::Guest { sync_path } => {
            commands::guest::execute(commands::guest::GuestArgs { sync_path })
        }

        Commands::Ipc { command } => execute_ipc_command(command),

        Commands::Login { token, headless } => {
            let rt = tokio::runtime::Runtime::new()?;
            match token {
                Some(token) => rt.block_on(auth::login_with_token(token)),
                None => rt.block_on(auth::login_with_store_device_flow(headless)),
            }
        }

        Commands::Logout => auth::logout(),

        Commands::Whoami => auth::status(),
    }
}

mod app;
mod binding;
mod config;
mod engine;
mod fetch;
mod inspect;
mod install;
mod ipc;
mod key;
mod package;
mod profile;
mod project;
pub(crate) mod publish;
pub(crate) mod registry;
mod run;
mod scaffold;
mod setup;
mod share;
mod source;
mod state;

use std::sync::Arc;

use anyhow::Result;

use crate::application::ports::OutputPort;
use crate::auth;
use crate::cli::{Cli, Commands};
use crate::commands;
use crate::project as crate_project;
use crate::reporters;

use self::app::execute_app_command;
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
            via,
            yes,
            verbose,
            agent,
            keep_failed_artifacts,
            auto_fix_toml,
            auto_fix_src,
            auto_fix_all,
            allow_unverified,
            read,
            write,
            read_write,
            cwd,
            args,
        } => run::execute_run_like_command(run::RunLikeCommandArgs {
            path,
            target,
            args,
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
            provider_toolchain: via,
            yes,
            verbose,
            agent_mode: agent,
            keep_failed_artifacts,
            auto_fix_mode: crate::GitHubAutoFixMode::from_cli_flags(
                auto_fix_toml,
                auto_fix_src,
                auto_fix_all,
            ),
            allow_unverified,
            read,
            write,
            read_write,
            cwd,
            deprecation_warning: None,
            reporter: Arc::new(reporters::CliReporter::new_run(json)),
        }),

        Commands::Resolve {
            handle,
            target,
            registry,
            json: command_json,
        } => crate::app_control::resolve_handle(
            &handle,
            target.as_deref(),
            registry.as_deref(),
            json || command_json,
        ),

        Commands::Encap {
            path,
            share,
            save_only,
            print_plan,
        } => share::execute_encap_command(share::EncapCommandArgs {
            path,
            share,
            save_only,
            print_plan,
            reporter: reporter.clone(),
        }),

        Commands::Decap { input, into, plan } => {
            share::execute_decap_command(share::DecapCommandArgs {
                input,
                into,
                plan,
                reporter: reporter.clone(),
            })
        }

        Commands::Engine { command } => {
            engine::execute_engine_command(command, nacelle, reporter.clone())
        }

        Commands::Registry { command } => registry::execute_registry_command(command),

        Commands::Setup {
            path,
            registry,
            yes,
            json,
            dry_run,
        } => setup::execute_setup_command(setup::SetupCommandArgs {
            path,
            registry,
            yes,
            json,
            dry_run,
        }),

        Commands::Init { path, yes } => crate_project::init::execute_durable_init(
            crate_project::init::InitArgs {
                path: Some(path),
                yes,
            },
            reporter.clone(),
        ),

        Commands::New { name, template } => {
            let result = crate_project::new::execute(
                crate_project::new::NewArgs {
                    name,
                    template: Some(template),
                },
                reporter.clone(),
            )?;
            if reporter.is_json() {
                println!("{}", serde_json::to_string(&result)?);
            }
            Ok(())
        }

        Commands::Build {
            dir,
            init,
            key,
            standalone,
            force_large_payload,
            paid_large_payload,
            enforcement,
            keep_failed_artifacts,
            timings,
            strict_v3,
        } => {
            let result = crate::commands::build::execute_pack_command(
                dir,
                init,
                key,
                standalone,
                force_large_payload,
                paid_large_payload,
                keep_failed_artifacts,
                strict_v3,
                enforcement.as_str().to_string(),
                reporter.clone(),
                timings,
                json,
                nacelle,
            )?;

            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }

            Ok(())
        }

        Commands::Validate {
            path,
            json: command_json,
        } => {
            crate::commands::validate::execute(path, json || command_json)?;
            Ok(())
        }

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
            auto_fix_toml,
            auto_fix_src,
            auto_fix_all,
        } => install::execute_install_command(install::InstallCommandArgs {
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
            auto_fix_mode: crate::GitHubAutoFixMode::from_cli_flags(
                auto_fix_toml,
                auto_fix_src,
                auto_fix_all,
            ),
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
        } => registry::execute_search_command(registry::SearchCommandArgs {
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
            paid_large_payload,
            finalize_local,
            allow_external_finalize,
            fix,
            ci,
            dry_run,
            no_tui,
            json,
        } => execute_publish_command(
            publish::PublishCommandArgs {
                registry,
                artifact,
                scoped_id,
                allow_existing,
                prepare,
                build,
                deploy,
                legacy_full_publish,
                force_large_payload,
                paid_large_payload,
                finalize_local,
                allow_external_finalize,
                fix,
                no_tui,
                json,
            },
            ci,
            dry_run,
            force_large_payload,
            paid_large_payload,
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

        Commands::App { command } => execute_app_command(command, json),

        Commands::State { command } => state::execute_state_command(command),

        Commands::Binding { command } => binding::execute_binding_command(command),

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

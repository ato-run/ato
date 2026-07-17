mod app;
mod app_update;
mod attest;
mod binding;
mod cache;
mod config;
mod console;
mod engine;
mod explain_hash;
mod fetch;
mod gc;
mod import_cmd;
mod inspect;
mod install;
mod internal;
mod ipc;
mod key;
mod launch;
mod package;
mod profile;
mod project;
pub(crate) mod publish;
mod receipts;
mod reconstruct;
pub(crate) mod registry;
mod replay;
mod revisions;
mod rollback;
mod run;
mod scaffold;
mod secrets;
mod session;
mod setup;
mod share;
mod source;
mod state;

use std::sync::Arc;

use anyhow::Result;

use crate::application::ports::OutputPort;
use crate::auth;
use crate::cli::shared::EncapVisibility;
use crate::cli::workspace::WorkspaceCommands;
use crate::cli::{Cli, Commands};
use crate::commands;
use crate::project as crate_project;
use crate::reporters;

use self::app::execute_app_command;
use self::attest::execute_attest_command;
use self::cache::execute_cache_command;
use self::config::execute_config_command;
use self::explain_hash::execute_explain_hash_command;
use self::fetch::{execute_fetch_command, execute_finalize_command};
use self::import_cmd::execute_import_command;
use self::inspect::execute_inspect_command;
use self::ipc::execute_ipc_command;
use self::key::execute_key_command;
use self::package::execute_package_command;
use self::profile::execute_profile_command;
use self::project::{execute_project_command, execute_unproject_command};
use self::publish::execute_publish_command;
use self::receipts::execute_receipts_command;
use self::replay::execute_replay_command;
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
            entry,
            env_file,
            prompt_env,
            watch,
            background,
            nacelle,
            registry,
            state,
            managed_state_root,
            inject,
            enforcement,
            sandbox_mode,
            unsafe_mode_legacy,
            unsafe_bypass_sandbox_legacy,
            dangerously_skip_permissions,
            compatibility_fallback,
            via,
            use_existing_toml,
            commit,
            cache,
            yes,
            verbose,
            agent,
            keep_failed_artifacts,
            auto_fix_toml,
            auto_fix_src,
            auto_fix_all,
            allow_unverified,
            rebuild,
            no_build,
            plan_only,
            strict_realization,
            oci_compose,
            oci_install_sh,
            read,
            write,
            read_write,
            cwd,
            args,
        } => run::execute_run_like_command(run::RunLikeCommandArgs {
            path,
            target,
            entry,
            env_file,
            prompt_env,
            args,
            watch,
            background,
            nacelle,
            registry,
            state,
            managed_state_root,
            inject,
            enforcement,
            sandbox_mode,
            unsafe_mode_legacy,
            unsafe_bypass_sandbox_legacy,
            dangerously_skip_permissions,
            compatibility_fallback,
            provider_toolchain: via,
            use_existing_toml,
            explicit_commit: commit,
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
            build_policy: crate::application::build_materialization::BuildPolicy::from_flags(
                rebuild, no_build,
            ),
            read,
            write,
            read_write,
            cwd,
            cache_strategy: cache,
            deprecation_warning: None,
            plan_only,
            strict_realization,
            oci_compose,
            oci_install_sh,
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

        Commands::ExplainHash { capsule } => execute_explain_hash_command(&capsule),

        Commands::Import(args) => execute_import_command(args),

        Commands::Cache { command } => execute_cache_command(command),

        Commands::Attest { command } => execute_attest_command(command),

        Commands::Workspace { command } => match command {
            WorkspaceCommands::Share {
                path,
                internal,
                private,
                local,
                print_plan,
                dry_run,
                git_mode,
                tool_runtime,
                allow_dirty,
                yes,
                save_config,
                dev,
            } => {
                let visibility = if internal {
                    EncapVisibility::Internal
                } else if private {
                    EncapVisibility::Private
                } else if local {
                    EncapVisibility::Local
                } else {
                    EncapVisibility::Public
                };
                share::execute_encap_command(share::EncapCommandArgs {
                    path,
                    visibility,
                    print_plan,
                    dry_run,
                    git_mode,
                    tool_runtime,
                    allow_dirty,
                    yes,
                    save_config,
                    dev,
                    reporter: reporter.clone(),
                })
            }

            WorkspaceCommands::Setup {
                input,
                into,
                plan,
                tool_runtime,
                strict,
                dev,
            } => share::execute_decap_command(share::DecapCommandArgs {
                input,
                into,
                plan,
                tool_runtime,
                strict,
                dev,
                reporter: reporter.clone(),
            }),
        },

        Commands::Encap {
            path,
            internal,
            private,
            local,
            print_plan,
            dry_run,
            git_mode,
            tool_runtime,
            allow_dirty,
            yes,
            save_config,
        } => {
            eprintln!("warning: `ato encap` is deprecated. Use `ato workspace share` instead.");
            let visibility = if internal {
                EncapVisibility::Internal
            } else if private {
                EncapVisibility::Private
            } else if local {
                EncapVisibility::Local
            } else {
                EncapVisibility::Public
            };
            share::execute_encap_command(share::EncapCommandArgs {
                path,
                visibility,
                print_plan,
                dry_run,
                git_mode,
                tool_runtime,
                allow_dirty,
                yes,
                save_config,
                dev: false,
                reporter: reporter.clone(),
            })
        }

        Commands::Decap {
            input,
            into,
            plan,
            tool_runtime,
            strict,
        } => {
            eprintln!("warning: `ato decap` is deprecated. Use `ato workspace setup` instead.");
            share::execute_decap_command(share::DecapCommandArgs {
                input,
                into,
                plan,
                tool_runtime,
                strict,
                dev: true,
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

        Commands::Lock {
            path,
            timings,
            json: command_json,
        } => {
            crate::commands::lock::execute(path, timings, json || command_json, reporter.clone())?;
            Ok(())
        }

        Commands::SelfUpdate => {
            commands::update::update()?;
            Ok(())
        }

        Commands::Uninstall {
            purge,
            include_config,
            include_keys,
            dry_run,
            yes,
        } => commands::uninstall::uninstall(commands::uninstall::UninstallOptions {
            purge,
            include_config,
            include_keys,
            dry_run,
            yes,
        }),

        Commands::Inspect { command } => execute_inspect_command(command, json),

        Commands::Replay {
            id,
            strict,
            best_effort,
            json: command_json,
        } => execute_replay_command(id, strict, best_effort, json || command_json, nacelle),

        Commands::Reconstruct {
            id,
            json: command_json,
            execute,
        } => reconstruct::execute_reconstruct_command(id, json || command_json, execute),

        Commands::Receipts { command } => execute_receipts_command(command, json),

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
            from_local,
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
            from_local,
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

        Commands::Launch {
            install_profile_key,
            yes,
            verbose,
            json: command_json,
            nacelle,
            detached_session,
        } => launch::execute_launch_command(
            launch::LaunchArgs {
                install_profile_key,
                yes,
                verbose,
                json: json || command_json,
                nacelle,
                detached_session,
            },
            reporter,
        ),

        Commands::Revisions {
            install_profile_key,
            json: command_json,
        } => revisions::execute_revisions_command(revisions::RevisionsArgs {
            install_profile_key,
            json: json || command_json,
        }),

        Commands::Rollback {
            install_profile_key,
            revision_id,
        } => rollback::execute_rollback_command(rollback::RollbackArgs {
            install_profile_key,
            revision_id,
        }),

        Commands::AppUpdate {
            install_profile_key,
            yes,
            json: command_json,
        } => app_update::execute_app_update_command(app_update::AppUpdateArgs {
            install_profile_key,
            yes,
            json: json || command_json,
        }),

        Commands::Gc {
            dry_run,
            keep_last,
            retention_days,
            json: command_json,
        } => gc::execute_gc_command(gc::GcArgs {
            dry_run,
            keep_last,
            retention_days,
            json: json || command_json,
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
            toml,
            source,
            yes,
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
                toml,
                source,
                yes,
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
            target,
            id,
            name,
            all,
            force,
        } => commands::close::execute(
            commands::close::CloseArgs {
                id: id.or(target),
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

        Commands::Internal { command } => internal::execute_internal_command(command),

        Commands::State { command } => state::execute_state_command(command),

        Commands::Binding { command } => binding::execute_binding_command(command),

        Commands::Guest { sync_path } => {
            commands::guest::execute(commands::guest::GuestArgs { sync_path })
        }

        Commands::Ipc { command } => execute_ipc_command(command),

        Commands::Secrets { command } => secrets::execute_secrets_command(command),

        Commands::Session { command } => session::execute_session_command(command),

        Commands::Runner { command } => {
            let rt = tokio::runtime::Runtime::new()?;
            match command {
                crate::cli::RunnerCommands::Login {
                    api_base,
                    site_base,
                    display_name,
                    public_base_url,
                    headless,
                    enrollment_token,
                } => rt.block_on(crate::application::runner_agent::run_login(
                    api_base,
                    site_base,
                    display_name,
                    public_base_url,
                    headless,
                    enrollment_token,
                )),
                crate::cli::RunnerCommands::Serve {
                    api_base,
                    display_name,
                    public_base_url,
                    proxy_listen,
                    max_slots,
                    public_url_template,
                } => rt.block_on(crate::application::runner_agent::run_serve(
                    api_base,
                    display_name,
                    public_base_url,
                    proxy_listen,
                    max_slots,
                    public_url_template,
                )),
                crate::cli::RunnerCommands::Doctor { profile, json } => {
                    crate::application::gpu_provision::run_doctor_for_profile(&profile, json)
                }
                crate::cli::RunnerCommands::Provision {
                    profile,
                    force,
                    resume,
                    enroll,
                    json,
                    dry_run,
                } => rt.block_on(crate::application::gpu_provision::run_provision(
                    &profile, force, resume, enroll, json, dry_run,
                )),
                crate::cli::RunnerCommands::Setup {
                    fix,
                    yes,
                    artifact_root,
                    api_url,
                    official_preview,
                    public_base_url,
                    max_slots,
                    caddyfile,
                } => {
                    // clap enforces official_preview ⇔ public_base_url via `requires`.
                    let official = official_preview.then(|| {
                        crate::application::runner_bootstrap::official_preview::OfficialPreviewConfig {
                            public_base_url: public_base_url.unwrap_or_default(),
                            max_slots: max_slots.unwrap_or(1),
                            caddyfile_path: caddyfile.unwrap_or_else(|| {
                                crate::application::runner_bootstrap::official_preview::DEFAULT_CADDYFILE_PATH
                                    .to_string()
                            }),
                        }
                    });
                    crate::application::runner_bootstrap::setup::run(
                        crate::application::runner_bootstrap::setup::SetupOptions {
                            fix,
                            yes,
                            artifact_root,
                            api_url,
                            official,
                        },
                    )
                }
                crate::cli::RunnerCommands::Smoke {
                    proxy_listen,
                    keep,
                    json,
                } => rt.block_on(crate::application::runner_bootstrap::smoke::run(
                    crate::application::runner_bootstrap::smoke::SmokeOptions {
                        proxy_listen,
                        keep,
                        json,
                    },
                )),
                crate::cli::RunnerCommands::Enroll {
                    api_url,
                    site_base,
                    display_name,
                    public_base_url,
                    headless,
                    enrollment_token,
                    start,
                } => rt.block_on(crate::application::runner_enroll::run_enroll(
                    crate::application::runner_enroll::EnrollOptions {
                        api_url,
                        site_base,
                        display_name,
                        public_base_url,
                        headless,
                        enrollment_token,
                        start,
                    },
                )),
                crate::cli::RunnerCommands::Status { json } => {
                    rt.block_on(crate::application::runner_enroll::run_status(json))
                }
            }
        }

        Commands::Doctor { target } => match target {
            crate::cli::DoctorTarget::NativeInference { json } => {
                crate::application::native_inference_doctor::run(json)
            }
            crate::cli::DoctorTarget::Disk { json } => crate::application::disk_doctor::run(json),
            crate::cli::DoctorTarget::DesktopRunner { json } => {
                crate::application::desktop_runner::diagnostics::run(json)
            }
            crate::cli::DoctorTarget::Runner { json } => {
                crate::application::runner_bootstrap::doctor::run(json)
            }
        },

        Commands::Login {
            token,
            headless,
            desktop,
        } => {
            let rt = tokio::runtime::Runtime::new()?;
            match token {
                Some(token) => rt.block_on(auth::login_with_token(token)),
                None if desktop => rt.block_on(auth::login_with_store_device_flow_desktop()),
                None => rt.block_on(auth::login_with_store_device_flow(headless)),
            }
        }

        Commands::Logout => auth::logout(),

        Commands::DesktopAuthHandoff => auth::desktop_auth_handoff(),

        Commands::Whoami => auth::status(),

        Commands::Console { command } => console::execute_console_command(command),

        Commands::Community { command } => match command {
            crate::cli::community::CommunityCommands::Submit {
                source,
                toml_path,
                dry_run,
                yes,
            } => {
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(crate::community::submit::execute_submit(
                    &source, &toml_path, dry_run, yes, json,
                ))
            }
            crate::cli::community::CommunityCommands::Receipt { command } => match command {
                crate::cli::community::ReceiptCommands::Upload {
                    capsule_toml_id,
                    receipt,
                    dry_run,
                    yes,
                } => {
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(crate::community::receipt_upload::execute_receipt_upload(
                        &capsule_toml_id,
                        &receipt,
                        dry_run,
                        yes,
                        json,
                    ))
                }
            },
        },
    }
}

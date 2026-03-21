use anyhow::{Context, Result};
use capsule_core::CapsuleReporter;
use clap::{CommandFactory, Parser};
use colored::Colorize;
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

fn print_animated_logo() {
    let logo = r#"
    ___    __       
   /   |  / /_____  
  / /| | / __/ __ \ 
 / ___ |/ /_/ /_/ / 
/_/  |_|\__/\____/  
"#;

    for line in logo.lines() {
        println!("{}", line.cyan().bold());
        io::stdout().flush().unwrap();
        thread::sleep(Duration::from_millis(30));
    }
    println!();
}

struct SidecarCleanup {
    sidecar: Option<common::sidecar::SidecarHandle>,
    reporter: std::sync::Arc<reporters::CliReporter>,
}

impl SidecarCleanup {
    fn new(
        sidecar: Option<common::sidecar::SidecarHandle>,
        reporter: std::sync::Arc<reporters::CliReporter>,
    ) -> Self {
        Self { sidecar, reporter }
    }

    fn stop_now(&mut self) {
        if let Some(sidecar) = self.sidecar.take() {
            if let Err(err) = sidecar.stop() {
                let _ = futures::executor::block_on(
                    self.reporter
                        .warn(format!("⚠️  Failed to stop sidecar: {}", err)),
                );
            }
        }
    }
}

impl Drop for SidecarCleanup {
    fn drop(&mut self) {
        self.stop_now();
    }
}

mod artifact_hash;
mod ato_error_jsonl;
mod auth;
mod binding;
mod capsule_archive;
mod cli;
mod commands;
mod common;
mod consent_store;
mod data_injection;
mod diagnostics;
mod engine_manager;
mod env;
mod error_codes;
mod executors;
mod external_capsule;
mod fs_copy;
mod gen_ci;
mod guest_protocol;
mod inference_feedback;
mod ingress_proxy;
mod init;
mod install;
mod ipc;
mod keygen;
mod local_input;
mod main_support;
mod native_delivery;
mod new;
mod orchestration;
mod payload_guard;
mod preview;
mod process_manager;
mod profile;
mod progressive_ui;
mod provisioner;
mod publish;
mod registry;
mod registry_http;
mod registry_serve;
mod registry_store;
mod registry_url;
mod reporters;
mod runtime_manager;
mod runtime_overrides;
mod runtime_tree;
mod scaffold;
mod scoped_id_prompt;
mod search;
mod sign;
mod skill;
mod skill_resolver;
mod source;
mod state;
mod tui;
mod verify;

pub(crate) use cli::*;
pub(crate) use main_support::*;
pub(crate) use orchestration::build_validate as build_validate_orchestration;
pub(crate) use orchestration::catalog_registry as catalog_registry_orchestration;
pub(crate) use orchestration::install_command as install_command_orchestration;
pub(crate) use orchestration::publish_command as publish_command_orchestration;
pub(crate) use orchestration::run_install as run_install_orchestration;
pub(crate) use orchestration::support_command as support_command_orchestration;
pub(crate) use publish::artifact as publish_artifact;
pub(crate) use publish::ci as publish_ci;
pub(crate) use publish::dry_run as publish_dry_run;
pub(crate) use publish::official as publish_official;
pub(crate) use publish::preflight as publish_preflight;
pub(crate) use publish::prepare as publish_prepare;
pub(crate) use publish::private as publish_private;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let json_mode = args.iter().any(|arg| arg == "--json");
    let command_context = diagnostics::detect_command_context(&args);

    if let Err(err) = run() {
        if json_mode && commands::inspect::try_emit_json_error(&err) {
            std::process::exit(error_codes::EXIT_USER_ERROR);
        }

        if ato_error_jsonl::try_emit_from_anyhow(&err, json_mode) {
            std::process::exit(error_codes::EXIT_USER_ERROR);
        }

        let diagnostic = diagnostics::from_anyhow(&err, command_context);
        let exit_code = diagnostics::map_exit_code(&diagnostic, &err);

        if json_mode {
            if let Ok(payload) = serde_json::to_string(&diagnostic.to_json_envelope()) {
                println!("{}", payload);
            } else {
                let fallback_payload = r#"{"schema_version":"1","status":"error","error":{"code":"E999","name":"internal_error","phase":"internal","message":"failed to serialize error payload","retryable":true,"interactive_resolution":false,"causes":[]}}"#;
                println!("{fallback_payload}");
            }
        } else {
            eprintln!("{:?}", miette::Report::new(diagnostic));
        }

        std::process::exit(exit_code);
    }
}

fn run() -> Result<()> {
    let is_no_args = std::env::args_os().count() == 1;

    if is_no_args {
        print_animated_logo();
        let mut cmd = Cli::command();
        cmd.print_help().context("failed to print CLI help")?;
        println!();
        return Ok(());
    }

    let cli = Cli::parse();
    let reporter = std::sync::Arc::new(reporters::CliReporter::new(cli.json));

    match cli.command {
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
        } => run_install_orchestration::execute_run_like_command(
            run_install_orchestration::RunLikeCommandArgs {
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
            },
        ),

        Commands::Engine { command } => support_command_orchestration::execute_engine_command(
            command,
            cli.nacelle,
            reporter.clone(),
        ),

        Commands::Registry { command } => {
            catalog_registry_orchestration::execute_registry_command(command)
        }

        Commands::Setup {
            engine,
            version,
            skip_verify,
        } => support_command_orchestration::execute_setup_command(
            engine,
            version,
            skip_verify,
            reporter.clone(),
        ),

        Commands::Init => init::execute_prompt(init::PromptArgs { path: None }, reporter.clone()),

        Commands::New { name, template } => new::execute(
            new::NewArgs {
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
        } => build_validate_orchestration::execute_build_like_command(
            build_validate_orchestration::BuildLikeCommandArgs {
                dir,
                init,
                key,
                standalone,
                force_large_payload,
                enforcement: enforcement.as_str().to_string(),
                keep_failed_artifacts,
                timings,
                strict_v3,
                json: cli.json,
                nacelle: cli.nacelle,
                deprecation_warning: None,
                reporter: reporter.clone(),
            },
        ),

        Commands::Validate { path, json } => {
            build_validate_orchestration::execute_validate_command(path, cli.json || json)
        }

        Commands::Update => {
            commands::update::update()?;
            Ok(())
        }

        Commands::Inspect { command } => match command {
            InspectCommands::Requirements {
                target,
                registry,
                json,
            } => {
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    commands::inspect::execute_requirements(target, registry, cli.json || json)
                        .await
                        .map(|_| ())
                        .map_err(anyhow::Error::from)
                })
            }
        },

        Commands::Keygen { out, force, json } => {
            keygen::execute(keygen::KeygenArgs { out, force, json }, reporter.clone())
        }

        Commands::Key { command } => match command {
            KeyCommands::Gen { out, force, json } => {
                keygen::execute(keygen::KeygenArgs { out, force, json }, reporter.clone())
            }
            KeyCommands::Sign { target, key, out } => {
                sign::execute(sign::SignArgs { target, key, out }, reporter.clone())
            }
            KeyCommands::Verify {
                target,
                sig,
                signer,
                json,
            } => verify::execute(
                verify::VerifyArgs {
                    target,
                    sig,
                    signer,
                    json,
                },
                reporter.clone(),
            ),
        },

        Commands::Scaffold {
            command:
                ScaffoldCommands::Docker {
                    manifest,
                    output,
                    output_dir,
                    force,
                },
        } => scaffold::execute_docker(
            scaffold::ScaffoldDockerArgs {
                manifest_path: manifest,
                output_dir,
                output,
                force,
            },
            reporter.clone(),
        ),

        Commands::Sign { target, key, out } => {
            sign::execute(sign::SignArgs { target, key, out }, reporter.clone())
        }

        Commands::Verify {
            target,
            sig,
            signer,
            json,
        } => verify::execute(
            verify::VerifyArgs {
                target,
                sig,
                signer,
                json,
            },
            reporter.clone(),
        ),

        Commands::Profile { command } => match command {
            ProfileCommands::Create {
                name,
                bio,
                avatar,
                key,
                output,
                website,
                github,
                twitter,
            } => profile::execute_create(
                profile::CreateArgs {
                    name,
                    bio,
                    avatar,
                    key,
                    output,
                    website,
                    github,
                    twitter,
                },
                reporter.clone(),
            ),
            ProfileCommands::Show { path, json } => {
                profile::execute_show(profile::ShowArgs { path, json }, reporter.clone())
            }
        },

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
        } => install_command_orchestration::execute_install_command(
            install_command_orchestration::InstallCommandArgs {
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
            },
        ),

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
        } => catalog_registry_orchestration::execute_search_command(
            catalog_registry_orchestration::SearchCommandArgs {
                query,
                category,
                tags,
                limit,
                cursor,
                registry,
                json,
                no_tui,
                show_manifest,
            },
        ),

        Commands::Fetch {
            capsule_ref,
            registry,
            version,
            json,
        } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                if install::is_slug_only_ref(&capsule_ref) {
                    let suggestions =
                        install::suggest_scoped_capsules(&capsule_ref, registry.as_deref(), 5)
                            .await?;
                    if suggestions.is_empty() {
                        anyhow::bail!(
                            "scoped_id_required: '{}' is ambiguous. Use publisher/slug (for example: koh0920/{})",
                            capsule_ref,
                            capsule_ref
                        );
                    }
                    let mut message = format!(
                        "scoped_id_required: '{}' requires publisher scope.\n\nDid you mean one of these?",
                        capsule_ref
                    );
                    for suggestion in suggestions {
                        message.push_str(&format!(
                            "\n  - {}  ({} downloads)",
                            suggestion.scoped_id, suggestion.downloads
                        ));
                    }
                    anyhow::bail!(message);
                }

                let result = native_delivery::execute_fetch(
                    &capsule_ref,
                    registry.as_deref(),
                    version.as_deref(),
                )
                .await?;
                if cli.json || json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("✅ Fetched to: {}", result.cache_dir.display());
                    println!("   Scoped ID: {}", result.scoped_id);
                    println!("   Version:   {}", result.version);
                    println!("   Digest:    {}", result.parent_digest);
                }
                Ok(())
            })
        }

        Commands::Finalize {
            fetched_artifact_dir,
            allow_external_finalize,
            output_dir,
            json,
        } => {
            let result = native_delivery::execute_finalize(
                &fetched_artifact_dir,
                &output_dir,
                allow_external_finalize,
            )?;
            if cli.json || json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("✅ Finalized to: {}", result.output_dir.display());
                println!("   App:      {}", result.derived_app_path.display());
                println!("   Parent:   {}", result.parent_digest);
                println!("   Derived:  {}", result.derived_digest);
            }
            Ok(())
        }

        Commands::Project {
            derived_app_path,
            launcher_dir,
            json,
            command,
        } => match command {
            Some(ProjectCommands::Ls {
                json: subcommand_json,
            }) => {
                let result = native_delivery::execute_project_ls()?;
                if cli.json || json || subcommand_json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else if result.projections.is_empty() {
                    println!("No experimental projections found.");
                } else {
                    for projection in result.projections {
                        let marker = if projection.state == "ok" {
                            "✅"
                        } else {
                            "⚠️"
                        };
                        println!(
                            "{} [{}] {} -> {}",
                            marker,
                            projection.state,
                            projection.projected_path.display(),
                            projection.derived_app_path.display()
                        );
                        println!("   ID:       {}", projection.projection_id);
                        if !projection.problems.is_empty() {
                            println!("   Problems: {}", projection.problems.join(", "));
                        }
                    }
                }
                Ok(())
            }
            None => {
                let derived_app_path = derived_app_path.ok_or_else(|| {
                    anyhow::anyhow!(
                        "ato project requires <DERIVED_APP_PATH> or use `ato project ls` for read-only status"
                    )
                })?;
                let result =
                    native_delivery::execute_project(&derived_app_path, launcher_dir.as_deref())?;
                if cli.json || json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("✅ Projected to: {}", result.projected_path.display());
                    println!("   ID:       {}", result.projection_id);
                    println!("   Target:   {}", result.derived_app_path.display());
                    println!("   State:    {}", result.state);
                    println!("   Metadata: {}", result.metadata_path.display());
                }
                Ok(())
            }
        },

        Commands::Unproject {
            projection_ref,
            json,
        } => {
            let result = native_delivery::execute_unproject(&projection_ref)?;
            if cli.json || json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("✅ Unprojected: {}", result.projected_path.display());
                println!("   ID:      {}", result.projection_id);
                println!("   State:   {}", result.state_before);
                println!(
                    "   Removed: metadata={}, symlink={}",
                    result.removed_metadata, result.removed_projected_path
                );
            }
            Ok(())
        }

        Commands::Config { command } => match command {
            ConfigCommands::Engine { command } => match command {
                ConfigEngineCommands::Features => {
                    support_command_orchestration::execute_engine_command(
                        EngineCommands::Features,
                        cli.nacelle,
                        reporter.clone(),
                    )
                }
                ConfigEngineCommands::Register {
                    name,
                    path,
                    default,
                } => support_command_orchestration::execute_engine_command(
                    EngineCommands::Register {
                        name,
                        path,
                        default,
                    },
                    cli.nacelle,
                    reporter.clone(),
                ),
                ConfigEngineCommands::Install {
                    engine,
                    version,
                    skip_verify,
                } => support_command_orchestration::execute_setup_command(
                    engine,
                    version,
                    skip_verify,
                    reporter.clone(),
                ),
            },
            ConfigCommands::Registry { command } => {
                let mapped = match command {
                    ConfigRegistryCommands::Resolve { domain, json } => {
                        RegistryCommands::Resolve { domain, json }
                    }
                    ConfigRegistryCommands::List { json } => RegistryCommands::List { json },
                    ConfigRegistryCommands::ClearCache => RegistryCommands::ClearCache,
                };
                catalog_registry_orchestration::execute_registry_command(mapped)
            }
        },

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
        } => {
            if ci {
                publish_command_orchestration::execute_publish_ci_command(
                    json,
                    force_large_payload,
                    reporter.clone(),
                )
            } else if dry_run {
                publish_command_orchestration::execute_publish_dry_run_command(
                    json,
                    reporter.clone(),
                )
            } else {
                publish_command_orchestration::execute_publish_command(
                    publish_command_orchestration::PublishCommandArgs {
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
                    reporter.clone(),
                )
            }
        }

        Commands::GenCi => gen_ci::execute(reporter.clone()),

        Commands::Package {
            command:
                PackageCommands::Search {
                    query,
                    category,
                    tags,
                    limit,
                    cursor,
                    registry,
                    json,
                    no_tui,
                    show_manifest,
                },
        } => catalog_registry_orchestration::execute_search_command(
            catalog_registry_orchestration::SearchCommandArgs {
                query,
                category,
                tags,
                limit,
                cursor,
                registry,
                json,
                no_tui,
                show_manifest,
            },
        ),

        Commands::Source { command } => match command {
            SourceCommands::SyncStatus {
                source_id,
                sync_run_id,
                registry,
                json,
            } => catalog_registry_orchestration::execute_source_sync_status_command(
                source_id,
                sync_run_id,
                registry,
                json,
            ),
            SourceCommands::Rebuild {
                source_id,
                reference,
                wait,
                registry,
                json,
            } => catalog_registry_orchestration::execute_source_rebuild_command(
                source_id, reference, wait, registry, json,
            ),
        },

        Commands::Ps { all, json } => {
            commands::ps::execute(commands::ps::PsArgs { all, json }, reporter.clone())
        }

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

        Commands::State { command } => {
            support_command_orchestration::execute_state_command(command)
        }

        Commands::Binding { command } => {
            support_command_orchestration::execute_binding_command(command)
        }

        Commands::Guest { sync_path } => {
            commands::guest::execute(commands::guest::GuestArgs { sync_path })
        }

        Commands::Ipc {
            command: IpcCommands::Status { json },
        } => commands::ipc::run_ipc_status(json),

        Commands::Ipc {
            command: IpcCommands::Start { path, json },
        } => commands::ipc::run_ipc_start(path, json),

        Commands::Ipc {
            command: IpcCommands::Stop { name, force, json },
        } => commands::ipc::run_ipc_stop(name, force, json),

        Commands::Ipc {
            command:
                IpcCommands::Invoke {
                    path,
                    service,
                    method,
                    args,
                    id,
                    max_message_size,
                    json,
                },
        } => commands::ipc::run_ipc_invoke(path, service, method, args, id, max_message_size, json),

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

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;

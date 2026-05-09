use super::*;
use capsule_core::execution_plan::error::AtoExecutionError;
use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::dispatch::publish as publish_command_dispatch;
use crate::dispatch::registry as catalog_registry_dispatch;
use crate::install::support as run_install_dispatch;
use crate::install::support::{
    build_github_manual_intervention_error, build_github_manual_intervention_message,
    can_prompt_interactively, compare_semver, enforce_sandbox_mode_flags,
    ensure_run_auto_install_allowed, github_build_error_manual_review_reason,
    github_build_error_requires_manual_intervention, resolve_installed_capsule_archive_in_store,
    run_blocking_github_install_step, select_capsule_file_in_version, ParsedSemver,
};
use crate::ProviderToolchain;

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn semver_prefers_highest_stable_release() {
    let stable = ParsedSemver::parse("1.2.0").unwrap();
    let prerelease = ParsedSemver::parse("1.2.0-rc1").unwrap();
    let older = ParsedSemver::parse("1.1.9").unwrap();

    assert_eq!(compare_semver(&stable, &prerelease), Ordering::Greater);
    assert_eq!(compare_semver(&stable, &older), Ordering::Greater);
    assert_eq!(compare_semver(&prerelease, &older), Ordering::Greater);
}

#[test]
fn select_capsule_file_is_deterministic() {
    let tmp = tempfile::tempdir().unwrap();
    let version_dir = tmp.path().join("1.0.0");
    std::fs::create_dir_all(&version_dir).unwrap();
    std::fs::write(version_dir.join("zeta.capsule"), b"z").unwrap();
    std::fs::write(version_dir.join("alpha.capsule"), b"a").unwrap();

    let selected = select_capsule_file_in_version(&version_dir)
        .unwrap()
        .unwrap();
    assert_eq!(
        selected.file_name().and_then(|name| name.to_str()),
        Some("alpha.capsule")
    );
}

#[test]
fn resolve_installed_capsule_uses_highest_version() {
    let tmp = tempfile::tempdir().unwrap();
    let slug = "demo-app";
    let slug_dir = tmp.path().join(slug);
    std::fs::create_dir_all(slug_dir.join("0.9.0")).unwrap();
    std::fs::create_dir_all(slug_dir.join("1.2.0-rc1")).unwrap();
    std::fs::create_dir_all(slug_dir.join("1.2.0")).unwrap();

    std::fs::write(slug_dir.join("0.9.0/old.capsule"), b"old").unwrap();
    std::fs::write(slug_dir.join("1.2.0-rc1/preview.capsule"), b"preview").unwrap();
    std::fs::write(slug_dir.join("1.2.0/new.capsule"), b"new").unwrap();

    let resolved = resolve_installed_capsule_archive_in_store(tmp.path(), slug, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        resolved.file_name().and_then(|name| name.to_str()),
        Some("new.capsule")
    );
}

#[test]
fn resolve_installed_capsule_can_target_exact_version() {
    let tmp = tempfile::tempdir().unwrap();
    let slug = "demo-app";
    let slug_dir = tmp.path().join(slug);
    std::fs::create_dir_all(slug_dir.join("1.0.0")).unwrap();
    std::fs::create_dir_all(slug_dir.join("2.0.0")).unwrap();

    std::fs::write(slug_dir.join("1.0.0/rolled-back.capsule"), b"old").unwrap();
    std::fs::write(slug_dir.join("2.0.0/current.capsule"), b"new").unwrap();

    let resolved = resolve_installed_capsule_archive_in_store(tmp.path(), slug, Some("1.0.0"))
        .unwrap()
        .unwrap();
    assert_eq!(
        resolved.file_name().and_then(|name| name.to_str()),
        Some("rolled-back.capsule")
    );
}

#[test]
fn tty_prompt_gate_requires_both_streams() {
    assert!(can_prompt_interactively(true, true));
    assert!(!can_prompt_interactively(true, false));
    assert!(!can_prompt_interactively(false, true));
    assert!(!can_prompt_interactively(false, false));
}

#[test]
fn run_auto_install_gate_requires_yes_or_tty() {
    assert!(ensure_run_auto_install_allowed(false, false, true, true).is_ok());
    assert!(ensure_run_auto_install_allowed(true, false, false, false).is_ok());

    let err = ensure_run_auto_install_allowed(false, false, false, false)
        .expect_err("non-interactive auto-install must fail without --yes");
    assert!(err
        .to_string()
        .contains("Interactive install confirmation requires a TTY"));

    let err = ensure_run_auto_install_allowed(false, true, true, true)
        .expect_err("json mode must require --yes");
    assert!(err
        .to_string()
        .contains("Non-interactive JSON mode requires -y/--yes"));
}

#[test]
fn resolve_run_target_rejects_noncanonical_github_url_input() {
    let reporter = std::sync::Arc::new(reporters::CliReporter::new(false));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let error = runtime
        .block_on(run_install_dispatch::resolve_run_target_or_install(
            PathBuf::from("https://github.com/Koh0920/demo-repo"),
            true,
            ProviderToolchain::Auto,
            None,
            false,
            None,
            false,
            None,
            reporter,
        ))
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("ato run github.com/Koh0920/demo-repo"),
        "error={error:#}"
    );
}

#[test]
fn resolve_run_target_rejects_via_for_local_paths() {
    let reporter = std::sync::Arc::new(reporters::CliReporter::new(false));
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let error = runtime
        .block_on(run_install_dispatch::resolve_run_target_or_install(
            PathBuf::from("."),
            true,
            ProviderToolchain::Uv,
            None,
            false,
            None,
            false,
            None,
            reporter,
        ))
        .expect_err("non-provider target must reject --via");

    assert!(
        error
            .to_string()
            .contains("is only supported for provider-backed"),
        "error={error:#}"
    );
}

#[test]
fn resolve_run_target_requires_yes_or_tty_for_github_repo_install() {
    let error = ensure_run_auto_install_allowed(false, false, false, false)
        .expect_err("non-interactive auto-install must fail without --yes");
    assert!(
        error
            .to_string()
            .contains("Interactive install confirmation requires a TTY"),
        "error={error:#}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn github_install_build_step_runs_outside_async_runtime_worker() {
    let value = run_blocking_github_install_step(|| {
        let runtime = tokio::runtime::Runtime::new()?;
        Ok::<u8, anyhow::Error>(runtime.block_on(async { 7 }))
    })
    .await
    .unwrap();

    assert_eq!(value, 7);
}

#[test]
fn search_tui_gate_requires_tty_and_flags_allowing_tui() {
    assert!(catalog_registry_dispatch::should_use_search_tui(
        true, true, false, false,
    ));
    assert!(!catalog_registry_dispatch::should_use_search_tui(
        false, true, false, false,
    ));
    assert!(!catalog_registry_dispatch::should_use_search_tui(
        true, false, false, false,
    ));
    assert!(!catalog_registry_dispatch::should_use_search_tui(
        true, true, true, false,
    ));
    assert!(!catalog_registry_dispatch::should_use_search_tui(
        true, true, false, true,
    ));
}

#[test]
fn run_command_parses_explicit_state_bindings() {
    let cli = Cli::try_parse_from([
        "ato",
        "run",
        ".",
        "--state",
        "data=/var/lib/ato/persistent/demo",
        "--state",
        "cache=/var/lib/ato/persistent/cache",
    ])
    .expect("parse");

    match cli.command {
        Commands::Run { state, .. } => assert_eq!(
            state,
            vec![
                "data=/var/lib/ato/persistent/demo".to_string(),
                "cache=/var/lib/ato/persistent/cache".to_string()
            ]
        ),
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn run_command_parses_agent_mode() {
    let cli = Cli::try_parse_from(["ato", "run", ".", "--agent", "force"]).expect("parse");

    match cli.command {
        Commands::Run { agent, .. } => assert_eq!(agent, RunAgentMode::Force),
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn run_command_parses_entry_and_env_flags() {
    let cli = Cli::try_parse_from([
        "ato",
        "run",
        "https://ato.run/s/demo",
        "--entry",
        "dashboard",
        "--env-file",
        "./local.env",
        "--prompt-env",
    ])
    .expect("parse");

    match cli.command {
        Commands::Run {
            path,
            entry,
            env_file,
            prompt_env,
            ..
        } => {
            assert_eq!(path, PathBuf::from("https://ato.run/s/demo"));
            assert_eq!(entry.as_deref(), Some("dashboard"));
            assert_eq!(env_file, Some(PathBuf::from("./local.env")));
            assert!(prompt_env);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn encap_command_parses_default_flags() {
    let cli = Cli::try_parse_from(["ato", "encap", "."]).expect("parse");
    match cli.command {
        Commands::Encap {
            path,
            internal,
            private,
            local,
            print_plan,
            ..
        } => {
            assert_eq!(path, PathBuf::from("."));
            assert!(!internal);
            assert!(!private);
            assert!(!local);
            assert!(!print_plan);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn encap_command_parses_internal_flag() {
    let cli = Cli::try_parse_from(["ato", "encap", "--internal"]).expect("parse");
    match cli.command {
        Commands::Encap {
            internal,
            private,
            local,
            ..
        } => {
            assert!(internal);
            assert!(!private);
            assert!(!local);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn encap_command_parses_private_flag() {
    let cli = Cli::try_parse_from(["ato", "encap", "--private"]).expect("parse");
    match cli.command {
        Commands::Encap {
            internal,
            private,
            local,
            ..
        } => {
            assert!(!internal);
            assert!(private);
            assert!(!local);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn encap_command_parses_local_flag() {
    let cli = Cli::try_parse_from(["ato", "encap", "--local"]).expect("parse");
    match cli.command {
        Commands::Encap {
            internal,
            private,
            local,
            ..
        } => {
            assert!(!internal);
            assert!(!private);
            assert!(local);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn encap_command_mutual_exclusion_fails() {
    assert!(Cli::try_parse_from(["ato", "encap", "--internal", "--private"]).is_err());
    assert!(Cli::try_parse_from(["ato", "encap", "--internal", "--local"]).is_err());
    assert!(Cli::try_parse_from(["ato", "encap", "--private", "--local"]).is_err());
}

#[test]
fn encap_command_default_path_is_cwd() {
    let cli = Cli::try_parse_from(["ato", "encap"]).expect("parse");
    match cli.command {
        Commands::Encap { path, .. } => assert_eq!(path, PathBuf::from(".")),
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn decap_command_requires_into_and_parses_plan() {
    let cli = Cli::try_parse_from([
        "ato",
        "decap",
        "https://ato.run/s/demo",
        "--into",
        "./demo",
        "--plan",
    ])
    .expect("parse");
    match cli.command {
        Commands::Decap {
            input, into, plan, ..
        } => {
            assert_eq!(input, "https://ato.run/s/demo");
            assert_eq!(into, PathBuf::from("./demo"));
            assert!(plan);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let error = Cli::try_parse_from(["ato", "decap", "https://ato.run/s/demo"]);
    assert!(error.is_err(), "missing --into must fail");
    let rendered = error.err().expect("parse error").to_string();
    assert!(rendered.contains("--into"));
}

#[test]
fn run_command_parses_provider_toolchain_via_flag() {
    let cli = Cli::try_parse_from(["ato", "run", "npm:tsx", "--via", "pnpm", "--", "--help"])
        .expect("parse");

    match cli.command {
        Commands::Run { via, args, .. } => {
            assert_eq!(via, ProviderToolchain::Pnpm);
            assert_eq!(args, vec!["--help".to_string()]);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn run_command_parses_verbose_flag() {
    let cli = Cli::try_parse_from(["ato", "run", "--verbose", "npm:prettier", "--", "--version"])
        .expect("parse");

    match cli.command {
        Commands::Run {
            verbose,
            path,
            args,
            ..
        } => {
            assert!(verbose);
            assert_eq!(path, PathBuf::from("npm:prettier"));
            assert_eq!(args, vec!["--version".to_string()]);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn run_command_parses_trailing_args_after_separator() {
    let cli =
        Cli::try_parse_from(["ato", "run", "@demo/tool", "--", "--help", "-v"]).expect("parse");

    match cli.command {
        Commands::Run { path, args, .. } => {
            assert_eq!(path, PathBuf::from("@demo/tool"));
            assert_eq!(args, vec!["--help".to_string(), "-v".to_string()]);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn run_command_parses_trailing_args_with_target_flag() {
    let cli = Cli::try_parse_from([
        "ato",
        "run",
        "--target",
        "cli",
        "@demo/tool",
        "--",
        "--help",
    ])
    .expect("parse");

    match cli.command {
        Commands::Run {
            path, target, args, ..
        } => {
            assert_eq!(path, PathBuf::from("@demo/tool"));
            assert_eq!(target.as_deref(), Some("cli"));
            assert_eq!(args, vec!["--help".to_string()]);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn run_command_parses_sandbox_io_grants() {
    let cli = Cli::try_parse_from([
        "ato",
        "run",
        "--sandbox",
        "--read",
        "./in.pdf",
        "--write",
        "./out.md",
        "--read-write",
        "./cache",
        "./tool.py",
        "--",
        "./in.pdf",
    ])
    .expect("parse");

    match cli.command {
        Commands::Run {
            read,
            write,
            read_write,
            args,
            ..
        } => {
            assert_eq!(read, vec!["./in.pdf".to_string()]);
            assert_eq!(write, vec!["./out.md".to_string()]);
            assert_eq!(read_write, vec!["./cache".to_string()]);
            assert_eq!(args, vec!["./in.pdf".to_string()]);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn run_command_parses_cwd_override() {
    let cli = Cli::try_parse_from([
        "ato",
        "run",
        "--cwd",
        "./workspace",
        "./tool.py",
        "--",
        "./input.txt",
    ])
    .expect("parse");

    match cli.command {
        Commands::Run { cwd, args, .. } => {
            assert_eq!(cwd, Some(PathBuf::from("./workspace")));
            assert_eq!(args, vec!["./input.txt".to_string()]);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn run_command_parses_hidden_provider_toolchain_hint() {
    let cli =
        Cli::try_parse_from(["ato", "run", "pypi:markitdown[pdf]", "--via", "uv"]).expect("parse");

    match cli.command {
        Commands::Run { path, via, .. } => {
            assert_eq!(path, PathBuf::from("pypi:markitdown[pdf]"));
            assert_eq!(via, ProviderToolchain::Uv);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn init_command_defaults_to_durable_workspace_materialization() {
    let cli = Cli::try_parse_from(["ato", "init"]).expect("parse");

    match cli.command {
        Commands::Init { path, yes } => {
            assert_eq!(path, PathBuf::from("."));
            assert!(!yes);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn setup_command_defaults_to_project_dependency_fetch() {
    let cli = Cli::try_parse_from(["ato", "setup"]).expect("parse");

    match cli.command {
        Commands::Setup {
            path,
            registry,
            yes,
            json,
            dry_run,
        } => {
            assert_eq!(path, PathBuf::from("."));
            assert!(registry.is_none());
            assert!(!yes);
            assert!(!json);
            assert!(!dry_run);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn state_command_parses_register_and_inspect_forms() {
    let register = Cli::try_parse_from([
        "ato",
        "state",
        "register",
        "--manifest",
        ".",
        "--name",
        "data",
        "--path",
        "/var/lib/ato/persistent/demo",
    ])
    .expect("parse register");

    match register.command {
        Commands::State {
            command:
                StateCommands::Register {
                    manifest,
                    state_name,
                    path,
                    json,
                },
        } => {
            assert_eq!(manifest, PathBuf::from("."));
            assert_eq!(state_name, "data");
            assert_eq!(path, PathBuf::from("/var/lib/ato/persistent/demo"));
            assert!(!json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let inspect =
        Cli::try_parse_from(["ato", "state", "inspect", "state-demo"]).expect("parse inspect");
    match inspect.command {
        Commands::State {
            command: StateCommands::Inspect { state_ref, json },
        } => {
            assert_eq!(state_ref, "state-demo");
            assert!(!json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn app_command_parses_resolve_status_bootstrap_and_repair_forms() {
    let resolve = Cli::try_parse_from([
        "ato",
        "app",
        "resolve",
        "capsule://store/ato/ato-desktop",
        "--target",
        "desktop",
        "--json",
    ])
    .expect("parse app resolve");
    match resolve.command {
        Commands::App {
            command:
                AppCommands::Resolve {
                    handle,
                    target,
                    registry,
                    json,
                },
        } => {
            assert_eq!(handle, "capsule://store/ato/ato-desktop");
            assert_eq!(target.as_deref(), Some("desktop"));
            assert!(registry.is_none());
            assert!(json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let session_start = Cli::try_parse_from([
        "ato",
        "app",
        "session",
        "start",
        "./samples/desky-mock-tauri",
        "--target",
        "desktop",
        "--json",
    ])
    .expect("parse app session start");
    match session_start.command {
        Commands::App {
            command:
                AppCommands::Session {
                    command:
                        SessionCommands::Start {
                            handle,
                            target,
                            json,
                        },
                },
        } => {
            assert_eq!(handle, "./samples/desky-mock-tauri");
            assert_eq!(target.as_deref(), Some("desktop"));
            assert!(json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let session_stop = Cli::try_parse_from([
        "ato",
        "app",
        "session",
        "stop",
        "ato-desktop-session-123",
        "--json",
    ])
    .expect("parse app session stop");
    match session_stop.command {
        Commands::App {
            command:
                AppCommands::Session {
                    command: SessionCommands::Stop { session_id, json },
                },
        } => {
            assert_eq!(session_id, "ato-desktop-session-123");
            assert!(json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let session_watch_parent = Cli::try_parse_from([
        "ato",
        "app",
        "session",
        "watch-parent",
        "ato-desktop-session-123",
        "--parent-pid",
        "4242",
        "--parent-start-time-unix-ms",
        "1700000000000",
        "--poll-ms",
        "50",
    ])
    .expect("parse app session watch-parent");
    match session_watch_parent.command {
        Commands::App {
            command:
                AppCommands::Session {
                    command:
                        SessionCommands::WatchParent {
                            session_id,
                            parent_pid,
                            parent_start_time_unix_ms,
                            poll_ms,
                        },
                },
        } => {
            assert_eq!(session_id, "ato-desktop-session-123");
            assert_eq!(parent_pid, 4242);
            assert_eq!(parent_start_time_unix_ms, Some(1_700_000_000_000));
            assert_eq!(poll_ms, 50);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let status = Cli::try_parse_from(["ato", "app", "status", "ato/ato-desktop", "--json"])
        .expect("parse app status");
    match status.command {
        Commands::App {
            command: AppCommands::Status { package_id, json },
        } => {
            assert_eq!(package_id, "ato/ato-desktop");
            assert!(json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let bootstrap = Cli::try_parse_from([
        "ato",
        "app",
        "bootstrap",
        "ato/ato-desktop",
        "--finalize",
        "--workspace",
        "~/Workspace",
        "--model-tier",
        "balanced",
        "--privacy-mode",
        "strict",
    ])
    .expect("parse app bootstrap");
    match bootstrap.command {
        Commands::App {
            command:
                AppCommands::Bootstrap {
                    package_id,
                    finalize,
                    workspace,
                    model_tier,
                    privacy_mode,
                    json,
                },
        } => {
            assert_eq!(package_id, "ato/ato-desktop");
            assert!(finalize);
            assert_eq!(workspace.as_deref(), Some("~/Workspace"));
            assert_eq!(
                model_tier
                    .map(|value| value.as_str().to_string())
                    .as_deref(),
                Some("balanced")
            );
            assert_eq!(
                privacy_mode
                    .map(|value| value.as_str().to_string())
                    .as_deref(),
                Some("strict")
            );
            assert!(!json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let repair = Cli::try_parse_from([
        "ato",
        "app",
        "repair",
        "ato/ato-desktop",
        "--action",
        "restart-services",
    ])
    .expect("parse app repair");
    match repair.command {
        Commands::App {
            command:
                AppCommands::Repair {
                    package_id,
                    action,
                    json,
                },
        } => {
            assert_eq!(package_id, "ato/ato-desktop");
            assert_eq!(action.as_str(), "restart-services");
            assert!(!json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn inspect_command_parses_lock_preview_diagnostics_and_remediation() {
    let lock =
        Cli::try_parse_from(["ato", "inspect", "lock", "./demo"]).expect("parse inspect lock");
    match lock.command {
        Commands::Inspect {
            command: InspectCommands::Lock { path, json },
        } => {
            assert_eq!(path, PathBuf::from("./demo"));
            assert!(!json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let preview = Cli::try_parse_from(["ato", "inspect", "preview", "--json"])
        .expect("parse inspect preview");
    match preview.command {
        Commands::Inspect {
            command: InspectCommands::Preview { path, json },
        } => {
            assert_eq!(path, PathBuf::from("."));
            assert!(json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let diagnostics =
        Cli::try_parse_from(["ato", "inspect", "diagnostics"]).expect("parse inspect diagnostics");
    match diagnostics.command {
        Commands::Inspect {
            command: InspectCommands::Diagnostics { path, json },
        } => {
            assert_eq!(path, PathBuf::from("."));
            assert!(!json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let remediation = Cli::try_parse_from(["ato", "inspect", "remediation", "./capsule.toml"])
        .expect("parse inspect remediation");
    match remediation.command {
        Commands::Inspect {
            command: InspectCommands::Remediation { path, json },
        } => {
            assert_eq!(path, PathBuf::from("./capsule.toml"));
            assert!(!json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let execution = Cli::try_parse_from([
        "ato",
        "inspect",
        "execution",
        "blake3:abc",
        "--compare",
        "blake3:def",
        "--json",
    ])
    .expect("parse inspect execution");
    match execution.command {
        Commands::Inspect {
            command: InspectCommands::Execution { id, compare, json },
        } => {
            assert_eq!(id, "blake3:abc");
            assert_eq!(compare.as_deref(), Some("blake3:def"));
            assert!(json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn replay_command_parses_explicit_modes() {
    let strict = Cli::try_parse_from(["ato", "replay", "blake3:abc", "--strict"])
        .expect("parse strict replay");
    match strict.command {
        Commands::Replay {
            id,
            strict,
            best_effort,
            json,
        } => {
            assert_eq!(id, "blake3:abc");
            assert!(strict);
            assert!(!best_effort);
            assert!(!json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }

    let best_effort =
        Cli::try_parse_from(["ato", "replay", "blake3:def", "--best-effort", "--json"])
            .expect("parse best-effort replay");
    match best_effort.command {
        Commands::Replay {
            id,
            strict,
            best_effort,
            json,
        } => {
            assert_eq!(id, "blake3:def");
            assert!(!strict);
            assert!(best_effort);
            assert!(json);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn uninstall_command_parses_purge_flags() {
    let cli = Cli::try_parse_from([
        "ato",
        "uninstall",
        "--purge",
        "--include-config",
        "--include-keys",
        "--dry-run",
        "--yes",
    ])
    .expect("parse uninstall");

    match cli.command {
        Commands::Uninstall {
            purge,
            include_config,
            include_keys,
            dry_run,
            yes,
        } => {
            assert!(purge);
            assert!(include_config);
            assert!(include_keys);
            assert!(dry_run);
            assert!(yes);
        }
        other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
    }
}

#[test]
fn uninstall_command_requires_purge_for_sensitive_flags() {
    assert!(Cli::try_parse_from(["ato", "uninstall", "--include-config"]).is_err());
    assert!(Cli::try_parse_from(["ato", "uninstall", "--include-keys"]).is_err());
}

#[test]
fn parse_sha256_for_artifact_supports_sha256sums_format() {
    let body = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  nacelle-v1.2.3-darwin-arm64
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  nacelle-v1.2.3-linux-x64
";
    let parsed = crate::engine_manager::parse_sha256_for_artifact(body, "nacelle-v1.2.3-linux-x64");
    assert_eq!(
        parsed.as_deref(),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
}

#[test]
fn parse_sha256_for_artifact_supports_bsd_style_format() {
    let body = "SHA256 (nacelle-v1.2.3-darwin-arm64) = CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
    let parsed =
        crate::engine_manager::parse_sha256_for_artifact(body, "nacelle-v1.2.3-darwin-arm64");
    assert_eq!(
        parsed.as_deref(),
        Some("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
    );
}

#[test]
fn extract_first_sha256_hex_reads_single_file_checksum() {
    let body = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd  nacelle-v1.2.3-darwin-arm64";
    let parsed = crate::engine_manager::extract_first_sha256_hex(body);
    assert_eq!(
        parsed.as_deref(),
        Some("dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd")
    );
}

#[test]
fn dangerous_skip_permissions_requires_explicit_opt_in_env() {
    let _guard = env_lock().lock().expect("env lock");
    std::env::remove_var("CAPSULE_ALLOW_UNSAFE");

    let reporter = std::sync::Arc::new(reporters::CliReporter::new(true));
    let err = enforce_sandbox_mode_flags(EnforcementMode::Strict, false, true, None, reporter)
        .expect_err("must fail closed without env opt-in");
    assert!(err
        .to_string()
        .contains("--dangerously-skip-permissions requires CAPSULE_ALLOW_UNSAFE=1"));
}

#[test]
fn dangerous_skip_permissions_allows_with_explicit_opt_in_env() {
    let _guard = env_lock().lock().expect("env lock");
    std::env::set_var("CAPSULE_ALLOW_UNSAFE", "1");

    let reporter = std::sync::Arc::new(reporters::CliReporter::new(true));
    let result = enforce_sandbox_mode_flags(EnforcementMode::Strict, false, true, None, reporter);
    assert!(result.is_ok());

    std::env::remove_var("CAPSULE_ALLOW_UNSAFE");
}

#[test]
fn compatibility_fallback_is_mutually_exclusive_with_dangerous_mode() {
    let reporter = std::sync::Arc::new(reporters::CliReporter::new(true));
    let err = enforce_sandbox_mode_flags(
        EnforcementMode::Strict,
        false,
        true,
        Some(CompatibilityFallbackBackend::Host),
        reporter,
    )
    .expect_err("must reject overlapping fallback and dangerous mode");

    assert!(err.to_string().contains("mutually exclusive"));
}

#[test]
fn publish_private_status_message_for_build_path() {
    assert_eq!(
        publish_command_dispatch::publish_private_status_message(
            publish_command_dispatch::PublishTargetMode::CustomDirect,
            false,
        ),
        "📦 Building capsule artifact for private registry publish..."
    );
}

#[test]
fn publish_private_status_message_for_upload_path() {
    assert_eq!(
        publish_command_dispatch::publish_private_status_message(
            publish_command_dispatch::PublishTargetMode::CustomDirect,
            true,
        ),
        "📤 Publishing provided artifact to private registry..."
    );
}

#[test]
fn publish_private_status_message_for_personal_dock_build_path() {
    assert_eq!(
        publish_command_dispatch::publish_private_status_message(
            publish_command_dispatch::PublishTargetMode::PersonalDockDirect,
            false,
        ),
        "📦 Building capsule artifact for Personal Dock publish..."
    );
}

#[test]
fn publish_private_status_message_for_personal_dock_upload_path() {
    assert_eq!(
        publish_command_dispatch::publish_private_status_message(
            publish_command_dispatch::PublishTargetMode::PersonalDockDirect,
            true,
        ),
        "📤 Publishing provided artifact to Personal Dock..."
    );
}

#[test]
fn publish_private_start_summary_line_build_path() {
    let line = publish_command_dispatch::publish_private_start_summary_line(
        publish_command_dispatch::PublishTargetMode::CustomDirect,
        "http://127.0.0.1:8787",
        "build",
        "local/demo-app",
        "1.2.3",
        false,
    );
    assert!(line.contains("registry=http://127.0.0.1:8787"));
    assert!(line.contains("source=build"));
    assert!(line.contains("scoped_id=local/demo-app"));
    assert!(line.contains("version=1.2.3"));
    assert!(line.contains("allow_existing=false"));
}

#[test]
fn publish_private_start_summary_line_artifact_path() {
    let line = publish_command_dispatch::publish_private_start_summary_line(
        publish_command_dispatch::PublishTargetMode::CustomDirect,
        "http://127.0.0.1:8787",
        "artifact",
        "team-x/demo-app",
        "1.2.3",
        true,
    );
    assert!(line.contains("source=artifact"));
    assert!(line.contains("allow_existing=true"));
}

#[test]
fn publish_private_start_summary_line_marks_personal_dock_target() {
    let line = publish_command_dispatch::publish_private_start_summary_line(
        publish_command_dispatch::PublishTargetMode::PersonalDockDirect,
        "https://api.ato.run",
        "artifact",
        "koh0920/demo-app",
        "1.2.3",
        false,
    );
    assert!(line.contains("🔎 dock publish target"));
}

fn test_publish_args() -> publish_command_dispatch::PublishCommandArgs {
    publish_command_dispatch::PublishCommandArgs {
        registry: Some("http://127.0.0.1:8787".to_string()),
        artifact: None,
        scoped_id: None,
        allow_existing: false,
        prepare: false,
        build: false,
        deploy: false,
        legacy_full_publish: false,
        force_large_payload: false,
        paid_large_payload: false,
        finalize_local: false,
        allow_external_finalize: false,
        fix: false,
        no_tui: false,
        json: true,
    }
}

#[test]
fn publish_phase_selection_defaults_to_all_for_private() {
    let selected = publish_command_dispatch::select_publish_phases(
        false, false, false, false, false, false, false,
    );
    assert_eq!(
        selected.start,
        publish_command_dispatch::PublishPhaseBoundary::Prepare
    );
    assert_eq!(
        selected.stop,
        publish_command_dispatch::PublishPhaseBoundary::Publish
    );
    assert!(!selected.explicit_filter);
}

#[test]
fn publish_phase_selection_respects_filter_flags() {
    let selected = publish_command_dispatch::select_publish_phases(
        true, false, true, true, false, false, false,
    );
    assert_eq!(
        selected.start,
        publish_command_dispatch::PublishPhaseBoundary::Prepare
    );
    assert_eq!(
        selected.stop,
        publish_command_dispatch::PublishPhaseBoundary::Publish
    );
    assert!(selected.explicit_filter);
}

#[test]
fn publish_phase_selection_defaults_to_deploy_for_official() {
    let selected = publish_command_dispatch::select_publish_phases(
        false, false, false, true, false, false, false,
    );
    assert_eq!(
        selected.start,
        publish_command_dispatch::PublishPhaseBoundary::Publish
    );
    assert_eq!(
        selected.stop,
        publish_command_dispatch::PublishPhaseBoundary::Publish
    );
    assert!(!selected.explicit_filter);
}

#[test]
fn publish_phase_selection_legacy_full_publish_keeps_all_for_official() {
    let selected = publish_command_dispatch::select_publish_phases(
        false, false, false, true, true, false, false,
    );
    assert_eq!(
        selected.start,
        publish_command_dispatch::PublishPhaseBoundary::Prepare
    );
    assert_eq!(
        selected.stop,
        publish_command_dispatch::PublishPhaseBoundary::Publish
    );
    assert!(!selected.explicit_filter);
}

#[test]
fn publish_phase_selection_artifact_build_is_verify_only() {
    let selected = publish_command_dispatch::select_publish_phases(
        false, true, false, false, false, true, false,
    );
    assert_eq!(
        selected.start,
        publish_command_dispatch::PublishPhaseBoundary::Verify
    );
    assert_eq!(
        selected.stop,
        publish_command_dispatch::PublishPhaseBoundary::Verify
    );
    assert!(selected.runs_verify());
    assert!(!selected.runs_install());
    assert!(!selected.runs_dry_run());
    assert!(!selected.runs_publish());
}

#[test]
fn publish_phase_selection_official_deploy_only_keeps_publish_only_even_with_artifact() {
    let selected = publish_command_dispatch::select_publish_phases(
        false, false, true, true, false, true, false,
    );
    assert_eq!(
        selected.start,
        publish_command_dispatch::PublishPhaseBoundary::Publish
    );
    assert_eq!(
        selected.stop,
        publish_command_dispatch::PublishPhaseBoundary::Publish
    );
    assert!(!selected.runs_install());
    assert!(!selected.runs_dry_run());
    assert!(selected.runs_publish());
}

#[test]
fn publish_phase_selection_private_deploy_runs_install_and_dry_run() {
    let selected = publish_command_dispatch::select_publish_phases(
        false, false, true, false, false, false, false,
    );
    assert!(selected.runs_prepare());
    assert!(selected.runs_build());
    assert!(selected.runs_verify());
    assert!(selected.runs_install());
    assert!(selected.runs_dry_run());
    assert!(selected.runs_publish());
}

#[test]
fn publish_phase_selection_official_default_skips_install_and_dry_run() {
    let selected = publish_command_dispatch::select_publish_phases(
        false, false, false, true, false, false, false,
    );
    assert!(!selected.runs_prepare());
    assert!(!selected.runs_build());
    assert!(!selected.runs_verify());
    assert!(!selected.runs_install());
    assert!(!selected.runs_dry_run());
    assert!(selected.runs_publish());
}

#[test]
fn resolve_publish_target_prefers_cli_registry_over_other_sources() {
    let resolved = publish_command_dispatch::resolve_publish_target_from_sources(
        Some("https://api.ato.run"),
        Some("http://127.0.0.1:8787"),
        Some("koh0920"),
    )
    .expect("resolve");

    assert_eq!(resolved.registry_url, "https://api.ato.run");
    assert_eq!(
        resolved.mode,
        publish_command_dispatch::PublishTargetMode::OfficialCi
    );
}

#[test]
fn resolve_publish_target_uses_manifest_before_logged_in_default() {
    let resolved = publish_command_dispatch::resolve_publish_target_from_sources(
        None,
        Some("http://127.0.0.1:8787"),
        Some("koh0920"),
    )
    .expect("resolve");

    assert_eq!(resolved.registry_url, "http://127.0.0.1:8787");
    assert_eq!(
        resolved.mode,
        publish_command_dispatch::PublishTargetMode::CustomDirect
    );
}

#[test]
fn resolve_publish_target_uses_logged_in_default_when_no_explicit_target_exists() {
    let resolved =
        publish_command_dispatch::resolve_publish_target_from_sources(None, None, Some("koh0920"))
            .expect("resolve");

    assert_eq!(resolved.registry_url, "https://api.ato.run");
    assert_eq!(
        resolved.mode,
        publish_command_dispatch::PublishTargetMode::PersonalDockDirect
    );
    assert_eq!(resolved.publisher_handle.as_deref(), Some("koh0920"));
}

#[test]
fn resolve_publish_target_errors_without_login_or_registry_override() {
    let err = publish_command_dispatch::resolve_publish_target_from_sources(None, None, None)
        .expect_err("must fail without any publish target");

    assert!(err.to_string().contains("Run `ato login`"));
    assert!(err.to_string().contains("--registry https://api.ato.run"));
}

#[test]
fn resolve_publish_target_rejects_legacy_dock_registry_urls() {
    let err = publish_command_dispatch::resolve_publish_target_from_sources(
        Some("https://ato.run/d/koh0920"),
        None,
        Some("koh0920"),
    )
    .expect_err("must reject legacy dock url");
    assert!(err.to_string().contains("https://api.ato.run"));
    assert!(err.to_string().contains("/d/<handle>"));
}

#[test]
fn is_legacy_dock_publish_registry_detects_dock_path_prefix() {
    assert!(publish_command_dispatch::is_legacy_dock_publish_registry(
        "https://ato.run/d/koh0920"
    ));
    assert!(publish_command_dispatch::is_legacy_dock_publish_registry(
        "https://ato.run/publish/d/koh0920"
    ));
    assert!(!publish_command_dispatch::is_legacy_dock_publish_registry(
        "https://api.ato.run"
    ));
}

#[test]
fn publish_validate_rejects_allow_existing_without_deploy() {
    let mut args = test_publish_args();
    args.allow_existing = true;
    let selected = publish_command_dispatch::select_publish_phases(
        false, true, false, false, false, false, false,
    );
    let err = publish_command_dispatch::validate_publish_phase_options(&args, selected, false)
        .expect_err("must fail closed");
    assert!(err.to_string().contains("--allow-existing"));
}

#[test]
fn publish_validate_rejects_fix_for_private_registry() {
    let mut args = test_publish_args();
    args.fix = true;
    let selected = publish_command_dispatch::select_publish_phases(
        false, false, true, false, false, false, false,
    );
    let err = publish_command_dispatch::validate_publish_phase_options(&args, selected, false)
        .expect_err("must fail closed");
    assert!(err.to_string().contains("--fix"));
}

#[test]
fn publish_validate_allows_private_deploy_from_source() {
    let args = test_publish_args();
    let selected = publish_command_dispatch::select_publish_phases(
        false, false, true, false, false, false, false,
    );
    publish_command_dispatch::validate_publish_phase_options(&args, selected, false)
        .expect("source deploy should auto-resolve earlier phases");
}

#[test]
fn publish_validate_rejects_artifact_prepare_stop_point() {
    let mut args = test_publish_args();
    args.artifact = Some(std::path::PathBuf::from("demo.capsule"));
    args.prepare = true;
    let selected = publish_command_dispatch::select_publish_phases(
        true, false, false, false, false, true, false,
    );
    let err = publish_command_dispatch::validate_publish_phase_options(&args, selected, false)
        .expect_err("artifact + prepare must fail closed");
    assert!(err.to_string().contains("cannot be combined"));
}

#[test]
fn github_manual_intervention_extracts_required_env() {
    let required = preview::required_env_from_preview_toml(
        r#"
[env]
required = ["DATABASE_URL", "REDIS_URL"]
"#,
    );

    assert_eq!(required, vec!["DATABASE_URL", "REDIS_URL"]);
}

#[test]
fn github_manual_intervention_prefers_root_required_env() {
    let required = preview::required_env_from_preview_toml(
        r#"
required_env = ["DATABASE_URL", "REDIS_URL"]

[env]
required = ["LEGACY_ONLY"]
"#,
    );

    assert_eq!(required, vec!["DATABASE_URL", "REDIS_URL"]);
}

#[test]
fn github_manual_intervention_message_mentions_manifest_and_repo() {
    let draft = install::GitHubInstallDraftResponse {
        repo: install::GitHubInstallDraftRepo {
            owner: "octocat".to_string(),
            repo: "hello-world".to_string(),
            full_name: "octocat/hello-world".to_string(),
            default_branch: "main".to_string(),
        },
        capsule_toml: install::GitHubInstallDraftCapsuleToml { exists: false },
        repo_ref: "octocat/hello-world".to_string(),
        proposed_run_command: None,
        proposed_install_command: "ato run github.com/octocat/hello-world".to_string(),
        resolved_ref: install::GitHubInstallDraftResolvedRef {
            ref_name: "main".to_string(),
            sha: "deadbeef".to_string(),
        },
        manifest_source: "inferred".to_string(),
        preview_toml: Some(
            r#"
[env]
required = ["DATABASE_URL"]
"#
            .to_string(),
        ),
        capsule_hint: Some(install::GitHubInstallDraftHint {
            confidence: "medium".to_string(),
            warnings: vec!["外部DBの準備が必要です。".to_string()],
            launchability: Some("manual_review".to_string()),
        }),
        inference_mode: Some("rules".to_string()),
        retryable: false,
    };

    let message = build_github_manual_intervention_message(
        "github.com/octocat/hello-world",
        &draft,
        std::path::Path::new("/repo/.ato/source-inference/attempt/capsule.toml"),
        "Smoke failed",
    );

    assert!(message.contains("manual intervention required"));
    assert!(message.contains("DATABASE_URL"));
    assert!(message.contains("github.com/octocat/hello-world"));
    assert!(message.contains("/repo/.ato/source-inference/attempt/capsule.toml"));
}

#[test]
fn github_build_error_requires_manual_intervention_for_missing_uv_lock() {
    let error = anyhow::anyhow!(
        "uv.lock is missing for '/tmp/demo/pyproject.toml'. Generate it with `uv lock`."
    );

    assert!(github_build_error_requires_manual_intervention(&error));
    assert!(github_build_error_manual_review_reason(&error).contains("uv.lock"));
}

#[test]
fn github_build_error_requires_manual_intervention_for_stale_bun_lock() {
    let error = anyhow::anyhow!(
        "provision command failed with exit code 1: bun install --frozen-lockfile\nerror: lockfile had changes, but lockfile is frozen"
    );

    assert!(github_build_error_requires_manual_intervention(&error));
    assert!(
        github_build_error_manual_review_reason(&error).contains("bun install --frozen-lockfile")
    );
}

#[test]
fn github_manual_intervention_returns_e103_for_required_env_failure() {
    let draft = install::GitHubInstallDraftResponse {
        repo: install::GitHubInstallDraftRepo {
            owner: "octocat".to_string(),
            repo: "hello-world".to_string(),
            full_name: "octocat/hello-world".to_string(),
            default_branch: "main".to_string(),
        },
        capsule_toml: install::GitHubInstallDraftCapsuleToml { exists: false },
        repo_ref: "octocat/hello-world".to_string(),
        proposed_run_command: None,
        proposed_install_command: "ato run github.com/octocat/hello-world".to_string(),
        resolved_ref: install::GitHubInstallDraftResolvedRef {
            ref_name: "main".to_string(),
            sha: "deadbeef".to_string(),
        },
        manifest_source: "inferred".to_string(),
        preview_toml: Some("required_env = [\"DATABASE_URL\"]\n".to_string()),
        capsule_hint: None,
        inference_mode: Some("rules".to_string()),
        retryable: false,
    };
    let tempdir = tempfile::tempdir().expect("tempdir");
    let manifest_path = tempdir.path().join("capsule.toml");

    let err = build_github_manual_intervention_error(
        &manifest_path,
        "github.com/octocat/hello-world",
        &draft,
        "DATABASE_URL is required",
    )
    .expect("manual intervention error");
    let execution_err = err
        .downcast_ref::<AtoExecutionError>()
        .expect("ato execution error");
    assert_eq!(execution_err.name, "missing_required_env");
}

#[test]
fn github_manual_intervention_returns_e104_for_lockfile_failure() {
    let draft = install::GitHubInstallDraftResponse {
        repo: install::GitHubInstallDraftRepo {
            owner: "octocat".to_string(),
            repo: "hello-world".to_string(),
            full_name: "octocat/hello-world".to_string(),
            default_branch: "main".to_string(),
        },
        capsule_toml: install::GitHubInstallDraftCapsuleToml { exists: false },
        repo_ref: "octocat/hello-world".to_string(),
        proposed_run_command: None,
        proposed_install_command: "ato run github.com/octocat/hello-world".to_string(),
        resolved_ref: install::GitHubInstallDraftResolvedRef {
            ref_name: "main".to_string(),
            sha: "deadbeef".to_string(),
        },
        manifest_source: "inferred".to_string(),
        preview_toml: Some("required_env = []\n".to_string()),
        capsule_hint: None,
        inference_mode: Some("rules".to_string()),
        retryable: false,
    };
    let tempdir = tempfile::tempdir().expect("tempdir");
    let manifest_path = tempdir.path().join("capsule.toml");

    let err = build_github_manual_intervention_error(
        &manifest_path,
        "github.com/octocat/hello-world",
        &draft,
        "uv.lock is missing for '/tmp/demo/pyproject.toml'. Generate it with `uv lock`.",
    )
    .expect("manual intervention error");
    let execution_err = err
        .downcast_ref::<AtoExecutionError>()
        .expect("ato execution error");
    assert_eq!(execution_err.name, "dependency_lock_missing");
}

// ---------------------------------------------------------------------------
// #73 PR-C: Canonical LaunchSessionCore / remove opaque supervisor
// ---------------------------------------------------------------------------

/// `start_orchestration_session_supervisor` historically translated
/// `CAPSULE_ALLOW_UNSAFE=1` into an explicit `--dangerously-skip-permissions`
/// argv on the spawned `ato run` child. PR-C removed that injection: the
/// in-process orchestration path carries the gate through the request types
/// and the legacy supervisor inherits the env var directly, so no spawned
/// child should be invoked with that argv from session.rs.
///
/// This is a source-string assertion rather than a behavioral test because
/// the only way to reach the legacy supervisor in PR-C is to set
/// `ATO_LEGACY_SUPERVISOR=1` and spawn a child `ato run`; unit tests cannot
/// observe that argv without an integration harness.
#[test]
fn pr_c_session_does_not_inject_dangerously_skip_permissions_into_supervisor_argv() {
    let session_rs = include_str!("app_control/session.rs");
    assert!(
        !session_rs.contains("cmd.arg(\"--dangerously-skip-permissions\")"),
        "session.rs must not inject --dangerously-skip-permissions argv into a spawned supervisor; \
         the unsafe gate is carried via env inheritance and ConsumerRunRequest.allow_unsafe (#73 PR-C)."
    );
}

/// `orchestration_supervisor_ready_timeout` (the 180s readiness floor) was
/// removed in PR-C. Both the in-process orchestration path and the legacy
/// supervisor fallback now use `session_ready_timeout`, which honors the
/// manifest's per-target `startup_timeout`.
#[test]
fn pr_c_session_no_longer_defines_orchestration_supervisor_ready_timeout() {
    let session_rs = include_str!("app_control/session.rs");
    assert!(
        !session_rs.contains("fn orchestration_supervisor_ready_timeout("),
        "session.rs must not define orchestration_supervisor_ready_timeout; \
         the 180s readiness floor was removed in #73 PR-C in favor of session_ready_timeout."
    );
}

/// `legacy_supervisor_enabled` is the only switch from the normal in-process
/// path to the legacy nested-`ato run` supervisor. Tested through the
/// pure-logic helper (`legacy_supervisor_enabled_for_value`) to avoid
/// racing other env-mutating tests in the crate.
#[test]
fn pr_c_legacy_supervisor_env_gate_only_accepts_literal_one() {
    use crate::app_control::session_runner::legacy_supervisor_enabled_for_value;
    assert!(
        !legacy_supervisor_enabled_for_value(None),
        "absence of ATO_LEGACY_SUPERVISOR must keep the in-process path",
    );
    assert!(
        legacy_supervisor_enabled_for_value(Some("1")),
        "ATO_LEGACY_SUPERVISOR=1 must select the legacy supervisor fallback",
    );
    // Non-"1" values must not flip the gate (avoid accidental enablement
    // from "true", "yes", etc., which the rest of the codebase does not
    // accept either).
    for v in ["true", "yes", "0", "", "TRUE", "y"] {
        assert!(
            !legacy_supervisor_enabled_for_value(Some(v)),
            "non-\"1\" value {v:?} must not select the legacy supervisor fallback",
        );
    }
}

/// The two orchestrator entry points (`execute_with_client` for foreground
/// `ato run` and `execute_until_ready_and_detach` for session start) must
/// remain distinct public symbols (#73 PR-C). If either is removed or
/// renamed, this `use` stops compiling.
#[test]
fn pr_c_orchestrator_detach_and_foreground_entry_points_remain_public() {
    #[allow(unused_imports)]
    use crate::executors::orchestrator::{execute_until_ready_and_detach, execute_with_client};
}

/// `ato run --help` must point users at the CLI consent-approval
/// command so a non-TTY caller that hits E302 can act without reading
/// the source. Issue #126 — the matching machine-readable surface is
/// the `ATO_INTERACTIVE_REQUIREMENT:` envelope on stderr.
#[test]
fn run_help_mentions_internal_consent_approve_path() {
    use clap::CommandFactory;

    let mut cmd = Cli::command();
    let run_cmd = cmd
        .find_subcommand_mut("run")
        .expect("`run` subcommand must exist");
    let mut buf: Vec<u8> = Vec::new();
    run_cmd
        .write_long_help(&mut buf)
        .expect("render long help for `ato run`");
    let rendered = String::from_utf8(buf).expect("help is utf-8");
    assert!(
        rendered.contains("ato internal consent approve-execution-plan"),
        "expected `ato run --help` to advertise the CLI consent-approval command, got:\n{rendered}",
    );
}

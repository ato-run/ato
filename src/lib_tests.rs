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
        std::path::Path::new("/repo/.tmp/ato-inference/attempt/capsule.toml"),
        "Smoke failed",
    );

    assert!(message.contains("manual intervention required"));
    assert!(message.contains("DATABASE_URL"));
    assert!(message.contains("github.com/octocat/hello-world"));
    assert!(message.contains("/repo/.tmp/ato-inference/attempt/capsule.toml"));
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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule_core::CapsuleReporter;
use serde::Serialize;

use crate::reporters;

pub(crate) fn execute_publish_ci_command(
    json_output: bool,
    force_large_payload: bool,
    reporter: Arc<reporters::CliReporter>,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let result = crate::publish_ci::execute(
            crate::publish_ci::PublishCiArgs {
                json_output,
                force_large_payload,
            },
            reporter.clone(),
        )
        .await?;

        if json_output {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!("✅ Successfully published to Ato Store!");
            println!();
            println!(
                "📦 Capsule:   {} v{}",
                result.capsule_scoped_id, result.version
            );
            if let Some(sha256) = &result.artifact_sha256 {
                println!("🛡️  Integrity: sha256:{}", sha256);
            } else if let Some(blake3) = &result.artifact_blake3 {
                println!("🛡️  Integrity: {}", blake3);
            }
            println!();
            println!("🌐 Store URL:      {}", result.urls.store);
            if let Some(playground) = &result.urls.playground {
                println!("🎮 Playground URL: {}", playground);
            }
            println!();
            println!("👉 Next step: ato run {}", result.capsule_scoped_id);
            println!();
            println!("   Event ID:   {}", result.publish_event_id);
            println!("   Release ID: {}", result.release_id);
            println!("   Artifact:   {}", result.artifact_id);
            println!("   Status:     {}", result.verification_status);
        }
        futures::executor::block_on(
            reporter
                .notify("CI publish completed using GitHub OIDC workflow identity.".to_string()),
        )?;
        Ok(())
    })
}

pub(crate) fn execute_publish_dry_run_command(
    json_output: bool,
    reporter: Arc<reporters::CliReporter>,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let result = crate::publish_dry_run::execute(crate::publish_dry_run::PublishDryRunArgs {
            json_output,
        })
        .await?;

        if json_output {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!("✅ Dry-run successful! Your capsule is ready to be published via CI.");
            println!("   Capsule: {}", result.capsule_name);
            println!("   Version: {}", result.version);
            println!("   Artifact: {}", result.artifact_path.display());
            println!("   Size: {} bytes", result.artifact_size_bytes);
        }
        futures::executor::block_on(
            reporter.notify("Local publish dry-run completed (no upload performed).".to_string()),
        )?;
        Ok(())
    })
}

#[allow(dead_code)]
pub(crate) fn execute_publish_guidance_command(
    json_output: bool,
    registry_url: &str,
) -> Result<()> {
    if json_output {
        let payload = serde_json::json!({
            "ok": false,
            "code": "CI_ONLY_PUBLISH",
            "message": "Official registry publishing is CI-first. Use `ato publish --ci` in GitHub Actions, or `ato publish --dry-run` locally."
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!(
            "❌ Direct local publishing is disabled for official registry ({}).",
            registry_url
        );
        println!();
        println!("Ato uses a strict CI-first publishing model via GitHub Actions (OIDC).");
        println!("This guarantees published capsules match committed source.");
        println!();
        println!("👉 Next steps:");
        println!("  1. Run `ato gen-ci` to generate `.github/workflows/ato-publish.yml`.");
        println!("  2. Commit and tag your release (e.g. `git tag v0.1.0`).");
        println!("  3. Push the tag to GitHub (`git push origin v0.1.0`).");
        println!("  4. GitHub Actions runs `ato publish --ci` automatically.");
        println!();
        println!("💡 Tip: Run `ato publish --dry-run` to validate locally before pushing.");
        println!("💡 Private registry directly publish: `ato publish --registry <url>`");
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct PublishCommandArgs {
    pub(crate) registry: Option<String>,
    pub(crate) artifact: Option<PathBuf>,
    pub(crate) scoped_id: Option<String>,
    pub(crate) allow_existing: bool,
    pub(crate) prepare: bool,
    pub(crate) build: bool,
    pub(crate) deploy: bool,
    pub(crate) legacy_full_publish: bool,
    pub(crate) force_large_payload: bool,
    pub(crate) fix: bool,
    pub(crate) no_tui: bool,
    pub(crate) json: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PublishPhaseSelection {
    pub(crate) prepare: bool,
    pub(crate) build: bool,
    pub(crate) deploy: bool,
    pub(crate) explicit_filter: bool,
}

#[derive(Debug, Clone, Serialize)]
struct PublishPhaseResult {
    name: &'static str,
    selected: bool,
    ok: bool,
    status: &'static str,
    elapsed_ms: u64,
    actionable_fix: Option<String>,
    message: String,
    result_kind: Option<String>,
    skipped_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublishTargetMode {
    PersonalDockDirect,
    OfficialCi,
    CustomDirect,
}

impl PublishTargetMode {
    fn is_official(self) -> bool {
        matches!(self, Self::OfficialCi)
    }

    fn is_personal_dock(self) -> bool {
        matches!(self, Self::PersonalDockDirect)
    }

    fn route_label(self) -> &'static str {
        match self {
            Self::PersonalDockDirect => "personal_dock_direct",
            Self::OfficialCi => "official",
            Self::CustomDirect => "private",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedPublishTarget {
    pub(crate) registry_url: String,
    pub(crate) mode: PublishTargetMode,
    pub(crate) publisher_handle: Option<String>,
}

#[derive(Debug, Clone)]
struct OfficialDeployOutcome {
    route: crate::publish_official::PublishRoutePlan,
    fix_result: crate::publish_official::WorkflowFixResult,
    diagnosis: crate::publish_official::OfficialPublishDiagnosis,
}

pub(crate) fn execute_publish_command(
    args: PublishCommandArgs,
    reporter: Arc<reporters::CliReporter>,
) -> Result<()> {
    let resolved_target = resolve_publish_target(args.registry.clone())?;
    let is_official = resolved_target.mode.is_official();
    let selection = select_publish_phases(
        args.prepare,
        args.build,
        args.deploy,
        is_official,
        args.legacy_full_publish,
    );
    if resolved_target.mode.is_personal_dock() && selection.deploy {
        let _ = crate::auth::require_session_token()?;
    }
    validate_publish_phase_options(&args, selection, is_official)?;
    maybe_warn_legacy_full_publish(&args, selection, is_official);
    let _ = args.no_tui;

    let mut phases = vec![
        new_phase_result("prepare", selection.prepare),
        new_phase_result("build", selection.build),
        new_phase_result("deploy", selection.deploy),
    ];

    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let mut built_artifact_path: Option<PathBuf> = None;
    let mut private_result: Option<crate::publish_private::PublishPrivateResult> = None;
    let mut official_result: Option<OfficialDeployOutcome> = None;

    let private_preview = if selection.deploy && !is_official {
        Some(crate::publish_private::summarize(
            &crate::publish_private::PublishPrivateArgs {
                registry_url: resolved_target.registry_url.clone(),
                publisher_hint: resolved_target.publisher_handle.clone(),
                artifact_path: args.artifact.clone(),
                force_large_payload: args.force_large_payload,
                scoped_id: args.scoped_id.clone(),
                allow_existing: args.allow_existing,
            },
        )?)
    } else {
        None
    };

    if selection.prepare {
        print_phase_line(args.json, "prepare", "RUN", "prepare command detection");
        let started = std::time::Instant::now();
        let prepare_spec = crate::publish_prepare::detect_prepare_command(&cwd)?;
        match prepare_spec {
            Some(spec) => {
                let message = format!("running {}", spec.source.as_label());
                crate::publish_prepare::run_prepare_command(&spec, &cwd, args.json)
                    .context("Failed to run publish prepare command")?;
                phase_mark_ok(
                    &mut phases[0],
                    started.elapsed().as_millis() as u64,
                    message.clone(),
                    None,
                );
                print_phase_line(args.json, "prepare", "OK", &message);
            }
            None => {
                let skipped_reason = "no prepare command configured".to_string();
                if selection.explicit_filter {
                    let fix = "capsule.toml に [build.lifecycle].prepare を設定するか package.json scripts[\"capsule:prepare\"] を追加して再実行してください。".to_string();
                    phase_mark_failed(
                        &mut phases[0],
                        started.elapsed().as_millis() as u64,
                        "--prepare was selected but no prepare command was found".to_string(),
                        Some(fix.clone()),
                    );
                    print_phase_line(args.json, "prepare", "FAIL", "prepare command not found");
                    if !args.json {
                        println!("👉 次に打つコマンド: {}", fix);
                    }
                    anyhow::bail!(
                        "--prepare was selected but no prepare command was found. Set `build.lifecycle.prepare` in capsule.toml or add package.json scripts[\"capsule:prepare\"]."
                    );
                }
                phase_mark_skipped(
                    &mut phases[0],
                    started.elapsed().as_millis() as u64,
                    skipped_reason.clone(),
                    skipped_reason.clone(),
                );
                print_phase_line(args.json, "prepare", "SKIP", &skipped_reason);
            }
        }
    } else {
        phase_mark_skipped(
            &mut phases[0],
            0,
            "prepare phase not selected".to_string(),
            "not selected".to_string(),
        );
        print_phase_line(args.json, "prepare", "SKIP", "not selected");
    }

    if selection.build {
        print_phase_line(args.json, "build", "RUN", "artifact build");
        let started = std::time::Instant::now();
        if args.artifact.is_some() {
            let skipped_reason = "--artifact provided".to_string();
            phase_mark_skipped(
                &mut phases[1],
                started.elapsed().as_millis() as u64,
                "build is skipped when --artifact is provided".to_string(),
                skipped_reason.clone(),
            );
            print_phase_line(args.json, "build", "SKIP", &skipped_reason);
        } else {
            let artifact_path = build_capsule_artifact_for_publish(&cwd)?;
            let elapsed = started.elapsed().as_millis() as u64;
            let message = format!("artifact built: {}", artifact_path.display());
            phase_mark_ok(&mut phases[1], elapsed, message.clone(), None);
            built_artifact_path = Some(artifact_path);
            print_phase_line(args.json, "build", "OK", &message);
        }
    } else {
        phase_mark_skipped(
            &mut phases[1],
            0,
            "build phase not selected".to_string(),
            "not selected".to_string(),
        );
        print_phase_line(args.json, "build", "SKIP", "not selected");
    }

    if selection.deploy {
        print_phase_line(args.json, "deploy", "RUN", "deploy execution");
        let started = std::time::Instant::now();
        if is_official {
            let outcome = run_official_deploy(resolved_target.registry_url.clone(), args.fix)?;

            if !args.json {
                println!(
                    "🔎 official publish route registry={} route={:?}",
                    outcome.route.registry_url, outcome.route.route
                );
                for stage in &outcome.diagnosis.stages {
                    let icon = if stage.ok { "✅" } else { "❌" };
                    println!("{} {:<14} {}", icon, stage.key, stage.message);
                }
                if outcome.fix_result.attempted {
                    if outcome.fix_result.applied {
                        let label = if outcome.fix_result.created {
                            "created"
                        } else {
                            "updated"
                        };
                        println!("🛠️  workflow {} via --fix", label);
                    } else {
                        println!("🛠️  --fix requested, but workflow was already up-to-date");
                    }
                }
            }

            if !outcome.diagnosis.can_handoff {
                let actions =
                    crate::publish_official::collect_issue_actions(&outcome.diagnosis.issues);
                let fix_line = actions.first().cloned().unwrap_or_else(|| {
                    "ato publish --deploy --registry https://api.ato.run".to_string()
                });
                phase_mark_failed(
                    &mut phases[2],
                    started.elapsed().as_millis() as u64,
                    "official publish diagnostics failed".to_string(),
                    Some(fix_line.clone()),
                );
                print_phase_line(args.json, "deploy", "FAIL", "official diagnostics failed");
                if !args.json {
                    println!("👉 次に打つコマンド: {}", fix_line);
                    if !actions.is_empty() {
                        println!();
                        println!("詳細:");
                        for issue in &outcome.diagnosis.issues {
                            println!(" - [{}] {}", issue.stage, issue.message);
                        }
                    }
                    anyhow::bail!("official publish diagnostics failed");
                }
                official_result = Some(outcome);
            } else {
                let success_message = "official CI handoff is ready".to_string();
                phase_mark_ok(
                    &mut phases[2],
                    started.elapsed().as_millis() as u64,
                    success_message.clone(),
                    Some("handoff".to_string()),
                );
                print_phase_line(args.json, "deploy", "OK", &success_message);
                official_result = Some(outcome);
            }
        } else {
            let source_is_artifact = args.artifact.is_some();
            let deploy_artifact = if let Some(path) = args.artifact.clone() {
                path
            } else if let Some(path) = built_artifact_path.clone() {
                path
            } else {
                let fix_line =
                    "ato publish --deploy --artifact <file.capsule> --registry <url> もしくは ato publish --build --deploy --registry <url>"
                        .to_string();
                phase_mark_failed(
                    &mut phases[2],
                    started.elapsed().as_millis() as u64,
                    "deploy phase requires artifact input".to_string(),
                    Some(fix_line.clone()),
                );
                print_phase_line(args.json, "deploy", "FAIL", "artifact input is missing");
                if !args.json {
                    println!("👉 次に打つコマンド: {}", fix_line);
                }
                anyhow::bail!(
                    "--deploy requires artifact input for private registry. Use --artifact or include --build."
                );
            };

            let preview = private_preview
                .as_ref()
                .context("missing private publish preview")?;
            if !args.json {
                println!(
                    "{}",
                    publish_private_start_summary_line(
                        resolved_target.mode,
                        &resolved_target.registry_url,
                        preview.source,
                        &preview.scoped_id,
                        &preview.version,
                        preview.allow_existing,
                    )
                );
            }

            let status = publish_private_status_message(resolved_target.mode, source_is_artifact);
            futures::executor::block_on(reporter.progress_start(status.to_string(), None))?;
            let scoped_override = if source_is_artifact {
                args.scoped_id.clone()
            } else {
                Some(preview.scoped_id.clone())
            };
            let upload_result =
                crate::publish_private::execute(crate::publish_private::PublishPrivateArgs {
                    registry_url: resolved_target.registry_url.clone(),
                    publisher_hint: resolved_target.publisher_handle.clone(),
                    artifact_path: Some(deploy_artifact),
                    force_large_payload: args.force_large_payload,
                    scoped_id: scoped_override,
                    allow_existing: args.allow_existing,
                });
            futures::executor::block_on(reporter.progress_finish(None))?;
            let result = upload_result?;

            let success_message = format!("uploaded {}", result.file_name);
            phase_mark_ok(
                &mut phases[2],
                started.elapsed().as_millis() as u64,
                success_message.clone(),
                Some("upload".to_string()),
            );
            print_phase_line(args.json, "deploy", "OK", &success_message);
            private_result = Some(result);
        }
    } else {
        phase_mark_skipped(
            &mut phases[2],
            0,
            "deploy phase not selected".to_string(),
            "not selected".to_string(),
        );
        print_phase_line(args.json, "deploy", "SKIP", "not selected");
    }

    if args.json {
        emit_publish_json_output(
            &resolved_target,
            &phases,
            private_result.as_ref(),
            official_result.as_ref(),
        )?;
    } else if let Some(result) = private_result.as_ref() {
        if resolved_target.mode.is_personal_dock() {
            println!("✅ Successfully published to Personal Dock!");
        } else {
            println!("✅ Successfully published to private registry!");
        }
        println!();
        println!("📦 Capsule:   {} v{}", result.scoped_id, result.version);
        println!("🛡️  Integrity: {}, {}", result.sha256, result.blake3);
        println!();
        println!("🌐 Registry: {}", result.registry_url);
        println!("🌐 Artifact URL: {}", result.artifact_url);
        println!();
        if result.already_existed {
            println!("ℹ️  Existing release reused (same sha256, no new upload).");
            println!();
        }
        if resolved_target.mode.is_personal_dock() {
            println!("👉 Next step: ato install {}", result.scoped_id);
        } else {
            println!(
                "👉 Next step: ato install {} --registry {}",
                result.scoped_id, result.registry_url
            );
        }
    } else if let Some(outcome) = official_result {
        println!();
        println!("✅ CI handoff ready. 次の順で実行してください:");
        for command in &outcome.diagnosis.next_commands {
            println!("  {}", command);
        }
        if let Some(repo) = &outcome.diagnosis.repository {
            println!(
                "  https://github.com/{}/actions/workflows/ato-publish.yml",
                repo
            );
        }
    } else {
        println!("✅ Selected publish phases completed.");
    }

    if !args.json {
        if selection.deploy && phases[2].ok {
            let notice = if is_official {
                "Official publish handoff prepared (CI-first: local upload is not executed)."
            } else if resolved_target.mode.is_personal_dock() {
                "Personal Dock publish completed."
            } else {
                "Private registry publish completed."
            };
            futures::executor::block_on(reporter.notify(notice.to_string()))?;
        } else {
            futures::executor::block_on(
                reporter.notify("Selected publish phases completed.".to_string()),
            )?;
        }
    }

    Ok(())
}

fn run_official_deploy(registry_url: String, fix: bool) -> Result<OfficialDeployOutcome> {
    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let route = crate::publish_official::build_route_plan(&registry_url);

    let mut fix_result = crate::publish_official::WorkflowFixResult::default();
    let mut diagnosis = crate::publish_official::diagnose_official(&cwd, &registry_url);
    if fix && diagnosis.needs_workflow_fix {
        fix_result = crate::publish_official::apply_workflow_fix_once(&cwd)?;
        diagnosis = crate::publish_official::diagnose_official(&cwd, &registry_url);
    }

    Ok(OfficialDeployOutcome {
        route,
        fix_result,
        diagnosis,
    })
}

pub(crate) fn publish_private_status_message(
    target_mode: PublishTargetMode,
    has_artifact: bool,
) -> &'static str {
    if target_mode.is_personal_dock() {
        if has_artifact {
            "📤 Publishing provided artifact to Personal Dock..."
        } else {
            "📦 Building capsule artifact for Personal Dock publish..."
        }
    } else if has_artifact {
        "📤 Publishing provided artifact to private registry..."
    } else {
        "📦 Building capsule artifact for private registry publish..."
    }
}

pub(crate) fn publish_private_start_summary_line(
    target_mode: PublishTargetMode,
    registry_url: &str,
    source: &str,
    scoped_id: &str,
    version: &str,
    allow_existing: bool,
) -> String {
    format!(
        "🔎 {} publish target registry={} source={} scoped_id={} version={} allow_existing={}",
        if target_mode.is_personal_dock() {
            "dock"
        } else {
            "private"
        },
        registry_url,
        source,
        scoped_id,
        version,
        allow_existing
    )
}

pub(crate) fn select_publish_phases(
    prepare: bool,
    build: bool,
    deploy: bool,
    is_official: bool,
    legacy_full_publish: bool,
) -> PublishPhaseSelection {
    let explicit_filter = prepare || build || deploy;
    if explicit_filter {
        PublishPhaseSelection {
            prepare,
            build,
            deploy,
            explicit_filter,
        }
    } else if is_official && !legacy_full_publish {
        PublishPhaseSelection {
            prepare: false,
            build: false,
            deploy: true,
            explicit_filter: false,
        }
    } else {
        PublishPhaseSelection {
            prepare: true,
            build: true,
            deploy: true,
            explicit_filter: false,
        }
    }
}

fn maybe_warn_legacy_full_publish(
    args: &PublishCommandArgs,
    selection: PublishPhaseSelection,
    is_official: bool,
) {
    if args.legacy_full_publish && is_official && !selection.explicit_filter {
        eprintln!(
            "⚠️  --legacy-full-publish is deprecated and will be removed in a future release. Use explicit --prepare/--build/--deploy flags instead."
        );
    }
}

pub(crate) fn validate_publish_phase_options(
    args: &PublishCommandArgs,
    selection: PublishPhaseSelection,
    is_official: bool,
) -> Result<()> {
    if args.fix && !(is_official && selection.deploy) {
        anyhow::bail!("--fix is only available when deploying to official registry");
    }

    if args.legacy_full_publish && !is_official {
        anyhow::bail!("--legacy-full-publish is only available for official registry publish");
    }

    if args.legacy_full_publish && selection.explicit_filter {
        anyhow::bail!("--legacy-full-publish cannot be combined with --prepare/--build/--deploy");
    }

    if args.allow_existing && (is_official || !selection.deploy) {
        anyhow::bail!("--allow-existing is only available for private registry deploy phase");
    }

    if !is_official && selection.deploy && !selection.build && args.artifact.is_none() {
        anyhow::bail!(
            "--deploy requires --artifact for private registry publish (or include --build)"
        );
    }

    Ok(())
}

fn build_capsule_artifact_for_publish(cwd: &Path) -> Result<PathBuf> {
    let manifest_path = cwd.join("capsule.toml");
    let manifest_raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest = capsule_core::types::CapsuleManifest::from_toml(&manifest_raw)
        .map_err(|err| anyhow::anyhow!("Failed to parse capsule.toml: {}", err))?;
    let version = if manifest.version.trim().is_empty() {
        "auto"
    } else {
        manifest.version.trim()
    };
    crate::publish_ci::build_capsule_artifact(&manifest_path, &manifest.name, version)
        .with_context(|| "Failed to build artifact for publish")
}

fn emit_publish_json_output(
    resolved_target: &ResolvedPublishTarget,
    phases: &[PublishPhaseResult],
    private_result: Option<&crate::publish_private::PublishPrivateResult>,
    official_result: Option<&OfficialDeployOutcome>,
) -> Result<()> {
    if let Some(outcome) = official_result {
        let payload = serde_json::json!({
            "ok": outcome.diagnosis.can_handoff,
            "code": if outcome.diagnosis.can_handoff { "CI_HANDOFF_READY" } else { "CI_ONLY_PUBLISH" },
            "message": if outcome.diagnosis.can_handoff {
                "Official registry publishing is CI-first. Handoff is ready."
            } else {
                "Official registry publishing is CI-first. Run the suggested local fixes, then push tag to trigger CI."
            },
            "route": outcome.route.route,
            "registry": outcome.route.registry_url,
            "fix": outcome.fix_result,
            "diagnosis": outcome.diagnosis,
            "phases": phases,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if let Some(result) = private_result {
        let mut payload = serde_json::to_value(result)?;
        if let serde_json::Value::Object(map) = &mut payload {
            map.insert("phases".to_string(), serde_json::to_value(phases)?);
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    let payload = serde_json::json!({
        "ok": true,
        "code": "PUBLISH_PHASES_COMPLETED",
        "message": "Selected publish phases completed.",
        "registry": resolved_target.registry_url,
        "route": resolved_target.mode.route_label(),
        "phases": phases,
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

fn new_phase_result(name: &'static str, selected: bool) -> PublishPhaseResult {
    PublishPhaseResult {
        name,
        selected,
        ok: !selected,
        status: "skipped",
        elapsed_ms: 0,
        actionable_fix: None,
        message: if selected {
            "pending".to_string()
        } else {
            "not selected".to_string()
        },
        result_kind: None,
        skipped_reason: if selected {
            None
        } else {
            Some("not selected".to_string())
        },
    }
}

fn phase_mark_ok(
    phase: &mut PublishPhaseResult,
    elapsed_ms: u64,
    message: String,
    result_kind: Option<String>,
) {
    phase.ok = true;
    phase.status = "ok";
    phase.elapsed_ms = elapsed_ms;
    phase.actionable_fix = None;
    phase.message = message;
    phase.result_kind = result_kind;
    phase.skipped_reason = None;
}

fn phase_mark_skipped(
    phase: &mut PublishPhaseResult,
    elapsed_ms: u64,
    message: String,
    skipped_reason: String,
) {
    phase.ok = true;
    phase.status = "skipped";
    phase.elapsed_ms = elapsed_ms;
    phase.actionable_fix = None;
    phase.message = message;
    phase.result_kind = None;
    phase.skipped_reason = Some(skipped_reason);
}

fn phase_mark_failed(
    phase: &mut PublishPhaseResult,
    elapsed_ms: u64,
    message: String,
    actionable_fix: Option<String>,
) {
    phase.ok = false;
    phase.status = "failed";
    phase.elapsed_ms = elapsed_ms;
    phase.actionable_fix = actionable_fix;
    phase.message = message;
    phase.result_kind = None;
    phase.skipped_reason = None;
}

fn print_phase_line(json_output: bool, phase: &str, state: &str, detail: &str) {
    if json_output {
        return;
    }
    println!("PHASE {:<7} {:<4} {}", phase, state, detail);
}

fn resolve_publish_target(cli_registry: Option<String>) -> Result<ResolvedPublishTarget> {
    let manifest_registry = discover_manifest_publish_registry()?;
    let publisher_handle = crate::auth::current_publisher_handle()?;

    resolve_publish_target_from_sources(
        cli_registry.as_deref(),
        manifest_registry.as_deref(),
        publisher_handle.as_deref(),
    )
}

fn discover_manifest_publish_registry() -> Result<Option<String>> {
    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let manifest_path = cwd.join("capsule.toml");
    if !manifest_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let parsed: toml::Value = toml::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

    Ok(parsed
        .get("store")
        .and_then(|v| v.get("registry"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned))
}

pub(crate) fn resolve_publish_target_from_sources(
    cli_registry: Option<&str>,
    manifest_registry: Option<&str>,
    publisher_handle: Option<&str>,
) -> Result<ResolvedPublishTarget> {
    if let Some(url) = cli_registry {
        return resolve_explicit_publish_target(url);
    }

    if let Some(url) = manifest_registry {
        return resolve_explicit_publish_target(url);
    }

    if let Some(handle) = publisher_handle {
        return Ok(ResolvedPublishTarget {
            registry_url: crate::auth::default_store_registry_url(),
            mode: PublishTargetMode::PersonalDockDirect,
            publisher_handle: Some(handle.to_string()),
        });
    }

    anyhow::bail!(
        "No default publish target found. Run `ato login` to publish to your Personal Dock, or pass `--registry https://api.ato.run` / `--ci` for the official Store."
    );
}

fn resolve_explicit_publish_target(raw: &str) -> Result<ResolvedPublishTarget> {
    let normalized = crate::registry::http::normalize_registry_url(raw, "registry")?;
    if is_legacy_dock_publish_registry(&normalized) {
        anyhow::bail!(
            "Registry URL `{}` is no longer supported. Personal Dock publish now uses `https://api.ato.run`; `/d/<handle>` is a UI page, not a registry.",
            normalized
        );
    }

    Ok(ResolvedPublishTarget {
        registry_url: normalized.clone(),
        mode: if is_official_publish_registry(&normalized) {
            PublishTargetMode::OfficialCi
        } else {
            PublishTargetMode::CustomDirect
        },
        publisher_handle: None,
    })
}

fn is_official_publish_registry(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("api.ato.run") || host.eq_ignore_ascii_case("staging.api.ato.run")
}

pub(crate) fn is_legacy_dock_publish_registry(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(mut segments) = parsed.path_segments() else {
        return false;
    };
    while let Some(segment) = segments.next() {
        if segment == "d" {
            return segments
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some();
        }
    }
    false
}

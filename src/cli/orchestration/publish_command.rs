use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use capsule_core::CapsuleReporter;

use crate::application::pipeline::executor::HourglassPhaseRunner;
use crate::application::pipeline::phases::install as install_phase;
use crate::application::pipeline::phases::publish as publish_phase;
use crate::application::pipeline::producer::{
    self, ProducerPipeline, PublishDryRunStageResult, PublishInstallResult, PublishPhaseOptions,
    PublishPipelineRequest, PublishPipelineState,
};
use crate::orchestration::hourglass::{
    self, phase_is_ok, phase_mark_failed, phase_mark_ok, phase_mark_skipped, phase_mut,
    HourglassFlow, HourglassPhaseState,
};
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
    args: PublishCommandArgs,
    reporter: Arc<reporters::CliReporter>,
) -> Result<()> {
    if args.prepare || args.build || args.deploy {
        anyhow::bail!("--dry-run cannot be combined with --prepare/--build/--deploy");
    }
    execute_publish_pipeline(args, reporter, true)
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

#[cfg(test)]
pub(crate) use crate::application::pipeline::producer::select_publish_phases;
pub(crate) use crate::orchestration::hourglass::HourglassPhase as PublishPhaseBoundary;
#[cfg(test)]
pub(crate) fn validate_publish_phase_options(
    args: &PublishCommandArgs,
    selection: crate::orchestration::hourglass::HourglassPhaseSelection,
    is_official: bool,
) -> Result<()> {
    producer::validate_publish_phase_options(
        PublishPhaseOptions {
            fix: args.fix,
            legacy_full_publish: args.legacy_full_publish,
            allow_existing: args.allow_existing,
        },
        selection,
        is_official,
    )
}

type PublishPhaseResult = hourglass::HourglassPhaseResult;

struct PublishCommandExecution<'a> {
    args: &'a PublishCommandArgs,
    reporter: Arc<reporters::CliReporter>,
    resolved_target: ResolvedPublishTarget,
    top_level_dry_run: bool,
    is_official: bool,
    phases: Vec<PublishPhaseResult>,
    cwd: PathBuf,
    state: PublishPipelineState,
    pipeline_preview: Option<crate::publish_private::PublishPrivateSummary>,
    private_result: Option<crate::publish_private::PublishPrivateResult>,
    official_result: Option<OfficialDeployOutcome>,
}

impl<'a> PublishCommandExecution<'a> {
    fn new(
        args: &'a PublishCommandArgs,
        reporter: Arc<reporters::CliReporter>,
        resolved_target: ResolvedPublishTarget,
        top_level_dry_run: bool,
        is_official: bool,
        phases: Vec<PublishPhaseResult>,
        cwd: PathBuf,
        pipeline_preview: Option<crate::publish_private::PublishPrivateSummary>,
    ) -> Self {
        let state = if let Some(preview) = pipeline_preview.as_ref() {
            PublishPipelineState::default()
                .with_resolved_release(preview.scoped_id.clone(), preview.version.clone())
        } else {
            PublishPipelineState::default()
        };
        Self {
            args,
            reporter,
            resolved_target,
            top_level_dry_run,
            is_official,
            phases,
            cwd,
            state,
            pipeline_preview,
            private_result: None,
            official_result: None,
        }
    }

    fn mark_not_selected(&mut self, boundary: PublishPhaseBoundary) {
        phase_mark_skipped(
            phase_mut(&mut self.phases, boundary),
            0,
            format!("{} phase not selected", boundary.as_str()),
            "not selected".to_string(),
        );
        hourglass::print_phase_line(
            self.args.json,
            boundary,
            HourglassPhaseState::Skip,
            "not selected",
        );
    }

    fn run_prepare_phase(&mut self) -> Result<()> {
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::Prepare,
            HourglassPhaseState::Run,
            "prepare command detection",
        );
        let started = std::time::Instant::now();
        let prepare_spec = crate::publish_prepare::detect_prepare_command(&self.cwd)?;
        match prepare_spec {
            Some(spec) => {
                let message = format!("running {}", spec.source.as_label());
                crate::publish_prepare::run_prepare_command(&spec, &self.cwd, self.args.json)
                    .context("Failed to run publish prepare command")?;
                phase_mark_ok(
                    phase_mut(&mut self.phases, PublishPhaseBoundary::Prepare),
                    started.elapsed().as_millis() as u64,
                    message.clone(),
                    None,
                );
                hourglass::print_phase_line(
                    self.args.json,
                    PublishPhaseBoundary::Prepare,
                    HourglassPhaseState::Ok,
                    &message,
                );
                Ok(())
            }
            None => {
                let skipped_reason = "no prepare command configured".to_string();
                if self.args.prepare {
                    let fix = "capsule.toml に [build.lifecycle].prepare を設定するか package.json scripts[\"capsule:prepare\"] を追加して再実行してください。".to_string();
                    phase_mark_failed(
                        phase_mut(&mut self.phases, PublishPhaseBoundary::Prepare),
                        started.elapsed().as_millis() as u64,
                        "--prepare was selected but no prepare command was found".to_string(),
                        Some(fix.clone()),
                    );
                    hourglass::print_phase_line(
                        self.args.json,
                        PublishPhaseBoundary::Prepare,
                        HourglassPhaseState::Fail,
                        "prepare command not found",
                    );
                    if !self.args.json {
                        println!("👉 次に打つコマンド: {}", fix);
                    }
                    anyhow::bail!(
                        "--prepare was selected but no prepare command was found. Set `build.lifecycle.prepare` in capsule.toml or add package.json scripts[\"capsule:prepare\"]."
                    );
                }
                phase_mark_skipped(
                    phase_mut(&mut self.phases, PublishPhaseBoundary::Prepare),
                    started.elapsed().as_millis() as u64,
                    skipped_reason.clone(),
                    skipped_reason.clone(),
                );
                hourglass::print_phase_line(
                    self.args.json,
                    PublishPhaseBoundary::Prepare,
                    HourglassPhaseState::Skip,
                    &skipped_reason,
                );
                Ok(())
            }
        }
    }

    fn run_build_phase(&mut self) -> Result<()> {
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::Build,
            HourglassPhaseState::Run,
            "artifact build",
        );
        let started = std::time::Instant::now();
        if self.args.artifact.is_some() {
            let skipped_reason = "start phase is Verify".to_string();
            phase_mark_skipped(
                phase_mut(&mut self.phases, PublishPhaseBoundary::Build),
                started.elapsed().as_millis() as u64,
                "build is skipped because artifact input starts at Verify".to_string(),
                skipped_reason.clone(),
            );
            hourglass::print_phase_line(
                self.args.json,
                PublishPhaseBoundary::Build,
                HourglassPhaseState::Skip,
                &skipped_reason,
            );
            return Ok(());
        }

        let artifact_path = build_capsule_artifact_for_publish(&self.cwd)?;
        let elapsed = started.elapsed().as_millis() as u64;
        let message = format!("artifact built: {}", artifact_path.display());
        phase_mark_ok(
            phase_mut(&mut self.phases, PublishPhaseBoundary::Build),
            elapsed,
            message.clone(),
            Some("artifact_built".to_string()),
        );
        self.state.record_built_artifact(artifact_path);
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::Build,
            HourglassPhaseState::Ok,
            &message,
        );
        Ok(())
    }

    fn run_verify_phase(&mut self) -> Result<()> {
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::Verify,
            HourglassPhaseState::Run,
            "artifact verification",
        );
        let started = std::time::Instant::now();
        let artifact_path = if let Some(path) = self
            .args
            .artifact
            .clone()
            .or_else(|| self.state.artifact_path_or(None))
        {
            path
        } else {
            phase_mark_failed(
                phase_mut(&mut self.phases, PublishPhaseBoundary::Verify),
                started.elapsed().as_millis() as u64,
                "verify phase requires artifact input".to_string(),
                None,
            );
            hourglass::print_phase_line(
                self.args.json,
                PublishPhaseBoundary::Verify,
                HourglassPhaseState::Fail,
                "artifact input is missing",
            );
            anyhow::bail!("verify phase requires an artifact produced earlier in the pipeline");
        };
        let verification = crate::publish_artifact::verify_artifact(&artifact_path)?;
        let message = format!(
            "artifact verified: {} (sha256={}, size={} bytes)",
            artifact_path.display(),
            verification.sha256,
            verification.size_bytes
        );
        phase_mark_ok(
            phase_mut(&mut self.phases, PublishPhaseBoundary::Verify),
            started.elapsed().as_millis() as u64,
            message.clone(),
            Some("artifact_verified".to_string()),
        );
        self.state
            .record_verified_artifact(artifact_path, verification);
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::Verify,
            HourglassPhaseState::Ok,
            &message,
        );
        Ok(())
    }

    fn run_install_phase(&mut self) -> Result<()> {
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::Install,
            HourglassPhaseState::Run,
            "verified artifact unpack into dry-run sandbox",
        );
        let started = std::time::Instant::now();
        let preview = self
            .pipeline_preview
            .as_ref()
            .context("missing publish pipeline preview for install stage")?;
        let artifact_path = self
            .state
            .artifact_path()
            .context("install phase requires a verified artifact")?;
        let verification = self
            .state
            .verified_artifact()
            .context("install phase requires verified artifact metadata")?;
        let install_result =
            install_phase::run_publish_install_phase(artifact_path, preview, verification)?;
        let message = format!("unpacked {}", install_result.path.display());
        phase_mark_ok(
            phase_mut(&mut self.phases, PublishPhaseBoundary::Install),
            started.elapsed().as_millis() as u64,
            message.clone(),
            Some("sandboxed_unpack".to_string()),
        );
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::Install,
            HourglassPhaseState::Ok,
            &message,
        );
        self.state.record_install_result(install_result);
        Ok(())
    }

    fn run_dry_run_phase(&mut self) -> Result<()> {
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::DryRun,
            HourglassPhaseState::Run,
            "publish preflight",
        );
        let started = std::time::Instant::now();
        if self.is_official {
            let outcome =
                run_official_deploy(self.resolved_target.registry_url.clone(), self.args.fix)?;
            if !self.args.json {
                print_official_diagnosis(&outcome);
            }
            let dry_run_result = PublishDryRunStageResult {
                kind: "official_ci_handoff",
                diagnosis: Some(outcome.diagnosis.clone()),
                registry: Some(outcome.route.registry_url.clone()),
                upload_endpoint: None,
                reachable: None,
                auth_ready: None,
                permission_check: None,
            };
            let can_handoff = outcome.diagnosis.can_handoff;
            self.official_result = Some(outcome);
            self.state.record_dry_run_result(dry_run_result);
            if can_handoff {
                let message = "official CI handoff is ready".to_string();
                phase_mark_ok(
                    phase_mut(&mut self.phases, PublishPhaseBoundary::DryRun),
                    started.elapsed().as_millis() as u64,
                    message.clone(),
                    Some("handoff".to_string()),
                );
                hourglass::print_phase_line(
                    self.args.json,
                    PublishPhaseBoundary::DryRun,
                    HourglassPhaseState::Ok,
                    &message,
                );
                return Ok(());
            }

            let actions = crate::publish_official::collect_issue_actions(
                &self
                    .official_result
                    .as_ref()
                    .expect("official outcome")
                    .diagnosis
                    .issues,
            );
            let fix_line = actions.first().cloned().unwrap_or_else(|| {
                "ato publish --deploy --registry https://api.ato.run".to_string()
            });
            phase_mark_failed(
                phase_mut(&mut self.phases, PublishPhaseBoundary::DryRun),
                started.elapsed().as_millis() as u64,
                "official publish diagnostics failed".to_string(),
                Some(fix_line.clone()),
            );
            hourglass::print_phase_line(
                self.args.json,
                PublishPhaseBoundary::DryRun,
                HourglassPhaseState::Fail,
                "official diagnostics failed",
            );
            if !self.args.json {
                println!("👉 次に打つコマンド: {}", fix_line);
                if !actions.is_empty() {
                    println!();
                    println!("詳細:");
                    for issue in &self
                        .official_result
                        .as_ref()
                        .expect("official outcome")
                        .diagnosis
                        .issues
                    {
                        println!(" - [{}] {}", issue.stage, issue.message);
                    }
                }
                anyhow::bail!("official publish diagnostics failed");
            }
            if self.top_level_dry_run {
                return Ok(());
            }
            return Ok(());
        }

        let preview = self
            .pipeline_preview
            .as_ref()
            .context("missing publish pipeline preview for dry-run stage")?;
        let verification = self
            .state
            .verified_artifact()
            .context("dry-run phase requires verified artifact metadata")?;
        let dry_run_result = publish_phase::run_direct_publish_dry_run_phase(
            &publish_phase::DirectPublishDryRunRequest {
                registry_url: &self.resolved_target.registry_url,
                scoped_id: &preview.scoped_id,
                version: &preview.version,
                artifact_version: &verification.version,
                allow_existing: self.args.allow_existing,
                requires_session_token: self.resolved_target.mode.is_personal_dock(),
            },
        )?;
        let dry_run_ok = publish_phase::direct_publish_dry_run_is_ready(
            &dry_run_result,
            self.resolved_target.mode.is_personal_dock(),
        );
        let failure_message = publish_phase::direct_publish_dry_run_failure_message(
            &dry_run_result,
            self.resolved_target.mode.is_personal_dock(),
        );
        self.state.record_dry_run_result(dry_run_result);
        if dry_run_ok {
            let message = "publish preflight passed".to_string();
            phase_mark_ok(
                phase_mut(&mut self.phases, PublishPhaseBoundary::DryRun),
                started.elapsed().as_millis() as u64,
                message.clone(),
                Some("preflight".to_string()),
            );
            hourglass::print_phase_line(
                self.args.json,
                PublishPhaseBoundary::DryRun,
                HourglassPhaseState::Ok,
                &message,
            );
            return Ok(());
        }

        phase_mark_failed(
            phase_mut(&mut self.phases, PublishPhaseBoundary::DryRun),
            started.elapsed().as_millis() as u64,
            failure_message.clone(),
            None,
        );
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::DryRun,
            HourglassPhaseState::Fail,
            &failure_message,
        );
        if self.top_level_dry_run && self.args.json {
            return Ok(());
        }
        anyhow::bail!("{}", failure_message)
    }

    fn run_publish_phase(&mut self) -> Result<()> {
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::Publish,
            HourglassPhaseState::Run,
            "publish execution",
        );
        let started = std::time::Instant::now();
        if self.is_official {
            let outcome =
                run_official_deploy(self.resolved_target.registry_url.clone(), self.args.fix)?;
            if !self.args.json {
                print_official_diagnosis(&outcome);
            }

            if !outcome.diagnosis.can_handoff {
                let actions =
                    crate::publish_official::collect_issue_actions(&outcome.diagnosis.issues);
                let fix_line = actions.first().cloned().unwrap_or_else(|| {
                    "ato publish --deploy --registry https://api.ato.run".to_string()
                });
                phase_mark_failed(
                    phase_mut(&mut self.phases, PublishPhaseBoundary::Publish),
                    started.elapsed().as_millis() as u64,
                    "official publish diagnostics failed".to_string(),
                    Some(fix_line.clone()),
                );
                hourglass::print_phase_line(
                    self.args.json,
                    PublishPhaseBoundary::Publish,
                    HourglassPhaseState::Fail,
                    "official diagnostics failed",
                );
                if !self.args.json {
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
                self.official_result = Some(outcome);
                return Ok(());
            }

            let success_message = "official CI handoff is ready".to_string();
            phase_mark_ok(
                phase_mut(&mut self.phases, PublishPhaseBoundary::Publish),
                started.elapsed().as_millis() as u64,
                success_message.clone(),
                Some("handoff".to_string()),
            );
            hourglass::print_phase_line(
                self.args.json,
                PublishPhaseBoundary::Publish,
                HourglassPhaseState::Ok,
                &success_message,
            );
            self.official_result = Some(outcome);
            return Ok(());
        }

        let source_is_artifact = self.args.artifact.is_some();
        let publish_artifact = self
            .state
            .artifact_path_or(None)
            .or_else(|| self.args.artifact.clone())
            .context("publish phase requires a verified artifact")?;

        let preview = self
            .pipeline_preview
            .as_ref()
            .context("missing private publish preview")?;
        if !self.args.json {
            println!(
                "{}",
                publish_private_start_summary_line(
                    self.resolved_target.mode,
                    &self.resolved_target.registry_url,
                    preview.source,
                    &preview.scoped_id,
                    &preview.version,
                    preview.allow_existing,
                )
            );
        }

        let status = publish_private_status_message(self.resolved_target.mode, source_is_artifact);
        if !self.args.json {
            futures::executor::block_on(self.reporter.progress_start(status.to_string(), None))?;
        }
        let scoped_override = if source_is_artifact {
            self.args.scoped_id.clone()
        } else {
            Some(preview.scoped_id.clone())
        };
        let upload_result =
            crate::publish_private::execute(crate::publish_private::PublishPrivateArgs {
                registry_url: self.resolved_target.registry_url.clone(),
                publisher_hint: self.resolved_target.publisher_handle.clone(),
                artifact_path: Some(publish_artifact),
                force_large_payload: self.args.force_large_payload,
                scoped_id: scoped_override,
                allow_existing: self.args.allow_existing,
            });
        if !self.args.json {
            futures::executor::block_on(self.reporter.progress_finish(None))?;
        }
        let result = upload_result?;

        let success_message = format!("uploaded {}", result.file_name);
        phase_mark_ok(
            phase_mut(&mut self.phases, PublishPhaseBoundary::Publish),
            started.elapsed().as_millis() as u64,
            success_message.clone(),
            Some("upload".to_string()),
        );
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::Publish,
            HourglassPhaseState::Ok,
            &success_message,
        );
        self.private_result = Some(result);
        Ok(())
    }
}

#[async_trait(?Send)]
impl HourglassPhaseRunner for PublishCommandExecution<'_> {
    async fn run_phase(&mut self, phase: PublishPhaseBoundary) -> Result<()> {
        match phase {
            PublishPhaseBoundary::Prepare => self.run_prepare_phase(),
            PublishPhaseBoundary::Build => self.run_build_phase(),
            PublishPhaseBoundary::Verify => self.run_verify_phase(),
            PublishPhaseBoundary::Install => self.run_install_phase(),
            PublishPhaseBoundary::DryRun => self.run_dry_run_phase(),
            PublishPhaseBoundary::Publish => self.run_publish_phase(),
            PublishPhaseBoundary::Execute => {
                anyhow::bail!("unsupported publish pipeline phase {}", phase.as_str())
            }
        }
    }

    async fn skip_phase(&mut self, phase: PublishPhaseBoundary) -> Result<()> {
        match phase {
            PublishPhaseBoundary::Prepare
            | PublishPhaseBoundary::Build
            | PublishPhaseBoundary::Verify
            | PublishPhaseBoundary::Install
            | PublishPhaseBoundary::DryRun
            | PublishPhaseBoundary::Publish => {
                self.mark_not_selected(phase);
                Ok(())
            }
            PublishPhaseBoundary::Execute => {
                anyhow::bail!("unsupported publish pipeline phase {}", phase.as_str())
            }
        }
    }
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
    execute_publish_pipeline(args, reporter, false)
}

fn execute_publish_pipeline(
    args: PublishCommandArgs,
    reporter: Arc<reporters::CliReporter>,
    top_level_dry_run: bool,
) -> Result<()> {
    ensure_publish_source_manifest_ready(&args)?;
    let resolved_target = resolve_publish_target(args.registry.clone())?;
    let is_official = resolved_target.mode.is_official();
    let plan = producer::build_publish_pipeline_plan(
        PublishPipelineRequest {
            top_level_dry_run,
            prepare: args.prepare,
            build: args.build,
            deploy: args.deploy,
            is_official,
            has_artifact: args.artifact.is_some(),
        },
        PublishPhaseOptions {
            fix: args.fix,
            legacy_full_publish: args.legacy_full_publish,
            allow_existing: args.allow_existing,
        },
    )?;
    let selection = plan.selection;
    if resolved_target.mode.is_personal_dock() && selection.runs_publish() {
        let _ = crate::auth::require_session_token()?;
    }
    maybe_warn_legacy_full_publish(plan.warn_legacy_full_publish);
    let _ = args.no_tui;
    let phases = hourglass::new_phase_results(HourglassFlow::ProducerPublish, selection);

    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let pipeline_preview = if selection.runs_install()
        || selection.runs_dry_run()
        || (!is_official && selection.runs_publish())
    {
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

    let pipeline = ProducerPipeline::new(selection);
    let mut execution = PublishCommandExecution::new(
        &args,
        reporter.clone(),
        resolved_target.clone(),
        top_level_dry_run,
        is_official,
        phases,
        cwd,
        pipeline_preview,
    );
    futures::executor::block_on(pipeline.run_until(selection.stop, &mut execution))?;

    let phases = execution.phases;
    let state = execution.state;
    let private_result = execution.private_result;
    let official_result = execution.official_result;

    if args.json {
        emit_publish_json_output(
            &resolved_target,
            &phases,
            state.install_result(),
            state.dry_run_result(),
            private_result.as_ref(),
            official_result.as_ref(),
            top_level_dry_run,
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
    } else if let Some(outcome) = official_result.as_ref() {
        println!();
        if top_level_dry_run {
            println!("✅ Dry-run completed. 次の順で実行してください:");
        } else {
            println!("✅ CI handoff ready. 次の順で実行してください:");
        }
        for command in &outcome.diagnosis.next_commands {
            println!("  {}", command);
        }
        if let Some(repo) = &outcome.diagnosis.repository {
            println!(
                "  https://github.com/{}/actions/workflows/ato-publish.yml",
                repo
            );
        }
    } else if top_level_dry_run {
        println!("✅ Dry-run successful! No upload performed.");
        if let Some(install_result) = state.install_result() {
            println!("📦 Test sandbox: {}", install_result.path.display());
        }
        if let Some(dry_run_result) = state.dry_run_result() {
            if let Some(registry) = dry_run_result.registry.as_deref() {
                println!("🌐 Registry: {}", registry);
            }
            if let Some(endpoint) = dry_run_result.upload_endpoint.as_deref() {
                println!("🧪 Upload endpoint: {}", endpoint);
            }
        }
    } else {
        println!("✅ Selected publish phases completed.");
    }

    if !args.json {
        if top_level_dry_run && phase_is_ok(&phases, PublishPhaseBoundary::DryRun) {
            futures::executor::block_on(
                reporter
                    .notify("Local publish dry-run completed (no upload performed).".to_string()),
            )?;
        } else if selection.runs_publish() && phase_is_ok(&phases, PublishPhaseBoundary::Publish) {
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

fn print_official_diagnosis(outcome: &OfficialDeployOutcome) {
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

fn maybe_warn_legacy_full_publish(should_warn: bool) {
    if should_warn {
        eprintln!(
            "⚠️  --legacy-full-publish is deprecated and will be removed in a future release. Use explicit --prepare/--build/--deploy flags instead."
        );
    }
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
    install_result: Option<&PublishInstallResult>,
    dry_run_result: Option<&PublishDryRunStageResult>,
    private_result: Option<&crate::publish_private::PublishPrivateResult>,
    official_result: Option<&OfficialDeployOutcome>,
    top_level_dry_run: bool,
) -> Result<()> {
    if let Some(outcome) = official_result {
        let mut payload = serde_json::json!({
            "ok": outcome.diagnosis.can_handoff,
            "code": if outcome.diagnosis.can_handoff { "CI_HANDOFF_READY" } else { "CI_ONLY_PUBLISH" },
            "message": if outcome.diagnosis.can_handoff && top_level_dry_run {
                "Official registry publish dry-run completed. CI handoff is ready."
            } else if outcome.diagnosis.can_handoff {
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
        if let serde_json::Value::Object(map) = &mut payload {
            if let Some(install) = install_result {
                map.insert("install".to_string(), serde_json::to_value(install)?);
            }
            if let Some(dry_run) = dry_run_result {
                map.insert("dry_run".to_string(), serde_json::to_value(dry_run)?);
            }
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if let Some(result) = private_result {
        let mut payload = serde_json::to_value(result)?;
        if let serde_json::Value::Object(map) = &mut payload {
            map.insert("phases".to_string(), serde_json::to_value(phases)?);
            if let Some(install) = install_result {
                map.insert("install".to_string(), serde_json::to_value(install)?);
            }
            if let Some(dry_run) = dry_run_result {
                map.insert("dry_run".to_string(), serde_json::to_value(dry_run)?);
            }
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    let mut payload = serde_json::json!({
        "ok": true,
        "code": if top_level_dry_run {
            "PUBLISH_DRY_RUN_COMPLETED"
        } else {
            "PUBLISH_PHASES_COMPLETED"
        },
        "message": if top_level_dry_run {
            "Publish dry-run completed. No upload performed."
        } else {
            "Selected publish phases completed."
        },
        "registry": resolved_target.registry_url,
        "route": resolved_target.mode.route_label(),
        "phases": phases,
    });
    if let serde_json::Value::Object(map) = &mut payload {
        if let Some(install) = install_result {
            map.insert("install".to_string(), serde_json::to_value(install)?);
        }
        if let Some(dry_run) = dry_run_result {
            map.insert("dry_run".to_string(), serde_json::to_value(dry_run)?);
        }
    }
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

fn resolve_publish_target(cli_registry: Option<String>) -> Result<ResolvedPublishTarget> {
    if let Some(url) = cli_registry {
        return resolve_explicit_publish_target(&url);
    }

    let manifest_registry = discover_manifest_publish_registry()?;
    if let Some(url) = manifest_registry {
        return resolve_explicit_publish_target(&url);
    }

    let publisher_handle = crate::auth::current_publisher_handle()?;

    resolve_publish_target_from_sources(None, None, publisher_handle.as_deref())
}

fn ensure_publish_source_manifest_ready(args: &PublishCommandArgs) -> Result<()> {
    if args.artifact.is_some() {
        return Ok(());
    }

    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let manifest_path = cwd.join("capsule.toml");
    if !manifest_path.exists() {
        anyhow::bail!(
            "capsule.toml not found in current directory: {}",
            manifest_path.display()
        );
    }

    Ok(())
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

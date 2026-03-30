use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use capsule_core::CapsuleReporter;

use crate::application::pipeline::cleanup::PipelineAttemptContext;
use crate::application::pipeline::executor::HourglassPhaseRunner;
use crate::application::pipeline::hourglass::{
    self, phase_is_ok, phase_mark_failed, phase_mark_ok, phase_mark_skipped, phase_mut,
    HourglassPhaseState,
};
use crate::application::pipeline::phases::install as install_phase;
use crate::application::pipeline::phases::publish as publish_phase;
use crate::application::pipeline::producer::{
    self, ProducerPipeline, PublishDryRunStageResult, PublishInstallResult, PublishPhaseOptions,
    PublishPipelineRequest, PublishPipelineState,
};
use crate::application::producer_input::{
    rematerialize_source_authoritative_input, resolve_producer_authoritative_input,
};

use super::Reporter;

pub(crate) use crate::application::pipeline::producer::{PublishTargetMode, ResolvedPublishTarget};

#[cfg(test)]
pub(crate) use crate::application::pipeline::producer::{
    is_legacy_dock_publish_registry, resolve_publish_target_from_sources,
};

pub(crate) fn execute_publish_ci_command(
    json_output: bool,
    force_large_payload: bool,
    reporter: Reporter,
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
    reporter: Reporter,
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
        println!("  1. Ensure `.github/workflows/ato-publish.yml` is present and committed.");
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
    pub(crate) finalize_local: bool,
    pub(crate) allow_external_finalize: bool,
    pub(crate) fix: bool,
    pub(crate) no_tui: bool,
    pub(crate) json: bool,
}

pub(crate) use crate::application::pipeline::hourglass::HourglassPhase as PublishPhaseBoundary;
#[cfg(test)]
pub(crate) use crate::application::pipeline::producer::select_publish_phases;

#[cfg(test)]
pub(crate) fn validate_publish_phase_options(
    args: &PublishCommandArgs,
    selection: crate::application::pipeline::hourglass::HourglassPhaseSelection,
    is_official: bool,
) -> Result<()> {
    producer::validate_publish_phase_options(
        PublishPhaseOptions {
            fix: args.fix,
            legacy_full_publish: args.legacy_full_publish,
            allow_existing: args.allow_existing,
            finalize_local: args.finalize_local,
            allow_external_finalize: args.allow_external_finalize,
        },
        selection,
        is_official,
    )
}

type PublishPhaseResult = hourglass::HourglassPhaseResult;

struct PublishCommandExecution<'a> {
    args: &'a PublishCommandArgs,
    reporter: Reporter,
    resolved_target: ResolvedPublishTarget,
    top_level_dry_run: bool,
    is_official: bool,
    phases: Vec<PublishPhaseResult>,
    cwd: PathBuf,
    state: PublishPipelineState,
    pipeline_preview: Option<publish_phase::PrivatePublishSummary>,
    private_result: Option<publish_phase::PrivatePublishResult>,
    official_result: Option<publish_phase::OfficialPublishOutcome>,
    authoritative_input: Option<crate::application::producer_input::ProducerAuthoritativeInput>,
}

impl<'a> PublishCommandExecution<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        args: &'a PublishCommandArgs,
        reporter: Reporter,
        resolved_target: ResolvedPublishTarget,
        top_level_dry_run: bool,
        is_official: bool,
        phases: Vec<PublishPhaseResult>,
        cwd: PathBuf,
        pipeline_preview: Option<publish_phase::PrivatePublishSummary>,
        authoritative_input: Option<crate::application::producer_input::ProducerAuthoritativeInput>,
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
            authoritative_input,
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
            "prepare dependency resolution",
        );
        let started = std::time::Instant::now();
        let execution_working_directory = self
            .authoritative_input
            .as_ref()
            .map(|input| input.descriptor.execution_working_directory())
            .unwrap_or_else(|| self.cwd.clone());
        let prepare_specs =
            crate::publish_prepare::detect_prepare_specs(&self.cwd, &execution_working_directory)?;
        match prepare_specs.as_slice() {
            [spec] => {
                let message = format!("running {}", spec.source.as_label());
                crate::publish_prepare::run_prepare_command(spec, self.args.json).with_context(
                    || format!("Failed to run publish prepare step: {}", spec.command),
                )?;
                self.refresh_authoritative_input_after_prepare()?;
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
            [_, _, ..] => {
                let specs = prepare_specs.as_slice();
                for spec in specs {
                    crate::publish_prepare::run_prepare_command(spec, self.args.json)
                        .with_context(|| {
                            format!("Failed to run publish prepare step: {}", spec.command)
                        })?;
                }
                self.refresh_authoritative_input_after_prepare()?;
                let message = format!("ran {} prepare steps", specs.len());
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
            [] => {
                let skipped_reason = "no prepare step configured".to_string();
                if self.args.prepare {
                    let fix = "lockfile に基づく依存解決を用意するか、capsule.toml に [build.lifecycle].prepare を設定するか、package.json scripts[\"capsule:prepare\"] を追加して再実行してください。".to_string();
                    phase_mark_failed(
                        phase_mut(&mut self.phases, PublishPhaseBoundary::Prepare),
                        started.elapsed().as_millis() as u64,
                        "--prepare was selected but no prepare step was found".to_string(),
                        Some(fix.clone()),
                    );
                    hourglass::print_phase_line(
                        self.args.json,
                        PublishPhaseBoundary::Prepare,
                        HourglassPhaseState::Fail,
                        "prepare step not found",
                    );
                    if !self.args.json {
                        println!("👉 次に打つコマンド: {}", fix);
                    }
                    anyhow::bail!(
                        "--prepare was selected but no prepare step was found. Add lockfile-backed dependency resolution, set `build.lifecycle.prepare` in capsule.toml, or add package.json scripts[\"capsule:prepare\"]."
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

        let authoritative_input = self
            .authoritative_input
            .as_ref()
            .context("build phase requires authoritative source input")?;
        if self.args.finalize_local {
            authoritative_input.ensure_finalize_local_publish_ready()?;
        } else {
            authoritative_input.ensure_desktop_source_publish_ready()?;
        }
        let artifact_path = build_capsule_artifact_for_publish(
            &self.cwd,
            Some(authoritative_input),
            !self.args.json,
        )?;
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

    fn refresh_authoritative_input_after_prepare(&mut self) -> Result<()> {
        let should_refresh = self
            .authoritative_input
            .as_ref()
            .and_then(|input| input.desktop_source_publish_contract())
            .is_some();
        if !should_refresh {
            return Ok(());
        }

        self.authoritative_input = Some(rematerialize_source_authoritative_input(
            &self.cwd,
            std::sync::Arc::new(crate::reporters::CliReporter::new(false)),
            false,
        )?);
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

    async fn run_install_phase(&mut self, attempt: &mut PipelineAttemptContext) -> Result<()> {
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
        let verification = self.state.verified_artifact();
        let install_result = install_phase::run_publish_install_phase_async(
            artifact_path,
            preview,
            verification,
            Some(attempt),
        )
        .await?;
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

    fn run_finalize_phase(&mut self) -> Result<()> {
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::Finalize,
            HourglassPhaseState::Run,
            "local finalize and repack",
        );
        let started = std::time::Instant::now();
        let authoritative_input = self
            .authoritative_input
            .as_ref()
            .context("finalize phase requires authoritative source input")?;
        authoritative_input.ensure_finalize_local_publish_ready()?;
        let preview = self
            .pipeline_preview
            .as_ref()
            .context("missing publish pipeline preview for finalize stage")?;
        let unsigned_artifact = self.state.artifact_path().context(
            "finalize phase requires an unsigned artifact built earlier in the pipeline",
        )?;
        let lock_json = authoritative_input.serialized_lock_json()?;
        let signed = crate::build::native_delivery::finalize_capsule_artifact_for_publish(
            unsigned_artifact,
            &preview.scoped_id,
            &preview.version,
            Some(lock_json.as_str()),
            self.args.allow_external_finalize,
        )?;
        let message = format!("finalized {}", signed.artifact_path.display());
        phase_mark_ok(
            phase_mut(&mut self.phases, PublishPhaseBoundary::Finalize),
            started.elapsed().as_millis() as u64,
            message.clone(),
            Some(signed.identity_class.to_string()),
        );
        self.state.record_built_artifact(signed.artifact_path);
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::Finalize,
            HourglassPhaseState::Ok,
            &message,
        );
        Ok(())
    }

    async fn run_dry_run_phase(&mut self) -> Result<()> {
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::DryRun,
            HourglassPhaseState::Run,
            "publish preflight",
        );
        let started = std::time::Instant::now();
        if self.is_official {
            let outcome = publish_phase::run_official_publish_phase(
                &publish_phase::OfficialPublishRequest {
                    cwd: &self.cwd,
                    registry_url: &self.resolved_target.registry_url,
                    fix: self.args.fix,
                },
            )?;
            if !self.args.json {
                for line in publish_phase::official_publish_diagnosis_lines(&outcome) {
                    println!("{}", line);
                }
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

            let fix_line = publish_phase::official_publish_failure_action(
                self.official_result.as_ref().expect("official outcome"),
            );
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
                let issue_lines = publish_phase::official_publish_issue_lines(
                    self.official_result.as_ref().expect("official outcome"),
                );
                if !issue_lines.is_empty() {
                    println!();
                    println!("詳細:");
                    for line in issue_lines {
                        println!("{}", line);
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
        let registry_url = self.resolved_target.registry_url.clone();
        let scoped_id = preview.scoped_id.clone();
        let version = preview.version.clone();
        let artifact_version = verification.version.clone();
        let allow_existing = self.args.allow_existing;
        let requires_session_token = self.resolved_target.mode.is_personal_dock();
        let dry_run_result = tokio::task::spawn_blocking(move || {
            publish_phase::run_direct_publish_dry_run_phase(
                &publish_phase::DirectPublishDryRunRequest {
                    registry_url: &registry_url,
                    scoped_id: &scoped_id,
                    version: &version,
                    artifact_version: &artifact_version,
                    allow_existing,
                    requires_session_token,
                },
            )
        })
        .await
        .context("publish dry-run worker panicked")??;
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

    async fn run_publish_phase(&mut self) -> Result<()> {
        hourglass::print_phase_line(
            self.args.json,
            PublishPhaseBoundary::Publish,
            HourglassPhaseState::Run,
            "publish execution",
        );
        let started = std::time::Instant::now();
        if self.is_official {
            let outcome = publish_phase::run_official_publish_phase(
                &publish_phase::OfficialPublishRequest {
                    cwd: &self.cwd,
                    registry_url: &self.resolved_target.registry_url,
                    fix: self.args.fix,
                },
            )?;
            if !self.args.json {
                for line in publish_phase::official_publish_diagnosis_lines(&outcome) {
                    println!("{}", line);
                }
            }

            if !outcome.diagnosis.can_handoff {
                let fix_line = publish_phase::official_publish_failure_action(&outcome);
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
                    let issue_lines = publish_phase::official_publish_issue_lines(&outcome);
                    if !issue_lines.is_empty() {
                        println!();
                        println!("詳細:");
                        for line in issue_lines {
                            println!("{}", line);
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
            self.reporter
                .progress_start(status.to_string(), None)
                .await?;
        }
        let scoped_id = if source_is_artifact {
            self.args.scoped_id.clone()
        } else {
            Some(preview.scoped_id.clone())
        };
        let source_lock_metadata = if source_is_artifact {
            (None, None, None)
        } else {
            let resolved = self
                .authoritative_input
                .as_ref()
                .context("publish phase requires authoritative source input")?;
            let publish_metadata =
                resolved.publish_metadata_for_source_artifact(self.args.finalize_local);
            (
                resolved.lock_id.clone(),
                resolved.closure_digest.clone(),
                publish_metadata,
            )
        };
        let upload_result =
            publish_phase::run_private_publish_phase_async(publish_phase::PrivatePublishRequest {
                registry_url: self.resolved_target.registry_url.clone(),
                publisher_hint: self.resolved_target.publisher_handle.clone(),
                artifact_path: Some(publish_artifact),
                force_large_payload: self.args.force_large_payload,
                scoped_id,
                allow_existing: self.args.allow_existing,
                lock_id: source_lock_metadata.0,
                closure_digest: source_lock_metadata.1,
                publish_metadata: source_lock_metadata.2,
            })
            .await;
        if !self.args.json {
            self.reporter.progress_finish(None).await?;
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
    async fn run_phase(
        &mut self,
        phase: PublishPhaseBoundary,
        attempt: &mut PipelineAttemptContext,
    ) -> Result<()> {
        match phase {
            PublishPhaseBoundary::Prepare => self.run_prepare_phase(),
            PublishPhaseBoundary::Build => self.run_build_phase(),
            PublishPhaseBoundary::Finalize => self.run_finalize_phase(),
            PublishPhaseBoundary::Verify => self.run_verify_phase(),
            PublishPhaseBoundary::Install => self.run_install_phase(attempt).await,
            PublishPhaseBoundary::DryRun => self.run_dry_run_phase().await,
            PublishPhaseBoundary::Publish => self.run_publish_phase().await,
            PublishPhaseBoundary::Execute => {
                anyhow::bail!("unsupported publish pipeline phase {}", phase.as_str())
            }
        }
    }

    async fn skip_phase(
        &mut self,
        phase: PublishPhaseBoundary,
        _attempt: &mut PipelineAttemptContext,
    ) -> Result<()> {
        match phase {
            PublishPhaseBoundary::Prepare
            | PublishPhaseBoundary::Build
            | PublishPhaseBoundary::Finalize
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

pub(crate) fn execute_publish_command(
    args: PublishCommandArgs,
    ci: bool,
    dry_run: bool,
    force_large_payload: bool,
    json: bool,
    reporter: Reporter,
) -> Result<()> {
    if ci {
        execute_publish_ci_command(json, force_large_payload, reporter)
    } else if dry_run {
        execute_publish_dry_run_command(args, reporter)
    } else {
        execute_publish_pipeline(args, reporter, false)
    }
}

fn execute_publish_pipeline(
    args: PublishCommandArgs,
    reporter: Reporter,
    top_level_dry_run: bool,
) -> Result<()> {
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
            finalize_local: args.finalize_local,
        },
        PublishPhaseOptions {
            fix: args.fix,
            legacy_full_publish: args.legacy_full_publish,
            allow_existing: args.allow_existing,
            finalize_local: args.finalize_local,
            allow_external_finalize: args.allow_external_finalize,
        },
    )?;
    let selection = plan.selection;
    let authoritative_input = resolve_publish_source_authoritative_input(&args)?;
    if is_official
        && authoritative_input
            .as_ref()
            .and_then(|input| input.desktop_source_publish_contract())
            .is_some()
    {
        anyhow::bail!(
            "official/CI publish does not yet support Tauri/Electron/Wails source publish; use private/local registry publish first"
        );
    }
    if resolved_target.mode.is_personal_dock() && selection.runs_publish() {
        let _ = crate::auth::require_session_token()?;
    }
    maybe_warn_legacy_full_publish(plan.warn_legacy_full_publish);
    let _ = args.no_tui;
    let phases = hourglass::new_phase_results(selection.flow, selection);

    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let pipeline_preview = if selection.runs_install()
        || selection.runs_dry_run()
        || (!is_official && selection.runs_publish())
    {
        Some(publish_phase::summarize_private_publish(
            &publish_phase::PrivatePublishRequest {
                registry_url: resolved_target.registry_url.clone(),
                publisher_hint: resolved_target.publisher_handle.clone(),
                artifact_path: args.artifact.clone(),
                force_large_payload: args.force_large_payload,
                scoped_id: args.scoped_id.clone(),
                allow_existing: args.allow_existing,
                lock_id: None,
                closure_digest: None,
                publish_metadata: None,
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
        authoritative_input,
    );
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle.block_on(pipeline.run_until(selection.stop, &mut execution))
        })?,
        Err(_) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(pipeline.run_until(selection.stop, &mut execution))?;
        }
    }

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

fn build_capsule_artifact_for_publish(
    cwd: &Path,
    authoritative_input: Option<&crate::application::producer_input::ProducerAuthoritativeInput>,
    stream_output: bool,
) -> Result<PathBuf> {
    let owned_input;
    let authoritative_input = if let Some(authoritative_input) = authoritative_input {
        authoritative_input
    } else {
        owned_input = resolve_producer_authoritative_input(
            cwd,
            std::sync::Arc::new(crate::reporters::CliReporter::new(false)),
            false,
        )?;
        &owned_input
    };
    let metadata = &authoritative_input.descriptor.runtime_model.metadata;
    let name = metadata
        .name
        .clone()
        .filter(|value| !value.trim().is_empty())
        .context("authoritative lock metadata is missing package name")?;
    let version = if metadata
        .version
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        "auto"
    } else {
        metadata.version.as_deref().unwrap_or_default().trim()
    };
    crate::publish_ci::build_capsule_artifact_with_output(
        &name,
        version,
        Some(authoritative_input),
        None,
        stream_output,
    )
    .with_context(|| "Failed to build artifact for publish")
}

fn emit_publish_json_output(
    resolved_target: &ResolvedPublishTarget,
    phases: &[PublishPhaseResult],
    install_result: Option<&PublishInstallResult>,
    dry_run_result: Option<&PublishDryRunStageResult>,
    private_result: Option<&publish_phase::PrivatePublishResult>,
    official_result: Option<&publish_phase::OfficialPublishOutcome>,
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
        return producer::resolve_publish_target_from_sources(Some(&url), None, None);
    }

    let manifest_registry = discover_manifest_publish_registry()?;
    if let Some(url) = manifest_registry {
        return producer::resolve_publish_target_from_sources(Some(&url), None, None);
    }

    let publisher_handle = crate::auth::current_publisher_handle()?;

    producer::resolve_publish_target_from_sources(None, None, publisher_handle.as_deref())
}

fn resolve_publish_source_authoritative_input(
    args: &PublishCommandArgs,
) -> Result<Option<crate::application::producer_input::ProducerAuthoritativeInput>> {
    if args.artifact.is_some() {
        return Ok(None);
    }

    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    resolve_producer_authoritative_input(
        &cwd,
        std::sync::Arc::new(crate::reporters::CliReporter::new(false)),
        false,
    )
    .map(Some)
}

fn discover_manifest_publish_registry() -> Result<Option<String>> {
    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let authoritative_input = match resolve_producer_authoritative_input(
        &cwd,
        std::sync::Arc::new(crate::reporters::CliReporter::new(false)),
        false,
    ) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    Ok(authoritative_input.compatibility_publish_registry())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CwdGuard {
        original: PathBuf,
    }

    impl CwdGuard {
        fn set_to(path: &Path) -> Self {
            let original = std::env::current_dir().expect("cwd");
            std::env::set_current_dir(path).expect("set cwd");
            Self { original }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    #[test]
    fn build_capsule_artifact_for_publish_does_not_materialize_capsule_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"demo","scripts":{"start":"node index.js"}}"#,
        )
        .expect("package.json");
        std::fs::write(
            tmp.path().join("package-lock.json"),
            r#"{"name":"demo","lockfileVersion":3,"packages":{}}"#,
        )
        .expect("package-lock.json");
        std::fs::write(tmp.path().join("index.js"), "console.log('demo');\n").expect("index.js");
        let _guard = CwdGuard::set_to(tmp.path());

        let error = build_capsule_artifact_for_publish(tmp.path(), None, false)
            .expect_err("publish build may fail but must not materialize manifest");
        assert!(!error.to_string().is_empty());
        assert!(!tmp.path().join("capsule.toml").exists());
    }

    #[test]
    fn resolve_publish_source_authoritative_input_does_not_materialize_capsule_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"demo","version":"0.1.0","scripts":{"start":"node index.js"}}"#,
        )
        .expect("package.json");
        std::fs::write(
            tmp.path().join("package-lock.json"),
            r#"{"name":"demo","version":"0.1.0","lockfileVersion":3,"packages":{}}"#,
        )
        .expect("package-lock.json");
        std::fs::write(tmp.path().join("index.js"), "console.log('demo');\n").expect("index.js");
        let _guard = CwdGuard::set_to(tmp.path());

        resolve_publish_source_authoritative_input(&PublishCommandArgs {
            registry: None,
            artifact: None,
            scoped_id: None,
            allow_existing: false,
            prepare: false,
            build: false,
            deploy: false,
            legacy_full_publish: false,
            force_large_payload: false,
            finalize_local: false,
            allow_external_finalize: false,
            fix: false,
            no_tui: true,
            json: true,
        })
        .expect("authoritative publish source should resolve without manifest write")
        .expect("source authoritative input");

        assert!(!tmp.path().join("capsule.toml").exists());
    }

    #[test]
    fn discover_manifest_publish_registry_does_not_materialize_capsule_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"demo","version":"0.1.0","scripts":{"start":"node index.js"}}"#,
        )
        .expect("package.json");
        std::fs::write(
            tmp.path().join("package-lock.json"),
            r#"{"name":"demo","version":"0.1.0","lockfileVersion":3,"packages":{}}"#,
        )
        .expect("package-lock.json");
        std::fs::write(tmp.path().join("index.js"), "console.log('demo');\n").expect("index.js");
        let _guard = CwdGuard::set_to(tmp.path());

        let registry = discover_manifest_publish_registry().expect("discover publish registry");

        assert!(registry.is_none());
        assert!(!tmp.path().join("capsule.toml").exists());
    }
}

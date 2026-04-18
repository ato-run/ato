use anyhow::{Context, Result};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::router::{
    CompatManifestBridge, CompatProjectInput, ExecutionDescriptor, RuntimeDecision, RuntimeKind,
};
use capsule_core::types::{CapsuleManifest, ValidationMode};
use capsule_core::CapsuleReporter;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::IsTerminal;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::debug;
use walkdir::WalkDir;

use crate::application::producer_input::resolve_producer_authoritative_input;
use crate::build::native_delivery;
use crate::project::init;
use crate::reporters;
use crate::runtime::overrides as runtime_overrides;

const BUILD_CACHE_LAYOUT_VERSION: &str = "chml-build-cache-v1";
const BUILD_CACHE_IGNORED_DIRS: &[&str] = &[
    ".git",
    ".tmp",
    "node_modules",
    ".venv",
    "target",
    "__pycache__",
];

#[derive(Debug, Serialize)]
pub struct BuildResult {
    pub ok: bool,
    pub kind: String,
    pub artifact: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub build_strategy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
#[error("Smoke test failed: {report}")]
pub struct InferredManifestSmokeFailure {
    pub report: capsule_core::smoke::SmokeFailureReport,
}

fn runtime_kind_from_plan(plan: &ExecutionDescriptor) -> Result<RuntimeKind> {
    match plan
        .execution_runtime()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "source" | "native" => Ok(RuntimeKind::Source),
        "web" => Ok(RuntimeKind::Web),
        "wasm" => Ok(RuntimeKind::Wasm),
        "oci" | "docker" | "youki" | "runc" => Ok(RuntimeKind::Oci),
        other => anyhow::bail!("Unsupported runtime '{other}'"),
    }
}

fn build_decision_from_manifest_text(
    workspace_root: &Path,
    manifest_text: &str,
    validation_mode: ValidationMode,
) -> Result<(RuntimeDecision, CompatManifestBridge)> {
    let bridge = {
        // Parse and validate normally. Then get the intermediate normalized TOML (with
        // [targets.<label>] populated) separately, bypassing the re-validation that would reject
        // v0.2-style `entrypoint` fields produced by normalize_v03_target_table.
        let parsed = CapsuleManifest::from_toml(manifest_text)
            .map_err(|err| anyhow::anyhow!("Failed to parse manifest: {err}"))?;
        let compat_toml = CapsuleManifest::normalize_to_compat_toml(manifest_text)
            .map_err(|err| anyhow::anyhow!("Failed to normalize manifest: {err}"))?;
        CompatManifestBridge::from_compat_normalized(parsed, compat_toml)
    };
    bridge
        .manifest_model()
        .validate_for_mode(validation_mode)
        .map_err(|errors| {
            anyhow::anyhow!(
                "Manifest validation failed: {}",
                errors
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })?;
    let raw = bridge
        .toml_value()
        .context("Failed to parse raw manifest bridge TOML")?;
    let plan = capsule_core::router::execution_descriptor_from_manifest_parts(
        raw,
        workspace_root.join("capsule.toml"),
        workspace_root.to_path_buf(),
        capsule_core::router::ExecutionProfile::Release,
        None,
        std::collections::HashMap::new(),
    )?;
    let kind = runtime_kind_from_plan(&plan)?;
    Ok((
        RuntimeDecision {
            kind,
            reason: format!("compat target {}", plan.selected_target_label()),
            plan,
        },
        bridge,
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn execute_pack_command(
    dir: PathBuf,
    init_if_missing: bool,
    key: Option<PathBuf>,
    standalone: bool,
    force_large_payload: bool,
    paid_large_payload: bool,
    keep_failed_artifacts: bool,
    strict_manifest: bool,
    enforcement: String,
    reporter: std::sync::Arc<reporters::CliReporter>,
    timings: bool,
    cli_json: bool,
    nacelle_override: Option<PathBuf>,
) -> Result<BuildResult> {
    execute_pack_command_with_injected_manifest(
        dir,
        init_if_missing,
        key,
        standalone,
        force_large_payload,
        paid_large_payload,
        keep_failed_artifacts,
        strict_manifest,
        enforcement,
        reporter,
        timings,
        cli_json,
        nacelle_override,
        None,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn execute_pack_command_with_injected_manifest(
    dir: PathBuf,
    init_if_missing: bool,
    key: Option<PathBuf>,
    standalone: bool,
    force_large_payload: bool,
    paid_large_payload: bool,
    keep_failed_artifacts: bool,
    strict_manifest: bool,
    enforcement: String,
    reporter: std::sync::Arc<reporters::CliReporter>,
    timings: bool,
    cli_json: bool,
    nacelle_override: Option<PathBuf>,
    injected_manifest: Option<&str>,
    suppress_injected_manifest_warning: bool,
) -> Result<BuildResult> {
    let total_started = Instant::now();
    let mut timing_entries = Vec::new();
    let dir = dir
        .canonicalize()
        .with_context(|| format!("Failed to resolve directory: {}", dir.display()))?;
    if !dir.is_dir() {
        anyhow::bail!("Target is not a directory: {}", dir.display());
    }

    let manifest = dir.join("capsule.toml");
    let authoritative_input = if injected_manifest.is_none() {
        let resolved = resolve_producer_authoritative_input(&dir, reporter.clone(), false)?;
        for advisory in &resolved.advisories {
            futures::executor::block_on(reporter.warn(advisory.clone()))?;
        }
        Some(resolved)
    } else {
        None
    };

    let fallback_manifest_text = if authoritative_input.is_none() && !manifest.exists() {
        let stdin_is_tty = std::io::stdin().is_terminal();
        if init_if_missing {
            if !stdin_is_tty {
                anyhow::bail!("--init requires an interactive TTY");
            }
            if cli_json {
                anyhow::bail!("--init cannot be used with --json output");
            }
            init::write_legacy_detected_manifest(Some(dir.clone()), reporter.clone())?;
            None
        } else if let Some(manifest_text) = injected_manifest {
            if !suppress_injected_manifest_warning
                && crate::progressive_ui::can_use_progressive_ui(cli_json)
            {
                crate::progressive_ui::show_warning(
                    "No `capsule.toml` found. Using draft returned by ato store for this GitHub repository.",
                )?;
            } else if !suppress_injected_manifest_warning {
                futures::executor::block_on(reporter.warn(
                    "No `capsule.toml` found. Using draft returned by ato store for this GitHub repository.".to_string(),
                ))?;
            }
            Some(manifest_text.to_string())
        } else {
            futures::executor::block_on(reporter.warn(
                "No `capsule.toml` found. Using defaults. Run `ato init` to materialize `ato.lock.json`, or `ato build --init` to create an inferred compatibility `capsule.toml`.".to_string(),
            ))?;
            Some(infer_zero_config_manifest(&dir)?)
        }
    } else {
        None
    };

    if authoritative_input.is_none() && !manifest.exists() && fallback_manifest_text.is_none() {
        anyhow::bail!("capsule.toml not found after initialization");
    }

    let validation_mode = if injected_manifest.is_some() {
        ValidationMode::Preview
    } else {
        ValidationMode::Strict
    };

    let validation_started = Instant::now();
    let (decision, raw_manifest, capsule_name, capsule_version) = if let Some(authoritative_input) =
        authoritative_input.as_ref()
    {
        authoritative_input.validate_legacy_producer_bridge()?;
        let kind = runtime_kind_from_plan(&authoritative_input.descriptor)?;
        let capsule_name = authoritative_input.semantic_package_name()?;
        let capsule_version = authoritative_input.semantic_package_version();
        let raw_manifest = authoritative_input
            .legacy_producer_manifest_value()
            .unwrap_or_else(|| authoritative_input.descriptor.manifest.clone());
        (
            RuntimeDecision {
                kind,
                reason: format!(
                    "lock target {}",
                    authoritative_input.descriptor.selected_target_label()
                ),
                plan: authoritative_input.descriptor.clone(),
            },
            raw_manifest,
            capsule_name,
            capsule_version,
        )
    } else if let Some(manifest_text) = fallback_manifest_text.as_deref() {
        let (decision, bridge) =
            build_decision_from_manifest_text(&dir, manifest_text, validation_mode)?;
        (
            decision,
            bridge
                .toml_value()
                .context("Failed to parse fallback manifest bridge")?,
            bridge.package_name().to_string(),
            bridge.package_version().to_string(),
        )
    } else {
        let decision = capsule_core::router::route_manifest_with_validation_mode(
            &manifest,
            capsule_core::router::ExecutionProfile::Release,
            None,
            validation_mode,
        )?;
        let loaded_manifest =
            capsule_core::manifest::load_manifest_with_validation_mode(&manifest, validation_mode)?;
        let raw_manifest: toml::Value = toml::from_str(&loaded_manifest.raw_text)
            .context("Failed to parse manifest TOML for IPC validation")?;
        capsule_core::diagnostics::manifest::validate_manifest_for_build_with_mode(
            &manifest,
            decision.plan.selected_target_label(),
            validation_mode,
        )?;
        (
            decision,
            raw_manifest,
            loaded_manifest.model.name.clone(),
            loaded_manifest.model.version.clone(),
        )
    };
    let ipc_diagnostics =
        crate::ipc::validate::validate_manifest(&raw_manifest, &dir).map_err(|err| {
            AtoExecutionError::execution_contract_invalid(
                format!("IPC validation failed: {err}"),
                None,
                None,
            )
        })?;
    if crate::ipc::validate::has_errors(&ipc_diagnostics) {
        return Err(AtoExecutionError::execution_contract_invalid(
            crate::ipc::validate::format_diagnostics(&ipc_diagnostics),
            None,
            None,
        )
        .into());
    }
    for diagnostic in ipc_diagnostics {
        futures::executor::block_on(reporter.warn(diagnostic.to_string()))?;
    }
    run_v03_build_lifecycle_steps(&decision.plan, &reporter, injected_manifest.is_none())?;
    record_timing(
        &mut timing_entries,
        "build.validation",
        validation_started.elapsed(),
    );

    if crate::progressive_ui::can_use_progressive_ui(cli_json) {
        crate::progressive_ui::show_step(format!(
            "Packing capsule \"{}\" (v{})...",
            capsule_name, capsule_version
        ))?;
    } else {
        futures::executor::block_on(reporter.notify(format!(
            "📦 Packing capsule \"{}\" (v{})...",
            capsule_name, capsule_version
        )))?;
    }
    debug!(
        runtime_kind = ?decision.kind,
        reason = %decision.reason,
        "Build runtime routed"
    );
    let native_plan = native_delivery::detect_build_strategy_with_legacy_fallback(&decision.plan)?;

    if let Some(plan) = native_plan {
        let build_started = Instant::now();
        let result = native_delivery::build_native_artifact(&plan, None)?;
        record_timing(&mut timing_entries, "build.pack", build_started.elapsed());
        crate::payload_guard::ensure_payload_size(
            &result.artifact_path,
            force_large_payload,
            paid_large_payload,
            "--force-large-payload",
        )?;
        let _ = sign_if_requested(&result.artifact_path, key.as_ref(), reporter.clone())?;
        let size = std::fs::metadata(&result.artifact_path)?.len();
        if crate::progressive_ui::can_use_progressive_ui(cli_json) {
            crate::progressive_ui::show_success(format!(
                "Successfully built: {} ({:.1} KB)",
                result.artifact_path.display(),
                size as f64 / 1024.0
            ))?;
        } else {
            futures::executor::block_on(reporter.notify(format!(
                "✅ Successfully built: {} ({:.1} KB)",
                result.artifact_path.display(),
                size as f64 / 1024.0
            )))?;
        }
        record_timing(&mut timing_entries, "build.total", total_started.elapsed());
        emit_timings(reporter.clone(), timings, &timing_entries)?;
        return Ok(BuildResult {
            ok: true,
            kind: "capsule".to_string(),
            artifact: Some(result.artifact_path),
            image: None,
            build_strategy: result.build_strategy,
            schema_version: Some(result.schema_version),
            target: Some(result.target),
            derived_from: Some(result.derived_from),
        });
    }

    let result = match decision.kind {
        capsule_core::router::RuntimeKind::Source => {
            let compat_input = if let Some(authoritative_input) = authoritative_input.as_ref() {
                authoritative_input.packaging_compat_project_input()?
            } else {
                decision.plan.compat_project_input()?
            };
            let artifact_path = pack_source_bundle(
                &decision.plan,
                compat_input,
                &enforcement,
                standalone,
                strict_manifest,
                timings,
                nacelle_override.clone(),
                reporter.clone(),
                &mut timing_entries,
                "⏳ [build] Preparing source runtime bundle...",
            )?;

            if standalone {
                futures::executor::block_on(
                    reporter.warn(
                        "⚠️  Phase 1: --standalone build is not smoke-tested yet (planned in next phase)"
                            .to_string(),
                    ),
                )?;
            } else {
                debug!("Running smoke test");
                futures::executor::block_on(
                    reporter.progress_start("🧪 [build] Running smoke test...".to_string(), None),
                )?;
                let smoke_started = Instant::now();
                match capsule_core::smoke::run_capsule_smoke(
                    &artifact_path,
                    decision.plan.selected_target_label(),
                ) {
                    Ok(summary) => {
                        futures::executor::block_on(reporter.progress_finish(None))?;
                        record_timing(&mut timing_entries, "build.smoke", smoke_started.elapsed());
                        debug!(
                            "Smoke passed (timeout={}ms, port={:?}, checks={})",
                            summary.startup_timeout_ms,
                            summary.required_port,
                            summary.checked_commands
                        );
                    }
                    Err(err) => {
                        futures::executor::block_on(reporter.progress_finish(None))?;
                        record_timing(&mut timing_entries, "build.smoke", smoke_started.elapsed());
                        cleanup_failed_artifact(
                            &artifact_path,
                            keep_failed_artifacts,
                            reporter.clone(),
                        )?;
                        if injected_manifest.is_some() {
                            return Err(InferredManifestSmokeFailure { report: err }.into());
                        }
                        anyhow::bail!("Smoke test failed: {err}");
                    }
                }
            }

            finalize_built_artifact(
                &artifact_path,
                force_large_payload,
                paid_large_payload,
                key.as_ref(),
                reporter.clone(),
                &mut timing_entries,
            )?;
            BuildResult {
                ok: true,
                kind: "capsule".to_string(),
                artifact: Some(artifact_path),
                image: None,
                build_strategy: "source".to_string(),
                schema_version: None,
                target: None,
                derived_from: None,
            }
        }
        capsule_core::router::RuntimeKind::Oci => {
            let result = capsule_core::packers::oci::pack(&decision.plan, None, reporter.as_ref())?;
            let archive = result.archive.clone();
            if let Some(ref path) = archive {
                crate::payload_guard::ensure_payload_size(
                    path,
                    force_large_payload,
                    paid_large_payload,
                    "--force-large-payload",
                )?;
                let _ = sign_if_requested(path, key.as_ref(), reporter.clone())?;
                let size = std::fs::metadata(path)?.len();
                futures::executor::block_on(reporter.notify(format!(
                    "✅ Successfully built: {} ({:.1} KB)",
                    path.display(),
                    size as f64 / 1024.0
                )))?;
            } else if key.is_some() {
                futures::executor::block_on(
                    reporter.warn(
                        "ℹ️  Signature skipped: OCI pack produced no archive file".to_string(),
                    ),
                )?;
            } else {
                futures::executor::block_on(
                    reporter.notify(format!("✅ Pack complete: {}", result.image)),
                )?;
            }
            BuildResult {
                ok: true,
                kind: if archive.is_some() {
                    "capsule".to_string()
                } else {
                    "image".to_string()
                },
                artifact: archive,
                image: Some(result.image),
                build_strategy: "oci".to_string(),
                schema_version: None,
                target: None,
                derived_from: None,
            }
        }
        capsule_core::router::RuntimeKind::Wasm => {
            let result =
                capsule_core::packers::wasm::pack(&decision.plan, None, None, reporter.as_ref())?;
            crate::payload_guard::ensure_payload_size(
                &result.artifact,
                force_large_payload,
                paid_large_payload,
                "--force-large-payload",
            )?;
            let size = std::fs::metadata(&result.artifact)?.len();
            futures::executor::block_on(reporter.notify(format!(
                "✅ Successfully built: {} ({:.1} KB)",
                result.artifact.display(),
                size as f64 / 1024.0
            )))?;
            let _ = sign_if_requested(&result.artifact, key.as_ref(), reporter.clone())?;
            BuildResult {
                ok: true,
                kind: "capsule".to_string(),
                artifact: Some(result.artifact),
                image: None,
                build_strategy: "wasm".to_string(),
                schema_version: None,
                target: None,
                derived_from: None,
            }
        }
        capsule_core::router::RuntimeKind::Web => {
            let compat_input = if let Some(authoritative_input) = authoritative_input.as_ref() {
                authoritative_input.packaging_compat_project_input()?
            } else {
                decision.plan.compat_project_input()?
            };
            let driver = decision
                .plan
                .execution_driver()
                .map(|v| v.trim().to_ascii_lowercase())
                .ok_or_else(|| anyhow::anyhow!("runtime=web target requires driver"))?;

            let artifact_path = if driver == "static" {
                if standalone {
                    anyhow::bail!("--standalone is not supported for runtime=web driver=static");
                }
                capsule_core::packers::web::pack(
                    &decision.plan,
                    capsule_core::packers::web::WebPackOptions {
                        compat_input: compat_input.clone(),
                        workspace_root: decision.plan.workspace_root.clone(),
                        output: None,
                    },
                    reporter.clone(),
                )?
            } else {
                let artifact = pack_source_bundle(
                    &decision.plan,
                    compat_input,
                    &enforcement,
                    standalone,
                    strict_manifest,
                    timings,
                    nacelle_override.clone(),
                    reporter.clone(),
                    &mut timing_entries,
                    "⏳ [build] Preparing web runtime bundle...",
                )?;

                if standalone {
                    futures::executor::block_on(
                        reporter.warn(
                            "⚠️  Phase 1: --standalone build is not smoke-tested yet (planned in next phase)"
                                .to_string(),
                        ),
                    )?;
                }
                artifact
            };

            finalize_built_artifact(
                &artifact_path,
                force_large_payload,
                paid_large_payload,
                key.as_ref(),
                reporter.clone(),
                &mut timing_entries,
            )?;
            BuildResult {
                ok: true,
                kind: "capsule".to_string(),
                artifact: Some(artifact_path),
                image: None,
                build_strategy: "web".to_string(),
                schema_version: None,
                target: None,
                derived_from: None,
            }
        }
    };

    record_timing(&mut timing_entries, "build.total", total_started.elapsed());
    emit_timings(reporter.clone(), timings, &timing_entries)?;

    Ok(result)
}

fn record_timing(entries: &mut Vec<(String, Duration)>, label: &str, elapsed: Duration) {
    entries.push((label.to_string(), elapsed));
}

fn emit_timings(
    reporter: std::sync::Arc<reporters::CliReporter>,
    enabled: bool,
    entries: &[(String, Duration)],
) -> Result<()> {
    if !enabled {
        return Ok(());
    }

    for (label, elapsed) in entries {
        futures::executor::block_on(
            reporter.notify(format!("⏱ [timings] {label}: {} ms", elapsed.as_millis())),
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn pack_source_bundle(
    plan: &capsule_core::router::ManifestData,
    compat_input: Option<CompatProjectInput>,
    enforcement: &str,
    standalone: bool,
    strict_manifest: bool,
    timings: bool,
    nacelle_override: Option<PathBuf>,
    reporter: std::sync::Arc<reporters::CliReporter>,
    timing_entries: &mut Vec<(String, Duration)>,
    progress_label: &str,
) -> Result<PathBuf> {
    let prepare_started = Instant::now();
    let prepared_config = capsule_core::packers::source::prepare_source_config_from_descriptor(
        plan,
        enforcement.to_string(),
        standalone,
    )?;
    record_timing(
        timing_entries,
        "build.prepare_source_config",
        prepare_started.elapsed(),
    );
    futures::executor::block_on(reporter.progress_start(progress_label.to_string(), None))?;
    let pack_started = Instant::now();
    let artifact = capsule_core::packers::source::pack(
        plan,
        capsule_core::packers::source::SourcePackOptions {
            compat_input,
            workspace_root: plan.workspace_root.clone(),
            config_json: prepared_config.config_json.clone(),
            config_path: prepared_config.config_path.clone(),
            output: None,
            runtime: None,
            skip_l1: false,
            skip_validation: false,
            nacelle_override,
            standalone,
            strict_manifest,
            timings,
        },
        reporter.clone(),
    );
    futures::executor::block_on(reporter.progress_finish(None))?;
    let artifact = artifact?;
    record_timing(timing_entries, "build.pack", pack_started.elapsed());
    Ok(artifact)
}

fn finalize_built_artifact(
    artifact_path: &Path,
    force_large_payload: bool,
    paid_large_payload: bool,
    key: Option<&PathBuf>,
    reporter: std::sync::Arc<reporters::CliReporter>,
    timing_entries: &mut Vec<(String, Duration)>,
) -> Result<()> {
    let payload_guard_started = Instant::now();
    crate::payload_guard::ensure_payload_size(
        artifact_path,
        force_large_payload,
        paid_large_payload,
        "--force-large-payload",
    )?;
    record_timing(
        timing_entries,
        "build.payload_guard",
        payload_guard_started.elapsed(),
    );
    let sign_started = Instant::now();
    let _ = sign_if_requested(artifact_path, key, reporter.clone())?;
    record_timing(timing_entries, "build.sign", sign_started.elapsed());
    let size = std::fs::metadata(artifact_path)?.len();
    futures::executor::block_on(reporter.notify(format!(
        "✅ Successfully built: {} ({:.1} KB)",
        artifact_path.display(),
        size as f64 / 1024.0
    )))?;
    Ok(())
}

fn infer_zero_config_manifest(dir: &Path) -> Result<String> {
    let raw_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Failed to infer project name from directory"))?;
    let name = sanitize_kebab_case(raw_name);
    let name = if name.is_empty() {
        "app".to_string()
    } else {
        name
    };

    let entrypoint = infer_entrypoint(dir).ok_or_else(|| {
        anyhow::anyhow!(
            "capsule.toml not found and entrypoint could not be inferred. Add capsule.toml, run `ato init` for an agent prompt, or use `ato build --init`."
        )
    })?;

    Ok(format!(
        r#"schema_version = "0.3"
name = "{name}"
version = "0.1.0"
type = "app"

runtime = "source"
run = "{entrypoint}"
[metadata]
description = "Generated by zero-config build fallback"
"#,
        name = toml_escape(&name),
        entrypoint = toml_escape(entrypoint),
    ))
}

fn sanitize_kebab_case(input: &str) -> String {
    input
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn infer_entrypoint(dir: &Path) -> Option<&'static str> {
    let candidates = ["main.py", "app.py", "index.js", "main.rs", "main.sh"];
    candidates
        .into_iter()
        .find(|candidate| dir.join(candidate).exists())
}

fn toml_escape(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn cleanup_failed_artifact(
    artifact_path: &PathBuf,
    keep_failed_artifacts: bool,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<()> {
    if keep_failed_artifacts {
        futures::executor::block_on(reporter.warn(format!(
            "⚠️  Build failed but artifact kept for debugging: {}",
            artifact_path.display()
        )))?;
        return Ok(());
    }

    if artifact_path.exists() {
        if let Err(err) = std::fs::remove_file(artifact_path) {
            futures::executor::block_on(reporter.warn(format!(
                "⚠️  Failed to remove artifact after smoke failure: {} ({err})",
                artifact_path.display()
            )))?;
        }
    }

    Ok(())
}

fn run_v03_build_lifecycle_steps(
    plan: &capsule_core::router::ManifestData,
    reporter: &std::sync::Arc<reporters::CliReporter>,
    strict_lockfile: bool,
) -> Result<()> {
    let schema_version = plan
        .manifest
        .get("schema_version")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if schema_version != "0.3" {
        return Ok(());
    }

    let mut provisioned_roots = std::collections::HashSet::new();
    for target_label in plan.selected_target_package_order()? {
        let target_plan = plan.with_selected_target(target_label.clone());
        let working_dir = target_plan.execution_working_directory();

        if provisioned_roots.insert(working_dir.clone()) {
            if let Some(command) = plan_v03_build_provision_command(&target_plan, strict_lockfile)? {
                futures::executor::block_on(
                    reporter.notify(format!("⚙️  Provision [{}]: {}", target_label, command)),
                )?;
                run_build_lifecycle_shell_command(&target_plan, &command, "provision")?;
            }
        }

        if let Some(command) = target_plan
            .build_lifecycle_build()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            let build_cache = prepare_v03_build_cache(&target_plan, &command, reporter)?;
            if let Some(build_cache) = build_cache.as_ref() {
                if build_cache.restore_outputs()? {
                    futures::executor::block_on(reporter.notify(format!(
                        "♻️  Build cache hit [{}]: restored {}",
                        target_label,
                        build_cache.describe_outputs()
                    )))?;
                    continue;
                }
            }

            futures::executor::block_on(
                reporter.notify(format!("🏗️  Build [{}]: {}", target_label, command)),
            )?;
            run_build_lifecycle_shell_command(&target_plan, &command, "build")?;

            if let Some(build_cache) = build_cache.as_ref() {
                if build_cache.capture_outputs()? {
                    futures::executor::block_on(reporter.notify(format!(
                        "💾 Build cache saved [{}]: {}",
                        target_label,
                        build_cache.describe_outputs()
                    )))?;
                } else {
                    futures::executor::block_on(reporter.warn(format!(
                        "⚠️  Build cache skipped [{}]: declared outputs were not produced",
                        target_label
                    )))?;
                }
            }
        }
    }

    Ok(())
}

fn plan_v03_build_provision_command(
    plan: &capsule_core::router::ManifestData,
    strict_lockfile: bool,
) -> Result<Option<String>> {
    let runtime = plan.execution_runtime().unwrap_or_default();
    let driver = plan.execution_driver().unwrap_or_default();
    let runtime = runtime.trim().to_ascii_lowercase();
    let driver = driver.trim().to_ascii_lowercase();
    let workspace_root = plan.workspace_root.clone();
    let execution_working_directory = plan
        .compat_target_working_dir(plan.selected_target_label())
        .map(|value| plan.workspace_root.join(value))
        .unwrap_or_else(|| plan.execution_working_directory());

    if runtime == "web" && driver == "static" {
        debug!(
            phase = "build",
            runtime,
            driver,
            workspace_root = %workspace_root.display(),
            execution_working_directory = %execution_working_directory.display(),
            lockfile_check_paths = ?Vec::<(&str, std::path::PathBuf, bool)>::new(),
            "Provision command path diagnostics"
        );
        return Ok(None);
    }

    if matches!(driver.as_str(), "node") {
        let package_lock = execution_working_directory.join("package-lock.json");
        let yarn_lock = execution_working_directory.join("yarn.lock");
        let pnpm_lock = execution_working_directory.join("pnpm-lock.yaml");
        let bun_lock = execution_working_directory.join("bun.lock");
        let bun_lockb = execution_working_directory.join("bun.lockb");
        let lockfile_check_paths = vec![
            (
                "package-lock.json",
                package_lock.clone(),
                package_lock.exists(),
            ),
            ("yarn.lock", yarn_lock.clone(), yarn_lock.exists()),
            ("pnpm-lock.yaml", pnpm_lock.clone(), pnpm_lock.exists()),
            ("bun.lock", bun_lock.clone(), bun_lock.exists()),
            ("bun.lockb", bun_lockb.clone(), bun_lockb.exists()),
        ];
        debug!(
            phase = "build",
            runtime,
            driver,
            workspace_root = %workspace_root.display(),
            execution_working_directory = %execution_working_directory.display(),
            lockfile_check_paths = ?lockfile_check_paths,
            "Provision command path diagnostics"
        );
        let mut matches = Vec::new();
        if package_lock.exists() {
            matches.push(if strict_lockfile { "npm ci" } else { "npm install" });
        }
        if yarn_lock.exists() {
            matches.push(if strict_lockfile {
                "yarn install --frozen-lockfile"
            } else {
                "yarn install"
            });
        }
        if pnpm_lock.exists() {
            matches.push(if strict_lockfile {
                "pnpm install --frozen-lockfile"
            } else {
                "pnpm install"
            });
        }
        if bun_lock.exists() || bun_lockb.exists() {
            matches.push(if strict_lockfile {
                "bun install --frozen-lockfile"
            } else {
                "bun install"
            });
        }
        return match matches.as_slice() {
            [] => Err(AtoExecutionError::lock_incomplete(
                "source/node target requires one of package-lock.json, yarn.lock, pnpm-lock.yaml, bun.lock, or bun.lockb",
                Some("package-lock.json"),
            )
            .into()),
            [command] => Ok(Some((*command).to_string())),
            _ => Err(AtoExecutionError::lock_incomplete(
                "multiple node lockfiles detected; keep only one of package-lock.json, yarn.lock, pnpm-lock.yaml, bun.lock, or bun.lockb",
                Some("package-lock.json"),
            )
            .into()),
        };
    }

    if matches!(driver.as_str(), "python") {
        let uv_lock = execution_working_directory.join("uv.lock");
        debug!(
            phase = "build",
            runtime,
            driver,
            workspace_root = %workspace_root.display(),
            execution_working_directory = %execution_working_directory.display(),
            lockfile_check_paths = ?vec![("uv.lock", uv_lock.clone(), uv_lock.exists())],
            "Provision command path diagnostics"
        );
        return if uv_lock.exists() {
            Ok(Some("uv sync --frozen".to_string()))
        } else {
            Err(AtoExecutionError::lock_incomplete(
                "source/python target requires uv.lock for fail-closed provisioning",
                Some("uv.lock"),
            )
            .into())
        };
    }

    let cargo_lock = execution_working_directory.join("Cargo.lock");
    debug!(
        phase = "build",
        runtime,
        driver,
        workspace_root = %workspace_root.display(),
        execution_working_directory = %execution_working_directory.display(),
        lockfile_check_paths = ?vec![("Cargo.lock", cargo_lock.clone(), cargo_lock.exists())],
        "Provision command path diagnostics"
    );
    if matches!(driver.as_str(), "native") && cargo_lock.exists() {
        return Ok(Some("cargo fetch --locked".to_string()));
    }

    Ok(None)
}

#[derive(Debug, Clone)]
struct BuildCacheOutputSpec {
    relative_path: PathBuf,
}

#[derive(Debug, Clone)]
struct V03BuildCache {
    working_dir: PathBuf,
    cache_dir: PathBuf,
    outputs: Vec<BuildCacheOutputSpec>,
}

impl V03BuildCache {
    fn describe_outputs(&self) -> String {
        self.outputs
            .iter()
            .map(|output| output.relative_path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn restore_outputs(&self) -> Result<bool> {
        let cache_outputs_dir = self.cache_dir.join("outputs");
        if !cache_outputs_dir.exists() {
            return Ok(false);
        }

        for output in &self.outputs {
            let source = cache_outputs_dir.join(&output.relative_path);
            if !source.exists() {
                return Ok(false);
            }
        }

        for output in &self.outputs {
            let source = cache_outputs_dir.join(&output.relative_path);
            let destination = self.working_dir.join(&output.relative_path);
            remove_path_if_exists(&destination)?;
            crate::fs_copy::copy_path_recursive(&source, &destination)?;
        }

        Ok(true)
    }

    fn capture_outputs(&self) -> Result<bool> {
        remove_path_if_exists(&self.cache_dir)?;
        let cache_outputs_dir = self.cache_dir.join("outputs");
        fs::create_dir_all(&cache_outputs_dir)?;

        let mut captured_any = false;
        for output in &self.outputs {
            let source = self.working_dir.join(&output.relative_path);
            if !source.exists() {
                continue;
            }

            let destination = cache_outputs_dir.join(&output.relative_path);
            crate::fs_copy::copy_path_recursive(&source, &destination)?;
            captured_any = true;
        }

        if !captured_any {
            remove_path_if_exists(&self.cache_dir)?;
        }

        Ok(captured_any)
    }
}

fn prepare_v03_build_cache(
    plan: &capsule_core::router::ManifestData,
    build_command: &str,
    reporter: &std::sync::Arc<reporters::CliReporter>,
) -> Result<Option<V03BuildCache>> {
    let outputs = plan.build_cache_outputs();
    if outputs.is_empty() {
        return Ok(None);
    }

    let output_specs = match normalize_build_cache_outputs(&outputs) {
        Ok(specs) => specs,
        Err(reason) => {
            futures::executor::block_on(reporter.warn(format!(
                "⚠️  Build cache disabled [{}]: {}",
                plan.selected_target_label(),
                reason
            )))?;
            return Ok(None);
        }
    };

    let cache_key = compute_v03_build_cache_key(plan, &output_specs, build_command)?;
    let cache_dir = capsule_core::common::paths::nacelle_home_dir()?
        .join("build-cache")
        .join("chml")
        .join(cache_key);

    Ok(Some(V03BuildCache {
        working_dir: plan.execution_working_directory(),
        cache_dir,
        outputs: output_specs,
    }))
}

fn normalize_build_cache_outputs(raw_outputs: &[String]) -> Result<Vec<BuildCacheOutputSpec>> {
    let mut outputs = Vec::new();

    for raw_output in raw_outputs {
        let mut normalized = raw_output.trim();
        if normalized.is_empty() {
            continue;
        }

        if normalized.ends_with("/**") {
            normalized = normalized.trim_end_matches("/**");
        }
        normalized = normalized.trim_start_matches("./");
        normalized = normalized.trim_end_matches('/');

        if normalized.is_empty() {
            anyhow::bail!(
                "outputs entries must resolve to a relative path inside the package root"
            );
        }
        if normalized.contains('*') || normalized.contains('?') || normalized.contains('[') {
            anyhow::bail!(
                "unsupported outputs pattern '{}'; only exact relative paths and '<dir>/**' are supported",
                raw_output
            );
        }

        let path = Path::new(normalized);
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
        {
            anyhow::bail!(
                "outputs entry '{}' must stay inside the package root",
                raw_output
            );
        }

        outputs.push(BuildCacheOutputSpec {
            relative_path: path.to_path_buf(),
        });
    }

    Ok(outputs)
}

fn compute_v03_build_cache_key(
    plan: &capsule_core::router::ManifestData,
    outputs: &[BuildCacheOutputSpec],
    build_command: &str,
) -> Result<String> {
    let working_dir = plan.execution_working_directory();
    let mut hasher = Sha256::new();

    update_hash_text(&mut hasher, BUILD_CACHE_LAYOUT_VERSION);
    update_hash_text(&mut hasher, &plan.workspace_root.display().to_string());
    update_hash_text(&mut hasher, plan.selected_target_label());
    update_hash_text(&mut hasher, build_command);

    if let Some(runtime) = plan.execution_runtime() {
        update_hash_text(&mut hasher, &runtime);
    }
    if let Some(driver) = plan.execution_driver() {
        update_hash_text(&mut hasher, &driver);
    }

    for dependency in plan.selected_target_package_order()? {
        update_hash_text(&mut hasher, &dependency);
    }

    let mut build_env = plan.build_cache_env();
    build_env.sort();
    for key in build_env {
        update_hash_text(&mut hasher, &key);
        match std::env::var(&key) {
            Ok(value) => update_hash_text(&mut hasher, &value),
            Err(_) => update_hash_text(&mut hasher, "<missing>"),
        }
    }

    for lockfile in native_lockfiles_for_build_cache(&working_dir) {
        update_hash_text(&mut hasher, &lockfile.display().to_string());
        hash_file_contents(&mut hasher, &lockfile)?;
    }

    for relative_path in collect_build_cache_source_files(&working_dir, outputs)? {
        update_hash_text(&mut hasher, &relative_path.display().to_string());
        hash_file_contents(&mut hasher, &working_dir.join(&relative_path))?;
    }

    Ok(hex::encode(hasher.finalize()))
}

fn native_lockfiles_for_build_cache(working_dir: &Path) -> Vec<PathBuf> {
    let mut paths = [
        "package-lock.json",
        "pnpm-lock.yaml",
        "bun.lock",
        "bun.lockb",
        "uv.lock",
        "Cargo.lock",
        "deno.lock",
        "poetry.lock",
    ]
    .into_iter()
    .map(|name| working_dir.join(name))
    .filter(|path| path.exists())
    .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn collect_build_cache_source_files(
    working_dir: &Path,
    outputs: &[BuildCacheOutputSpec],
) -> Result<Vec<PathBuf>> {
    let ignored_dynamic_roots = dynamic_build_cache_ignored_roots(working_dir);
    let mut files = Vec::new();
    let walker = WalkDir::new(working_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let Ok(relative_path) = entry.path().strip_prefix(working_dir) else {
                return true;
            };
            if relative_path.as_os_str().is_empty() {
                return true;
            }
            if path_is_within_any_root(relative_path, &ignored_dynamic_roots) {
                return false;
            }
            if entry.file_type().is_dir() {
                if let Some(name) = relative_path.file_name().and_then(|value| value.to_str()) {
                    if BUILD_CACHE_IGNORED_DIRS.contains(&name) {
                        return false;
                    }
                }
            }
            !path_is_within_cached_outputs(relative_path, outputs)
        });

    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative_path = entry
            .path()
            .strip_prefix(working_dir)
            .with_context(|| format!("Failed to relativize {}", entry.path().display()))?;
        if path_is_within_any_root(relative_path, &ignored_dynamic_roots) {
            continue;
        }
        if path_is_within_cached_outputs(relative_path, outputs) {
            continue;
        }
        files.push(relative_path.to_path_buf());
    }

    files.sort();
    Ok(files)
}

fn path_is_within_cached_outputs(path: &Path, outputs: &[BuildCacheOutputSpec]) -> bool {
    outputs
        .iter()
        .any(|output| path.starts_with(&output.relative_path))
}

fn dynamic_build_cache_ignored_roots(working_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(ato_home) = capsule_core::common::paths::nacelle_home_dir() {
        if let Ok(relative) = ato_home.strip_prefix(working_dir) {
            if !relative.as_os_str().is_empty() {
                roots.push(relative.to_path_buf());
            }
        }
    }
    roots
}

fn path_is_within_any_root(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn update_hash_text(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hash_file_contents(hasher: &mut Sha256, path: &Path) -> Result<()> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("Failed to read build cache input: {}", path.display()))?;
    let mut buffer = [0u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(())
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path)
            .with_context(|| format!("Failed to remove directory {}", path.display()))?;
    } else {
        fs::remove_file(path)
            .with_context(|| format!("Failed to remove file {}", path.display()))?;
    }
    Ok(())
}

fn run_build_lifecycle_shell_command(
    plan: &capsule_core::router::ManifestData,
    command: &str,
    phase: &str,
) -> Result<()> {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-lc", command]);
        cmd
    };

    cmd.current_dir(plan.execution_working_directory())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .env("COREPACK_ENABLE_STRICT", "0")
        // Disable pnpm 10's auto-manage-package-manager-versions to prevent it from
        // attempting to download the pinned pnpm version in offline/CI environments.
        .env("npm_config_manage_package_manager_versions", "false")
        .env("npm_config_approve_builds", "on");

    for (key, value) in runtime_overrides::merged_env(plan.execution_env()) {
        cmd.env(key, value);
    }
    if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
        cmd.env("PORT", port.to_string());
    }

    let status = cmd
        .status()
        .with_context(|| format!("Failed to execute {} command", phase))?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{} command failed with exit code {}: {}",
            phase,
            status.code().unwrap_or(1),
            command
        ))
    }
}

fn sign_if_requested(
    target: &std::path::Path,
    key: Option<&PathBuf>,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<Option<PathBuf>> {
    if let Some(key_path) = key {
        futures::executor::block_on(
            reporter.notify("🔐 Generating detached signature...".to_string()),
        )?;
        let sig_path = capsule_core::signing::sign_artifact(target, key_path, "ato-cli", None)?;
        futures::executor::block_on(
            reporter.notify(format!("✅ Signature: {}", sig_path.display())),
        )?;
        return Ok(Some(sig_path));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{
        build_decision_from_manifest_text, execute_pack_command,
        execute_pack_command_with_injected_manifest, plan_v03_build_provision_command,
        run_v03_build_lifecycle_steps,
    };
    use capsule_core::router::{ExecutionProfile, ManifestData};
    use capsule_core::types::ValidationMode;
    use std::ffi::OsString;
    use std::path::PathBuf;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn v03_build_provision_uses_target_working_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app_dir = tmp.path().join("apps").join("web");
        std::fs::create_dir_all(&app_dir).expect("create app dir");
        std::fs::write(app_dir.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
            .expect("write pnpm lock");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("node".to_string())),
                ("working_dir", toml::Value::String("apps/web".to_string())),
                (
                    "run_command",
                    toml::Value::String("pnpm start -- --port $PORT".to_string()),
                ),
            ],
        );

        let command = plan_v03_build_provision_command(&plan, true).expect("plan provision");
        assert_eq!(command.as_deref(), Some("pnpm install --frozen-lockfile"));
    }

    #[test]
    fn v03_build_provision_supports_yarn_lockfile() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("yarn.lock"), "# yarn lockfile v1\n")
            .expect("write yarn lock");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("node".to_string())),
                ("run_command", toml::Value::String("yarn build".to_string())),
            ],
        );

        let command = plan_v03_build_provision_command(&plan, true).expect("plan provision");
        assert_eq!(command.as_deref(), Some("yarn install --frozen-lockfile"));
    }

    #[test]
    fn v03_build_cache_restores_outputs_and_skips_rebuild() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cache_home = tmp.path().join("ato-home");
        let _ato_home_guard = EnvVarGuard::set_path("ATO_HOME", &cache_home);
        std::fs::write(tmp.path().join("main.ts"), "console.log('ok')").expect("write source");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("native".to_string())),
                (
                    "build_command",
                    toml::Value::String(
                        "mkdir -p .tmp dist && printf x >> .tmp/build-count.txt && printf cached > dist/out.txt"
                            .to_string(),
                    ),
                ),
                (
                    "outputs",
                    toml::Value::Array(vec![toml::Value::String("dist/**".to_string())]),
                ),
                (
                    "build_env",
                    toml::Value::Array(vec![toml::Value::String(
                        "ATO_BUILD_CACHE_TEST_ENV".to_string(),
                    )]),
                ),
                (
                    "run_command",
                    toml::Value::String("./dist/out.txt".to_string()),
                ),
            ],
        );
        let reporter = std::sync::Arc::new(crate::reporters::CliReporter::new(true));

        std::env::set_var("ATO_BUILD_CACHE_TEST_ENV", "test");
        run_v03_build_lifecycle_steps(&plan, &reporter, true).expect("first build");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(".tmp/build-count.txt")).expect("read count"),
            "x"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("dist/out.txt")).expect("read output"),
            "cached"
        );

        std::fs::remove_dir_all(tmp.path().join("dist")).expect("remove dist");
        run_v03_build_lifecycle_steps(&plan, &reporter, true).expect("cache restore");

        assert_eq!(
            std::fs::read_to_string(tmp.path().join(".tmp/build-count.txt"))
                .expect("read count after restore"),
            "x"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("dist/out.txt")).expect("read restored output"),
            "cached"
        );
        std::env::remove_var("ATO_BUILD_CACHE_TEST_ENV");
    }

    #[test]
    fn injected_v03_web_static_manifest_builds_from_root_index_html() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let site_dir = tmp.path().join("site");
        std::fs::create_dir_all(&site_dir).expect("site dir");
        std::fs::write(site_dir.join("index.html"), "<h1>hello</h1>").expect("write index.html");
        let reporter = std::sync::Arc::new(crate::reporters::CliReporter::new(true));
        let manifest = r#"
schema_version = "0.3"
name = "hello-capsule"
version = "0.1.0"
type = "app"

runtime = "web/static"
run = "site""#;

        let result = execute_pack_command_with_injected_manifest(
            tmp.path().to_path_buf(),
            false,
            None,
            false,
            false,
            false,
            true,
            false,
            "strict".to_string(),
            reporter,
            false,
            true,
            None,
            Some(manifest),
            true,
        )
        .expect("build inferred web/static manifest");

        assert!(result.ok);
        assert_eq!(result.build_strategy, "web");
        assert!(result.artifact.as_ref().is_some_and(|path| path.exists()));
        assert!(!tmp.path().join("capsule.toml").exists());
    }

    #[test]
    fn build_decision_from_manifest_text_does_not_materialize_capsule_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("main.js"), "console.log('demo');\n").expect("main.js");
        let manifest = r#"
schema_version = "0.3"
name = "build-helper-demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
runtime_version = "20.11.0"
run = "main.js""#;

        let (decision, bridge) =
            build_decision_from_manifest_text(tmp.path(), manifest, ValidationMode::Strict)
                .expect("build decision from manifest text");

        assert_eq!(decision.plan.selected_target_label(), "app");
        assert_eq!(bridge.package_name(), "build-helper-demo");
        assert!(!tmp.path().join("capsule.toml").exists());
    }

    #[test]
    fn source_only_authoritative_build_does_not_materialize_capsule_toml() {
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

        let reporter = std::sync::Arc::new(crate::reporters::CliReporter::new(true));
        let _error = execute_pack_command(
            tmp.path().to_path_buf(),
            false,
            None,
            false,
            false,
            false,
            true,
            false,
            "strict".to_string(),
            reporter,
            false,
            true,
            None,
        )
        .expect_err("source-only build should still avoid manifest materialization on failure");
        assert!(!tmp.path().join("capsule.toml").exists());
    }

    #[test]
    fn injected_source_standalone_build_does_not_materialize_capsule_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("main.js"), "console.log('bundle');\n").expect("main.js");

        let nacelle = tmp.path().join("nacelle");
        std::fs::write(&nacelle, "#!/bin/sh\nexit 0\n").expect("nacelle");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&nacelle).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&nacelle, perms).expect("chmod");
        }

        let reporter = std::sync::Arc::new(crate::reporters::CliReporter::new(true));
        let manifest = r#"
schema_version = "0.3"
name = "bundle-demo"
version = "0.1.0"
type = "app"

runtime = "source"
run = "main.js""#;

        let result = execute_pack_command_with_injected_manifest(
            tmp.path().to_path_buf(),
            false,
            None,
            true,
            false,
            false,
            true,
            false,
            "strict".to_string(),
            reporter,
            false,
            true,
            Some(nacelle),
            Some(manifest),
            true,
        )
        .expect("standalone source build with injected manifest must not materialize manifest");

        assert!(result.ok);
        assert_eq!(result.build_strategy, "source");
        assert!(!tmp.path().join("capsule.toml").exists());
    }

    #[test]
    fn injected_native_delivery_build_does_not_materialize_capsule_toml() {
        if !cfg!(target_os = "macos") {
            return;
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let app_dir = tmp.path().join("MyApp.app/Contents/MacOS");
        std::fs::create_dir_all(&app_dir).expect("app dir");
        std::fs::write(app_dir.join("MyApp"), "#!/bin/sh\necho native\n").expect("app binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let binary = app_dir.join("MyApp");
            let mut perms = std::fs::metadata(&binary).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(binary, perms).expect("chmod");
        }

        let reporter = std::sync::Arc::new(crate::reporters::CliReporter::new(true));
        let manifest = r#"
schema_version = "0.3"
name = "native-demo"
version = "0.1.0"
type = "app"

runtime = "source/native"
run = "MyApp.app"
[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "MyApp.app"

[finalize]
tool = "codesign"
args = ["--force", "--sign", "-", "MyApp.app"]
"#;

        let result = execute_pack_command_with_injected_manifest(
            tmp.path().to_path_buf(),
            false,
            None,
            false,
            false,
            false,
            true,
            false,
            "strict".to_string(),
            reporter,
            false,
            true,
            None,
            Some(manifest),
            true,
        )
        .expect("build native delivery artifact without materializing manifest");

        assert!(result.ok);
        assert_eq!(result.build_strategy, "native-delivery");
        assert!(result.artifact.as_ref().is_some_and(|path| path.exists()));
        assert!(!tmp.path().join("capsule.toml").exists());
    }

    fn manifest_with_schema_and_target(
        schema_version: &str,
        manifest_dir: PathBuf,
        entries: Vec<(&str, toml::Value)>,
    ) -> ManifestData {
        let runtime = entries
            .iter()
            .find(|(key, _)| *key == "runtime")
            .and_then(|(_, value)| value.as_str())
            .unwrap_or("source")
            .to_string();
        let driver = entries
            .iter()
            .find(|(key, _)| *key == "driver")
            .and_then(|(_, value)| value.as_str())
            .unwrap_or("node")
            .to_string();
        let entrypoint = entries
            .iter()
            .find(|(key, _)| *key == "entrypoint")
            .and_then(|(_, value)| value.as_str())
            .unwrap_or("main.ts")
            .to_string();
        let mut manifest = toml::map::Map::new();
        manifest.insert(
            "schema_version".to_string(),
            toml::Value::String(schema_version.to_string()),
        );
        manifest.insert("name".to_string(), toml::Value::String("demo".to_string()));
        manifest.insert(
            "version".to_string(),
            toml::Value::String("0.1.0".to_string()),
        );
        manifest.insert("type".to_string(), toml::Value::String("app".to_string()));
        manifest.insert(
            "default_target".to_string(),
            toml::Value::String("default".to_string()),
        );

        let mut target = toml::map::Map::new();
        for (key, value) in &entries {
            target.insert((*key).to_string(), value.clone());
        }

        let mut targets = toml::map::Map::new();
        targets.insert("default".to_string(), toml::Value::Table(target));
        manifest.insert("targets".to_string(), toml::Value::Table(targets));

        let mut lock = capsule_core::ato_lock::AtoLock::default();
        lock.contract.entries.insert(
            "metadata".to_string(),
            serde_json::json!({
                "name": "demo",
                "default_target": "default",
            }),
        );
        lock.contract.entries.insert(
            "process".to_string(),
            serde_json::json!({
                "driver": driver,
                "entrypoint": entrypoint,
            }),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            serde_json::json!({
                "kind": runtime,
                "selected_target": "default",
            }),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            serde_json::json!([{
                "label": "default",
                "runtime": runtime,
                "driver": driver,
                "entrypoint": entrypoint,
            }]),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            serde_json::json!({
                "status": "complete",
                "kind": "metadata_only",
                "digestable": false
            }),
        );
        let lock_path = manifest_dir.join("ato.lock.json");
        let workspace_root = manifest_dir.clone();
        let runtime_model = capsule_core::lock_runtime::resolve_lock_runtime_model(&lock, None)
            .expect("resolve test runtime model");

        let manifest_value = toml::Value::Table(manifest);
        let compat_manifest =
            capsule_core::router::CompatManifestBridge::from_manifest_value(&manifest_value)
                .expect("compat manifest bridge");

        ManifestData {
            manifest: manifest_value,
            compat_manifest: Some(compat_manifest),
            manifest_path: manifest_dir.join("capsule.toml"),
            manifest_dir,
            lock,
            lock_path,
            workspace_root,
            profile: ExecutionProfile::Dev,
            selected_target: "default".to_string(),
            runtime_model,
            state_source_overrides: std::collections::HashMap::new(),
        }
    }
}

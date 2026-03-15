use anyhow::{Context, Result};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::CapsuleReporter;
use serde::Serialize;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::debug;

use crate::init;
use crate::native_delivery;
use crate::reporters;
use crate::runtime_overrides;

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

#[allow(clippy::too_many_arguments)]
pub fn execute_pack_command(
    dir: PathBuf,
    init_if_missing: bool,
    key: Option<PathBuf>,
    standalone: bool,
    force_large_payload: bool,
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
        keep_failed_artifacts,
        strict_manifest,
        enforcement,
        reporter,
        timings,
        cli_json,
        nacelle_override,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn execute_pack_command_with_injected_manifest(
    dir: PathBuf,
    init_if_missing: bool,
    key: Option<PathBuf>,
    standalone: bool,
    force_large_payload: bool,
    keep_failed_artifacts: bool,
    strict_manifest: bool,
    enforcement: String,
    reporter: std::sync::Arc<reporters::CliReporter>,
    timings: bool,
    cli_json: bool,
    nacelle_override: Option<PathBuf>,
    injected_manifest: Option<&str>,
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
    let mut temporary_manifest: Option<TemporaryManifestGuard> = None;
    if !manifest.exists() {
        let stdin_is_tty = std::io::stdin().is_terminal();
        if init_if_missing {
            if !stdin_is_tty {
                anyhow::bail!("--init requires an interactive TTY");
            }
            if cli_json {
                anyhow::bail!("--init cannot be used with --json output");
            }
            init::execute_manifest_init(
                init::InitArgs {
                    path: Some(dir.clone()),
                    yes: false,
                },
                reporter.clone(),
            )?;
        } else if let Some(manifest_text) = injected_manifest {
            futures::executor::block_on(reporter.warn(
                "No `capsule.toml` found. Using draft returned by ato store for this GitHub repository.".to_string(),
            ))?;
            std::fs::write(&manifest, manifest_text).with_context(|| {
                format!("Failed to write temporary manifest: {}", manifest.display())
            })?;
            temporary_manifest = Some(TemporaryManifestGuard::new(manifest.clone()));
        } else {
            futures::executor::block_on(reporter.warn(
                "No `capsule.toml` found. Using defaults. Run `ato init` to generate an agent prompt, or `ato build --init` to create `capsule.toml` interactively.".to_string(),
            ))?;
            let inferred = infer_zero_config_manifest(&dir)?;
            std::fs::write(&manifest, inferred).with_context(|| {
                format!("Failed to write temporary manifest: {}", manifest.display())
            })?;
            temporary_manifest = Some(TemporaryManifestGuard::new(manifest.clone()));
        }
    }

    if !manifest.exists() {
        anyhow::bail!("capsule.toml not found after initialization");
    }

    let _temporary_manifest_guard = temporary_manifest;

    let validation_started = Instant::now();
    let decision = capsule_core::router::route_manifest(
        &manifest,
        capsule_core::router::ExecutionProfile::Release,
        None,
    )?;
    let loaded_manifest = capsule_core::manifest::load_manifest(&manifest)?;
    let capsule_name = loaded_manifest.model.name.clone();
    let capsule_version = loaded_manifest.model.version.clone();
    capsule_core::diagnostics::manifest::validate_manifest_for_build(
        &manifest,
        decision.plan.selected_target_label(),
    )?;
    let ipc_diagnostics = crate::ipc::validate::validate_manifest(
        &decision.plan.manifest,
        &decision.plan.manifest_dir,
    )
    .map_err(|err| AtoExecutionError::policy_violation(format!("IPC validation failed: {err}")))?;
    if crate::ipc::validate::has_errors(&ipc_diagnostics) {
        return Err(
            AtoExecutionError::policy_violation(crate::ipc::validate::format_diagnostics(
                &ipc_diagnostics,
            ))
            .into(),
        );
    }
    for diagnostic in ipc_diagnostics {
        futures::executor::block_on(reporter.warn(diagnostic.to_string()))?;
    }
    run_v03_build_lifecycle_steps(&decision.plan, &reporter)?;
    record_timing(
        &mut timing_entries,
        "build.validation",
        validation_started.elapsed(),
    );

    futures::executor::block_on(reporter.notify(format!(
        "📦 Packing capsule \"{}\" (v{})...",
        capsule_name, capsule_version
    )))?;
    debug!(
        runtime_kind = ?decision.kind,
        reason = %decision.reason,
        "Build runtime routed"
    );

    let manifest_dir = manifest
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    if let Some(plan) = native_delivery::detect_build_strategy(&manifest_dir)? {
        let build_started = Instant::now();
        let result = native_delivery::build_native_artifact(&plan, None)?;
        record_timing(&mut timing_entries, "build.pack", build_started.elapsed());
        crate::payload_guard::ensure_payload_size(
            &result.artifact_path,
            force_large_payload,
            "--force-large-payload",
        )?;
        let _ = sign_if_requested(&result.artifact_path, key.as_ref(), reporter.clone())?;
        let size = std::fs::metadata(&result.artifact_path)?.len();
        futures::executor::block_on(reporter.notify(format!(
            "✅ Successfully built: {} ({:.1} KB)",
            result.artifact_path.display(),
            size as f64 / 1024.0
        )))?;
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
            let prepare_started = Instant::now();
            let prepared_config = capsule_core::packers::source::prepare_source_config(
                &manifest,
                enforcement.clone(),
                standalone,
            )?;
            record_timing(
                &mut timing_entries,
                "build.prepare_source_config",
                prepare_started.elapsed(),
            );
            futures::executor::block_on(reporter.progress_start(
                "⏳ [build] Preparing source runtime bundle...".to_string(),
                None,
            ))?;
            let pack_started = Instant::now();
            let artifact_path = capsule_core::packers::source::pack(
                &decision.plan,
                capsule_core::packers::source::SourcePackOptions {
                    manifest_path: manifest.clone(),
                    manifest_dir: manifest_dir.clone(),
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
            let artifact_path = artifact_path?;
            record_timing(&mut timing_entries, "build.pack", pack_started.elapsed());

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

            let payload_guard_started = Instant::now();
            crate::payload_guard::ensure_payload_size(
                &artifact_path,
                force_large_payload,
                "--force-large-payload",
            )?;
            record_timing(
                &mut timing_entries,
                "build.payload_guard",
                payload_guard_started.elapsed(),
            );
            let sign_started = Instant::now();
            let _ = sign_if_requested(&artifact_path, key.as_ref(), reporter.clone())?;
            record_timing(&mut timing_entries, "build.sign", sign_started.elapsed());
            let size = std::fs::metadata(&artifact_path)?.len();
            futures::executor::block_on(reporter.notify(format!(
                "✅ Successfully built: {} ({:.1} KB)",
                artifact_path.display(),
                size as f64 / 1024.0
            )))?;
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
                        manifest_path: manifest.clone(),
                        manifest_dir: manifest_dir.clone(),
                        output: None,
                    },
                    reporter.clone(),
                )?
            } else {
                let prepare_started = Instant::now();
                let prepared_config = capsule_core::packers::source::prepare_source_config(
                    &manifest,
                    enforcement.clone(),
                    standalone,
                )?;
                record_timing(
                    &mut timing_entries,
                    "build.prepare_source_config",
                    prepare_started.elapsed(),
                );
                futures::executor::block_on(reporter.progress_start(
                    "⏳ [build] Preparing web runtime bundle...".to_string(),
                    None,
                ))?;
                let pack_started = Instant::now();
                let artifact = capsule_core::packers::source::pack(
                    &decision.plan,
                    capsule_core::packers::source::SourcePackOptions {
                        manifest_path: manifest.clone(),
                        manifest_dir: manifest_dir.clone(),
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
                record_timing(&mut timing_entries, "build.pack", pack_started.elapsed());

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

            crate::payload_guard::ensure_payload_size(
                &artifact_path,
                force_large_payload,
                "--force-large-payload",
            )?;
            let _ = sign_if_requested(&artifact_path, key.as_ref(), reporter.clone())?;
            let size = std::fs::metadata(&artifact_path)?.len();
            futures::executor::block_on(reporter.notify(format!(
                "✅ Successfully built: {} ({:.1} KB)",
                artifact_path.display(),
                size as f64 / 1024.0
            )))?;
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

struct TemporaryManifestGuard {
    path: PathBuf,
}

impl TemporaryManifestGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TemporaryManifestGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
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
        r#"schema_version = "0.2"
name = "{name}"
version = "0.1.0"
type = "app"
default_target = "cli"

[metadata]
description = "Generated by zero-config build fallback"

[targets.cli]
runtime = "source"
entrypoint = "{entrypoint}"
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
            if let Some(command) = plan_v03_build_provision_command(&target_plan)? {
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
            futures::executor::block_on(
                reporter.notify(format!("🏗️  Build [{}]: {}", target_label, command)),
            )?;
            run_build_lifecycle_shell_command(&target_plan, &command, "build")?;
        }
    }

    Ok(())
}

fn plan_v03_build_provision_command(
    plan: &capsule_core::router::ManifestData,
) -> Result<Option<String>> {
    let runtime = plan.execution_runtime().unwrap_or_default();
    let driver = plan.execution_driver().unwrap_or_default();
    let runtime = runtime.trim().to_ascii_lowercase();
    let driver = driver.trim().to_ascii_lowercase();
    let manifest_dir = plan.execution_working_directory();

    if runtime == "web" && driver == "static" {
        return Ok(None);
    }

    if matches!(driver.as_str(), "node") {
        let package_lock = manifest_dir.join("package-lock.json");
        let pnpm_lock = manifest_dir.join("pnpm-lock.yaml");
        let bun_lock = manifest_dir.join("bun.lock");
        let bun_lockb = manifest_dir.join("bun.lockb");
        let mut matches = Vec::new();
        if package_lock.exists() {
            matches.push("npm ci");
        }
        if pnpm_lock.exists() {
            matches.push("pnpm install --frozen-lockfile");
        }
        if bun_lock.exists() || bun_lockb.exists() {
            matches.push("bun install --frozen-lockfile");
        }
        return match matches.as_slice() {
            [] => Err(AtoExecutionError::lock_incomplete(
                "source/node target requires one of package-lock.json, pnpm-lock.yaml, bun.lock, or bun.lockb",
                Some("package-lock.json"),
            )
            .into()),
            [command] => Ok(Some((*command).to_string())),
            _ => Err(AtoExecutionError::lock_incomplete(
                "multiple node lockfiles detected; keep only one of package-lock.json, pnpm-lock.yaml, bun.lock, or bun.lockb",
                Some("package-lock.json"),
            )
            .into()),
        };
    }

    if matches!(driver.as_str(), "python") {
        return if manifest_dir.join("uv.lock").exists() {
            Ok(Some("uv sync --frozen".to_string()))
        } else {
            Err(AtoExecutionError::lock_incomplete(
                "source/python target requires uv.lock for fail-closed provisioning",
                Some("uv.lock"),
            )
            .into())
        };
    }

    if matches!(driver.as_str(), "native") && manifest_dir.join("Cargo.lock").exists() {
        return Ok(Some("cargo fetch --locked".to_string()));
    }

    Ok(None)
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
        .stderr(std::process::Stdio::inherit());

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
    use super::plan_v03_build_provision_command;
    use capsule_core::router::{ExecutionProfile, ManifestData};
    use std::path::PathBuf;

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

        let command = plan_v03_build_provision_command(&plan).expect("plan provision");
        assert_eq!(command.as_deref(), Some("pnpm install --frozen-lockfile"));
    }

    fn manifest_with_schema_and_target(
        schema_version: &str,
        manifest_dir: PathBuf,
        entries: Vec<(&str, toml::Value)>,
    ) -> ManifestData {
        let mut manifest = toml::map::Map::new();
        manifest.insert(
            "schema_version".to_string(),
            toml::Value::String(schema_version.to_string()),
        );
        manifest.insert("name".to_string(), toml::Value::String("demo".to_string()));
        manifest.insert(
            "default_target".to_string(),
            toml::Value::String("default".to_string()),
        );

        let mut target = toml::map::Map::new();
        for (key, value) in entries {
            target.insert(key.to_string(), value);
        }

        let mut targets = toml::map::Map::new();
        targets.insert("default".to_string(), toml::Value::Table(target));
        manifest.insert("targets".to_string(), toml::Value::Table(targets));

        ManifestData {
            manifest: toml::Value::Table(manifest),
            manifest_path: manifest_dir.join("capsule.toml"),
            manifest_dir,
            profile: ExecutionProfile::Dev,
            selected_target: "default".to_string(),
            state_source_overrides: std::collections::HashMap::new(),
        }
    }
}

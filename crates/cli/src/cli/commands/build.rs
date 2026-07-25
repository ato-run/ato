use anyhow::{Context, Result};
use capsule::CapsuleReporter;
use capsule::execution_contract::{ContentDigest, DigestAlgorithm, ExecutionContractEnvelopeV1};
use capsule::execution_contract_finalize::{ExecutionObservationV1, FinalizationError};
use capsule::execution_plan::error::AtoExecutionError;
use capsule::router::{
    CompatManifestBridge, CompatProjectInput, ExecutionDescriptor, RuntimeDecision, RuntimeKind,
};
use capsule::routing::input_resolver::resolve_canonical_lock_path;
use capsule::types::{CapsuleManifest, MANIFEST_SCHEMA_V03, ValidationMode};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs;
use std::io::IsTerminal;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::debug;

use crate::adapters::runtime::provisioning::{
    dependency_root, python_requirements_lock_missing, python_requirements_lock_sync_command,
};
use crate::application::producer_input::resolve_producer_authoritative_input;
use crate::application::source_inventory::{
    OutputSpec, collect_source_files, native_lockfiles, normalize_outputs,
};
use crate::build::native_delivery;
use crate::project::init;
use crate::reporters;
use crate::runtime::manager as runtime_manager;
use crate::runtime::overrides as runtime_overrides;

const BUILD_CACHE_LAYOUT_VERSION: &str = "chml-build-cache-v1";

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
    pub report: capsule::smoke::SmokeFailureReport,
}

fn runtime_kind_from_plan(plan: &ExecutionDescriptor) -> Result<RuntimeKind> {
    match plan
        .execution_runtime()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .split('/')
        .next()
        .unwrap_or_default()
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
    let plan = capsule::router::execution_descriptor_from_manifest_parts(
        raw,
        workspace_root.join("capsule.toml"),
        workspace_root.to_path_buf(),
        capsule::router::ExecutionProfile::Release,
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
                "No `capsule.toml` found. Using defaults. Run `ato init` to materialize `capsule.lock`, or `ato build --init` to create an inferred compatibility `capsule.toml`.".to_string(),
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
    let (decision, raw_manifest, capsule_name, capsule_version) =
        if let Some(authoritative_input) = authoritative_input.as_ref() {
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
            let decision = capsule::router::route_manifest_with_validation_mode(
                &manifest,
                capsule::router::ExecutionProfile::Release,
                None,
                validation_mode,
            )?;
            let loaded_manifest =
                capsule::manifest::load_manifest_with_validation_mode(&manifest, validation_mode)?;
            let raw_manifest: toml::Value = toml::from_str(&loaded_manifest.raw_text)
                .context("Failed to parse manifest TOML for IPC validation")?;
            capsule::diagnostics::manifest::validate_manifest_for_build_with_mode(
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
    // Source/GitHub builds never enforce --frozen-lockfile: lockfiles may come from a different
    // platform and would fail checksum validation. Only user-injected manifests (explicit
    // capsule.toml) could benefit from strict enforcement, and even then it's not required.
    run_v03_build_lifecycle_steps(&decision.plan, &reporter, false)?;
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
        capsule::router::RuntimeKind::Source => {
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
                match capsule::smoke::run_capsule_smoke(
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
        capsule::router::RuntimeKind::NativeInference => {
            // Inc1 is run-only: native-inference resolves a local engine binary
            // and model at launch, so there is no source/image bundle to pack.
            anyhow::bail!(
                "`ato build` does not support runtime=native-inference yet \
                 (Inc1 is run-only; engine/model are resolved locally at `ato run`)"
            );
        }
        capsule::router::RuntimeKind::Oci => {
            let result = capsule::packers::oci::pack(&decision.plan, None, reporter.as_ref())?;
            let archive = result.archive.clone();
            if let Some(path) = &archive {
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
        capsule::router::RuntimeKind::Wasm => {
            let result =
                capsule::packers::wasm::pack(&decision.plan, None, None, reporter.as_ref())?;
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
        capsule::router::RuntimeKind::Web => {
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
                capsule::packers::web::pack(
                    &decision.plan,
                    capsule::packers::web::WebPackOptions {
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

    // Ready-State seal branch (additive; legacy build is byte-for-byte unchanged
    // when ATO_READY_STATE_ENABLED is off). Side-channel only — never mutates the
    // BuildResult, so the legacy JSON output schema is identical.
    seal_ready_state_if_enabled(
        &dir,
        &raw_manifest,
        result.artifact.as_deref(),
        reporter.as_ref(),
    )?;

    record_timing(&mut timing_entries, "build.total", total_started.elapsed());
    emit_timings(reporter.clone(), timings, &timing_entries)?;

    Ok(result)
}

/// Seal the just-built capsule into a Ready-State artifact when
/// `ATO_READY_STATE_ENABLED` is on. No-op (and legacy build unchanged) when off.
/// Fails the build CLOSED if a Ready-State build cannot seal (GPU guard,
/// no-secret gate, explicit-but-unavailable backend, missing Firecracker rootfs).
fn seal_ready_state_if_enabled(
    workspace_root: &Path,
    raw_manifest: &toml::Value,
    artifact: Option<&std::path::Path>,
    reporter: &reporters::CliReporter,
) -> anyhow::Result<()> {
    use crate::application::ready_state;
    if !ready_state::flags::ready_state_enabled() {
        return Ok(());
    }
    let manifest = capsule::types::CapsuleManifest::from_toml(&toml::to_string(raw_manifest)?)?;
    if !manifest.is_ready_state_eligible() {
        futures::executor::block_on(
            reporter.notify(
                "READY-STATE: skipped — capsule is not Ready-State-eligible (no warm [snapshot])"
                    .to_string(),
            ),
        )?;
        return Ok(());
    }
    // Sealing a binding-required capsule is allowed (the artifact is pre-bind &
    // secret-free — no binding VALUES are ever injected/recorded); this guard
    // documents that contract and rejects any future mode that would.
    ready_state::bindings::ensure_no_unwired_runtime_bindings(
        &manifest,
        ready_state::bindings::BindingGuardMode::BuildSeal,
    )?;
    let backend = ready_state::backend::select_backend()?;
    let hash = ready_state::capsule_manifest_hash(raw_manifest)?;
    let state_root = ready_state::state_root();
    let layers = ready_state::assemble_build_layers(backend.id(), artifact)?;

    // Capsule v1 execution identity (`ato.execution-contract/v1`, RFC §4.6
    // strict finalization gate) — confirm-only, best-effort. See
    // `attempt_v1_execution_identity`'s doc comment for exactly which facets
    // are measured for real here vs. why this legitimately has nothing to
    // confirm (or nothing to measure) on essentially every build today.
    let v1_identity = attempt_v1_execution_identity(workspace_root, &layers)?;
    if let Some(envelope) = &v1_identity {
        futures::executor::block_on(reporter.notify(format!(
            "READY-STATE: Capsule v1 execution identity confirmed by real measurement: {}",
            envelope.execution_id
        )))?;
    }

    let receipt = ready_state::build::seal(
        &state_root,
        hash.clone(),
        &manifest,
        layers,
        backend.as_ref(),
        // `v1_identity` above is a genuinely confirmed `ExecutionContractEnvelopeV1`
        // when `Some` (never a caller-supplied/self-attested one — see its doc).
        // But `ready_state::build::seal`'s `V1SealRequest` additionally requires a
        // `seal_at_argv` (RFC §6.1's disposable-restore acceptance command) to
        // mint a v1 Snapshot, and no manifest field for `seal_at.command` exists
        // anywhere in this codebase yet (a repo-wide search finds only doc
        // comments/struct fields referencing the RFC concept, no TOML parser) —
        // so there is no real argv to supply even when the identity itself is
        // confirmed. Minting a v1 Snapshot from `ato build` is therefore still
        // not wired; only the identity CONFIRMATION step above is new here.
        None,
    )?;
    futures::executor::block_on(reporter.notify(format!(
        "READY-STATE: sealed {hash} backend={} no_secret_clean={} sealed_bytes={} -> {}",
        backend.id(),
        receipt.no_secret_proof.is_clean(),
        receipt.sealed_bytes,
        ready_state::store::artifact_dir(&state_root, &hash).display(),
    )))?;
    Ok(())
}

/// Attempt to confirm this build's Capsule v1 Execution Identity
/// (`ato.execution-contract/v1`) against an already-locked, already-verified
/// expected contract: the D2 `execution_contract` envelope in this
/// workspace's canonical lock (`capsule.lock`, or its deprecated
/// `ato.lock.json` read alias), if one exists. Which of the two names is
/// authoritative is decided by
/// [`capsule::routing::input_resolver::resolve_canonical_lock_path`], never by
/// this function. It only ever READS that lock — it never derives, invents, or
/// self-attests an "expected" contract; the only sanctioned source of one is a
/// persisted lock a producer already wrote and this call re-verifies
/// fail-closed.
///
/// Returns:
/// * `Ok(None)` — nothing to confirm. Either the workspace has no canonical
///   lock at all, the
///   lock carries no D2 `execution_contract` yet, or the RFC §4.6 strict gate
///   legitimately refused because some required facet has no measurement
///   producer anywhere in this codebase yet
///   ([`FinalizationError::UnmeasuredFacet`] — see "Honest scope" below for
///   exactly which facet that is in practice today). Neither case is an
///   error: `ato build` proceeds with its legacy (non-v1) behavior unchanged.
/// * `Ok(Some(envelope))` — every facet this function measured agreed with
///   the locked expectation AND every other required facet was already
///   satisfied. Not reachable with any producer coverage that exists in this
///   codebase today (see "Honest scope"), but this is the success path a
///   future producer PR unlocks without any further change here.
/// * `Err(_)` — a genuine, caught problem: the workspace has no single
///   authoritative lock (both `capsule.lock` and its deprecated alias exist, a
///   non-regular node occupies a lock name, or a lock name is unreadable), the
///   lock itself failed
///   verification (tampered `execution_id` / bad `lock_id`), or one of the
///   facets this function actually measures for real
///   ([`FinalizationError::FacetMismatch`] on `source.digest`,
///   `dependencies`, or `filesystem.readonly_layers`) disagreed with what was
///   locked. That is real drift this gate exists to catch — it is
///   deliberately NOT downgraded to a warning.
///
/// ## Honest scope: which facets are measured for real, and why the rest are not
///
/// Only the three G0-2-recognized producers are wired here, each reusing a
/// value this command already materializes rather than inventing a new
/// canonicalization scheme:
///
/// * `source.digest` — [`capsule::blob::materialized_source_tree_hash`], the
///   SAME RFC-A1v2 content-addressable tree hash the `source_materialize`
///   builder job already uses elsewhere in this codebase. It is computed
///   over `workspace_root` AS IT EXISTS when Ready-State sealing runs (i.e.
///   after this build's own install/build lifecycle has already run against
///   that same directory) — NOT a pristine pre-install snapshot. Retiming
///   this to capture the workspace before dependency installation would
///   require restructuring this command's control flow and is deliberately
///   not attempted here; a lock whose `source.digest` was established from a
///   pre-install snapshot will legitimately mismatch against this
///   post-install measurement, and that is a real (if currently unresolved)
///   discrepancy to fix in a follow-up, not a bug in this function.
/// * `dependencies[]` — measured only in the (common) trivial case where the
///   locked contract itself declares zero dependencies: zero declared and
///   zero observed is a real, honest measurement, not a placeholder. When the
///   lock declares one or more dependencies, this is left UNMEASURED: no
///   per-dependency derivation/output digest producer exists anywhere in this
///   codebase today (confirmed by a repo-wide search — the only occurrences
///   of `derivation_digest`/`output_digest` outside the contract's own types
///   are in `crates/snapshot/src/contract_fixtures.rs`, an explicit test
///   fixture).
/// * `filesystem.readonly_layers` — the content digest of the actual sealed
///   rootfs bytes (`layers.rootfs`), the one layer `assemble_build_layers`
///   ever populates for `ato build` (`runtime`/`dependency`/`app` are always
///   `None` there). Measured only when the locked contract also declares
///   exactly one readonly layer — a different count is left unmeasured
///   rather than guessing a layer-to-index mapping.
///
/// Every OTHER required facet — `source.projection_digest` foremost, since it
/// is the second facet [`ExecutionObservationV1::finalize`] checks, right
/// after `source.digest` — has no measurement producer anywhere in this
/// codebase yet. Because `finalize`'s facet checks run in a fixed order and
/// stop at the first missing one, this means **in every real invocation
/// today, a lock that carries a D2 `execution_contract` will make this
/// function return `Ok(None)` citing `source.projection_digest`**, regardless
/// of the three real measurements above. Those three are still wired
/// correctly — and exercised by this module's tests via a synthetic
/// full-measurement fixture, matching `execution_contract_finalize`'s own
/// test pattern — so they are already correct for the day a
/// `source.projection_digest` producer (and the others) land, but they
/// cannot be observed to succeed end-to-end via a real `ato build` today.
/// This mirrors `execution_contract_finalize`'s own module doc precisely and
/// is not a gap specific to this file.
fn attempt_v1_execution_identity(
    workspace_root: &Path,
    layers: &snapshot::BuildLayers,
) -> Result<Option<ExecutionContractEnvelopeV1>> {
    // Canonical-vs-alias selection and the fail-closed presence rules belong to
    // the shared resolver (`capsule.lock` preferred; `ato.lock.json` a
    // deprecated read alias; coexistence, a non-regular node under a lock name,
    // and any non-`NotFound` metadata error are errors). Only the resolver's
    // `Ok(None)` — no lock at either name — means "nothing to confirm"; a
    // resolver Err must propagate, never collapse into `Ok(None)`, because
    // that would turn a fail-closed refusal into a silent skip of this gate.
    let Some(lock_path) = resolve_canonical_lock_path(workspace_root)? else {
        return Ok(None);
    };
    let lock = capsule::capsule_lock::load_verified_from_path(&lock_path)
        .with_context(|| format!("verify {}", lock_path.display()))?;
    let Some(expected_envelope) = lock.execution_contract.as_ref() else {
        return Ok(None);
    };
    let expected = &expected_envelope.execution_contract;

    let mut observation = ExecutionObservationV1::new();

    // source.digest — real: hash the workspace as materialized right now (see
    // the doc comment above for the post-install-lifecycle caveat), EXCLUDING
    // the resolved canonical lock itself (see `measure_workspace_source_digest`'s
    // doc for why: hashing a tree that contains its own lock file is a
    // hash-quine and can never stably confirm).
    let source_digest = measure_workspace_source_digest(workspace_root, Some(&lock_path))?;
    observation = observation.measured_source_digest(source_digest);

    // dependencies[] — real only in the trivial zero-dependency case (see doc).
    if expected.dependencies.is_empty() {
        observation = observation.measured_dependencies(Vec::new());
    }

    // filesystem.readonly_layers — real: content digest of the actual sealed
    // rootfs bytes, only when the lock declares exactly one such layer.
    if expected.filesystem.readonly_layers.len() == 1 {
        let algorithm = expected.filesystem.readonly_layers[0].algorithm();
        observation = observation
            .measured_readonly_layers(vec![content_digest_of(&layers.rootfs, algorithm)]);
    }

    match observation.finalize(expected) {
        Ok(finalized) => Ok(Some(finalized.into_envelope())),
        Err(FinalizationError::UnmeasuredFacet(_)) => Ok(None),
        Err(other) => Err(anyhow::anyhow!(
            "Capsule v1 execution identity check failed against the locked expectation in {}: {other}",
            lock_path.display()
        )),
    }
}

/// Measure `source.digest` for `workspace_root`: the RFC-A1v2
/// [`capsule::blob::materialized_source_tree_hash`] of the workspace,
/// EXCLUDING the top-level entry named by `canonical_lock_path` — the lock
/// the resolver actually selected for this workspace, not a hardcoded name.
/// `None` (the workspace has no canonical lock) excludes nothing.
///
/// The exclusion is not optional polish — it fixes a real hash-quine. The
/// canonical lock lives directly inside `workspace_root` (right next to
/// `capsule.toml`), the same directory this measures as "source". If a
/// future lock-writer embedded `source.digest` in that same file WITHOUT
/// excluding the file from its own hash input, no value could ever be
/// embedded that equals the hash of the tree containing it (changing the
/// embedded digest changes the file's bytes, which changes the tree hash,
/// which no longer matches the newly-embedded value, indefinitely — the same
/// class of problem as hashing a directory that contains its own checksum
/// file). Excluding the lock itself breaks the cycle; this mirrors why
/// content-addressed systems conventionally exclude their own metadata
/// (e.g. `.git`) from what they hash as tree content.
///
/// The excluded name is threaded in from the resolved path rather than fixed
/// here because spec §5 gives the lock two admissible names (`capsule.lock`
/// and its deprecated `ato.lock.json` alias): excluding a constant would
/// reintroduce exactly this quine for every workspace holding the other name.
///
/// Implementation note: [`capsule::blob::materialized_source_tree_hash`] has
/// no exclude-list parameter (by design — RFC A1's frozen algorithm takes a
/// bare root path), so this copies `workspace_root` into a scratch directory
/// minus that one entry, then hashes the copy. Every other file is preserved
/// verbatim, including permissions bits (the A1 hash commits the executable
/// bit) and symlinks (preserved as real symlinks so the SAME admissibility
/// walk that would reject a symlink in `workspace_root` itself still rejects
/// it here — silently dropping symlinks during the copy would make this
/// measurement quietly accept a tree the frozen algorithm is specified to
/// refuse).
fn measure_workspace_source_digest(
    workspace_root: &Path,
    canonical_lock_path: Option<&Path>,
) -> Result<ContentDigest> {
    let excluded_top_level_entry = canonical_lock_path.and_then(|path| path.file_name());
    let scratch = tempfile::tempdir().context("create scratch directory for source hashing")?;
    copy_source_tree_excluding_top_level_lock(
        workspace_root,
        scratch.path(),
        true,
        excluded_top_level_entry,
    )?;
    let source_hash = capsule::blob::materialized_source_tree_hash(scratch.path())
        .with_context(|| format!("hash workspace source tree at {}", workspace_root.display()))?;
    ContentDigest::try_from(source_hash)
        .context("materialized_source_tree_hash output did not parse as a ContentDigest")
}

/// Recursively mirrors `src` into the already-created, empty directory `dst`,
/// skipping `excluded_top_level_entry` when `is_root` (i.e. only at the top
/// level — a nested file that happens to share that name is ordinary source
/// content, not this workspace's own lock). `None` skips nothing. See
/// [`measure_workspace_source_digest`] for why this exclusion exists.
fn copy_source_tree_excluding_top_level_lock(
    src: &Path,
    dst: &Path,
    is_root: bool,
    excluded_top_level_entry: Option<&OsStr>,
) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("read directory {}", src.display()))? {
        let entry =
            entry.with_context(|| format!("read directory entry under {}", src.display()))?;
        let file_name = entry.file_name();
        if is_root && excluded_top_level_entry == Some(file_name.as_os_str()) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&file_name);
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", from.display()))?;
        if file_type.is_dir() {
            fs::create_dir(&to).with_context(|| format!("create directory {}", to.display()))?;
            copy_source_tree_excluding_top_level_lock(&from, &to, false, excluded_top_level_entry)?;
        } else if file_type.is_symlink() {
            #[cfg(unix)]
            {
                let target = fs::read_link(&from)
                    .with_context(|| format!("read symlink {}", from.display()))?;
                std::os::unix::fs::symlink(&target, &to)
                    .with_context(|| format!("recreate symlink {}", to.display()))?;
            }
            #[cfg(not(unix))]
            {
                anyhow::bail!(
                    "cannot measure source.digest across a symlink at {} on this platform \
                     (materialized_source_tree_hash rejects symlinks in the admissibility walk \
                     regardless, so this tree could never yield a source.digest either way)",
                    from.display()
                );
            }
        } else if file_type.is_file() {
            fs::copy(&from, &to)
                .with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&from)
                    .with_context(|| format!("stat {}", from.display()))?
                    .permissions()
                    .mode();
                fs::set_permissions(&to, fs::Permissions::from_mode(mode))
                    .with_context(|| format!("set permissions on {}", to.display()))?;
            }
        } else {
            anyhow::bail!("unsupported file type at {}", from.display());
        }
    }
    Ok(())
}

/// A real (not placeholder) content digest of `bytes`, using whichever
/// algorithm the corresponding expected-contract field declares — a
/// measurement must match the expected value's own algorithm choice to have
/// any chance of agreeing with it (`ContentDigest` does not fix one
/// algorithm the way the opaque `*_digest` facets do).
fn content_digest_of(bytes: &[u8], algorithm: DigestAlgorithm) -> ContentDigest {
    match algorithm {
        DigestAlgorithm::Blake3 => {
            ContentDigest::new(DigestAlgorithm::Blake3, *blake3::hash(bytes).as_bytes())
        }
        DigestAlgorithm::Sha256 => {
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            let digest = hasher.finalize();
            let mut buffer = [0u8; 32];
            buffer.copy_from_slice(&digest);
            ContentDigest::new(DigestAlgorithm::Sha256, buffer)
        }
    }
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
    plan: &capsule::router::ManifestData,
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
    let prepared_config = capsule::packers::source::prepare_source_config_from_descriptor(
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
    let artifact = capsule::packers::source::pack(
        plan,
        capsule::packers::source::SourcePackOptions {
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
            publish_profile: capsule::packers::pack_filter::PublishProfile::Artifact,
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

    if artifact_path.exists()
        && let Err(err) = std::fs::remove_file(artifact_path)
    {
        futures::executor::block_on(reporter.warn(format!(
            "⚠️  Failed to remove artifact after smoke failure: {} ({err})",
            artifact_path.display()
        )))?;
    }

    Ok(())
}

fn run_v03_build_lifecycle_steps(
    plan: &capsule::router::ManifestData,
    reporter: &std::sync::Arc<reporters::CliReporter>,
    strict_lockfile: bool,
) -> Result<()> {
    let schema_version = plan
        .manifest
        .get("schema_version")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if schema_version != MANIFEST_SCHEMA_V03 {
        return Ok(());
    }

    let target_labels = plan.selected_target_package_order()?;
    let lifecycle_targets = build_lifecycle_targets(plan, &target_labels)?;
    let root_install_plan = build_root_install_plan(&lifecycle_targets)?;

    let mut provisioned_roots = std::collections::HashSet::new();
    for root in root_order(&lifecycle_targets) {
        let Some(root_target) = lifecycle_targets
            .iter()
            .find(|target| target.working_dir == root)
        else {
            continue;
        };
        let target_plan = plan.with_selected_target(root_target.label.clone());
        if let Some(install) = root_install_plan.get(&root) {
            let install_plan = plan.with_selected_target(install.label.clone());
            futures::executor::block_on(reporter.notify(format!(
                "⚙️  Install [{}]: {}",
                install.label, install.command
            )))?;
            run_build_lifecycle_shell_command(&install_plan, &install.command, "install")?;
        } else if provisioned_roots.insert(root.clone())
            && let Some(command) = plan_v03_build_provision_command(&target_plan, strict_lockfile)?
        {
            futures::executor::block_on(reporter.notify(format!(
                "⚙️  Provision [{}]: {}",
                root_target.label, command
            )))?;
            run_build_lifecycle_shell_command(&target_plan, &command, "provision")?;
        }
    }

    for target in lifecycle_targets {
        let target_plan = plan.with_selected_target(target.label.clone());
        if let Some(command) = target_plan
            .build_lifecycle_build()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            let build_cache = prepare_v03_build_cache(&target_plan, &command, reporter)?;
            if let Some(build_cache) = build_cache.as_ref()
                && build_cache.restore_outputs()?
            {
                futures::executor::block_on(reporter.notify(format!(
                    "♻️  Build cache hit [{}]: restored {}",
                    target.label,
                    build_cache.describe_outputs()
                )))?;
                continue;
            }

            futures::executor::block_on(
                reporter.notify(format!("🏗️  Build [{}]: {}", target.label, command)),
            )?;
            run_build_lifecycle_shell_command(&target_plan, &command, "build")?;

            if let Some(build_cache) = build_cache.as_ref() {
                if build_cache.capture_outputs()? {
                    futures::executor::block_on(reporter.notify(format!(
                        "💾 Build cache saved [{}]: {}",
                        target.label,
                        build_cache.describe_outputs()
                    )))?;
                } else {
                    futures::executor::block_on(reporter.warn(format!(
                        "⚠️  Build cache skipped [{}]: declared outputs were not produced",
                        target.label
                    )))?;
                }
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct BuildLifecycleTarget {
    label: String,
    working_dir: PathBuf,
    install: Option<String>,
}

#[derive(Debug, Clone)]
struct BuildRootInstallCommand {
    label: String,
    command: String,
}

fn build_lifecycle_targets(
    plan: &capsule::router::ManifestData,
    target_labels: &[String],
) -> Result<Vec<BuildLifecycleTarget>> {
    target_labels
        .iter()
        .map(|label| {
            let target_plan = plan.with_selected_target(label.clone());
            Ok(BuildLifecycleTarget {
                label: label.clone(),
                working_dir: dependency_root(&target_plan),
                install: crate::commands::run::explicit_install_command_string(&target_plan)?,
            })
        })
        .collect()
}

fn root_order(targets: &[BuildLifecycleTarget]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut roots = Vec::new();
    for target in targets {
        if seen.insert(target.working_dir.clone()) {
            roots.push(target.working_dir.clone());
        }
    }
    roots
}

fn build_root_install_plan(
    targets: &[BuildLifecycleTarget],
) -> Result<std::collections::HashMap<PathBuf, BuildRootInstallCommand>> {
    let mut by_root = std::collections::HashMap::<PathBuf, BuildRootInstallCommand>::new();
    for target in targets {
        let Some(command) = target.install.as_ref() else {
            continue;
        };
        if let Some(existing) = by_root.get(&target.working_dir) {
            if existing.command != *command {
                return Err(AtoExecutionError::execution_contract_invalid(
                    format!(
                        "conflicting install lifecycle commands for dependency root '{}': target '{}' declares '{}', target '{}' declares '{}'. Use one root-level install command for targets that share a dependency root.",
                        target.working_dir.display(),
                        existing.label,
                        existing.command,
                        target.label,
                        command
                    ),
                    Some("targets.<label>.install"),
                    Some(&target.label),
                )
                .into());
            }
            continue;
        }
        by_root.insert(
            target.working_dir.clone(),
            BuildRootInstallCommand {
                label: target.label.clone(),
                command: command.clone(),
            },
        );
    }
    Ok(by_root)
}

fn plan_v03_build_provision_command(
    plan: &capsule::router::ManifestData,
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
        if !execution_working_directory.join("package.json").exists() {
            return Ok(None);
        }
        let mut matches = Vec::new();
        if package_lock.exists() {
            matches.push(if strict_lockfile {
                "npm ci"
            } else {
                "npm install"
            });
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
        // Priority order: pnpm > npm > yarn > bun
        let preferred_order = if strict_lockfile {
            [
                "pnpm install --frozen-lockfile",
                "npm ci",
                "yarn install --frozen-lockfile",
                "bun install --frozen-lockfile",
            ]
        } else {
            ["pnpm install", "npm install", "yarn install", "bun install"]
        };
        return match matches.as_slice() {
            [] => Ok(None),
            [command] => Ok(Some((*command).to_string())),
            _ => {
                // Multiple lockfiles: pick the highest-priority one
                let chosen = preferred_order
                    .iter()
                    .find(|&&cmd| matches.contains(&cmd))
                    .copied()
                    .unwrap_or(matches[0]);
                Ok(Some(chosen.to_string()))
            }
        };
    }

    if matches!(driver.as_str(), "python") {
        let uv_lock = execution_working_directory.join("uv.lock");
        let pyproject = execution_working_directory.join("pyproject.toml");
        let requirements = execution_working_directory.join("requirements.txt");
        debug!(
            phase = "build",
            runtime,
            driver,
            workspace_root = %workspace_root.display(),
            execution_working_directory = %execution_working_directory.display(),
            lockfile_check_paths = ?vec![
                ("pyproject.toml", pyproject.clone(), pyproject.exists()),
                ("requirements.txt", requirements.clone(), requirements.exists()),
                ("uv.lock", uv_lock.clone(), uv_lock.exists()),
            ],
            "Provision command path diagnostics"
        );
        if pyproject.exists() {
            return if uv_lock.exists() {
                Ok(Some("uv sync --frozen".to_string()))
            } else {
                Err(AtoExecutionError::lock_incomplete(
                    "source/python target has pyproject.toml but is missing uv.lock for fail-closed provisioning",
                    Some("uv.lock"),
                )
                .into())
            };
        }
        if requirements.exists() {
            return if uv_lock.exists() {
                Ok(Some(python_requirements_lock_sync_command(None)))
            } else {
                Err(python_requirements_lock_missing(
                    "source/python target has requirements.txt but is missing uv.lock for fail-closed provisioning",
                )
                .into())
            };
        }
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
struct V03BuildCache {
    working_dir: PathBuf,
    cache_dir: PathBuf,
    outputs: Vec<OutputSpec>,
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
    plan: &capsule::router::ManifestData,
    build_command: &str,
    reporter: &std::sync::Arc<reporters::CliReporter>,
) -> Result<Option<V03BuildCache>> {
    let outputs = plan.build_cache_outputs();
    if outputs.is_empty() {
        return Ok(None);
    }

    let output_specs = match normalize_outputs(&outputs) {
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
    let cache_dir = capsule::common::paths::nacelle_home_dir()?
        .join("build-cache")
        .join("chml")
        .join(cache_key);

    Ok(Some(V03BuildCache {
        working_dir: plan.execution_working_directory(),
        cache_dir,
        outputs: output_specs,
    }))
}

fn compute_v03_build_cache_key(
    plan: &capsule::router::ManifestData,
    outputs: &[OutputSpec],
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

    for lockfile in native_lockfiles(&working_dir) {
        update_hash_text(&mut hasher, &lockfile.display().to_string());
        hash_file_contents(&mut hasher, &lockfile)?;
    }

    for relative_path in collect_source_files(&working_dir, outputs)? {
        update_hash_text(&mut hasher, &relative_path.display().to_string());
        hash_file_contents(&mut hasher, &working_dir.join(&relative_path))?;
    }

    Ok(hex::encode(hasher.finalize()))
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
    plan: &capsule::router::ManifestData,
    command: &str,
    phase: &str,
) -> Result<()> {
    // Prepend the ato-managed Node bin dir to PATH so the lifecycle command finds
    // the pinned node/npm (#294). Use `ensure_node_binary_with_authority(plan, None)`
    // so provider-backed targets (npm:pkg) that store runtime_version in capsule.toml
    // are handled correctly.
    let managed_node_dir: Option<PathBuf> =
        match runtime_manager::ensure_node_binary_with_authority(plan, None) {
            Ok(node_bin) => node_bin.parent().map(|dir| dir.to_path_buf()),
            Err(_) => None,
        };

    // The mechanism for injecting that dir is shell-specific:
    //   * `sh -lc` sources login-profile scripts that *reset* PATH, so on Unix we
    //     must inject `export PATH=…:$PATH;` *inside* the command string (after the
    //     reset) for it to survive.
    //   * `cmd /C` does not source any profile and never resets PATH, so on Windows
    //     we set PATH directly on the child env. Injecting `export PATH=…:$PATH;`
    //     into a `cmd /C` string is invalid syntax — cmd reports
    //     "'export' is not recognized" and the command fails (Windows install bug).
    #[cfg(windows)]
    let effective_command = command.to_string();

    #[cfg(not(windows))]
    let effective_command = match &managed_node_dir {
        Some(dir) => format!("export PATH={}:$PATH; {}", dir.display(), command),
        None => command.to_string(),
    };

    #[cfg(windows)]
    let mut cmd = {
        // `cmd.exe /D /S /C "<command>"` via raw_arg: deterministic quote
        // handling, no std argv re-escaping (cmd.exe cannot parse `\"`), and
        // shell operators like `&&` survive verbatim.
        let mut cmd = crate::common::host_shell::windows_cmd_shell_command(&effective_command);
        if let Some(dir) = &managed_node_dir {
            let existing = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{};{}", dir.display(), existing));
        }
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-lc", &effective_command]);
        cmd
    };

    cmd.current_dir(capsule::common::paths::windows_child_compatible_path(
        &plan.execution_working_directory(),
    ))
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::inherit())
    .stderr(std::process::Stdio::inherit())
    .env("COREPACK_ENABLE_STRICT", "0")
    // Disable pnpm 10's auto-manage-package-manager-versions to prevent it from
    // attempting to download the pinned pnpm version in offline/CI environments.
    .env("npm_config_manage_package_manager_versions", "false")
    .env("npm_config_approve_builds", "on")
    // Skip git-hooks managers: the capsule workspace has no .git dir so their
    // prepare/postinstall scripts would fail with exit 128.
    .env("HUSKY", "0")
    .env("LEFTHOOK", "0");

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
        let sig_path = capsule::signing::sign_artifact(target, key_path, "ato-cli", None)?;
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
        attempt_v1_execution_identity, build_decision_from_manifest_text, content_digest_of,
        execute_pack_command, execute_pack_command_with_injected_manifest,
        measure_workspace_source_digest, plan_v03_build_provision_command,
        run_v03_build_lifecycle_steps,
    };
    use capsule::execution_contract::{
        ContentDigest, DigestAlgorithm, EXECUTION_CONTRACT_V1_SCHEMA, ExecutionContractEnvelopeV1,
        ExecutionContractV1, ExecutionId, GuestPath, GuestSurfaceContract, OpaqueContractDomainV1,
        ResolvedArtifactContract, ResolvedFilesystemContract, ResolvedLaunchContract,
        ResolvedPolicyContract, ResolvedSourceContract, ResolvedTargetContract,
        opaque_subcontract_digest,
    };
    use capsule::input_resolver::{
        CAPSULE_LOCK_FILE_NAME, DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
    };
    use capsule::router::{ExecutionProfile, ManifestData};
    use capsule::types::ValidationMode;
    use sha2::{Digest, Sha256};
    use std::ffi::OsString;
    use std::path::PathBuf;

    const ZERO_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    const RUNTIME_METADATA_TRIPLES: &[&str] = &[
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
    ];

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                unsafe {
                    std::env::set_var(self.key, previous);
                }
            } else {
                unsafe {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn seed_runtime_metadata_cache(ato_home: &std::path::Path, name: &str, version: &str) {
        for target_triple in RUNTIME_METADATA_TRIPLES {
            let cache_path = ato_home
                .join("metadata-cache")
                .join("runtime")
                .join(name)
                .join(version)
                .join(format!("{target_triple}.sha256"));
            std::fs::create_dir_all(cache_path.parent().expect("cache parent"))
                .expect("create metadata cache");
            std::fs::write(cache_path, ZERO_SHA256).expect("write metadata cache");
        }
    }

    #[test]
    fn v03_build_provision_uses_target_working_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app_dir = tmp.path().join("apps").join("web");
        std::fs::create_dir_all(&app_dir).expect("create app dir");
        std::fs::write(app_dir.join("package.json"), "{}\n").expect("write package.json");
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
        std::fs::write(tmp.path().join("package.json"), "{}\n").expect("write package.json");
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
    fn v03_build_provision_uses_requirements_uv_lock_with_pip_sync() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("requirements.txt"), "fastapi==0.115.6\n")
            .expect("write requirements");
        std::fs::write(tmp.path().join("uv.lock"), "# pip-compile lock\n").expect("write lock");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("python".to_string())),
                (
                    "run_command",
                    toml::Value::String("python app.py".to_string()),
                ),
            ],
        );

        let command = plan_v03_build_provision_command(&plan, true).expect("plan provision");
        assert_eq!(
            command.as_deref(),
            Some("uv venv --seed --clear && uv pip sync uv.lock")
        );
    }

    #[test]
    fn v03_build_provision_rejects_requirements_without_uv_lock() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("requirements.txt"), "fastapi==0.115.6\n")
            .expect("write requirements");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("python".to_string())),
                (
                    "run_command",
                    toml::Value::String("python app.py".to_string()),
                ),
            ],
        );

        let err = plan_v03_build_provision_command(&plan, true)
            .expect_err("requirements without uv.lock should fail closed");
        assert!(err.to_string().contains("missing uv.lock"));
        assert!(err.to_string().contains("requirements.txt"));
    }

    #[test]
    fn v03_build_provision_prefers_pyproject_uv_lock_over_requirements_lock() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .expect("write pyproject");
        std::fs::write(tmp.path().join("requirements.txt"), "fastapi==0.115.6\n")
            .expect("write requirements");
        std::fs::write(tmp.path().join("uv.lock"), "version = 1\n").expect("write lock");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("python".to_string())),
                (
                    "run_command",
                    toml::Value::String("python app.py".to_string()),
                ),
            ],
        );

        let command = plan_v03_build_provision_command(&plan, true).expect("plan provision");
        assert_eq!(command.as_deref(), Some("uv sync --frozen"));
    }

    #[test]
    #[serial_test::serial]
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
                        "mkdir -p build-scratch dist && printf x >> build-scratch/build-count.txt && printf cached > dist/out.txt"
                            .to_string(),
                    ),
                ),
                (
                    "outputs",
                    toml::Value::Array(vec![
                        toml::Value::String("dist/**".to_string()),
                        // Exclude the build-script counter from cache key
                        // inputs — otherwise the side-effect counter created
                        // by the first build would alter the cache key the
                        // second time around.
                        toml::Value::String("build-scratch/**".to_string()),
                    ]),
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

        unsafe {
            std::env::set_var("ATO_BUILD_CACHE_TEST_ENV", "test");
        }
        run_v03_build_lifecycle_steps(&plan, &reporter, true).expect("first build");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("build-scratch/build-count.txt"))
                .expect("read count"),
            "x"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("dist/out.txt")).expect("read output"),
            "cached"
        );

        std::fs::remove_dir_all(tmp.path().join("dist")).expect("remove dist");
        run_v03_build_lifecycle_steps(&plan, &reporter, true).expect("cache restore");

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("build-scratch/build-count.txt"))
                .expect("read count after restore"),
            "x"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("dist/out.txt")).expect("read restored output"),
            "cached"
        );
        unsafe {
            std::env::remove_var("ATO_BUILD_CACHE_TEST_ENV");
        }
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
    #[serial_test::serial]
    fn source_only_authoritative_build_does_not_materialize_capsule_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cache_home = tmp.path().join("ato-home");
        seed_runtime_metadata_cache(&cache_home, "node", "20.12.0");
        let _ato_home_guard = EnvVarGuard::set_path("ATO_HOME", &cache_home);

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
        std::fs::write(tmp.path().join(".node-version"), "20.12.0\n").expect(".node-version");
        std::fs::write(tmp.path().join("index.js"), "console.log('demo');\n").expect("index.js");

        let reporter = std::sync::Arc::new(crate::reporters::CliReporter::new(true));
        let result = execute_pack_command(
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
        .expect("source-only build should avoid manifest materialization");
        assert!(result.ok, "build result should succeed: {result:?}");
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
    #[serial_test::serial]
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

        let mut lock = capsule::capsule_lock::CapsuleLock::default();
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
        let lock_path = manifest_dir.join("capsule.lock");
        let workspace_root = manifest_dir.clone();
        let runtime_model = capsule::lock_runtime::resolve_lock_runtime_model(&lock, None)
            .expect("resolve test runtime model");

        let manifest_value = toml::Value::Table(manifest);
        let compat_manifest =
            capsule::router::CompatManifestBridge::from_manifest_value(&manifest_value)
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
            ingress: None,
        }
    }

    // ---- attempt_v1_execution_identity / content_digest_of ----
    //
    // These tests never fabricate a measurement: the "expected" contract is
    // always written by the TEST (playing the part of a producer that already
    // pinned a lock — no code under test invents it), and every real
    // measurement the function under test performs is checked against a
    // genuinely independent computation (a real tree hash for source.digest,
    // a direct blake3/sha256 call for the layer digest).

    fn placeholder_opaque_digest() -> capsule::execution_contract::OpaqueContractDigestV1 {
        opaque_subcontract_digest(
            OpaqueContractDomainV1::SourceProjection,
            &serde_json::json!({}),
        )
        .expect("placeholder opaque digest")
    }

    /// A minimal but fully valid `ExecutionContractV1`: zero dependencies,
    /// exactly one readonly layer, and every opaque facet filled with the same
    /// placeholder digest (none of them are ever measured by the function
    /// under test, so their exact value never matters to these tests — only
    /// that the contract as a whole is well-formed enough to compute a real
    /// `execution_id` and pass `capsule_lock` persisted validation).
    fn minimal_contract(
        source_digest: ContentDigest,
        readonly_layer: ContentDigest,
    ) -> ExecutionContractV1 {
        let placeholder = placeholder_opaque_digest();
        ExecutionContractV1 {
            schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            source: ResolvedSourceContract {
                digest: source_digest,
                projection_digest: placeholder,
            },
            target: ResolvedTargetContract {
                os: "linux".to_string(),
                architecture: "x86_64".to_string(),
                abi: "gnu".to_string(),
                libc: None,
                observable_features: std::collections::BTreeMap::new(),
            },
            runtime: ResolvedArtifactContract {
                kind: "node".to_string(),
                digest: ContentDigest::new(DigestAlgorithm::Blake3, [2u8; 32]),
                dynamic_contract_digest: placeholder,
            },
            dependencies: Vec::new(),
            build_outputs: Vec::new(),
            launch: ResolvedLaunchContract {
                argv: vec!["node".to_string(), "server.js".to_string()],
                cwd: GuestPath::parse("/workspace").unwrap(),
                process_model_digest: placeholder,
                environment: Vec::new(),
                environment_policy_digest: placeholder,
                secret_bindings: Vec::new(),
            },
            filesystem: ResolvedFilesystemContract {
                view_digest: ContentDigest::new(DigestAlgorithm::Blake3, [7u8; 32]),
                topology_digest: placeholder,
                readonly_layers: vec![readonly_layer],
                writable_paths: Vec::new(),
            },
            policy: ResolvedPolicyContract {
                network_digest: placeholder,
                capability_digest: placeholder,
                filesystem_digest: placeholder,
            },
            guest_surface: GuestSurfaceContract {
                bind_address: "0.0.0.0".to_string(),
                protocol: "ato-guest/v1".to_string(),
                port: None,
                features: Vec::new(),
            },
            external_state: Vec::new(),
        }
    }

    fn envelope_of(contract: ExecutionContractV1) -> ExecutionContractEnvelopeV1 {
        let execution_id = contract
            .compute_execution_id()
            .expect("compute execution_id");
        ExecutionContractEnvelopeV1 {
            execution_contract: contract,
            execution_id,
            // No ADR-014 parent-association claim: this fixture is a bare
            // execution envelope, matching `FinalizedExecutionIdentityV1::
            // into_envelope`'s own default.
            capsule_program_id: None,
            resolved_refs: Default::default(),
            generated_at: None,
            provenance: serde_json::Value::Null,
            diagnostics: serde_json::Value::Null,
            evidence: serde_json::Value::Null,
        }
    }

    /// Writes a real, persisted-valid canonical lock (recomputed `lock_id`,
    /// full write-path verification via `write_pretty_to_path`) carrying the
    /// given D2 `execution_contract`, under `file_name` — the canonical
    /// `capsule.lock` unless a test is deliberately exercising the deprecated
    /// `ato.lock.json` read alias. Mirrors `capsule::capsule_lock`'s own test
    /// fixtures (`base_lock`/`lock_with` in `capsule_lock::execution`'s tests).
    fn write_lock_with_contract_as(
        workspace: &std::path::Path,
        envelope: ExecutionContractEnvelopeV1,
        file_name: &str,
    ) {
        let mut lock = capsule::capsule_lock::CapsuleLock {
            execution_contract: Some(envelope),
            ..capsule::capsule_lock::CapsuleLock::default()
        };
        lock.resolution
            .entries
            .insert("runtime".to_string(), serde_json::json!({"kind": "node"}));
        lock.contract.entries.insert(
            "process".to_string(),
            serde_json::json!({"entrypoint": "server.js"}),
        );
        capsule::capsule_lock::recompute_lock_id(&mut lock).expect("recompute lock_id");
        capsule::capsule_lock::write_pretty_to_path(&lock, &workspace.join(file_name))
            .expect("write canonical lock");
    }

    /// [`write_lock_with_contract_as`] under the canonical `capsule.lock` name.
    fn write_lock_with_contract(
        workspace: &std::path::Path,
        envelope: ExecutionContractEnvelopeV1,
    ) {
        write_lock_with_contract_as(workspace, envelope, CAPSULE_LOCK_FILE_NAME);
    }

    fn empty_build_layers(rootfs: Vec<u8>) -> snapshot::BuildLayers {
        snapshot::BuildLayers {
            rootfs,
            runtime: None,
            dependency: None,
            app: None,
            vmstate: Vec::new(),
            memory: Vec::new(),
        }
    }

    #[test]
    fn attempt_v1_execution_identity_returns_none_without_a_lock_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layers = empty_build_layers(b"artifact-bytes".to_vec());
        let result = attempt_v1_execution_identity(tmp.path(), &layers)
            .expect("no canonical lock must not be an error");
        assert!(result.is_none());
    }

    #[test]
    fn attempt_v1_execution_identity_returns_none_when_lock_has_no_execution_contract() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut lock = capsule::capsule_lock::CapsuleLock::default();
        lock.resolution
            .entries
            .insert("runtime".to_string(), serde_json::json!({"kind": "node"}));
        lock.contract.entries.insert(
            "process".to_string(),
            serde_json::json!({"entrypoint": "server.js"}),
        );
        capsule::capsule_lock::recompute_lock_id(&mut lock).expect("recompute lock_id");
        capsule::capsule_lock::write_pretty_to_path(
            &lock,
            &tmp.path().join(CAPSULE_LOCK_FILE_NAME),
        )
        .expect("write capsule.lock");

        let layers = empty_build_layers(b"artifact-bytes".to_vec());
        let result = attempt_v1_execution_identity(tmp.path(), &layers)
            .expect("a lock with no D2 section must not be an error");
        assert!(result.is_none());
    }

    #[test]
    fn attempt_v1_execution_identity_refuses_on_unmeasured_facet_even_when_the_three_real_measurements_match()
     {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("main.js"), b"console.log(1);\n")
            .expect("write source file");
        let source_hash = capsule::blob::materialized_source_tree_hash(tmp.path())
            .expect("hash workspace source tree");
        let source_digest = ContentDigest::try_from(source_hash).expect("parse source digest");

        let rootfs_bytes = b"the-sealed-rootfs-bytes".to_vec();
        let readonly_layer = content_digest_of(&rootfs_bytes, DigestAlgorithm::Blake3);

        let contract = minimal_contract(source_digest, readonly_layer);
        write_lock_with_contract(tmp.path(), envelope_of(contract));

        let layers = empty_build_layers(rootfs_bytes);

        // The 3 G0-2 facets this function measures all genuinely agree
        // (real source tree hash, zero dependencies, and the real rootfs
        // layer digest) — proving those measurements are wired correctly —
        // yet `.finalize()` still legitimately refuses because
        // `source.projection_digest` (checked right after `source.digest`)
        // has no measurement producer. That refusal must surface as
        // `Ok(None)`, never an error: if any of the 3 real measurements were
        // wrong, this would instead see a `FacetMismatch` surfaced as `Err`
        // by `attempt_v1_execution_identity`, so this also proves they are
        // computed correctly.
        let result = attempt_v1_execution_identity(tmp.path(), &layers).expect(
            "an UnmeasuredFacet refusal must be reported as Ok(None), never Err — if this \
             instead errors, one of the 3 real measurements disagreed with the fixture",
        );
        assert!(result.is_none());
    }

    #[test]
    fn attempt_v1_execution_identity_errors_on_a_genuine_source_digest_mismatch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("main.js"), b"console.log(1);\n")
            .expect("write source file");

        // Deliberately wrong: does not match the real tree hash of `tmp`.
        let wrong_source_digest = ContentDigest::new(DigestAlgorithm::Sha256, [0xEE; 32]);
        let rootfs_bytes = b"the-sealed-rootfs-bytes".to_vec();
        let readonly_layer = content_digest_of(&rootfs_bytes, DigestAlgorithm::Blake3);

        let contract = minimal_contract(wrong_source_digest, readonly_layer);
        write_lock_with_contract(tmp.path(), envelope_of(contract));

        let layers = empty_build_layers(rootfs_bytes);

        let error = attempt_v1_execution_identity(tmp.path(), &layers).expect_err(
            "a real source.digest mismatch is caught drift and must be surfaced, not swallowed",
        );
        assert!(
            error
                .to_string()
                .contains("Capsule v1 execution identity check failed"),
            "{error}"
        );
    }

    #[test]
    fn attempt_v1_execution_identity_errors_when_the_lock_itself_is_tampered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("main.js"), b"console.log(1);\n")
            .expect("write source file");
        let source_hash = capsule::blob::materialized_source_tree_hash(tmp.path())
            .expect("hash workspace source tree");
        let source_digest = ContentDigest::try_from(source_hash).expect("parse source digest");
        let readonly_layer = content_digest_of(b"bytes", DigestAlgorithm::Blake3);
        let contract = minimal_contract(source_digest, readonly_layer);
        let mut envelope = envelope_of(contract);
        // Tamper the stored execution_id so it no longer matches the
        // contract's own canonical hash — the read-path verification must
        // reject this rather than trust it.
        envelope.execution_id =
            ExecutionId::new(format!("blake3:{}", "0".repeat(64))).expect("valid-shaped id");
        let mut lock = capsule::capsule_lock::CapsuleLock {
            execution_contract: Some(envelope),
            ..capsule::capsule_lock::CapsuleLock::default()
        };
        capsule::capsule_lock::recompute_lock_id(&mut lock).expect("recompute lock_id");
        // Bypass `write_pretty_to_path`'s own write-time verification (which
        // would itself refuse to persist a tampered artifact) so the tampered
        // lock reaches disk — mirroring how `capsule_lock`'s own read-path tests
        // (`load_verified_rejects_tampered_execution_id`) probe this boundary.
        let raw = serde_json::to_string(&lock).expect("serialize tampered lock");
        std::fs::write(tmp.path().join(CAPSULE_LOCK_FILE_NAME), raw).expect("write tampered lock");

        let layers = empty_build_layers(b"bytes".to_vec());
        let error = attempt_v1_execution_identity(tmp.path(), &layers)
            .expect_err("a tampered lock must fail verification, not be silently trusted");
        assert!(error.to_string().contains("verify"), "{error}");
    }

    #[test]
    fn attempt_v1_execution_identity_reads_a_lock_stored_under_the_deprecated_alias_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("main.js"), b"console.log(1);\n")
            .expect("write source file");

        // Deliberately wrong: does not match the real tree hash of `tmp`.
        let wrong_source_digest = ContentDigest::new(DigestAlgorithm::Sha256, [0xEE; 32]);
        let rootfs_bytes = b"the-sealed-rootfs-bytes".to_vec();
        let readonly_layer = content_digest_of(&rootfs_bytes, DigestAlgorithm::Blake3);
        let contract = minimal_contract(wrong_source_digest, readonly_layer);
        write_lock_with_contract_as(
            tmp.path(),
            envelope_of(contract),
            DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
        );

        let layers = empty_build_layers(rootfs_bytes);

        // The alias is a read-compatible name for the canonical lock, so this
        // lock IS the expectation this build is confirmed against. A missing
        // lock would instead be `Ok(None)`, so surfacing the mismatch proves
        // the alias was resolved and read rather than skipped.
        let error = attempt_v1_execution_identity(tmp.path(), &layers)
            .expect_err("a lock under the deprecated alias name must still be read and enforced");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("Capsule v1 execution identity check failed"),
            "{rendered}"
        );
        assert!(
            rendered.contains(DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME),
            "{rendered}"
        );
    }

    #[test]
    fn attempt_v1_execution_identity_fails_closed_when_both_lock_names_coexist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("main.js"), b"console.log(1);\n")
            .expect("write source file");
        let source_hash = capsule::blob::materialized_source_tree_hash(tmp.path())
            .expect("hash workspace source tree");
        let source_digest = ContentDigest::try_from(source_hash).expect("parse source digest");
        let rootfs_bytes = b"the-sealed-rootfs-bytes".to_vec();
        let readonly_layer = content_digest_of(&rootfs_bytes, DigestAlgorithm::Blake3);
        let contract = minimal_contract(source_digest, readonly_layer);
        write_lock_with_contract_as(
            tmp.path(),
            envelope_of(contract.clone()),
            CAPSULE_LOCK_FILE_NAME,
        );
        write_lock_with_contract_as(
            tmp.path(),
            envelope_of(contract),
            DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
        );

        let layers = empty_build_layers(rootfs_bytes);

        // Both names occupied is split-brain: no automatic authority choice is
        // made. The resolver's refusal must reach the caller as an error —
        // downgrading it to `Ok(None)` ("nothing to confirm") would silently
        // skip this gate on exactly the ambiguous tree it exists to catch.
        let error = attempt_v1_execution_identity(tmp.path(), &layers)
            .expect_err("coexisting lock names must fail closed, never resolve to Ok(None)");
        assert!(
            format!("{error:#}").contains("Both capsule.lock and ato.lock.json exist"),
            "{error:#}"
        );
    }

    #[test]
    fn measure_workspace_source_digest_excludes_whichever_lock_name_resolved() {
        // The hash-quine fix must key on the RESOLVED lock name, not a
        // constant: after the spec §5 rename a workspace may legitimately hold
        // either name, and hashing a tree that contains its own lock can never
        // stably confirm.
        for lock_name in [
            CAPSULE_LOCK_FILE_NAME,
            DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            std::fs::write(tmp.path().join("main.js"), b"console.log(1);\n")
                .expect("write source file");
            std::fs::create_dir(tmp.path().join("nested")).expect("create nested dir");
            // A nested file sharing the lock name is ordinary source content,
            // so it must stay in the digest.
            std::fs::write(tmp.path().join("nested").join(lock_name), b"not-a-lock\n")
                .expect("write nested namesake");

            let lock_free_hash = capsule::blob::materialized_source_tree_hash(tmp.path())
                .expect("hash lock-free workspace");
            let expected = ContentDigest::try_from(lock_free_hash).expect("parse lock-free digest");

            let lock_path = tmp.path().join(lock_name);
            std::fs::write(&lock_path, b"{\"schema_version\":1}\n").expect("write lock file");

            let measured = measure_workspace_source_digest(tmp.path(), Some(&lock_path))
                .expect("measure source digest");
            assert_eq!(measured, expected, "lock_name={lock_name}");

            // With nothing resolved, nothing is excluded — the same tree now
            // hashes differently, proving the exclusion is what did the work.
            let unexcluded = measure_workspace_source_digest(tmp.path(), None)
                .expect("measure source digest without exclusion");
            assert_ne!(unexcluded, expected, "lock_name={lock_name}");
        }
    }

    #[test]
    fn content_digest_of_matches_independent_hashing_for_both_algorithms() {
        let bytes = b"hello capsule v1";

        let blake3_digest = content_digest_of(bytes, DigestAlgorithm::Blake3);
        assert_eq!(blake3_digest.algorithm(), DigestAlgorithm::Blake3);
        assert_eq!(blake3_digest.bytes(), *blake3::hash(bytes).as_bytes());

        let sha256_digest = content_digest_of(bytes, DigestAlgorithm::Sha256);
        assert_eq!(sha256_digest.algorithm(), DigestAlgorithm::Sha256);
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let expected: [u8; 32] = hasher.finalize().into();
        assert_eq!(sha256_digest.bytes(), expected);
    }
}

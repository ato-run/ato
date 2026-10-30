//! Hosted Formation frontend for `ato.capsule/2` sources.
//!
//! A v1 job compiles intent into a plan and lets the Runtime build it. A v2
//! source already declares a portable application — workspace, state,
//! observations, scenarios and one or more Derivations — so nothing here
//! infers an application. The frontend:
//!
//! 1. parses the document with the same strict grammar `ato pack` uses;
//! 2. materializes the control plane's copy of the manifest into the attempt's
//!    source root (the bundle's workspace digest excludes `capsule.toml`, so
//!    this does not change the source closure);
//! 3. builds the bundle with the SAME builder `ato pack` calls — a second
//!    builder would be a second answer;
//! 4. proves the authored state / observations / scenarios reached the bundle,
//!    and fails closed when one did not;
//! 5. selects a Derivation explicitly (never by planner);
//! 6. runs the bundle through the shared portable attempt and publishes the
//!    additive v2 result the control plane accepts.
//!
//! A v1 source never enters this module: `job.rs` routes on the declared
//! schema, and the v1 path below the branch is unchanged.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use ato_formation::authoring::{HTTP_CONTRACT_VERIFIER, SCENARIO_CONTRACT_VERIFIER};
use ato_formation::capsule_toml::{CAPSULE_FILE_NAME, declared_schema};
use ato_formation::capsule_toml_v2::{
    CAPSULE_SCHEMA_V2, PortableAuthoringDraftV2, parse_capsule_toml_v2,
};
use ato_formation::failure::{FailureStage, FormationFailure};
use ato_formation::request::AttemptOutcomes;
use ato_formation::source::SourceClosureRef;
use ato_formation::verify::{ContractVerification, ContractVerificationReceipt};
use ato_portable_application::portable_run::{
    PortableBundleAttempt, PortableBundleAttemptOutcome, run_portable_bundle_attempt,
};
use ato_portable_application::{ValidatedPortableApplication, validate_all_derivations};
use ato_runtime_attempt::build_sandbox::NetworkPolicy;
use ato_runtime_attempt::journal::AttemptPermit;
use ato_runtime_attempt::plan::digest;

use crate::api::FormationApi;
use crate::build::BuildAttempt;
use crate::job::source_evidence::FormationSourceEvidenceV1;
use crate::job::{HostedAttemptFailure, JobOutcome, persist_hosted_outcome};

/// The typed refusal codes this frontend adds. Everything else reuses the
/// vocabulary the existing parsers and attempt pipeline already state.
pub const CAPSULE_V2_MANIFEST_NOT_MATERIALIZED: &str = "capsule_v2_manifest_not_materialized";
pub const CAPSULE_V2_BUNDLE_INVALID: &str = "capsule_v2_bundle_invalid";
pub const CAPSULE_V2_SEMANTICS_DROPPED: &str = "capsule_v2_semantics_dropped";
pub const CAPSULE_V2_NETWORK_NOT_ADMITTED: &str = "capsule_v2_network_not_admitted";
pub const CAPSULE_V2_DERIVATION_SELECTION_REQUIRED: &str = "derivation_selection_required";
pub const CAPSULE_V2_DERIVATION_NOT_DECLARED: &str = "derivation_selection_not_declared";
pub const CAPSULE_V2_BUNDLE_PUBLICATION_FAILED: &str = "capsule_v2_bundle_publication_failed";
pub const CAPSULE_V2_CONTRACT_NOT_SATISFIED: &str = "capsule_v2_contract_not_satisfied";

/// Whether a job's authored document belongs to this frontend.
///
/// Routing only: the parser decides validity. A document that declares
/// `ato.capsule/2` gets the v2 frontend; every other document (including an
/// unknown schema, which the v1 parser refuses as `unsupported_capsule_schema`)
/// keeps the v1 path byte-for-byte.
pub(crate) fn owns(manifest: &str) -> Result<bool, FormationFailure> {
    Ok(declared_schema(manifest).map_err(FormationFailure::from)?
        == Some(CAPSULE_SCHEMA_V2.to_owned()))
}

/// Everything the v2 frontend needs from the claimed job.
pub(crate) struct V2Request<'a> {
    pub api: &'a FormationApi,
    pub attempt: &'a BuildAttempt,
    /// The manifest the control plane accepted (its copy wins over the tree's).
    pub manifest: &'a str,
    pub source_root: &'a Path,
    pub attempt_root: &'a Path,
    pub work_root: &'a Path,
    pub worker_id: &'a str,
    pub shim: &'a Path,
    pub closure_ref: &'a SourceClosureRef,
    pub source_evidence: &'a FormationSourceEvidenceV1,
    pub compute_id: &'a str,
    pub capsule_revision_id: &'a str,
    /// `job.target.triple`, recorded on the materialization.
    pub triple: &'a str,
    /// `job.policy.network`, verbatim.
    pub network_policy: &'a str,
    /// `job.authoring.derivation_ref` — required only when the bundle declares
    /// more than one Derivation. v1 jobs never carry it.
    pub requested_derivation: Option<&'a str>,
    pub permit: Option<Box<dyn AttemptPermit>>,
}

/// The target a v2 result records. A portable bundle executes on any Runner
/// that supports its declared realizations; the guest root is the workspace
/// path inside them, which the portable profile fixes at `/app`.
const PORTABLE_GUEST_ROOT: &str = "/app";

pub(crate) fn run(mut request: V2Request<'_>) -> Result<JobOutcome> {
    let attempt = request.attempt;
    let draft = parse_capsule_toml_v2(request.manifest).map_err(|error| {
        formation_failure(error.code(), FailureStage::Authoring, error.to_string())
    })?;

    // 1. The control plane's copy is the document it accepted and recorded.
    //    `capsule.toml` is excluded from the bundle's workspace digest, so
    //    writing it here cannot change the source closure or the tree.
    let manifest_path = request.source_root.join(CAPSULE_FILE_NAME);
    fs::write(&manifest_path, request.manifest).map_err(|error| {
        formation_failure(
            CAPSULE_V2_MANIFEST_NOT_MATERIALIZED,
            FailureStage::Authoring,
            format!(
                "cannot materialize the accepted manifest at {}: {error}",
                manifest_path.display()
            ),
        )
    })?;

    // 2. Same builder as `ato pack`. No second compiler.
    let (bundle_bytes, bundle) =
        ato_portable_application::build_authored_bundle_v2(request.source_root)
            .map_err(bundle_failure)?;
    let transport = ato_portable_application::bundle_sha256(&bundle_bytes);

    // 3. Which Derivation runs is the caller's decision, never a planner's.
    let derivation_ref = select_derivation(&bundle, request.requested_derivation)?;
    let routes = validate_all_derivations(&bundle).map_err(bundle_failure)?;
    let validated = routes
        .iter()
        .find(|route| route.derivation_ref.as_str() == derivation_ref)
        .with_context(|| {
            format!("selected Derivation {derivation_ref} is not a validated route")
        })?;

    // 4. Nothing the author declared may be silently ignored.
    verify_semantics(&draft, validated)?;

    // 5. An OCI route pulls its pinned image. This Runtime may only do that
    //    under an admitted policy; `denied` is an admission refusal, not a
    //    build failure, and it names itself.
    let network = match request.network_policy {
        "dependency_resolution" => NetworkPolicy::DependencyResolution,
        _ => NetworkPolicy::Denied,
    };
    if matches!(
        validated.realization,
        ato_portable_application::PortableRealizationKind::OciContainer
            | ato_portable_application::PortableRealizationKind::OciServiceGroup
    ) && network != NetworkPolicy::DependencyResolution
    {
        return Err(formation_failure(
            CAPSULE_V2_NETWORK_NOT_ADMITTED,
            FailureStage::Admission,
            "a portable OCI Derivation pulls its pinned image, and this job's network policy \
             does not admit dependency resolution"
                .to_owned(),
        )
        .into());
    }

    let requires_gpu = validated
        .derivation
        .requirements
        .host
        .as_ref()
        .is_some_and(|host| !host.accelerators.is_empty());
    if ato_runtime_attempt::launch::host_boundary::active().is_some() && !requires_gpu {
        return Err(formation_failure(
            "formation_gpu_route_required",
            FailureStage::Admission,
            "dedicated GPU Formation requires an explicitly selected GPU Derivation".to_owned(),
        )
        .into());
    }
    // Keep the validated candidate available for an explicit owner-authorized
    // GPU retry. Registration is quarantine, never a satisfied result.
    let prepared_artifact = if requires_gpu {
        Some(
            request
                .api
                .publish_portable_bundle(
                    &attempt.attempt_id,
                    &transport,
                    validated.contract_ref.as_str(),
                    &derivation_ref,
                    &bundle_bytes,
                )
                .map_err(|error| {
                    formation_failure(
                        CAPSULE_V2_BUNDLE_PUBLICATION_FAILED,
                        FailureStage::Projection,
                        format!("GPU candidate registration failed: {error:#}"),
                    )
                })?,
        )
    } else {
        None
    };

    if requires_gpu && ato_runtime_attempt::launch::host_boundary::active().is_none() {
        return Err(formation_failure(
            "gpu_host_unavailable",
            FailureStage::Admission,
            "GPU verification requires an explicitly authorized dedicated runtime".to_owned(),
        )
        .into());
    }
    let profile = crate::local::probe_local_runtime();
    let cache_root = request.work_root.join("cache");
    if ato_runtime_attempt::launch::host_boundary::active().is_some() {
        prepare_gpu_models(&request, validated, &cache_root).map_err(|error| {
            formation_failure(
                "model_set_unavailable",
                FailureStage::Admission,
                format!("GPU Formation input delivery refused: {error:#}"),
            )
        })?;
    }
    let outcome = run_portable_bundle_attempt(
        &PortableBundleAttempt {
            bundle_bytes: &bundle_bytes,
            bundle: &bundle,
            derivation_ref: &derivation_ref,
            runtime_root: request.attempt_root,
            cache_root: &cache_root,
            sandbox_shim: request.shim,
            sandbox_subcommand: "sandbox-exec",
            request_id: &attempt.job_id,
            attempt_id: &attempt.attempt_id,
            label: "portable_v2",
            runtime_id: request.worker_id,
            profile: &profile,
            authorization: ato_runtime_attempt::admission::EffectAuthorization::Unattended,
            network,
            receipt_target: ato_formation::verify::VerificationTargetKind::FormationRuntime,
            run_id: None,
            binding_environment: &Default::default(),
            interrupt: None,
        },
        request
            .permit
            .take()
            .context("the v2 frontend was given no attempt permit")?,
    )
    .map_err(|error| {
        formation_failure(
            CAPSULE_V2_BUNDLE_INVALID,
            FailureStage::Projection,
            format!("the validated portable bundle could not be prepared: {error:#}"),
        )
    })?;

    // 6. A result is published only for a fully satisfied Contract: a
    //    `fully_satisfied=false` receipt is evidence, never a published result.
    let receipt = outcome.receipt.clone().ok_or_else(|| {
        attempt_failure(
            &outcome,
            CAPSULE_V2_CONTRACT_NOT_SATISFIED,
            "the candidate produced no verification receipt".to_owned(),
        )
    })?;
    if !receipt.fully_satisfied {
        let code = outcome
            .attempt
            .failure
            .as_ref()
            .map(|failure| failure.code.clone())
            .unwrap_or_else(|| failed_observation_code(&receipt).to_owned());
        return Err(attempt_failure(
            &outcome,
            &code,
            format!(
                "the candidate did not satisfy K ({} observations)",
                receipt.observations.len()
            ),
        )
        .into());
    }

    // 7. Register the bundle before the result names it: an artifact ref that
    //    resolves to nothing is worse than a result that was never published.
    let artifact_ref = if let Some(artifact) = prepared_artifact {
        artifact
    } else {
        request
            .api
            .publish_portable_bundle(
                &attempt.attempt_id,
                &transport,
                validated.contract_ref.as_str(),
                &derivation_ref,
                &bundle_bytes,
            )
            .map_err(|error| {
                HostedAttemptFailure::new(
                    CAPSULE_V2_BUNDLE_PUBLICATION_FAILED,
                    "publish",
                    outcome.attempt_record.clone(),
                    Some(&receipt),
                    Some(&outcome.outcomes),
                    Some(error),
                )
            })?
    };

    let verification = outcome.verification.clone().ok_or_else(|| {
        attempt_failure(
            &outcome,
            CAPSULE_V2_CONTRACT_NOT_SATISFIED,
            "the candidate produced no verification".to_owned(),
        )
    })?;

    // Keep the receipt and outcome beside the attempt as durable evidence even
    // if publication is refused.
    let receipt_bytes = receipt
        .canonical_bytes()
        .context("canonicalize the verification receipt")?;
    fs::write(
        request.attempt_root.join("verification-receipt.json"),
        &receipt_bytes,
    )?;
    persist_hosted_outcome(request.attempt_root, &outcome.attempt, &outcome.outcomes)?;

    let result = compose_result(
        &request,
        validated,
        &derivation_ref,
        &transport,
        &artifact_ref,
        bundle_bytes.len() as u64,
        &verification,
        &receipt,
        &outcome.outcomes,
        network,
    )?;
    fs::write(
        request.attempt_root.join("formation-result-v2.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;

    let published =
        match request
            .api
            .publish_result(&result, request.compute_id, request.capsule_revision_id)
        {
            Ok(published) => published,
            Err(error) => {
                return Err(HostedAttemptFailure::new(
                    "result_publication_unconfirmed",
                    "publish",
                    outcome.attempt_record.clone(),
                    Some(&receipt),
                    Some(&outcome.outcomes),
                    Some(error),
                )
                .into());
            }
        };

    Ok(JobOutcome {
        attempt: attempt.clone(),
        closure_ref: request.closure_ref.clone(),
        materialization_ref: artifact_ref,
        outcome: published,
    })
}

/// The Derivation a v2 job runs.
///
/// One route is the bundle's own answer. More than one requires an explicit
/// selection: a hosted build must never choose a route on the uploader's
/// behalf, because the receipt names the route that ran.
/// Populate the existing verified ModelCache from only this attempt's grants.
/// No upstream URL or provider credential reaches the workload.
fn prepare_gpu_models(
    request: &V2Request<'_>,
    route: &ValidatedPortableApplication,
    cache_root: &Path,
) -> Result<()> {
    use ato_runtime_attempt::data_plane::{ModelCache, ModelSetEntry};
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};
    request.api.check_gpu_attempt(&request.attempt.attempt_id)?;
    let mut objects = BTreeMap::new();
    for set in &route.model_sets {
        for entry in &set.manifest.objects {
            let object = ModelSetEntry {
                path: entry.path.clone(),
                digest: entry.digest.clone(),
                bytes: entry.bytes,
            };
            if let Some(previous) = objects.insert(entry.digest.clone(), object) {
                anyhow::ensure!(
                    previous.bytes == entry.bytes,
                    "conflicting Model Object sizes"
                );
            }
        }
    }
    if objects.is_empty() {
        return Ok(());
    }
    let required = objects
        .values()
        .try_fold(0u64, |sum, entry| sum.checked_add(entry.bytes))
        .context("Model Set size overflow")?;
    let available = ato_runtime_attempt::host_resources::HostResources::probe(request.attempt_root)
        .scratch_mib
        .context("scratch capacity unavailable")?
        .saturating_sub(512)
        .saturating_mul(1024 * 1024);
    anyhow::ensure!(
        required <= available,
        "insufficient scratch for declared Model Sets"
    );
    let cache = ModelCache::open(cache_root)?;
    for entry in objects.values() {
        request.api.check_gpu_attempt(&request.attempt.attempt_id)?;
        let mut last_check = Instant::now();
        cache.ensure_bounded(
            entry,
            required,
            |start, end| {
                request
                    .api
                    .gpu_model_range(&request.attempt.attempt_id, &entry.digest, start, end)
            },
            || {
                if last_check.elapsed() >= Duration::from_secs(5) {
                    request.api.check_gpu_attempt(&request.attempt.attempt_id)?;
                    last_check = Instant::now();
                }
                Ok(())
            },
        )?;
    }
    Ok(())
}

fn select_derivation(
    bundle: &ato_objects::PortableApplicationBundle,
    requested: Option<&str>,
) -> Result<String, FormationFailure> {
    let declared = &bundle.index.derivations;
    match (declared.as_slice(), requested) {
        ([], _) => Err(formation_failure(
            CAPSULE_V2_BUNDLE_INVALID,
            FailureStage::Projection,
            "the bundle declares no Derivation".to_owned(),
        )),
        ([only], None) => Ok(only.clone()),
        ([only], Some(requested)) if requested == only => Ok(only.clone()),
        (many, None) => Err(formation_failure(
            CAPSULE_V2_DERIVATION_SELECTION_REQUIRED,
            FailureStage::Authoring,
            format!(
                "the bundle declares {} Derivations; name the one to run with \
                 authoring.derivation_ref",
                many.len()
            ),
        )),
        (many, Some(requested)) if !many.iter().any(|declared| declared == requested) => {
            Err(formation_failure(
                CAPSULE_V2_DERIVATION_NOT_DECLARED,
                FailureStage::Authoring,
                format!(
                    "the requested Derivation {requested} is not one of the bundle's {} declared routes",
                    many.len()
                ),
            ))
        }
        (_, Some(requested)) => Ok(requested.to_owned()),
    }
}

/// Prove the authored semantics reached the bundle.
///
/// The builder is strict, but "the builder is strict" is not a property this
/// frontend should merely believe: a refactor that dropped `[[state]]` would
/// still produce a valid bundle. Each authored declaration is compared with
/// what the bound Contract/Derivation actually carries, and a mismatch fails
/// closed instead of publishing a result for a different application.
fn verify_semantics(
    draft: &PortableAuthoringDraftV2,
    validated: &ValidatedPortableApplication,
) -> Result<(), FormationFailure> {
    let authored_states = draft
        .state
        .iter()
        .map(|state| state.id.as_str())
        .collect::<BTreeSet<_>>();
    let bound_states = validated
        .derivation
        .state
        .iter()
        .map(|state| state.id.as_str())
        .collect::<BTreeSet<_>>();
    if authored_states != bound_states {
        return Err(semantics_dropped("state", &authored_states, &bound_states));
    }

    let authored_observations = draft
        .observations
        .iter()
        .map(|observation| observation.id.as_str())
        .collect::<BTreeSet<_>>();
    let bound_observations = validated
        .contract
        .requirements
        .iter()
        .filter(|requirement| requirement.verifier == HTTP_CONTRACT_VERIFIER)
        .map(|requirement| requirement.id.as_str())
        .collect::<BTreeSet<_>>();
    if authored_observations != bound_observations {
        return Err(semantics_dropped(
            "contract.observation",
            &authored_observations,
            &bound_observations,
        ));
    }

    // Scenarios must survive as `ato.contract.scenario@1` requirements with
    // their canonical object in the closure: the existing verifier executes
    // from those, so a scenario that only parsed is not a scenario that runs.
    let authored_scenarios = draft
        .scenarios
        .iter()
        .map(|scenario| scenario.id.as_str())
        .collect::<BTreeSet<_>>();
    let bound_scenarios = validated
        .scenarios
        .iter()
        .map(|scenario| scenario.requirement_id.as_str())
        .collect::<BTreeSet<_>>();
    if authored_scenarios != bound_scenarios {
        return Err(semantics_dropped(
            "contract.observation.scenario",
            &authored_scenarios,
            &bound_scenarios,
        ));
    }
    for scenario in &validated.scenarios {
        let requirement = validated
            .contract
            .requirements
            .iter()
            .find(|requirement| requirement.id == scenario.requirement_id)
            .ok_or_else(|| {
                formation_failure(
                    CAPSULE_V2_SEMANTICS_DROPPED,
                    FailureStage::Projection,
                    format!(
                        "scenario {} has no contract.observation requirement in the bundle",
                        scenario.requirement_id
                    ),
                )
            })?;
        if requirement.verifier != SCENARIO_CONTRACT_VERIFIER
            || requirement.digest.as_deref() != Some(scenario.scenario_ref.as_str())
        {
            return Err(formation_failure(
                CAPSULE_V2_SEMANTICS_DROPPED,
                FailureStage::Projection,
                format!(
                    "scenario {} is not bound as a content-addressed requirement",
                    scenario.requirement_id
                ),
            ));
        }
    }

    let authored_bindings = draft
        .bindings
        .iter()
        .map(|binding| binding.id.as_str())
        .collect::<BTreeSet<_>>();
    let bound_bindings = validated
        .application
        .bindings
        .iter()
        .map(|binding| binding.id.as_str())
        .collect::<BTreeSet<_>>();
    if authored_bindings != bound_bindings {
        return Err(semantics_dropped(
            "binding",
            &authored_bindings,
            &bound_bindings,
        ));
    }

    Ok(())
}

fn semantics_dropped(
    field: &str,
    authored: &BTreeSet<&str>,
    bound: &BTreeSet<&str>,
) -> FormationFailure {
    formation_failure(
        CAPSULE_V2_SEMANTICS_DROPPED,
        FailureStage::Projection,
        format!(
            "the bundle does not carry the authored {field}: declared {authored:?}, bound {bound:?}"
        ),
    )
}

/// The additive v2 result. Every key v1 fills is filled here too; the portable
/// facts are additive, so a v1 reader keeps working and a v2 reader gets the
/// bundle it must import.
#[allow(clippy::too_many_arguments)]
fn compose_result(
    request: &V2Request<'_>,
    validated: &ValidatedPortableApplication,
    derivation_ref: &str,
    transport: &str,
    artifact_ref: &str,
    size_bytes: u64,
    verification: &ContractVerification,
    receipt: &ContractVerificationReceipt,
    outcomes: &AttemptOutcomes,
    network: NetworkPolicy,
) -> Result<serde_json::Value> {
    let surface_port = validated
        .application
        .surfaces
        .first()
        .map(|surface| surface.port.as_str())
        .unwrap_or("app.http");
    let exported_ports = validated
        .derivation
        .ports
        .iter()
        .filter(|port| port.id == surface_port)
        .map(|port| {
            serde_json::json!({
                "name": "http",
                "protocol": "http",
                "guest_port": port.guest_port,
            })
        })
        .collect::<Vec<_>>();
    let readiness_contracts = validated
        .contract
        .requirements
        .iter()
        .filter(|requirement| {
            requirement.verifier == HTTP_CONTRACT_VERIFIER
                && requirement.port.as_deref() == Some(surface_port)
        })
        .map(|requirement| {
            serde_json::json!({
                "kind": "http",
                "port_name": "http",
                "path": requirement.path.clone().unwrap_or_else(|| "/".to_owned()),
            })
        })
        .collect::<Vec<_>>();
    let state_slot_declarations = validated
        .derivation
        .state
        .iter()
        .map(|slot| {
            serde_json::json!({
                "state_key": slot.id,
                "mount_target": slot.mount,
                "access": "read_write",
                "protocol": "ato.state.filesystem@1",
            })
        })
        .collect::<Vec<_>>();
    let triple = request.triple;
    let deterministic_inputs_digest = digest(&serde_jcs::to_vec(&serde_json::json!({
        "source_closure_ref": request.closure_ref.as_str(),
        "derivation_ref": derivation_ref,
        "portable_bundle_sha256": transport,
        "target": {"triple": triple, "workspace_guest_root": PORTABLE_GUEST_ROOT},
    }))?);
    let formation_key = format!("v2:{deterministic_inputs_digest}");
    let source_revision_ref = format!(
        "srev_{}",
        &request.closure_ref.as_str()[7..23.min(request.closure_ref.as_str().len())]
    );

    Ok(serde_json::json!({
        "protocol": "ato.formation-result.v2",
        "job_id": request.attempt.job_id,
        "attempt_id": request.attempt.attempt_id,
        "attempt_fence": request.attempt.attempt_fence,
        "status": "succeeded",
        "formation_key": formation_key,
        "source_revision_ref": source_revision_ref,
        "source_closure_ref": request.closure_ref.as_str(),
        "source_evidence": request.source_evidence,
        "compute_schema_ref": digest(format!("{derivation_ref}|{transport}").as_bytes()),
        "materializations": [{
            "kind": "portable_application",
            "content_ref": transport,
            "media_type": ato_ipc::formation::PORTABLE_APPLICATION_MEDIA_TYPE,
            "digest": transport,
            "size_bytes": size_bytes,
            "target": {"triple": triple, "workspace_guest_root": PORTABLE_GUEST_ROOT},
            "compatibility": {"os": "linux"},
            "producer": concat!("ato-formation-worker/", env!("CARGO_PKG_VERSION")),
        }],
        "runtime_requirements": [],
        "realization_candidates": [{
            "kind": "portable_application",
            "derivation_ref": derivation_ref,
            "bundle_artifact_ref": artifact_ref,
            "schema_profile": ato_ipc::formation::PORTABLE_APPLICATION_PROFILE,
        }],
        "exported_ports": exported_ports,
        "readiness_contracts": readiness_contracts,
        "state_slot_declarations": state_slot_declarations,
        "binding_requirements": [],
        "provenance": {
            "formation_service_version": env!("CARGO_PKG_VERSION"),
            "builder_catalog_version": "catalog-2026-09",
            "policy_version": "formation-policy-v1",
            "network_policy": match network {
                NetworkPolicy::Denied => "denied",
                NetworkPolicy::DependencyResolution => "dependency_resolution",
                NetworkPolicy::Scoped => "phase_scoped_connect",
            },
            "isolation": if ato_runtime_attempt::launch::host_boundary::active().is_some() { "host-boundary-v1;uid+no_new_privs+landlock;network=denied" } else { network.provenance() },
            "field_origins": {},
        },
        "diagnostics": [],
        "contract_ref": validated.contract_ref.as_str(),
        "derivation_ref": derivation_ref,
        "contract_verification": {
            "summary": verification.summary(),
            "observations": verification
                .verdicts
                .iter()
                .map(|verdict| {
                    let (result, detail) = match &verdict.outcome {
                        ato_formation::verify::ObservationOutcome::Satisfied => {
                            ("satisfied", String::new())
                        }
                        ato_formation::verify::ObservationOutcome::Deferred { by } => {
                            ("deferred", by.clone())
                        }
                        ato_formation::verify::ObservationOutcome::Failed { code, detail } => {
                            (*code, detail.clone())
                        }
                    };
                    serde_json::json!({ "id": verdict.id, "result": result, "detail": detail })
                })
                .collect::<Vec<_>>(),
        },
        "deterministic_inputs_digest": deterministic_inputs_digest,
        "verification_receipt": receipt,
        // The portable attempt never publishes; this frontend does, and it has
        // already registered the bundle under `artifact_ref` before composing
        // this result. A `succeeded` Hosted result must say so: the control
        // plane refuses a success whose publication is `not_applicable`, and
        // that refusal is right — an artifact nobody could name is not a
        // published one.
        "outcomes": {
            "seal": outcomes.seal.clone(),
            "runtime_verification": outcomes.runtime_verification.clone(),
            "cleanup": outcomes.cleanup.clone(),
            "publication": ato_formation::request::Outcome::succeeded(),
        },
        "formation_kind": "portable_v2",
        "schema_profile": ato_ipc::formation::PORTABLE_APPLICATION_PROFILE,
        "portable_bundle_sha256": transport,
        "portable_bundle_artifact_ref": artifact_ref,
    }))
}

fn failed_observation_code(receipt: &ContractVerificationReceipt) -> &'static str {
    for observation in &receipt.observations {
        if let Some(failure) = &observation.failure {
            let code = failure.split(':').next().unwrap_or(failure);
            return match code {
                "scenario_step_failed" => "scenario_step_failed",
                "scenario_isolation_unavailable" => "scenario_isolation_unavailable",
                "runtime_version_not_patched" => "runtime_version_not_patched",
                "runtime_version_unverifiable" => "runtime_version_unverifiable",
                _ => CAPSULE_V2_CONTRACT_NOT_SATISFIED,
            };
        }
    }
    CAPSULE_V2_CONTRACT_NOT_SATISFIED
}

fn attempt_failure(
    outcome: &PortableBundleAttemptOutcome,
    fallback_code: &str,
    fallback_message: String,
) -> HostedAttemptFailure {
    let (code, stage, message) = match &outcome.attempt.failure {
        Some(failure) => (
            failure.code.clone(),
            failure.stage.clone(),
            failure.message.clone(),
        ),
        None => (
            fallback_code.to_owned(),
            "verification".to_owned(),
            fallback_message,
        ),
    };
    let mut failure = HostedAttemptFailure::new(
        &code,
        &stage,
        outcome.attempt_record.clone(),
        outcome.receipt.as_ref(),
        Some(&outcome.outcomes),
        outcome
            .error
            .as_ref()
            .map(|error| anyhow::anyhow!("{error:#}")),
    );
    if let Some(failure_record) = &outcome.attempt.failure {
        failure = failure.with_failure(Some(failure_record.clone()));
    }
    let _ = message;
    failure
}

/// The builder and the validator already know what refused: a runtime policy
/// refusal is an execution-boundary refusal, not a malformed bundle, and the
/// control plane branches on the code. Keep the typed code; only fall back to
/// `capsule_v2_bundle_invalid` when the error is not one of those decisions.
fn bundle_failure(error: ato_portable_application::PortableApplicationError) -> FormationFailure {
    let message = error.to_string();
    for code in [
        "runtime_version_not_patched",
        "runtime_version_unverifiable",
    ] {
        if message.contains(code) {
            return formation_failure(code, FailureStage::Admission, message);
        }
    }
    formation_failure(CAPSULE_V2_BUNDLE_INVALID, FailureStage::Projection, message)
}

fn formation_failure(code: &str, stage: FailureStage, message: String) -> FormationFailure {
    FormationFailure::new(code, stage, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const PROCESS_TOML: &str = r#"
schema = "ato.capsule/2"

[application]
title = "V2 unit"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[state]]
id = "data"
use = "ato.state.filesystem@1"
mount = "/data"
access = "read-write"

[contract]
mode = "all"

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
path = "/"
status = 200

[[contract.observation]]
id = "crud"
use = "ato.contract.scenario@1"
port = "app.http"
scenario = "scenarios/crud.json"

[[derivation]]
id = "app"
use = "ato.process@1"
argv = ["python3", "app.py"]
cwd = "."
guest_port = 8000
runtimes = { python = "3.12" }

[effects]
default = "pure"
"#;

    const TWO_ROUTES_TOML: &str = r#"
schema = "ato.capsule/2"

[application]
title = "V2 two routes"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[contract]
mode = "all"

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
path = "/"
status = 200

[[derivation]]
id = "first"
use = "ato.process@1"
argv = ["python3", "app.py"]
cwd = "."
guest_port = 8000
runtimes = { python = "3.12" }

[[derivation]]
id = "second"
use = "ato.process@1"
argv = ["python3", "other.py"]
cwd = "."
guest_port = 8001
runtimes = { python = "3.12" }

[effects]
default = "pure"
"#;

    const SCENARIO: &str = r#"{
  "schema": "ato.scenario/1",
  "verifier_version": 1,
  "isolation": { "fresh_instance": true, "network": "candidate-only" },
  "limits": { "step_timeout_ms": 5000, "total_timeout_ms": 60000, "max_body_bytes": 1048576 },
  "steps": [
    { "id": "read", "request": { "method": "GET", "path": "/" },
      "expect": { "status": 200 } }
  ]
}"#;

    fn source(manifest: &str) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(root.path().join("capsule.toml"), manifest).expect("manifest");
        fs::write(root.path().join("app.py"), "print('serve')").expect("app");
        fs::write(root.path().join("other.py"), "print('serve')").expect("app");
        fs::create_dir_all(root.path().join("scenarios")).expect("scenario dir");
        fs::write(root.path().join("scenarios/crud.json"), SCENARIO).expect("scenario");
        let path = root.path().to_path_buf();
        (root, path)
    }

    fn validated_bundle(manifest: &str) -> ato_objects::PortableApplicationBundle {
        let (_root, path) = source(manifest);
        let (_, bundle) =
            ato_portable_application::build_authored_bundle_v2(&path).expect("bundle builds");
        bundle
    }

    #[test]
    fn routing_reads_only_the_declared_schema() {
        assert!(!owns(r#"schema = "ato.capsule/1""#).expect("v1 routes elsewhere"));
        assert!(owns(PROCESS_TOML).expect("v2 routes here"));
        // A schema this build does not read stays on the v1 path, whose parser
        // refuses it as `unsupported_capsule_schema`; the v2 frontend does not
        // claim it and cannot invent a third grammar.
        assert!(!owns(r#"schema = "ato.capsule/3""#).expect("unknown routes nowhere new"));
        assert!(owns("not toml at all").is_err());
    }

    #[test]
    fn one_derivation_is_the_bundles_own_answer() {
        let bundle = validated_bundle(PROCESS_TOML);
        assert_eq!(bundle.index.derivations.len(), 1);
        assert_eq!(
            select_derivation(&bundle, None).expect("one route needs no selection"),
            bundle.index.derivations[0]
        );
    }

    #[test]
    fn several_derivations_require_an_explicit_selection() {
        let bundle = validated_bundle(TWO_ROUTES_TOML);
        assert_eq!(bundle.index.derivations.len(), 2);
        let refused = select_derivation(&bundle, None).expect_err("two routes need a choice");
        assert_eq!(refused.code, CAPSULE_V2_DERIVATION_SELECTION_REQUIRED);
        let selected = select_derivation(&bundle, Some(bundle.index.derivations[1].as_str()))
            .expect("a declared route is selectable");
        assert_eq!(selected, bundle.index.derivations[1]);
        let unknown = format!("sha256:{}", "a".repeat(64));
        let refused = select_derivation(&bundle, Some(&unknown)).expect_err("undeclared route");
        assert_eq!(refused.code, CAPSULE_V2_DERIVATION_NOT_DECLARED);
    }

    #[test]
    fn authored_state_observations_and_scenarios_reach_the_bundle() {
        let (_root, path) = source(PROCESS_TOML);
        let manifest = fs::read_to_string(path.join("capsule.toml")).expect("manifest");
        let draft = parse_capsule_toml_v2(&manifest).expect("parses");
        let (_, bundle) =
            ato_portable_application::build_authored_bundle_v2(&path).expect("bundle");
        let routes =
            ato_portable_application::validate_all_derivations(&bundle).expect("validates");
        verify_semantics(&draft, &routes[0]).expect("nothing authored was dropped");
        assert_eq!(routes[0].derivation.state.len(), 1);
        assert_eq!(routes[0].scenarios.len(), 1);
    }

    #[test]
    fn a_bundle_that_dropped_an_authored_declaration_is_refused() {
        // Same authoring, but the bound route below carries no state: exactly
        // the silent-drop shape this check exists to catch.
        let (_root, with_state) = source(PROCESS_TOML);
        let manifest = fs::read_to_string(with_state.join("capsule.toml")).expect("manifest");
        let draft = parse_capsule_toml_v2(&manifest).expect("parses");
        let stateless = PROCESS_TOML.replace(
            "[[state]]\nid = \"data\"\nuse = \"ato.state.filesystem@1\"\nmount = \"/data\"\naccess = \"read-write\"\n",
            "",
        );
        assert_ne!(stateless, PROCESS_TOML);
        let (_other, stateless_root) = source(&stateless);
        let (_, bundle) =
            ato_portable_application::build_authored_bundle_v2(&stateless_root).expect("bundle");
        let routes =
            ato_portable_application::validate_all_derivations(&bundle).expect("validates");
        let refused = verify_semantics(&draft, &routes[0]).expect_err("the drop is refused");
        assert_eq!(refused.code, CAPSULE_V2_SEMANTICS_DROPPED);
        assert!(refused.message.contains("state"));
    }

    #[test]
    fn the_manifest_is_not_part_of_the_workspace_tree() {
        let (_root, path) = source(PROCESS_TOML);
        let (_, first) = ato_portable_application::build_authored_bundle_v2(&path).expect("bundle");
        let first_tree = ato_portable_application::validate_all_derivations(&first)
            .expect("validates")
            .remove(0)
            .tree_ref
            .as_str()
            .to_owned();
        let entries = ato_portable_application::validate_all_derivations(&first)
            .expect("validates")
            .remove(0)
            .tree
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        assert!(
            !entries.iter().any(|entry| entry == "capsule.toml"),
            "the manifest is authoring input, not a workspace file: {entries:?}"
        );

        // Rewriting only the manifest must leave the workspace identity alone.
        fs::write(
            path.join("capsule.toml"),
            PROCESS_TOML.replace("title = \"V2 unit\"", "title = \"V2 unit, edited\""),
        )
        .expect("rewrite manifest");
        let (_, second) =
            ato_portable_application::build_authored_bundle_v2(&path).expect("bundle");
        let second_tree = ato_portable_application::validate_all_derivations(&second)
            .expect("validates")
            .remove(0)
            .tree_ref
            .as_str()
            .to_owned();
        assert_eq!(
            first_tree, second_tree,
            "capsule.toml must be excluded from the workspace digest"
        );
    }
}

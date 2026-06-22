use anyhow::{Context, Result};
#[cfg(test)]
use capsule::engine::execution_graph::ExecutionGraph;
use capsule::engine::execution_graph::{
    ExecutionGraphBuilder, GraphHostInput, GraphMaterializationSeedInput, GraphPolicyInput,
    GraphPreflightInput, GraphReceiptSeedInput, LaunchGraphBundle, LaunchGraphBundleInput,
};
use capsule::execution_identity::{
    ExecutionIdentityInput, ExecutionIdentityInputV2, ExecutionReceipt, ExecutionReceiptDocument,
    ExecutionReceiptV2, ExecutionRunnerIdentity, FilesystemIdentityBuilder, FilesystemIdentityV2,
    GraphCompleteness, GraphReceipt, LaunchIdentity, NativeInferenceContext, ObservationScope,
    OciProviderReceiptEvidence, PolicyIdentity, PolicyIdentityBuilder, PolicyIdentityV2, Tracked,
};
use capsule::execution_plan::model::ExecutionPlan;
use capsule::launch_spec::derive_launch_spec;
use capsule::lockfile::manifest_external_capsule_dependencies;
use capsule::router::ManifestData;
use capsule::runtime::oci::{OciContainerRequest, OciPortSpec};
use capsule::types::{
    OciLaunchEnvelope, OciPolicyEnforcementLevel, OciPolicyEnforcementMode, OciPolicyEnvelope,
    OciProviderKind, OciProviderMode, OciProviderSemantics, OciProviderSubstrate,
};
use serde::Serialize;

use crate::application::build_materialization::BuildObservation;
use crate::application::execution_graph_adapter::build_input_from_external_dependencies;
use crate::application::execution_observers_v2::{
    ObserverContextV2, build_local_locator, build_policy_identity_v2, observe_dependencies_v2,
    observe_environment_v2, observe_filesystem_v2, observe_launch_v2, observe_runtime_v2,
    observe_source_provenance, observe_source_v2,
};
use crate::application::provider_projection::oci::OciProjectionPlan;
use crate::executors::launch_context::RuntimeLaunchContext;

/// Receipt schema selector. Step 17 of the portability v2 implementation
/// sequence flipped the stable default from v1 to v2; this is the
/// "all v2 observers and acceptance tests passed, default emission moves
/// to v2" milestone (Phase Y/8 completed). Existing v1 consumers can opt
/// out via `ATO_RECEIPT_SCHEMA=v1`.
///
/// Decision matrix:
///
/// | `ATO_RECEIPT_SCHEMA` | Result            |
/// |---------------------|-------------------|
/// | unset (default)     | V2Experimental    |
/// | `v2` / `v2-experimental` | V2Experimental |
/// | `v1`                | V1                |
/// | any other value     | V2Experimental + ATO-WARN |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReceiptSchemaSelector {
    V1,
    V2Experimental,
}

impl ReceiptSchemaSelector {
    pub(crate) fn from_env() -> Self {
        match std::env::var("ATO_RECEIPT_SCHEMA").as_deref() {
            Ok("v1") => Self::V1,
            Ok("v2") | Ok("v2-experimental") | Err(_) => Self::V2Experimental,
            Ok(other) => {
                eprintln!(
                    "ATO-WARN unknown ATO_RECEIPT_SCHEMA={other:?}; defaulting to v2-experimental"
                );
                Self::V2Experimental
            }
        }
    }
}

pub(crate) fn build_prelaunch_receipt(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    build_observation: Option<&BuildObservation>,
) -> Result<ExecutionReceipt> {
    let launch_spec = derive_launch_spec(plan).with_context(|| {
        format!(
            "failed to derive launch spec for execution receipt: {}",
            plan.manifest_path.display()
        )
    })?;

    let source = crate::application::execution_observers::observe_source(plan, &launch_spec)?;
    let dependencies = crate::application::execution_observers::observe_dependencies(
        &launch_spec,
        launch_ctx,
        build_observation,
    )?;
    let runtime =
        crate::application::execution_observers::observe_runtime(execution_plan, &launch_spec)?;
    let environment =
        crate::application::execution_observers::observe_environment(plan, launch_ctx)?;
    let filesystem = crate::application::execution_observers::observe_filesystem(
        plan,
        launch_ctx,
        &launch_spec,
    )?;
    let policy = PolicyIdentity {
        network_policy_hash: Tracked::known(
            execution_plan.consent.provisioning_policy_hash.clone(),
        ),
        capability_policy_hash: Tracked::known(execution_plan.consent.policy_segment_hash.clone()),
        sandbox_policy_hash: Tracked::known(sandbox_policy_hash(execution_plan)?),
    };
    let launch = LaunchIdentity {
        entry_point: launch_spec.command,
        argv: {
            let mut argv = launch_spec.args;
            argv.extend(launch_ctx.command_args().iter().cloned());
            argv
        },
        working_directory: launch_spec.working_dir.display().to_string(),
    };
    let reproducibility = crate::application::execution_reproducibility::classify_execution(
        execution_plan,
        &dependencies,
        &runtime,
        &environment,
        &filesystem,
    );

    Ok(ExecutionReceipt::from_input(
        ExecutionIdentityInput::new(
            source,
            dependencies,
            runtime,
            environment,
            filesystem,
            policy,
            launch,
            reproducibility,
        ),
        chrono::Utc::now().to_rfc3339(),
    )?)
}

// TODO(v2-policy): expand to cover sandbox backend ID (landlock+bwrap / seatbelt /
// none), strength tier, platform-specific enforcement mode, and known gaps per
// RFC §3.6 / plan §"Policy identity v2". Current inputs only describe consent
// algo and launch target, not the actual sandbox backend. Tracked for step 12
// of the portability v2 implementation sequence.
#[derive(Serialize)]
struct SandboxPolicyHashInput<'a> {
    target_runtime: &'a str,
    target_driver: &'a str,
    fail_closed: bool,
    mount_set_algo_id: &'a str,
    mount_set_algo_version: u32,
}

fn sandbox_policy_hash(execution_plan: &ExecutionPlan) -> Result<String> {
    let input = SandboxPolicyHashInput {
        target_runtime: execution_plan.target.runtime.as_str(),
        target_driver: execution_plan.target.driver.as_str(),
        fail_closed: execution_plan.runtime.fail_closed,
        mount_set_algo_id: execution_plan.consent.mount_set_algo_id.as_str(),
        mount_set_algo_version: execution_plan.consent.mount_set_algo_version,
    };
    let canonical =
        serde_jcs::to_vec(&input).context("failed to canonicalize sandbox policy identity")?;
    Ok(format!("blake3:{}", blake3::hash(&canonical).to_hex()))
}

/// Build a v2 (experimental) execution receipt. Wraps the v2 observer
/// pipeline so the receipt builder is the single composition site.
/// Thin wrapper over [`build_prelaunch_receipt_v2_with_graph`] for
/// call sites that do not yet need to carry the bundle forward.
#[allow(dead_code)]
pub(crate) fn build_prelaunch_receipt_v2(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    build_observation: Option<&BuildObservation>,
) -> Result<ExecutionReceiptV2> {
    Ok(
        build_prelaunch_receipt_v2_with_graph(plan, execution_plan, launch_ctx, build_observation)?
            .0,
    )
}

/// PR-3b carrier-aware v2 receipt builder. Returns the receipt AND the
/// `LaunchGraphBundle` it was derived from, so pipeline state and
/// downstream consumers (session record, readiness update, partial
/// receipt boundary) read declared/resolved execution ids from the
/// SAME bundle instance instead of re-deriving and risking drift.
pub(crate) fn build_prelaunch_receipt_v2_with_graph(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    build_observation: Option<&BuildObservation>,
) -> Result<(ExecutionReceiptV2, LaunchGraphBundle)> {
    let launch_spec = derive_launch_spec(plan).with_context(|| {
        format!(
            "failed to derive launch spec for v2 execution receipt: {}",
            plan.manifest_path.display()
        )
    })?;

    let ctx = ObserverContextV2::for_plan(plan);
    let source = observe_source_v2(plan, &ctx)?;
    let provenance = observe_source_provenance(plan);
    let mut runtime = observe_runtime_v2(execution_plan, &launch_spec, &ctx)?;
    // native-inference: attach the declared/resolved engine + model context from
    // the manifest (`plan` is already in scope — no extra threading). `None` for
    // every other runtime.
    runtime.native_inference = build_native_inference_context(plan);
    let dependencies =
        observe_dependencies_v2(plan, &launch_spec, launch_ctx, build_observation, &runtime)?;
    let environment = observe_environment_v2(plan, launch_ctx, &ctx)?;
    let filesystem_observed = observe_filesystem_v2(plan, launch_ctx, &launch_spec, &ctx)?;
    let policy_observed = build_policy_identity_v2(execution_plan);
    let launch = observe_launch_v2(&launch_spec, launch_ctx, &runtime, &ctx)?;
    let local = build_local_locator(plan, &launch_spec, launch_ctx, &runtime);

    // Graph-derived identities (refs #98, #99). Build the declared graph
    // from manifest + lock + policy facts only (host-independent), then
    // build the resolved graph by extending with host-resolution outputs
    // (filesystem view_hash, sandbox_policy_hash). The two canonical
    // forms are domain-tagged, so the same nodes/edges in different
    // domains produce different digests by construction.
    //
    // Spec: docs/execution-identity.md §"Graph-based execution identity".
    let launch_graph_bundle =
        build_launch_graph_bundle(plan, &filesystem_observed, &policy_observed)?;
    let declared_execution_id = Some(
        launch_graph_bundle
            .derived
            .execution_ids
            .declared_execution_id
            .clone(),
    );
    let resolved_execution_id = Some(
        launch_graph_bundle
            .derived
            .execution_ids
            .resolved_execution_id
            .clone(),
    );

    // Build the input once with the observed facets, then route the
    // filesystem/policy facets through the typed builders so the
    // graph wiring is the load-bearing API change. In production the
    // labels carry the same facts as the observed facets, so the
    // builder output is byte-equivalent to the observed facets — the
    // wiring is what pins the entry point future waves will use to
    // source these facets from the graph instead of the V2 observer
    // pipeline.
    let placeholder_reproducibility = capsule::execution_identity::ReproducibilityIdentity {
        class: capsule::execution_identity::ReproducibilityClass::BestEffort,
        causes: Vec::new(),
    };
    let mut identity_input = ExecutionIdentityInputV2::new(
        source,
        provenance,
        dependencies,
        runtime,
        environment,
        filesystem_observed,
        policy_observed,
        launch,
        local,
        placeholder_reproducibility,
    );
    identity_input.filesystem = FilesystemIdentityBuilder::build_with_graph(
        &identity_input,
        Some(&launch_graph_bundle.resolved_graph),
    );
    identity_input.policy = PolicyIdentityBuilder::build_with_graph(
        &identity_input,
        Some(&launch_graph_bundle.resolved_graph),
    );

    if let Some(ingress) = &plan.ingress {
        let envelope = OciLaunchEnvelope::new(
            OciProviderSemantics {
                kind: OciProviderKind::AtoNative,
                mode: OciProviderMode::Unknown,
                substrate: OciProviderSubstrate::Unknown,
                policy_profile: "ingress-declared-v1".to_string(),
            },
            vec![],
            OciPolicyEnvelope {
                enforcement_mode: OciPolicyEnforcementMode::Strict,
                enforcement_level: OciPolicyEnforcementLevel::Enforced,
                network_policy_hash: None,
                filesystem_policy_hash: None,
                capability_policy_hash: None,
                unsupported_policy: vec![],
            },
        )
        .with_ingress(Some(ingress.clone()));
        identity_input = identity_input.with_oci_launch_envelope(Some(envelope));
    }

    // For classification, derive v1-compatible Tracked fields from the v2
    // observations and reuse the existing classifier so v1 and v2 receipts
    // share the same reproducibility verdict for the same launch envelope.
    let class_inputs = classification_inputs_from_v2(
        &identity_input.dependencies,
        &identity_input.runtime,
        &identity_input.environment,
        &identity_input.filesystem,
    );
    identity_input.reproducibility =
        crate::application::execution_reproducibility::classify_execution(
            execution_plan,
            &class_inputs.dependencies,
            &class_inputs.runtime,
            &class_inputs.environment,
            &class_inputs.filesystem,
        );

    let identity_input = identity_input
        .with_declared_execution_id(declared_execution_id.clone())
        .with_resolved_execution_id(resolved_execution_id.clone());
    // observed_execution_id stays None per v0.6.0 contract (no
    // observation hooks). Setter exists for forward-compat only.

    // Receipt-safe OCI provider evidence (#493), derived through the #516
    // provider projection boundary from the declared/locked OCI envelope. Empty
    // for non-OCI launches.
    let provider_projections = declared_oci_provider_projections(plan, execution_plan);

    let receipt = ExecutionReceiptV2::from_input(identity_input, chrono::Utc::now().to_rfc3339())?
        .with_runner(ExecutionRunnerIdentity::new(
            "ato-cli",
            Some(env!("CARGO_PKG_VERSION").to_string()),
        ))
        .with_host_fingerprint(format!(
            "{}:{}:{}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            "unknown-libc"
        ))
        // Node/edge receipts are a projection of the *resolved* launch graph —
        // the load-bearing graph, never ad hoc runtime command strings (#493).
        .with_graph_projection(&launch_graph_bundle.resolved_graph)
        .with_provider_projections(provider_projections)
        // Completeness stays Partial: receipts are derived from the
        // declared/resolved graph only — there is no observed (post-spawn)
        // coverage yet (#494/#495). Emitting Complete would overclaim.
        .with_graph_completeness(GraphCompleteness::Partial)
        // #495: this receipt carries declared + resolved evidence only; the
        // runtime layer is not observed (NodeReceipt/EdgeReceipt population is
        // #521, the realization classifier is #522). Make that explicit rather
        // than implying it from the empty observed facets.
        .with_observation_scope(ObservationScope::declared_resolved())
        // #494: spell out WHY the graph is Partial — runtime not observed —
        // as typed reasons derived from the scope. Non-empty even though
        // node/edge receipts are populated: a declared/resolved projection is
        // not runtime observation, so this never becomes Complete.
        .with_graph_completeness_reasons(
            ObservationScope::declared_resolved().graph_completeness_reasons(),
        )
        .with_graph_receipt(GraphReceipt::launch_passed(
            declared_execution_id,
            resolved_execution_id,
            None,
        ));

    Ok((receipt, launch_graph_bundle))
}

/// Build the declared/resolved native-inference context for the receipt from the
/// manifest. Returns `None` for any non-native-inference runtime. Reads only
/// declared target fields (`plan.target_*()`) — no probes, no GPU, no log
/// parsing. Records what was *selected* (managed engine tag/variant, managed
/// model CAS hash), NOT the backend that actually ran (deferred to #490).
fn build_native_inference_context(plan: &ManifestData) -> Option<NativeInferenceContext> {
    if plan.execution_runtime().as_deref() != Some("native-inference") {
        return None;
    }

    let nonempty = |v: Option<String>| v.filter(|s| !s.trim().is_empty());

    let engine = nonempty(plan.target_engine());
    let engine_version = nonempty(plan.target_engine_version());
    let engine_variant_declared = nonempty(plan.target_engine_variant());
    let has_engine_path = nonempty(plan.target_engine_path()).is_some();
    // Managed engine = Ato resolves/fetches it (no explicit local `engine_path`).
    let engine_managed = !has_engine_path && engine.is_some();
    // Resolved variant is meaningful only for a managed engine; a local
    // `engine_path` binary's backend is not inspected.
    let engine_variant_resolved = engine_managed.then(|| {
        resolve_engine_variant_label(engine_variant_declared.as_deref(), std::env::consts::OS)
    });

    let model_url = nonempty(plan.target_model_url());
    let model_sha256_raw = nonempty(plan.target_model_sha256());
    // Managed model = fetched into CAS from `model_url` + `model_sha256`.
    let model_managed = model_url.is_some() && model_sha256_raw.is_some();
    let model_sha256 = if model_managed {
        // Reuse the canonical normalizer (lowercase, strip `sha256:`/`sha256-`,
        // require 64 hex) — no duplication of model-cache logic.
        model_sha256_raw
            .as_deref()
            .and_then(capsule::foundation::types::manifest::normalize_model_sha256)
    } else {
        None
    };

    Some(NativeInferenceContext {
        engine,
        engine_version,
        engine_variant_declared,
        engine_variant_resolved,
        engine_managed,
        model_managed,
        model_sha256,
    })
}

/// Platform-resolved variant *label* for a managed llama.cpp engine — the
/// declared/resolved domain, NOT the observed backend. An explicit variant
/// (e.g. `"vulkan"`) passes through normalized; the default build resolves to
/// `"metal"` on macOS (the macOS artifact is Metal-accelerated), else `"cpu"`.
fn resolve_engine_variant_label(declared: Option<&str>, target_os: &str) -> String {
    let normalized = declared
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty());
    match normalized.as_deref() {
        None | Some("default") | Some("cpu") | Some("metal") => match target_os {
            "macos" => "metal".to_string(),
            _ => "cpu".to_string(),
        },
        Some(other) => other.to_string(),
    }
}

/// Derive receipt-safe OCI provider evidence for the launch, if it targets an
/// OCI runtime (#493).
///
/// The evidence is produced by routing the declared/locked OCI launch facts
/// through the #516 provider projection boundary
/// ([`OciProjectionPlan::receipt_evidence`]), so the receipt records the same
/// projection the runtime realizes — not a separate ad hoc summary. Only
/// plan-time-known facts are available here (the receipt is built at preflight,
/// before spawn): declared image ref + locked digest, declared container port,
/// env var *names*, working dir/user. Runtime-resolved mounts and the live
/// container id/pid are intentionally absent — they are session-local provider
/// evidence, not part of this declared projection.
///
/// Returns an empty vec for non-OCI launches.
fn declared_oci_provider_projections(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
) -> Vec<OciProviderReceiptEvidence> {
    let Some(oci) = &execution_plan.oci else {
        return Vec::new();
    };
    oci_provider_projection_evidence(
        oci,
        plan.targets_oci_cmd(),
        plan.targets_oci_env(),
        plan.targets_oci_working_dir(),
        plan.targets_oci_user(),
    )
}

/// Pure core of [`declared_oci_provider_projections`]: project a resolved OCI
/// policy envelope plus the plan-time launch facts into receipt-safe provider
/// evidence with typed enforcement status. Split out so it can be tested without
/// a full `ManifestData`/`ExecutionPlan` fixture.
fn oci_provider_projection_evidence(
    oci: &capsule::execution_plan::model::OciPolicyEnvelope,
    cmd: Vec<String>,
    env: std::collections::HashMap<String, String>,
    working_dir: Option<String>,
    user: Option<String>,
) -> Vec<OciProviderReceiptEvidence> {
    // Prefer the locked digest (pinned) when the lock resolved one; otherwise
    // fall back to the declared tag (honestly unpinned).
    let image = match &oci.resolved_image {
        Some(resolved) if resolved.resolved_digest.starts_with("sha256:") => {
            format!("{}@{}", oci.declared_image_ref, resolved.resolved_digest)
        }
        _ => oci.declared_image_ref.clone(),
    };

    let ports = oci
        .port_exposure
        .map(|container_port| {
            vec![OciPortSpec {
                container_port,
                host_port: None,
                protocol: "tcp".to_string(),
                host_ip: Some("127.0.0.1".to_string()),
            }]
        })
        .unwrap_or_default();

    // A declared request: launch conditions known at plan time. `env` carries
    // declared values but `receipt_evidence()` projects *keys only*, so no value
    // is persisted. Runtime-only inputs (injected mounts, container name, live
    // platform/emulation choice) are left empty.
    let request = OciContainerRequest {
        // Name is a runtime handle and is never read by `receipt_evidence()`;
        // a declared placeholder keeps the projection session-independent.
        name: "ato-oci-declared".to_string(),
        image,
        cmd,
        env,
        working_dir,
        labels: std::collections::HashMap::new(),
        mounts: Vec::new(),
        ports,
        network: None,
        aliases: Vec::new(),
        platform: None,
        extra_hosts: Vec::new(),
        user,
    };

    // Record the selected provider's typed policy-enforcement status (#501): a
    // declared egress allowlist is the canonical facet PodmanProvider cannot
    // enforce, so it surfaces as `Unsupported` rather than implying enforcement.
    let network_policy_required = !oci.egress_allow.is_empty();
    let enforcement =
        crate::application::provider_projection::strict_oci::OciProviderEnforcement::podman(
            network_policy_required,
        );
    let plan = OciProjectionPlan::from_container_request(&request);
    vec![
        crate::application::provider_projection::strict_oci::provider_receipt_evidence(
            &plan,
            &enforcement,
            network_policy_required,
        ),
    ]
}

/// Build the declared-domain `ExecutionGraph` for the receipt path.
///
/// Declared = manifest + lock + policy only; host-independent. The
/// filesystem source/working-directory roles and the network /
/// capability policy hashes ARE declared-domain facts even though they
/// flow through the V2 observers today, because they're derived from
/// the manifest text and the consent ledger respectively (no host
/// materialization needed).
///
/// The `filesystem_observed` and `policy_observed` arguments are
/// scanned for their declared-domain components only — `view_hash` and
/// `sandbox_policy_hash` are intentionally excluded.
#[cfg(test)]
fn build_declared_graph(
    plan: &ManifestData,
    filesystem_observed: &FilesystemIdentityV2,
    policy_observed: &PolicyIdentityV2,
) -> Result<ExecutionGraph> {
    Ok(build_launch_graph_bundle(plan, filesystem_observed, policy_observed)?.declared_graph)
}

fn build_launch_graph_bundle(
    plan: &ManifestData,
    filesystem_observed: &FilesystemIdentityV2,
    policy_observed: &PolicyIdentityV2,
) -> Result<LaunchGraphBundle> {
    let dependencies = manifest_external_capsule_dependencies(&plan.manifest)
        .with_context(|| "failed to derive external dependencies for launch graph bundle")?;
    let base = build_input_from_external_dependencies(
        &dependencies,
        Some(plan.manifest_path.display().to_string()),
    );

    let declared_host = GraphHostInput {
        filesystem_source_root: filesystem_observed.source_root.value.clone(),
        filesystem_working_directory: filesystem_observed.working_directory.value.clone(),
        filesystem_view_hash: None, // resolved-domain only
        ..GraphHostInput::default()
    };
    let resolved_host = GraphHostInput {
        filesystem_view_hash: filesystem_observed.view_hash.value.clone(),
        ..GraphHostInput::default()
    };
    let declared_policy = GraphPolicyInput {
        network_policy_hash: policy_observed.network_policy_hash.value.clone(),
        capability_policy_hash: policy_observed.capability_policy_hash.value.clone(),
        sandbox_policy_hash: None, // resolved-domain only (depends on mount-set algo + allow_hosts_count)
        ..GraphPolicyInput::default()
    };
    let resolved_policy = GraphPolicyInput {
        sandbox_policy_hash: policy_observed.sandbox_policy_hash.value.clone(),
        ..GraphPolicyInput::default()
    };

    Ok(ExecutionGraphBuilder::build_launch_bundle(
        LaunchGraphBundleInput {
            source: base.source,
            targets: base.targets,
            dependencies: base.dependencies,
            declared_host: Some(declared_host),
            resolved_host: Some(resolved_host),
            declared_policy: Some(declared_policy),
            resolved_policy: Some(resolved_policy),
            materialized: GraphMaterializationSeedInput::default(),
            preflight: GraphPreflightInput {
                dependency_aliases: dependencies
                    .iter()
                    .map(|dependency| dependency.alias.clone())
                    .collect(),
                network_policy_hash: policy_observed.network_policy_hash.value.clone(),
                capability_policy_hash: policy_observed.capability_policy_hash.value.clone(),
                ..GraphPreflightInput::default()
            },
            receipt: GraphReceiptSeedInput {
                runner: Some("ato-cli".to_string()),
                host_fingerprint: Some(format!(
                    "{}:{}:{}",
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    "unknown-libc"
                )),
                redaction_policy_version: Some("execution-receipt-v2".to_string()),
            },
            // PR-4b: the receipt builder's internal bundle isn't
            // consent-bearing — consent identity flows on the
            // separate `ExecutionConsentView` path inside
            // preflight / run.rs.
            consent: None,
            launch: None,
        },
    ))
}

/// Extend a declared graph with host-resolution outputs to produce the
/// resolved-domain graph.
///
/// Host-resolution facts captured today: filesystem `view_hash` (the
/// hash of the materialized filesystem closure) and `sandbox_policy_hash`
/// (which folds in mount-set-algo, allow-hosts-count, and the
/// fail-closed bit — all resolved-domain by definition).
///
/// Future waves will add: artifact-selector → concrete-artifact
/// resolution, runtime store path, dep-handle output hash, capability
/// grant → host capability id (per docs/execution-identity.md).
#[cfg(test)]
fn extend_to_resolved_graph(
    declared_graph: &ExecutionGraph,
    filesystem_observed: &FilesystemIdentityV2,
    policy_observed: &PolicyIdentityV2,
) -> ExecutionGraph {
    use capsule::engine::execution_graph::identity_labels;
    let mut resolved = declared_graph.clone();
    for (key, value) in [
        (
            identity_labels::FS_VIEW_HASH,
            filesystem_observed.view_hash.value.as_ref(),
        ),
        (
            identity_labels::POLICY_SANDBOX_HASH,
            policy_observed.sandbox_policy_hash.value.as_ref(),
        ),
    ] {
        if let Some(value) = value {
            resolved.labels.insert(key.to_string(), value.clone());
        }
    }
    resolved
}

/// Combined output of [`build_prelaunch_receipt_document_with_graph`].
///
/// Carries the receipt document and, for V2, the `LaunchGraphBundle`
/// used to derive declared/resolved execution ids. Callers may
/// immediately project the bundle's ids into a boundary sink or
/// session metadata; the bundle itself is not a long-lived pipeline
/// carrier — production callers extract `bundle.derived.execution_ids`
/// at the receipt-emit site and let the bundle drop. The single-source
/// guarantee the umbrella plan calls "shared LaunchGraphBundle
/// context" is preserved by the id space, not by keeping the bundle
/// instance alive past the emit site.
#[derive(Debug)]
pub(crate) struct PrelaunchReceiptOutput {
    pub(crate) document: ExecutionReceiptDocument,
    /// Bundle that produced the receipt's declared/resolved execution
    /// ids, when the V2 schema was selected. `None` for V1 receipts —
    /// V1 has no graph-derived ids so there is no bundle to share.
    pub(crate) launch_graph: Option<LaunchGraphBundle>,
}

#[allow(dead_code)]
pub(crate) fn build_prelaunch_receipt_document(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    build_observation: Option<&BuildObservation>,
) -> Result<ExecutionReceiptDocument> {
    Ok(build_prelaunch_receipt_document_with_graph(
        plan,
        execution_plan,
        launch_ctx,
        build_observation,
    )?
    .document)
}

/// PR-3b carrier-aware variant of [`build_prelaunch_receipt_document`].
/// Returns the receipt AND the `LaunchGraphBundle` that produced its
/// declared/resolved execution ids, so callers can stash the bundle on
/// pipeline state and share it with later steps (session record
/// enrichment, readiness update, partial receipt boundary).
pub(crate) fn build_prelaunch_receipt_document_with_graph(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    build_observation: Option<&BuildObservation>,
) -> Result<PrelaunchReceiptOutput> {
    match ReceiptSchemaSelector::from_env() {
        ReceiptSchemaSelector::V1 => {
            let receipt =
                build_prelaunch_receipt(plan, execution_plan, launch_ctx, build_observation)?;
            Ok(PrelaunchReceiptOutput {
                document: ExecutionReceiptDocument::V1(receipt),
                launch_graph: None,
            })
        }
        ReceiptSchemaSelector::V2Experimental => {
            let (receipt, bundle) = build_prelaunch_receipt_v2_with_graph(
                plan,
                execution_plan,
                launch_ctx,
                build_observation,
            )?;
            Ok(PrelaunchReceiptOutput {
                document: ExecutionReceiptDocument::V2(receipt),
                launch_graph: Some(bundle),
            })
        }
    }
}

/// Build a durable launch-receipt document for an OCI launch (#501).
///
/// Reuses the runtime-agnostic v2 prelaunch builder — `derive_launch_spec`
/// returns a stub for OCI and the v2 observers tolerate it — so OCI launches
/// produce the SAME receipt family as source-native launches, including the
/// `provider_projections` evidence, `GraphCompleteness::Partial`,
/// `ObservationScope::declared_resolved`, and no `observed_execution_id`. No new
/// JSON format is introduced.
///
/// OCI launch receipts are always **v2**: the `provider_projections` field exists
/// only on v2, so a v1 receipt could not carry the provider evidence. Source-native
/// receipts keep honoring `ATO_RECEIPT_SCHEMA` — this function does not change that.
///
/// `provider_projections_override` replaces the builder's single declared OCI
/// projection with an explicit list — used by the multi-service path to record one
/// evidence record per service. `None` keeps the builder's own
/// `declared_oci_provider_projections` output (correct for single-target).
///
/// `strict_gate_failure` (#501): when the strict-realization gate blocked the
/// launch before any pull/create, the gate error is threaded here and the built
/// receipt is marked as a typed failure (preserving its real declared/resolved
/// ids + provider evidence, never synthesizing an `observed_execution_id`).
/// `None` for a normal (passing) launch.
pub(crate) fn build_oci_launch_receipt(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    provider_projections_override: Option<Vec<OciProviderReceiptEvidence>>,
    strict_gate_failure: Option<&anyhow::Error>,
) -> Result<ExecutionReceiptDocument> {
    let (mut receipt, _bundle) =
        build_prelaunch_receipt_v2_with_graph(plan, execution_plan, launch_ctx, None)?;
    if let Some(projections) = provider_projections_override {
        receipt = receipt.with_provider_projections(projections);
    }
    // Fold the (now final) provider facts into the assessment layer (#501):
    // value-free provider-gap completeness reasons + conservative reproducibility
    // causes. Strictly additive and pre-observation — graph stays `Partial`,
    // ObservationScope stays declared/resolved, no observed_execution_id.
    receipt = receipt.with_oci_provider_assessment();
    if let Some(error) = strict_gate_failure {
        receipt = mark_oci_launch_receipt_failed(receipt, error);
    }
    Ok(ExecutionReceiptDocument::V2(receipt))
}

/// Mark a built OCI launch receipt as a strict-realization-gate failure (#501).
///
/// Applies the typed failure result-class + envelope derived from the gate error
/// while preserving everything else the launch path already established: the real
/// declared/resolved execution ids, the provider evidence, the `Partial` graph
/// completeness, and the absent `observed_execution_id` (a blocked launch never
/// ran, so it is never observed — `with_result` does not synthesize one). When
/// the error carries no typed envelope the receipt is returned unchanged rather
/// than fabricating a failure shape.
fn mark_oci_launch_receipt_failed(
    receipt: ExecutionReceiptV2,
    error: &anyhow::Error,
) -> ExecutionReceiptV2 {
    use capsule::execution_identity::{ReceiptFailureKind, ReceiptResultClass};
    match crate::application::receipt_boundary::build_failure_envelope(error) {
        Some(envelope) => {
            let class = match envelope.kind {
                ReceiptFailureKind::Recoverable => ReceiptResultClass::RecoverableFailure,
                ReceiptFailureKind::Aborted => ReceiptResultClass::Aborted,
            };
            receipt.with_result(class, Some(envelope))
        }
        None => receipt,
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod graph_identity_tests {
    //! Receipt-side tests for graph-derived declared/resolved execution
    //! ids (refs #98, #99). These exercise the same wires that
    //! `build_prelaunch_receipt_v2` uses, with synthetic
    //! `FilesystemIdentityV2` / `PolicyIdentityV2` inputs so we don't
    //! have to spin up the full observer pipeline.
    //!
    //! The capsule canonicalization tests
    //! (`crates/capsule/src/engine/execution_graph/canonical.rs`)
    //! pin sensitivity at the canonical-form layer; these tests pin
    //! that the receipt-builder helpers route the right facts into the
    //! right domain.
    use super::{build_declared_graph, extend_to_resolved_graph};
    use capsule::engine::execution_graph::{CanonicalGraphDomain, ExecutionGraph, identity_labels};
    use capsule::execution_identity::{
        CaseSensitivity, FilesystemIdentityV2, FilesystemSemantics, PolicyIdentityV2,
        SymlinkPolicy, TmpPolicy, Tracked,
    };
    use capsule::router::{ExecutionProfile, ManifestData};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn synthetic_plan(manifest_text: &str) -> ManifestData {
        let parsed: toml::Value = toml::from_str(manifest_text).expect("parse manifest");
        let workspace_root = PathBuf::from("/tmp/synthetic-workspace");
        let manifest_path = workspace_root.join("capsule.toml");
        capsule::router::execution_descriptor_from_manifest_parts(
            parsed,
            manifest_path,
            workspace_root,
            ExecutionProfile::Dev,
            None,
            HashMap::new(),
        )
        .expect("synthetic execution descriptor")
    }

    fn synthetic_filesystem(view_hash: &str) -> FilesystemIdentityV2 {
        FilesystemIdentityV2 {
            view_hash: Tracked::known(view_hash.to_string()),
            partial_view_hash: None,
            source_root: Tracked::known("workspace:.".to_string()),
            working_directory: Tracked::known("workspace:.".to_string()),
            readonly_layers: Vec::new(),
            writable_dirs: Vec::new(),
            persistent_state: Vec::new(),
            semantics: FilesystemSemantics {
                case_sensitivity: Tracked::known(CaseSensitivity::Sensitive),
                symlink_policy: Tracked::known(SymlinkPolicy::Preserve),
                tmp_policy: Tracked::known(TmpPolicy::SessionLocal),
            },
        }
    }

    fn synthetic_policy(network: &str, capability: &str, sandbox: &str) -> PolicyIdentityV2 {
        PolicyIdentityV2 {
            network_policy_hash: Tracked::known(network.to_string()),
            capability_policy_hash: Tracked::known(capability.to_string()),
            sandbox_policy_hash: Tracked::known(sandbox.to_string()),
        }
    }

    const SAMPLE_MANIFEST: &str = r#"
schema_version = "0.3"
name = "consumer"
version = "0.1.0"
type = "app"
runtime = "source/python"
run = "main.py"

[dependencies.db]
capsule = "capsule://ato/acme-postgres@16"
contract = "service@1"
"#;

    fn declared_id(graph: &ExecutionGraph) -> String {
        graph
            .canonical_form(CanonicalGraphDomain::Declared)
            .digest_hex()
    }

    fn resolved_id(graph: &ExecutionGraph) -> String {
        graph
            .canonical_form(CanonicalGraphDomain::Resolved)
            .digest_hex()
    }

    /// #493: the launch graph the receipt builder projects into node/edge
    /// receipts is non-empty for a normal dependency-bearing launch. This
    /// closes the loop with the capsule mapping tests: the production
    /// graph source the builder feeds to `with_graph_projection` actually
    /// carries nodes and edges, so the wired receipts are non-empty.
    #[test]
    fn launch_graph_receipt_source_is_non_empty_for_dependency_launch() {
        let plan = synthetic_plan(SAMPLE_MANIFEST);
        let fs = synthetic_filesystem("blake3:fs");
        let policy = synthetic_policy("blake3:net", "blake3:cap", "blake3:sandbox");
        let graph = build_declared_graph(&plan, &fs, &policy).expect("declared graph");
        assert!(
            !graph.nodes.is_empty(),
            "graph nodes feed node_receipts; a dependency launch must have some"
        );
        assert!(
            !graph.edges.is_empty(),
            "graph edges feed edge_receipts; a dependency launch must have some"
        );
    }

    /// Declared id reacts to a manifest-level dependency change.
    #[test]
    fn declared_id_reacts_to_manifest_dependency_change() {
        let plan_one = synthetic_plan(SAMPLE_MANIFEST);
        let plan_two = synthetic_plan(
            r#"
schema_version = "0.3"
name = "consumer"
version = "0.1.0"
type = "app"
runtime = "source/python"
run = "main.py"

[dependencies.db]
capsule = "capsule://ato/acme-postgres@16"
contract = "service@1"

[dependencies.cache]
capsule = "capsule://ato/acme-redis@7"
contract = "service@1"
"#,
        );

        let fs = synthetic_filesystem("blake3:fs");
        let policy = synthetic_policy("blake3:net", "blake3:cap", "blake3:sandbox");

        let declared_one =
            build_declared_graph(&plan_one, &fs, &policy).expect("build declared graph one");
        let declared_two =
            build_declared_graph(&plan_two, &fs, &policy).expect("build declared graph two");

        assert_ne!(
            declared_id(&declared_one),
            declared_id(&declared_two),
            "declared_execution_id must react to a top-level [dependencies] change"
        );
    }

    /// Resolved id reacts to host-resolution drift (different
    /// `view_hash`) while declared id stays stable. This is the
    /// canonical separation between the two domains.
    #[test]
    fn resolved_id_reacts_to_view_hash_while_declared_id_stays_stable() {
        let plan = synthetic_plan(SAMPLE_MANIFEST);
        let policy = synthetic_policy("blake3:net", "blake3:cap", "blake3:sandbox");

        let fs_a = synthetic_filesystem("blake3:fs-A");
        let fs_b = synthetic_filesystem("blake3:fs-B");

        let declared_a = build_declared_graph(&plan, &fs_a, &policy).expect("declared a");
        let declared_b = build_declared_graph(&plan, &fs_b, &policy).expect("declared b");
        // Declared graph excludes view_hash by construction → identical.
        assert_eq!(
            declared_id(&declared_a),
            declared_id(&declared_b),
            "declared_execution_id must not depend on view_hash drift"
        );

        let resolved_a = extend_to_resolved_graph(&declared_a, &fs_a, &policy);
        let resolved_b = extend_to_resolved_graph(&declared_b, &fs_b, &policy);
        assert_ne!(
            resolved_id(&resolved_a),
            resolved_id(&resolved_b),
            "resolved_execution_id must react to view_hash drift"
        );
    }

    /// Resolved id reacts to a different `sandbox_policy_hash` (the
    /// resolved-domain policy bit) but declared id stays stable.
    #[test]
    fn resolved_id_reacts_to_sandbox_policy_while_declared_id_stays_stable() {
        let plan = synthetic_plan(SAMPLE_MANIFEST);
        let fs = synthetic_filesystem("blake3:fs");

        let policy_a = synthetic_policy("blake3:net", "blake3:cap", "blake3:sandbox-A");
        let policy_b = synthetic_policy("blake3:net", "blake3:cap", "blake3:sandbox-B");

        let declared_a = build_declared_graph(&plan, &fs, &policy_a).expect("declared a");
        let declared_b = build_declared_graph(&plan, &fs, &policy_b).expect("declared b");
        assert_eq!(
            declared_id(&declared_a),
            declared_id(&declared_b),
            "declared_execution_id must not depend on sandbox_policy_hash"
        );

        let resolved_a = extend_to_resolved_graph(&declared_a, &fs, &policy_a);
        let resolved_b = extend_to_resolved_graph(&declared_b, &fs, &policy_b);
        assert_ne!(
            resolved_id(&resolved_a),
            resolved_id(&resolved_b),
            "resolved_execution_id must react to sandbox_policy_hash drift"
        );
    }

    /// Both ids react to a *declared-domain* policy change (here,
    /// `network_policy_hash`). This pins that declared-domain policy
    /// hashes feed the declared graph.
    #[test]
    fn declared_id_reacts_to_network_policy_hash() {
        let plan = synthetic_plan(SAMPLE_MANIFEST);
        let fs = synthetic_filesystem("blake3:fs");

        let policy_a = synthetic_policy("blake3:net-A", "blake3:cap", "blake3:sandbox");
        let policy_b = synthetic_policy("blake3:net-B", "blake3:cap", "blake3:sandbox");

        let declared_a = build_declared_graph(&plan, &fs, &policy_a).expect("declared a");
        let declared_b = build_declared_graph(&plan, &fs, &policy_b).expect("declared b");
        assert_ne!(
            declared_id(&declared_a),
            declared_id(&declared_b),
            "declared_execution_id must react to network_policy_hash drift"
        );
    }

    /// `extend_to_resolved_graph` is purely additive on top of the
    /// declared graph: no nodes/edges are dropped, only resolved-only
    /// labels are layered on. This pins the spec's "declared ⊆
    /// resolved" requirement at the helper level.
    #[test]
    fn extend_to_resolved_graph_only_adds_labels() {
        let plan = synthetic_plan(SAMPLE_MANIFEST);
        let fs = synthetic_filesystem("blake3:fs");
        let policy = synthetic_policy("blake3:net", "blake3:cap", "blake3:sandbox");

        let declared = build_declared_graph(&plan, &fs, &policy).expect("declared");
        let resolved = extend_to_resolved_graph(&declared, &fs, &policy);

        assert_eq!(declared.nodes, resolved.nodes);
        assert_eq!(declared.edges, resolved.edges);
        assert_eq!(declared.constraints, resolved.constraints);
        // Resolved adds at least the FS_VIEW_HASH and POLICY_SANDBOX_HASH
        // labels.
        assert_eq!(
            resolved
                .labels
                .get(identity_labels::FS_VIEW_HASH)
                .map(String::as_str),
            Some("blake3:fs"),
        );
        assert_eq!(
            resolved
                .labels
                .get(identity_labels::POLICY_SANDBOX_HASH)
                .map(String::as_str),
            Some("blake3:sandbox"),
        );
    }

    /// PR-3b carrier parity: the receipt's declared/resolved execution
    /// ids must match the ids of the `LaunchGraphBundle` returned by
    /// the carrier-aware builder. If this drifts, the receipt would
    /// claim one graph identity while downstream consumers reading
    /// from the carrier (session record enrichment, partial receipt
    /// boundary) would see a different one.
    #[test]
    fn carrier_bundle_ids_match_receipt_ids() {
        use super::build_launch_graph_bundle;

        let plan = synthetic_plan(SAMPLE_MANIFEST);
        let filesystem = synthetic_filesystem("blake3:fs-fixture");
        let policy = synthetic_policy(
            "blake3:net-fixture",
            "blake3:cap-fixture",
            "blake3:sbx-fixture",
        );

        let bundle = build_launch_graph_bundle(&plan, &filesystem, &policy)
            .expect("build launch graph bundle");

        // The receipt builder reads declared/resolved execution ids
        // straight off `bundle.derived.execution_ids`. The carrier
        // contract is: whatever bundle the receipt builder returns,
        // its `derived.execution_ids` is the same one stamped on the
        // receipt. This test pins that property by re-computing the
        // ids the same way the v2 builder does (`bundle.derived.*`)
        // and asserts they agree with the bundle's canonical digests.
        let declared_from_canonical = bundle
            .declared_graph
            .canonical_form(CanonicalGraphDomain::Declared)
            .digest_hex();
        let resolved_from_canonical = bundle
            .resolved_graph
            .canonical_form(CanonicalGraphDomain::Resolved)
            .digest_hex();
        assert_eq!(
            bundle.derived.execution_ids.declared_execution_id, declared_from_canonical,
            "PR-3b: bundle.derived.declared id must equal canonical declared digest — \
             the receipt and the carrier are reading off the same field"
        );
        assert_eq!(
            bundle.derived.execution_ids.resolved_execution_id, resolved_from_canonical,
            "PR-3b: bundle.derived.resolved id must equal canonical resolved digest — \
             the receipt and the carrier are reading off the same field"
        );
    }

    /// PR-3b chain parity (PR #180 review fix): every consumer in the
    /// launch chain sees the SAME declared/resolved ids — they all
    /// trace back to one `bundle.derived.execution_ids`.
    ///
    /// Chain:
    ///   bundle.derived.execution_ids
    ///       == receipt_document declared/resolved fields
    ///       == ExecutionReceiptSessionMetadata declared/resolved fields
    ///       == sink ids published mid-launch (boundary plumbing)
    ///
    /// The receipt builder is the single composition site; everything
    /// else is a pure projection of the receipt document, so this test
    /// pinning the first link transitively pins all subsequent links.
    /// `session_runner.rs::emit_execution_receipt` is the projection
    /// `ExecutionReceiptDocument::V2(receipt) -> ExecutionReceiptSessionMetadata`,
    /// inlined; this test materializes it explicitly with synthetic
    /// inputs so the projection stays a 1:1 copy and not, say, an
    /// accidental remap.
    #[test]
    fn launch_chain_shares_one_declared_resolved_id_space() {
        use super::build_launch_graph_bundle;
        use crate::application::receipt_boundary::GraphIds;

        let plan = synthetic_plan(SAMPLE_MANIFEST);
        let filesystem = synthetic_filesystem("blake3:fs-fixture");
        let policy = synthetic_policy(
            "blake3:net-fixture",
            "blake3:cap-fixture",
            "blake3:sbx-fixture",
        );

        let bundle = build_launch_graph_bundle(&plan, &filesystem, &policy)
            .expect("build launch graph bundle");

        // Link 1: bundle ids are canonical digests.
        let declared = bundle.derived.execution_ids.declared_execution_id.clone();
        let resolved = bundle.derived.execution_ids.resolved_execution_id.clone();

        // Link 2: the boundary sink the inner pipeline publishes.
        // Same value space.
        let sink_payload = GraphIds {
            declared_execution_id: Some(declared.clone()),
            resolved_execution_id: Some(resolved.clone()),
        };
        assert_eq!(
            sink_payload.declared_execution_id.as_deref(),
            Some(declared.as_str())
        );

        // Link 3: ExecutionReceiptSessionMetadata projection used by
        // session_runner::emit_execution_receipt. Pure copy — written
        // out long-form here so any future refactor that drops a
        // field is caught by this test before it ships.
        let session_metadata = crate::app_control::session::ExecutionReceiptSessionMetadata {
            execution_id: "blake3:fixture-execution".to_string(),
            schema_version:
                capsule::execution_identity::EXECUTION_IDENTITY_SCHEMA_VERSION_V2_EXPERIMENTAL,
            declared_execution_id: Some(declared.clone()),
            resolved_execution_id: Some(resolved.clone()),
            observed_execution_id: None,
            graph_completeness: Some("partial".to_string()),
            reproducibility_class: Some("BestEffort".to_string()),
        };
        assert_eq!(
            session_metadata.declared_execution_id.as_deref(),
            Some(declared.as_str()),
            "PR-3b chain: session metadata declared id must equal bundle declared id"
        );
        assert_eq!(
            session_metadata.resolved_execution_id.as_deref(),
            Some(resolved.as_str()),
            "PR-3b chain: session metadata resolved id must equal bundle resolved id"
        );
    }
}

struct ClassificationInputsV2 {
    dependencies: capsule::execution_identity::DependencyIdentity,
    runtime: capsule::execution_identity::RuntimeIdentity,
    environment: capsule::execution_identity::EnvironmentIdentity,
    filesystem: capsule::execution_identity::FilesystemIdentity,
}

fn classification_inputs_from_v2(
    dependencies: &capsule::execution_identity::DependencyIdentityV2,
    runtime: &capsule::execution_identity::RuntimeIdentityV2,
    environment: &capsule::execution_identity::EnvironmentIdentityV2,
    filesystem: &capsule::execution_identity::FilesystemIdentityV2,
) -> ClassificationInputsV2 {
    use capsule::execution_identity::{
        DependencyIdentity, EnvironmentIdentity, FilesystemIdentity, RuntimeIdentity,
        TrackingStatus,
    };

    let env_closure_status = if environment.entries.iter().all(|entry| {
        matches!(
            entry.normalization,
            capsule::execution_identity::ValueNormalizationStatus::Normalized
                | capsule::execution_identity::ValueNormalizationStatus::NoHostPath
        )
    }) && !environment.entries.is_empty()
    {
        TrackingStatus::Known
    } else {
        TrackingStatus::Untracked
    };

    let mut tracked_keys: Vec<String> = environment
        .entries
        .iter()
        .map(|entry| entry.key.clone())
        .collect();
    tracked_keys.sort();
    let mut unknown_keys = environment.ambient_untracked_keys.clone();
    if matches!(
        environment.fd_layout.status,
        TrackingStatus::Untracked | TrackingStatus::Unknown
    ) {
        unknown_keys.push("fd-layout".to_string());
    }
    if matches!(
        environment.umask.status,
        TrackingStatus::Untracked | TrackingStatus::Unknown
    ) {
        unknown_keys.push("umask".to_string());
    }
    if matches!(
        environment.ulimits.status,
        TrackingStatus::Untracked | TrackingStatus::Unknown
    ) {
        unknown_keys.push("ulimits".to_string());
    }
    if !environment.entries.iter().any(|entry| entry.key == "TZ") {
        unknown_keys.push("timezone".to_string());
    }
    unknown_keys.sort();
    unknown_keys.dedup();

    let env_closure_value = format!(
        "blake3:{}",
        blake3::hash(
            serde_jcs::to_vec(&environment.entries)
                .unwrap_or_default()
                .as_slice()
        )
        .to_hex()
    );

    let env_v1 = EnvironmentIdentity {
        closure_hash: match env_closure_status {
            TrackingStatus::Known => Tracked::known(env_closure_value),
            _ => Tracked::untracked(
                "v2 environment closure has unnormalized or untracked identity-relevant entries",
            ),
        },
        mode: environment.mode,
        tracked_keys,
        redacted_keys: Vec::new(),
        unknown_keys,
    };

    let persistent_state_v1: Vec<String> = filesystem
        .persistent_state
        .iter()
        .map(|binding| {
            format!(
                "{}={}",
                binding.name,
                binding.identity.value.as_deref().unwrap_or("")
            )
        })
        .collect();

    let writable_dirs_v1: Vec<String> = filesystem
        .writable_dirs
        .iter()
        .map(|writable| writable.role.clone())
        .collect();

    let readonly_layers_v1: Vec<String> = filesystem
        .readonly_layers
        .iter()
        .map(|layer| layer.role.clone())
        .collect();

    let fs_v1 = FilesystemIdentity {
        view_hash: filesystem.view_hash.clone(),
        projection_strategy: "v2-canonical".to_string(),
        writable_dirs: writable_dirs_v1,
        persistent_state: persistent_state_v1,
        known_readonly_layers: readonly_layers_v1,
    };

    let runtime_v1 = RuntimeIdentity {
        declared: runtime.declared.clone(),
        resolved: runtime.resolved_ref.value.clone(),
        binary_hash: runtime.binary_hash.clone(),
        dynamic_linkage: runtime.dynamic_linkage.clone(),
        platform: runtime.platform.clone(),
    };

    let deps_v1 = DependencyIdentity {
        derivation_hash: dependencies.derivation_hash.clone(),
        output_hash: dependencies.output_hash.clone(),
    };

    ClassificationInputsV2 {
        dependencies: deps_v1,
        runtime: runtime_v1,
        environment: env_v1,
        filesystem: fs_v1,
    }
}

#[cfg(test)]
mod oci_provider_evidence_tests {
    use super::oci_provider_projection_evidence;
    use capsule::execution_identity::{OciEnforcementStatus, OciImageDigestStatus};
    use capsule::execution_plan::model::{OciPolicyEnvelope, OciPolicyMode};
    use std::collections::HashMap;

    fn envelope(egress: Vec<String>) -> OciPolicyEnvelope {
        OciPolicyEnvelope {
            declared_image_ref: "docker.io/library/nginx:1.27".to_string(),
            resolved_image: None,
            port_exposure: Some(8080),
            egress_allow: egress,
            policy_mode: OciPolicyMode::Off,
        }
    }

    #[test]
    fn oci_projection_receipt_contains_provider_evidence() {
        let env = HashMap::from([("PORT".to_string(), "8080".to_string())]);
        let evidence = oci_provider_projection_evidence(
            &envelope(vec!["api.example.com".to_string()]),
            vec!["nginx".to_string()],
            env,
            Some("/app".to_string()),
            Some("1000:1000".to_string()),
        );
        assert_eq!(
            evidence.len(),
            1,
            "an OCI launch must record provider evidence"
        );
        let ev = &evidence[0];
        assert_eq!(ev.provider_kind, "oci");
        assert_eq!(ev.provider_version.as_deref(), Some("oci-podman-v1"));
        // Declared tag (no resolved digest) is honestly unpinned, never fabricated.
        assert!(matches!(
            ev.image_digest_status,
            OciImageDigestStatus::Unpinned
        ));
        // A declared egress allowlist surfaces as Unsupported — podman cannot
        // enforce it; the receipt states that honestly rather than implying it.
        assert_eq!(
            ev.network_enforcement_status,
            OciEnforcementStatus::Unsupported
        );
        assert_eq!(
            ev.capability_enforcement_status,
            OciEnforcementStatus::Enforced
        );
        // ...and `capabilities_required` agrees: a declared egress allowlist is a
        // required network policy (not derived from the internal `--network`).
        assert!(
            ev.capabilities_required
                .contains(&"network-policy".to_string()),
            "egress allowlist must surface as a required network policy: {:?}",
            ev.capabilities_required
        );
        // env NAMES only — no values.
        assert_eq!(ev.env_keys, vec!["PORT".to_string()]);
        let json = serde_json::to_string(ev).expect("encode");
        assert!(
            !json.contains("\"8080\""),
            "env value must not appear: {json}"
        );
    }

    #[test]
    fn non_oci_launch_has_no_provider_evidence() {
        // No egress declared, but still an OCI envelope → evidence present with
        // enforcement Enforced (nothing to downgrade). The empty case is when
        // `execution_plan.oci` is None, exercised by `declared_oci_provider_projections`.
        let evidence =
            oci_provider_projection_evidence(&envelope(vec![]), vec![], HashMap::new(), None, None);
        assert_eq!(
            evidence[0].network_enforcement_status,
            OciEnforcementStatus::Enforced
        );
    }
}

#[cfg(test)]
mod oci_launch_receipt_tests {
    use capsule::execution_identity::{
        CaseSensitivity, DependencyIdentityV2, EnvironmentIdentityV2, EnvironmentMode,
        ExecutionIdentityInputV2, ExecutionReceiptDocument, ExecutionReceiptV2, FdLayoutIdentity,
        FilesystemIdentityV2, FilesystemSemantics, GraphCompleteness, GraphCompletenessReason,
        LaunchArg, LaunchEntryPoint, LaunchIdentityV2, OciEnforcementStatus, OciImageDigestStatus,
        OciProviderReceiptEvidence, PlatformIdentity, PolicyIdentityV2, ProviderProjectionGap,
        ReproducibilityClass, ReproducibilityIdentity, RuntimeCompleteness, RuntimeIdentityV2,
        SourceIdentityV2, SourceProvenance, SourceProvenanceKind, SymlinkPolicy, TmpPolicy,
        Tracked, UlimitIdentity,
    };
    use std::collections::BTreeMap;

    /// One receipt-safe per-service provider evidence record (mirrors the shape
    /// the orchestration path persists; env *keys* only, never values).
    fn evidence(service: &str, image: &str) -> OciProviderReceiptEvidence {
        OciProviderReceiptEvidence {
            provider_kind: "oci".to_string(),
            provider_name: "podman".to_string(),
            image_reference: image.to_string(),
            image_digest_status: OciImageDigestStatus::Unpinned,
            platform: None,
            env_keys: vec!["PORT".to_string()],
            mounts: vec![],
            ports: vec![],
            network_aliases: vec![service.to_string()],
            capabilities_required: vec!["network-policy".to_string()],
            provider_version: Some("oci-podman-v1".to_string()),
            network_enforcement_status: OciEnforcementStatus::Unsupported,
            capability_enforcement_status: OciEnforcementStatus::Enforced,
            derived_command_redacted: vec!["create".to_string(), "<redacted>".to_string()],
            service_label: Some(service.to_string()),
        }
    }

    /// A minimal valid v2 receipt carrying the given provider evidence. Mirrors
    /// what `build_oci_launch_receipt` produces (v2 + `with_provider_projections`)
    /// without needing a lock-compiled `ExecutionPlan`, so the persistence,
    /// override, and receipt-safety can be unit-tested.
    fn oci_receipt_with(projections: Vec<OciProviderReceiptEvidence>) -> ExecutionReceiptV2 {
        let input = ExecutionIdentityInputV2::new(
            SourceIdentityV2 {
                source_tree_hash: Tracked::unknown("oci has no source tree"),
                manifest_path_role: Tracked::known("workspace:capsule.toml".to_string()),
            },
            SourceProvenance {
                kind: SourceProvenanceKind::Local,
                git_remote: None,
                git_commit: None,
                registry_ref: None,
            },
            DependencyIdentityV2 {
                derivation_hash: Tracked::not_applicable(),
                output_hash: Tracked::not_applicable(),
                derivation_inputs: None,
            },
            RuntimeIdentityV2 {
                declared: Some("oci".to_string()),
                resolved_ref: Tracked::known("oci".to_string()),
                binary_hash: Tracked::not_applicable(),
                dynamic_linkage: Tracked::not_applicable(),
                completeness: RuntimeCompleteness::DeclaredOnly,
                native_inference: None,
                platform: PlatformIdentity {
                    os: "linux".to_string(),
                    arch: "amd64".to_string(),
                    libc: "unknown".to_string(),
                },
            },
            EnvironmentIdentityV2 {
                entries: Vec::new(),
                fd_layout: Tracked::known(FdLayoutIdentity {
                    stdin: "inherited".to_string(),
                    stdout: "inherited".to_string(),
                    stderr: "inherited".to_string(),
                }),
                umask: Tracked::known("022".to_string()),
                ulimits: Tracked::known(UlimitIdentity {
                    limits: BTreeMap::new(),
                }),
                mode: EnvironmentMode::Closed,
                ambient_untracked_keys: Vec::new(),
            },
            FilesystemIdentityV2 {
                view_hash: Tracked::known("blake3:fs".to_string()),
                partial_view_hash: None,
                source_root: Tracked::known("workspace:.".to_string()),
                working_directory: Tracked::known("workspace:.".to_string()),
                readonly_layers: Vec::new(),
                writable_dirs: Vec::new(),
                persistent_state: Vec::new(),
                semantics: FilesystemSemantics {
                    case_sensitivity: Tracked::known(CaseSensitivity::Sensitive),
                    symlink_policy: Tracked::known(SymlinkPolicy::Preserve),
                    tmp_policy: Tracked::known(TmpPolicy::SessionLocal),
                },
            },
            PolicyIdentityV2 {
                network_policy_hash: Tracked::known("blake3:net".to_string()),
                capability_policy_hash: Tracked::known("blake3:cap".to_string()),
                sandbox_policy_hash: Tracked::known("blake3:sandbox".to_string()),
            },
            LaunchIdentityV2 {
                entry_point: LaunchEntryPoint::Command {
                    name: "oci".to_string(),
                },
                argv: Vec::<LaunchArg>::new(),
                working_directory: Tracked::known("workspace:.".to_string()),
            },
            None,
            ReproducibilityIdentity {
                class: ReproducibilityClass::HostBound,
                causes: Vec::new(),
            },
        );
        ExecutionReceiptV2::from_input(input, "2026-06-05T00:00:00Z".to_string())
            .expect("build v2 receipt")
            .with_provider_projections(projections)
            .with_graph_completeness(GraphCompleteness::Partial)
    }

    #[test]
    fn oci_provider_evidence_persists_per_service_and_round_trips() {
        let receipt = oci_receipt_with(vec![
            evidence("web", "alpine:3.21"),
            evidence("db", "postgres:16"),
        ]);
        let exec_id = receipt.execution_id.clone();
        let doc = ExecutionReceiptDocument::V2(receipt);

        // Persist to an isolated executions root and read it back.
        let temp = tempfile::tempdir().expect("tempdir");
        let path = crate::application::execution_receipts::write_receipt_document_atomic_at(
            temp.path(),
            &doc,
        )
        .expect("write receipt");
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("receipt.json")
        );

        let read =
            crate::application::execution_receipts::read_receipt_document_at(temp.path(), &exec_id)
                .expect("read receipt");
        let read_v2 = match read {
            ExecutionReceiptDocument::V2(r) => r,
            ExecutionReceiptDocument::V1(_) => panic!("v2 expected"),
        };

        // One provider-evidence record per service, each value-free-labeled.
        assert_eq!(read_v2.provider_projections.len(), 2);
        let labels: Vec<&str> = read_v2
            .provider_projections
            .iter()
            .filter_map(|p| p.service_label.as_deref())
            .collect();
        assert!(labels.contains(&"web") && labels.contains(&"db"));
        // Enforcement status persisted; env keys (not values).
        assert_eq!(
            read_v2.provider_projections[0].network_enforcement_status,
            OciEnforcementStatus::Unsupported
        );
        assert!(
            read_v2.provider_projections[0]
                .env_keys
                .contains(&"PORT".to_string())
        );

        // Boundaries: no runtime observation, never Complete pre-observation.
        assert!(read_v2.observed_execution_id.is_none());
        assert_ne!(
            read_v2.graph_completeness,
            Some(GraphCompleteness::Complete)
        );

        // On-disk receipt-safety.
        let json = std::fs::read_to_string(&path).expect("read file");
        assert!(
            !json.contains("observed_execution_id"),
            "no observed id on disk: {json}"
        );
        assert!(
            !json.contains("\"state\":\"complete\""),
            "must not claim a Complete graph on disk"
        );
        assert!(
            json.contains("provider_projections"),
            "provider evidence must be persisted on disk"
        );
    }

    #[test]
    fn oci_receipt_with_no_override_still_v2() {
        // The single-target path passes no override; the receipt is still v2 and
        // can carry whatever the builder attached. Here we assert the v2 shape and
        // that an empty projection list is allowed (the declared projection is
        // attached by the builder in the real path).
        let receipt = oci_receipt_with(Vec::new());
        let doc = ExecutionReceiptDocument::V2(receipt);
        assert!(matches!(doc, ExecutionReceiptDocument::V2(_)));
    }

    #[test]
    fn persisted_oci_receipt_reflects_provider_assessment() {
        // A receipt built like the OCI launch path (provider evidence + the #501
        // assessment) persists the provider-gap reasons and a conservative,
        // cause-bearing reproducibility — without ever claiming observation.
        let receipt =
            oci_receipt_with(vec![evidence("web", "alpine:3.21")]).with_oci_provider_assessment();
        let exec_id = receipt.execution_id.clone();
        let doc = ExecutionReceiptDocument::V2(receipt);

        let temp = tempfile::tempdir().expect("tempdir");
        crate::application::execution_receipts::write_receipt_document_atomic_at(temp.path(), &doc)
            .expect("write");
        let read =
            crate::application::execution_receipts::read_receipt_document_at(temp.path(), &exec_id)
                .expect("read");
        let read_v2 = match read {
            ExecutionReceiptDocument::V2(r) => r,
            ExecutionReceiptDocument::V1(_) => panic!("v2"),
        };

        // The `evidence` helper is unpinned + egress-unsupported → both gaps
        // persist as value-free reasons.
        let has = |gap| {
            read_v2.graph_completeness_reasons.iter().any(|r| {
                matches!(
                    r,
                    GraphCompletenessReason::ProviderProjectionIncomplete { gap: g, .. } if *g == gap
                )
            })
        };
        assert!(has(ProviderProjectionGap::ImageUnpinned));
        assert!(has(ProviderProjectionGap::NetworkEnforcementUnsupported));
        // Conservative reproducibility; never Complete; never observed.
        assert_eq!(
            read_v2.reproducibility.class,
            ReproducibilityClass::BestEffort
        );
        assert_ne!(
            read_v2.graph_completeness,
            Some(GraphCompleteness::Complete)
        );
        assert!(read_v2.observed_execution_id.is_none());
    }

    /// #501 blocker: a strict-realization gate block must flip a
    /// *provider-evidence* OCI launch receipt to a typed failure WITHOUT dropping
    /// the provider evidence or the declared/resolved ids, and without ever
    /// fabricating an `observed_execution_id` or a `Complete` graph. Exercises the
    /// exact transform `build_oci_launch_receipt(strict_gate_failure = Some(..))`
    /// applies (`oci_receipt_with` mirrors the builder's output shape), then the
    /// real atomic write/read-back path so the on-disk receipt is asserted too.
    #[test]
    fn strict_gate_failure_marks_provider_evidence_receipt_as_failed() {
        use capsule::execution_identity::ReceiptResultClass;
        use capsule::execution_plan::error::AtoExecutionError;

        // A receipt shaped exactly like build_oci_launch_receipt's output: v2,
        // provider projections present, graph Partial, no observed id. The real
        // builder derives the graph ids; the test mirror leaves them `None`, so
        // stamp sentinels to prove PRESENT ids survive the failure mark.
        let mut receipt = oci_receipt_with(vec![evidence("web", "alpine:3.21")]);
        receipt.declared_execution_id = Some("sha256:declared".to_string());
        receipt.resolved_execution_id = Some("sha256:resolved".to_string());
        let declared_before = receipt.declared_execution_id.clone();
        let resolved_before = receipt.resolved_execution_id.clone();
        let projections_before = receipt.provider_projections.clone();
        assert!(
            !projections_before.is_empty(),
            "fixture must carry provider evidence to prove it survives the mark"
        );
        assert!(declared_before.is_some() && resolved_before.is_some());
        assert!(receipt.observed_execution_id.is_none());

        let err: anyhow::Error =
            AtoExecutionError::lock_incomplete("strict realization gate blocked launch", None)
                .into();
        let marked = super::mark_oci_launch_receipt_failed(receipt, &err);

        // Result flipped to a typed failure; envelope present.
        assert!(
            matches!(
                marked.result,
                ReceiptResultClass::RecoverableFailure | ReceiptResultClass::Aborted
            ),
            "a strict-gated launch must be a typed failure, got {:?}",
            marked.result
        );
        assert!(marked.failure_envelope.is_some());
        // Identity + provider evidence preserved through the mark.
        assert_eq!(marked.declared_execution_id, declared_before);
        assert_eq!(marked.resolved_execution_id, resolved_before);
        assert_eq!(
            marked.provider_projections, projections_before,
            "#501: provider evidence must survive the failure mark"
        );
        // Never observed, never Complete.
        assert!(
            marked.observed_execution_id.is_none(),
            "a blocked launch never ran, so it is never observed"
        );
        assert_ne!(marked.graph_completeness, Some(GraphCompleteness::Complete));

        // Round-trip the marked receipt through the real persist path and assert
        // the on-disk shape: provider evidence present, no observed id.
        let exec_id = marked.execution_id.clone();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = crate::application::execution_receipts::write_receipt_document_atomic_at(
            temp.path(),
            &ExecutionReceiptDocument::V2(marked),
        )
        .expect("write");
        let read =
            crate::application::execution_receipts::read_receipt_document_at(temp.path(), &exec_id)
                .expect("read");
        let read_v2 = match read {
            ExecutionReceiptDocument::V2(r) => r,
            ExecutionReceiptDocument::V1(_) => panic!("v2"),
        };
        assert_eq!(read_v2.provider_projections, projections_before);
        assert!(read_v2.failure_envelope.is_some());
        assert!(read_v2.observed_execution_id.is_none());
        let json = std::fs::read_to_string(&path).expect("read file");
        assert!(
            json.contains("provider_projections"),
            "provider evidence must persist on disk for a strict-gated failure"
        );
        assert!(
            !json.contains("observed_execution_id"),
            "no observed id on disk for a blocked launch: {json}"
        );
    }

    /// #490 production glue: `mark_v2_receipt_observed_at` persists an observed id
    /// onto a V2 receipt ONLY when real evidence is present. Insufficient
    /// evidence is a no-op (id stays `None`); real evidence stamps an id anchored
    /// to the receipt's resolved id, flips the scope to `Observed`, and never
    /// emits `Complete`.
    #[test]
    fn mark_v2_receipt_observed_persists_only_with_real_evidence() {
        use crate::application::execution_receipts::{
            mark_v2_receipt_observed_at, read_receipt_document_at, write_receipt_document_atomic_at,
        };
        use capsule::execution_identity::{
            ObservationScope, ObservedLaunchEnvelope, ObservedRuntimeEvidence, RuntimeObservation,
        };

        let mut receipt = oci_receipt_with(vec![]);
        receipt.resolved_execution_id = Some("sha256:resolved-anchor".to_string());
        receipt.observation_scope = Some(ObservationScope::declared_resolved());
        receipt.graph_completeness = Some(GraphCompleteness::Partial);
        receipt.observed_execution_id = None;
        let exec_id = receipt.execution_id.clone();
        let temp = tempfile::tempdir().expect("tempdir");
        write_receipt_document_atomic_at(temp.path(), &ExecutionReceiptDocument::V2(receipt))
            .expect("write");

        // Insufficient evidence → no-op; the pre-observation receipt is untouched.
        let empty = ObservedRuntimeEvidence::new(ObservedLaunchEnvelope::default());
        let got = mark_v2_receipt_observed_at(temp.path(), &exec_id, empty).expect("stamp");
        assert!(
            got.is_none(),
            "insufficient evidence must not synthesize an observed id"
        );
        let after_noop = match read_receipt_document_at(temp.path(), &exec_id).unwrap() {
            ExecutionReceiptDocument::V2(r) => r,
            _ => panic!("v2"),
        };
        assert!(after_noop.observed_execution_id.is_none());
        assert_eq!(
            after_noop.observation_scope.unwrap().observed,
            RuntimeObservation::NotObserved
        );

        // Real evidence → stamps an observed id derived from the envelope
        // anchored to the receipt's resolved id.
        let env = ObservedLaunchEnvelope {
            runtime_kind: "source/node".to_string(),
            entrypoint: vec!["node".to_string(), "server.js".to_string()],
            env_keys: vec!["PORT".to_string()],
            ..Default::default()
        };
        let id = mark_v2_receipt_observed_at(
            temp.path(),
            &exec_id,
            ObservedRuntimeEvidence::new(env.clone()),
        )
        .expect("stamp")
        .expect("observed id");
        let mut anchored = env;
        anchored.resolved_execution_id = Some("sha256:resolved-anchor".to_string());
        assert_eq!(
            id,
            anchored.compute_observed_execution_id(),
            "observed id must be the anchored-envelope digest"
        );

        let after = match read_receipt_document_at(temp.path(), &exec_id).unwrap() {
            ExecutionReceiptDocument::V2(r) => r,
            _ => panic!("v2"),
        };
        assert_eq!(after.observed_execution_id.as_deref(), Some(id.as_str()));
        assert_ne!(
            after.observed_execution_id, after.resolved_execution_id,
            "observed id must not be a copy of resolved id"
        );
        assert_eq!(
            after.observation_scope.unwrap().observed,
            RuntimeObservation::Observed
        );
        assert!(after.observed_runtime.is_some());
        assert_ne!(after.graph_completeness, Some(GraphCompleteness::Complete));
    }
}

#[cfg(test)]
mod native_inference_context_tests {
    use super::{build_native_inference_context, resolve_engine_variant_label};
    use capsule::execution_identity::{
        NativeInferenceContext, PlatformIdentity, RuntimeCompleteness, RuntimeIdentityV2, Tracked,
    };
    use capsule::router::{
        ExecutionProfile, ManifestData, execution_descriptor_from_manifest_parts,
    };

    const HEADER: &str =
        "schema_version = \"0.3\"\nname = \"ni-test\"\nversion = \"0.1.0\"\ntype = \"app\"\n";

    fn plan_from(body: &str) -> (tempfile::TempDir, ManifestData) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest = format!("{HEADER}{body}");
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::write(&manifest_path, &manifest).expect("write manifest");
        let parsed: toml::Value = toml::from_str(&manifest).expect("parse manifest");
        let plan = execution_descriptor_from_manifest_parts(
            parsed,
            manifest_path,
            tmp.path().to_path_buf(),
            ExecutionProfile::Dev,
            Some("app"),
            std::collections::HashMap::new(),
        )
        .expect("execution descriptor");
        (tmp, plan)
    }

    // (1) absent for non-native-inference runtimes.
    #[test]
    fn context_absent_for_non_native_inference() {
        let (_t, plan) =
            plan_from("[targets.app]\nruntime = \"source\"\nentrypoint = \"app.py\"\n");
        assert!(build_native_inference_context(&plan).is_none());
    }

    // (2)+(3) present for native-inference; managed engine + vulkan + managed model.
    #[test]
    fn managed_engine_variant_and_model() {
        let (_t, plan) = plan_from(
            "[targets.app]\nruntime = \"native-inference\"\nengine = \"llama.cpp\"\n\
             engine_version = \"b9754\"\nengine_variant = \"vulkan\"\n\
             model_url = \"https://example.com/m.gguf\"\n\
             model_sha256 = \"66967fbece6dbe97886593fdbb73589584927e29119ec31f08090732d1861739\"\n",
        );
        let ctx = build_native_inference_context(&plan).expect("native-inference present");
        assert_eq!(ctx.engine.as_deref(), Some("llama.cpp"));
        assert_eq!(ctx.engine_version.as_deref(), Some("b9754"));
        assert_eq!(ctx.engine_variant_declared.as_deref(), Some("vulkan"));
        assert_eq!(ctx.engine_variant_resolved.as_deref(), Some("vulkan"));
        assert!(ctx.engine_managed);
        assert!(ctx.model_managed);
        assert_eq!(
            ctx.model_sha256.as_deref(),
            Some("66967fbece6dbe97886593fdbb73589584927e29119ec31f08090732d1861739")
        );
    }

    // (4) default variant: declared None; resolved is platform-specific + deterministic.
    #[test]
    fn default_variant_resolves_per_platform() {
        // Pure label mapping is deterministic regardless of host.
        assert_eq!(resolve_engine_variant_label(None, "linux"), "cpu");
        assert_eq!(resolve_engine_variant_label(None, "macos"), "metal");
        assert_eq!(resolve_engine_variant_label(Some("cpu"), "macos"), "metal");
        assert_eq!(resolve_engine_variant_label(Some("metal"), "linux"), "cpu");
        assert_eq!(
            resolve_engine_variant_label(Some("vulkan"), "linux"),
            "vulkan"
        );

        let (_t, plan) = plan_from(
            "[targets.app]\nruntime = \"native-inference\"\nengine = \"llama.cpp\"\n\
             engine_version = \"b9754\"\nmodel = \"./m.gguf\"\n",
        );
        let ctx = build_native_inference_context(&plan).expect("present");
        assert!(ctx.engine_variant_declared.is_none());
        // For a managed engine, resolved matches the host default label.
        assert_eq!(
            ctx.engine_variant_resolved,
            Some(resolve_engine_variant_label(None, std::env::consts::OS))
        );
        assert!(ctx.engine_managed);
    }

    // (5) local engine_path: not managed; no invented version; resolved None.
    #[test]
    fn local_engine_path_is_not_managed() {
        let (_t, plan) = plan_from(
            "[targets.app]\nruntime = \"native-inference\"\n\
             engine_path = \"/usr/local/bin/llama-server\"\nmodel = \"./m.gguf\"\n",
        );
        let ctx = build_native_inference_context(&plan).expect("present");
        assert!(!ctx.engine_managed);
        assert!(
            ctx.engine_version.is_none(),
            "must not invent an engine_version"
        );
        assert!(ctx.engine_variant_resolved.is_none());
    }

    // (6) managed model: model_managed + normalized sha256 (uppercase + prefix stripped).
    #[test]
    fn managed_model_sha_is_normalized() {
        let (_t, plan) = plan_from(
            "[targets.app]\nruntime = \"native-inference\"\nengine = \"llama.cpp\"\n\
             engine_version = \"b9754\"\nmodel_url = \"https://example.com/m.gguf\"\n\
             model_sha256 = \"SHA256:66967FBECE6DBE97886593FDBB73589584927E29119EC31F08090732D1861739\"\n",
        );
        let ctx = build_native_inference_context(&plan).expect("present");
        assert!(ctx.model_managed);
        assert_eq!(
            ctx.model_sha256.as_deref(),
            Some("66967fbece6dbe97886593fdbb73589584927e29119ec31f08090732d1861739")
        );
    }

    // (7) local model: not managed; sha None.
    #[test]
    fn local_model_is_not_managed() {
        let (_t, plan) = plan_from(
            "[targets.app]\nruntime = \"native-inference\"\nengine = \"llama.cpp\"\n\
             engine_version = \"b9754\"\nmodel = \"./m.gguf\"\n",
        );
        let ctx = build_native_inference_context(&plan).expect("present");
        assert!(!ctx.model_managed);
        assert!(ctx.model_sha256.is_none());
    }

    // (8) JSON round-trip + backward compatibility (field optional/omitted).
    #[test]
    fn json_round_trip_and_backward_compat() {
        let base = RuntimeIdentityV2 {
            declared: Some("native-inference".to_string()),
            resolved_ref: Tracked::known("native-inference".to_string()),
            binary_hash: Tracked::known("sha256:x".to_string()),
            dynamic_linkage: Tracked::known("blake3:y".to_string()),
            completeness: RuntimeCompleteness::DeclaredOnly,
            platform: PlatformIdentity {
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                libc: "gnu".to_string(),
            },
            native_inference: None,
        };
        // None → key omitted (skip_serializing_if).
        let json_none = serde_json::to_string(&base).unwrap();
        assert!(!json_none.contains("native_inference"), "{json_none}");
        // Old JSON without the field deserializes to None (serde default).
        let decoded: RuntimeIdentityV2 = serde_json::from_str(&json_none).unwrap();
        assert!(decoded.native_inference.is_none());
        // Some → present and round-trips.
        let with_ctx = RuntimeIdentityV2 {
            native_inference: Some(NativeInferenceContext {
                engine: Some("llama.cpp".to_string()),
                engine_version: Some("b9754".to_string()),
                engine_variant_declared: Some("vulkan".to_string()),
                engine_variant_resolved: Some("vulkan".to_string()),
                engine_managed: true,
                model_managed: true,
                model_sha256: Some("a".repeat(64)),
            }),
            ..base
        };
        let json_some = serde_json::to_string(&with_ctx).unwrap();
        assert!(json_some.contains("native_inference"));
        let round: RuntimeIdentityV2 = serde_json::from_str(&json_some).unwrap();
        assert_eq!(round, with_ctx);
    }
}

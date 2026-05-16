use anyhow::{Context, Result};
#[cfg(test)]
use capsule_core::engine::execution_graph::ExecutionGraph;
use capsule_core::engine::execution_graph::{
    ExecutionGraphBuilder, GraphHostInput, GraphMaterializationSeedInput, GraphPolicyInput,
    GraphPreflightInput, GraphReceiptSeedInput, LaunchGraphBundle, LaunchGraphBundleInput,
};
use capsule_core::execution_identity::{
    ExecutionIdentityInput, ExecutionIdentityInputV2, ExecutionReceipt, ExecutionReceiptDocument,
    ExecutionReceiptV2, ExecutionRunnerIdentity, FilesystemIdentityBuilder, FilesystemIdentityV2,
    GraphCompleteness, GraphReceipt, LaunchIdentity, PolicyIdentity, PolicyIdentityBuilder,
    PolicyIdentityV2, Tracked,
};
use capsule_core::execution_plan::model::ExecutionPlan;
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::lockfile::manifest_external_capsule_dependencies;
use capsule_core::router::ManifestData;
use serde::Serialize;

use crate::application::build_materialization::BuildObservation;
use crate::application::execution_graph_adapter::build_input_from_external_dependencies;
use crate::application::execution_observers_v2::{
    build_local_locator, build_policy_identity_v2, observe_dependencies_v2, observe_environment_v2,
    observe_filesystem_v2, observe_launch_v2, observe_runtime_v2, observe_source_provenance,
    observe_source_v2, ObserverContextV2,
};
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
pub(crate) fn build_prelaunch_receipt_v2(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    build_observation: Option<&BuildObservation>,
) -> Result<ExecutionReceiptV2> {
    Ok(build_prelaunch_receipt_v2_with_graph(
        plan,
        execution_plan,
        launch_ctx,
        build_observation,
    )?
    .0)
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
    let runtime = observe_runtime_v2(execution_plan, &launch_spec, &ctx)?;
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
    let placeholder_reproducibility = capsule_core::execution_identity::ReproducibilityIdentity {
        class: capsule_core::execution_identity::ReproducibilityClass::BestEffort,
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
        .with_graph_completeness(GraphCompleteness::Partial)
        .with_graph_receipt(GraphReceipt::launch_passed(
            declared_execution_id,
            resolved_execution_id,
            None,
        ));

    Ok((receipt, launch_graph_bundle))
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
    use capsule_core::engine::execution_graph::identity_labels;
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
/// Carries both the receipt document AND the `LaunchGraphBundle` used to
/// derive the receipt's declared/resolved execution ids, so downstream
/// consumers (session record enrichment, partial receipt boundary,
/// readiness updates) can share the SAME bundle instance the receipt was
/// built from.
///
/// PR-3b: this is the carrier the umbrella plan calls "shared
/// LaunchGraphBundle context" — instead of letting every consumer
/// re-build the bundle from inputs (cheap but lossy: re-derivation can
/// silently disagree if any input changes shape), we build once at
/// receipt-emit time and surface the bundle alongside the document.
#[derive(Debug)]
pub(crate) struct PrelaunchReceiptOutput {
    pub(crate) document: ExecutionReceiptDocument,
    /// Bundle that produced the receipt's declared/resolved execution
    /// ids, when the V2 schema was selected. `None` for V1 receipts —
    /// V1 has no graph-derived ids so there is no bundle to share.
    pub(crate) launch_graph: Option<LaunchGraphBundle>,
}

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

#[cfg(test)]
mod graph_identity_tests {
    //! Receipt-side tests for graph-derived declared/resolved execution
    //! ids (refs #98, #99). These exercise the same wires that
    //! `build_prelaunch_receipt_v2` uses, with synthetic
    //! `FilesystemIdentityV2` / `PolicyIdentityV2` inputs so we don't
    //! have to spin up the full observer pipeline.
    //!
    //! The capsule-core canonicalization tests
    //! (`crates/capsule-core/src/engine/execution_graph/canonical.rs`)
    //! pin sensitivity at the canonical-form layer; these tests pin
    //! that the receipt-builder helpers route the right facts into the
    //! right domain.
    use super::{build_declared_graph, extend_to_resolved_graph};
    use capsule_core::engine::execution_graph::{
        identity_labels, CanonicalGraphDomain, ExecutionGraph,
    };
    use capsule_core::execution_identity::{
        CaseSensitivity, FilesystemIdentityV2, FilesystemSemantics, PolicyIdentityV2,
        SymlinkPolicy, TmpPolicy, Tracked,
    };
    use capsule_core::router::{ExecutionProfile, ManifestData};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn synthetic_plan(manifest_text: &str) -> ManifestData {
        let parsed: toml::Value = toml::from_str(manifest_text).expect("parse manifest");
        let workspace_root = PathBuf::from("/tmp/synthetic-workspace");
        let manifest_path = workspace_root.join("capsule.toml");
        capsule_core::router::execution_descriptor_from_manifest_parts(
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
            bundle.derived.execution_ids.declared_execution_id,
            declared_from_canonical,
            "PR-3b: bundle.derived.declared id must equal canonical declared digest — \
             the receipt and the carrier are reading off the same field"
        );
        assert_eq!(
            bundle.derived.execution_ids.resolved_execution_id,
            resolved_from_canonical,
            "PR-3b: bundle.derived.resolved id must equal canonical resolved digest — \
             the receipt and the carrier are reading off the same field"
        );
    }
}

struct ClassificationInputsV2 {
    dependencies: capsule_core::execution_identity::DependencyIdentity,
    runtime: capsule_core::execution_identity::RuntimeIdentity,
    environment: capsule_core::execution_identity::EnvironmentIdentity,
    filesystem: capsule_core::execution_identity::FilesystemIdentity,
}

fn classification_inputs_from_v2(
    dependencies: &capsule_core::execution_identity::DependencyIdentityV2,
    runtime: &capsule_core::execution_identity::RuntimeIdentityV2,
    environment: &capsule_core::execution_identity::EnvironmentIdentityV2,
    filesystem: &capsule_core::execution_identity::FilesystemIdentityV2,
) -> ClassificationInputsV2 {
    use capsule_core::execution_identity::{
        DependencyIdentity, EnvironmentIdentity, FilesystemIdentity, RuntimeIdentity,
        TrackingStatus,
    };

    let env_closure_status = if environment.entries.iter().all(|entry| {
        matches!(
            entry.normalization,
            capsule_core::execution_identity::ValueNormalizationStatus::Normalized
                | capsule_core::execution_identity::ValueNormalizationStatus::NoHostPath
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

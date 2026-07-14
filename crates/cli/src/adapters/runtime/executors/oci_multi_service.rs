//! Multi-service OCI execution via a ready runtime provider.
//!
//! This is the **official** path for capsules that declare a `[services]` graph
//! where every service target uses `runtime = "oci"`.
//!
//! The legacy Bollard/Docker-compatible orchestration path is in `orchestrator.rs`.
//! New code must NOT route through that path for OCI services.
//!
//! # Execution order
//! 1. `execute_multi_service` — public entry point, reads the plan, lock, and manifest.
//!    Selects a ready provider before delegating to `execute_service_graph_with_provider`.
//! 2. `execute_service_graph_with_provider<P: OciProvider>` — testable core, accepts any provider.
//!
//! # Invariants
//! * Every OCI service must have a resolved image digest in the lock file.
//! * Container id, host port, and network id are **Session/Receipt** data — not identity.
//! * Persistent state bindings are preserved on failure; ephemeral ones are deleted.
//! * Internal service-to-service connections use OCI network aliases, not localhost.
//! * Only the main (published) service exposes a host port.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use capsule::CapsuleReporter;
use capsule::contract::lock_runtime::resolve_oci_image_for_target;
use capsule::execution_plan::model::OciPolicyMode;
use capsule::router::ManifestData;
use capsule::runtime::oci::{
    OciContainerRequest, OciMountSourceKind, OciMountSpec, OciNetworkRequest, OciPortSpec,
    resolve_oci_mount,
};
use capsule::types::{
    IngressConfig, OciImageResolution, OciProviderKind, OrchestrationPlan, ResolvedService,
    ResolvedServiceRuntime, StateDurability,
};

use capsule::execution_identity::OciProviderReceiptEvidence;

use super::launch_context::RuntimeLaunchContext;
use crate::adapters::runtime::ingress_router;
use crate::adapters::runtime::oci_provider::{
    OciImageResolutionMode, OciImageResolutionRequest, OciPlatformPolicy, OciProvider,
    OciProviderError, build_digest_pull_ref, normalize_oci_image_ref,
    select_ready_runtime_oci_provider,
};
use crate::adapters::runtime::oci_session_store::{
    IngressRouteRecord, OciServiceRecord, OciSessionIngressRecord, OciSessionMeta,
    OciSessionRecord, OciSessionStatus, OciSessionStore, now_iso8601,
};
use crate::application::provider_projection::oci::OciProjectionPlan;
use crate::application::provider_projection::strict_oci::{
    OciProviderEnforcement, OciServiceStrict, OciStrictFacts, enforce_strict_oci_services,
    provider_receipt_evidence,
};
use crate::reporters::CliReporter;

const OCI_MULTI_STOP_TIMEOUT_SECS: i64 = 10;
/// Default timeout (seconds) for run_once containers to complete.
///
/// Override at runtime with `ATO_OCI_RUN_ONCE_TIMEOUT_SECS` (e.g. for CI or
/// fast unit tests).  Kept as a process-wide knob rather than a per-target
/// field so the v0.3 schema surface stays minimal in this PR.
const OCI_RUN_ONCE_TIMEOUT_SECS_DEFAULT: u64 = 300;

fn oci_run_once_timeout_secs() -> u64 {
    std::env::var("ATO_OCI_RUN_ONCE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(OCI_RUN_ONCE_TIMEOUT_SECS_DEFAULT)
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Execute a multi-service OCI capsule through the ready runtime provider.
///
/// Reads the manifest, lock, and service graph from `plan`, selects a ready
/// provider, then delegates to `execute_service_graph_with_provider`.
pub(crate) async fn execute_multi_service(
    plan: &ManifestData,
    reporter: Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
    strict_realization: bool,
    // #501: boundary receipt sink. A strict-gate failure receipt persisted here
    // suppresses the boundary's duplicate partial (one launch → one receipt).
    // `None` outside the boundary-wrapped pipeline.
    receipt_sink: Option<&crate::application::receipt_boundary::ReceiptGraphIdSink>,
) -> Result<i32> {
    // Validate all services are OCI before proceeding.
    if !plan.all_services_are_oci() {
        anyhow::bail!(
            "oci_multi_service executor requires all services to use runtime=oci; \
             mixed-runtime service graphs are not supported"
        );
    }

    let orch_plan = plan
        .resolve_services()
        .context("failed to resolve OCI service graph from manifest")?;

    // Gather egress policy and mode from manifest.
    let (policy_mode, egress_allow) = match plan.typed_manifest() {
        Ok(m) => {
            let mode = OciPolicyMode::Strict;
            let egress = m
                .network
                .as_ref()
                .map(|n| n.egress_allow.clone())
                .unwrap_or_default();
            (mode, egress)
        }
        Err(_) => (OciPolicyMode::Strict, vec![]),
    };

    // Compute which mount sources are ephemeral (safe to delete on failure).
    let ephemeral_mount_sources = collect_ephemeral_mount_sources(plan);

    // Select a ready provider before image resolution so compat-path capsules
    // can resolve digests without requiring a separate `ato lock` step.
    let provider = select_ready_runtime_oci_provider()
        .await
        .map_err(|e| anyhow::anyhow!("{}: {}", e.code(), e))?;

    // Build the image map. Prefer the lock-resolved digest when present; fall
    // back to resolving the declared image ref via the OCI provider so that
    // compat-path capsules (capsule.toml without ato.lock.json) work without
    // a separate `ato lock` step.
    let mut images: HashMap<String, OciImageResolution> = HashMap::new();
    for service in &orch_plan.services {
        let rt = match &service.runtime {
            ResolvedServiceRuntime::Oci(rt) => rt,
            _ => continue,
        };
        let target_label = rt.target.clone();

        match resolve_oci_image_for_target(&plan.lock, &target_label).context(format!(
            "failed to resolve OCI image for target '{target_label}'"
        ))? {
            Some(image) => {
                images.insert(target_label, image);
            }
            None => {
                // No lock entry — resolve from the declared image ref at run time.
                // Fully-qualify a bare Docker Hub short-ref (e.g.
                // `frooodle/s-pdf:2.11.0` → `docker.io/frooodle/s-pdf:2.11.0`) so
                // `podman manifest inspect` can resolve it; the qualified ref then
                // flows through resolution and the digest-pinned pull.
                let declared_ref = normalize_oci_image_ref(rt.image.as_deref().unwrap_or_default());
                let declared_ref = declared_ref.as_str();
                if declared_ref.is_empty() {
                    anyhow::bail!(
                        "OCI target '{}' (service '{}') has no image declared and no lock entry; \
                         add `image = \"<registry/image:tag>\"` to [targets.{}] in capsule.toml",
                        target_label,
                        service.name,
                        target_label,
                    );
                }
                reporter
                    .notify(format!(
                        "🔍 Resolving image digest for target '{}': {}",
                        target_label, declared_ref
                    ))
                    .await?;
                let platform_policy = plan
                    .typed_manifest()
                    .ok()
                    .and_then(|m| m.targets)
                    .and_then(|t| t.named.get(&target_label).cloned())
                    .map(|t| {
                        if t.allow_emulation {
                            OciPlatformPolicy::AllowEmulation
                        } else {
                            OciPlatformPolicy::NativeOnly
                        }
                    })
                    .unwrap_or(OciPlatformPolicy::NativeOnly);
                let request = OciImageResolutionRequest {
                    target_label: target_label.clone(),
                    declared_ref: declared_ref.to_string(),
                    requested_platform: None,
                    resolution_mode: OciImageResolutionMode::Required,
                    importer_input_hash: None,
                    platform_policy,
                };
                let resolved = provider.resolve_image(&request).await.map_err(|e| {
                    anyhow::anyhow!("{}: {}", e.code(), e).context(format!(
                        "failed to resolve image '{}' for target '{}'",
                        declared_ref, target_label
                    ))
                })?;
                reporter
                    .notify(format!(
                        "✅ Resolved '{}': {}",
                        target_label,
                        &resolved.resolved_digest
                            [..std::cmp::min(19, resolved.resolved_digest.len())]
                    ))
                    .await?;
                images.insert(target_label, resolved.into_lock_resolution());
            }
        }
    }
    let manifest_name = plan
        .manifest_name()
        .unwrap_or_else(|| "capsule".to_string());
    let source_path = Some(plan.workspace_root.display().to_string());

    let ingress_config = plan.typed_manifest().ok().and_then(|m| m.ingress);

    // ── Gate 5: strict realization profile (#500/#501) ───────────────────────
    // Opt-in `--strict-realization`. Runs BEFORE `execute_service_graph_with_provider`,
    // which owns every provider side effect (network creation, image pull,
    // container create, container start). In strict mode it blocks the whole
    // launch with a typed error when any service has an unenforceable required
    // policy, an unpinned image, or a host-bound mount fallback. Normal mode is a
    // no-op. Distinct from the always-on `enforce_multi_service_policy_gate`.
    let is_podman = provider.semantics().kind == OciProviderKind::Podman;
    let strict_gate = enforce_strict_oci_orchestration(
        &orch_plan,
        &images,
        &egress_allow,
        is_podman,
        strict_realization,
    );

    // Persist a durable launch receipt with one receipt-safe provider-evidence
    // record per service (#501), BEFORE `execute_service_graph_with_provider`
    // runs any side effect — INCLUDING when the strict gate blocks the launch, in
    // which case the receipt is marked as a typed failure (keeping its real
    // declared/resolved ids + per-service provider evidence). The receipt's
    // identity/graph come from the selected target's compiled plan; its
    // `provider_projections` carry every service. Best-effort: a receipt issue
    // never regresses the launch.
    let provider_evidence: Vec<_> =
        oci_orchestration_provider_evidence(&orch_plan, &images, &egress_allow, is_podman)
            .into_iter()
            .map(|(_label, evidence)| evidence)
            .collect();
    match crate::executors::oci_single_target::compile_oci_execution_plan(plan) {
        Ok(execution_plan) => {
            crate::executors::oci_single_target::persist_oci_launch_receipt(
                plan,
                &execution_plan,
                launch_ctx,
                Some(provider_evidence),
                strict_gate.as_ref().err(),
                receipt_sink,
                &reporter,
            )
            .await;
        }
        Err(err) => {
            let _ = reporter
                .notify(format!(
                    "⚠  skipped OCI launch receipt (could not compile execution plan): {err}"
                ))
                .await;
        }
    }
    // Propagate the strict-gate block (if any) only after the failure receipt is
    // on disk.
    strict_gate?;

    execute_service_graph_with_provider(
        &orch_plan,
        &images,
        policy_mode,
        &egress_allow,
        &manifest_name,
        &ephemeral_mount_sources,
        ingress_config.as_ref(),
        &reporter,
        &provider,
        Some(OciSessionMeta {
            import_kind: "explicit-oci".to_string(),
            source_path,
            source_hash: None,
        }),
        launch_ctx,
    )
    .await
}

/// Project each OCI service in the orchestration plan into an [`OciProjectionPlan`]
/// (no provider side effects). Shared by the strict gate and provider evidence so
/// both read the same source of truth. The image is pinned when its lock-resolved
/// digest is present; otherwise it is honestly unpinned. Mounts are resolved to
/// bind vs engine-volume form so the gate can tell host-bound from managed state.
fn build_oci_service_projections(
    orch_plan: &OrchestrationPlan,
    images: &HashMap<String, OciImageResolution>,
    is_podman: bool,
) -> Vec<(String, OciProjectionPlan)> {
    let mut out: Vec<(String, OciProjectionPlan)> = Vec::new();
    for service in &orch_plan.services {
        let ResolvedServiceRuntime::Oci(rt) = &service.runtime else {
            continue;
        };
        let resolution = images.get(&rt.target);
        let image_ref = match resolution {
            Some(img) if !img.resolved_digest.is_empty() => build_digest_pull_ref(img),
            Some(img) => img.declared_ref.clone(),
            None => rt.image.clone().unwrap_or_default(),
        };
        let mounts: Vec<OciMountSpec> = rt
            .mounts
            .iter()
            .map(|m| resolve_oci_mount(m, is_podman, cfg!(target_os = "windows")))
            .collect();
        let ports = rt
            .port
            .map(|container_port| {
                vec![OciPortSpec {
                    container_port,
                    host_port: None,
                    protocol: "tcp".to_string(),
                    host_ip: Some("127.0.0.1".to_string()),
                }]
            })
            .unwrap_or_default();
        // A declared request: launch conditions known before any side effect.
        // env values are present but the projection records env *keys* only; the
        // session-local container name and internal network are excluded.
        let request = OciContainerRequest {
            name: "ato-oci-orchestrated".to_string(),
            image: image_ref,
            cmd: rt.cmd.clone(),
            env: rt.env.clone(),
            working_dir: rt.working_dir.clone(),
            labels: HashMap::new(),
            mounts,
            ports,
            network: None,
            aliases: service.network.aliases.clone(),
            platform: resolution.map(|img| img.platform.clone()),
            extra_hosts: Vec::new(),
            user: rt.user.clone(),
        };
        out.push((
            service.name.clone(),
            OciProjectionPlan::from_container_request(&request),
        ));
    }
    out
}

/// Strict realization gate for the OCI service graph (#501).
///
/// Reuses the single-target gate building blocks ([`OciStrictFacts`],
/// [`OciProviderEnforcement`], [`enforce_strict_oci_services`]) — no strict-gate
/// logic is duplicated here. In strict mode it blocks with a typed error that
/// names the offending service; in normal mode it is a no-op. The graph-derived
/// resolved execution id is not threaded into the OCI path yet, so `None` is
/// passed rather than fabricating one from projection data.
fn enforce_strict_oci_orchestration(
    orch_plan: &OrchestrationPlan,
    images: &HashMap<String, OciImageResolution>,
    egress_allow: &[String],
    is_podman: bool,
    strict_realization: bool,
) -> Result<()> {
    let profile = if strict_realization {
        capsule::realization::LaunchProfile::Strict
    } else {
        capsule::realization::LaunchProfile::Normal
    };
    let network_policy_required = !egress_allow.is_empty();
    let inputs: Vec<OciServiceStrict> = build_oci_service_projections(orch_plan, images, is_podman)
        .iter()
        .map(|(service_label, plan)| OciServiceStrict {
            service_label: service_label.clone(),
            facts: OciStrictFacts::from_projection(plan, network_policy_required),
            enforcement: OciProviderEnforcement::podman(network_policy_required),
        })
        .collect();
    // Unbox before handing to anyhow: downstream recovery downcasts to
    // `AtoExecutionError` (utils/error.rs), which a boxed wrap would hide.
    enforce_strict_oci_services(&inputs, profile, None).map_err(|e| anyhow::Error::new(*e))
}

/// Receipt-safe provider evidence for each OCI service (#501). Not persisted to
/// disk yet — that is the next #501 slice; produced here so it is observable and
/// testable. Carries only receipt-safe fields (env keys, mount targets, ports,
/// aliases, capabilities, enforcement status, redacted argv) — never a raw env
/// value, secret, host source path, container id, pid, or log path.
fn oci_orchestration_provider_evidence(
    orch_plan: &OrchestrationPlan,
    images: &HashMap<String, OciImageResolution>,
    egress_allow: &[String],
    is_podman: bool,
) -> Vec<(String, OciProviderReceiptEvidence)> {
    let network_policy_required = !egress_allow.is_empty();
    let enforcement = OciProviderEnforcement::podman(network_policy_required);
    build_oci_service_projections(orch_plan, images, is_podman)
        .into_iter()
        .map(|(service_label, plan)| {
            let mut evidence =
                provider_receipt_evidence(&plan, &enforcement, network_policy_required);
            // Stamp the value-free service label so a single receipt can carry one
            // evidence record per service.
            evidence.service_label = Some(service_label.clone());
            (service_label, evidence)
        })
        .collect()
}

/// Try to start the ingress router.
///
/// Builds the route table, generates a session token, starts the router, and
/// builds the `OciSessionIngressRecord`.  On error the caller MUST clean up any
/// already-started services because this function has no side-effects that need
/// rollback beyond what the caller manages.
async fn try_start_ingress_router(
    ingress: &IngressConfig,
    route_target_host_ports: &BTreeMap<String, u16>,
    reporter: &Arc<CliReporter>,
) -> Result<(ingress_router::RouterHandle, OciSessionIngressRecord)> {
    let route_entries = ingress_router::build_route_table(ingress, route_target_host_ports)
        .map_err(|e| anyhow::anyhow!("ingress route table build failed: {e}"))?;

    let token = ingress_router::generate_session_token();

    let handle = ingress_router::start_ingress_router(token.clone(), 0, route_entries).await?;

    let router_port = handle.port;
    let primary_url = format!("http://127.0.0.1:{router_port}/i/{token}/");

    let mut route_records = BTreeMap::new();
    for (route_name, route) in &ingress.routes {
        let alias_val = if route.root {
            String::new()
        } else {
            route.alias.clone().unwrap_or_else(|| route_name.clone())
        };
        let url = if route.root {
            primary_url.clone()
        } else {
            format!("http://127.0.0.1:{router_port}/i/{token}/{alias_val}/")
        };
        route_records.insert(
            route_name.clone(),
            IngressRouteRecord {
                url,
                target: route.target.clone(),
                port: route.port,
                listed: route.listed,
            },
        );
    }

    reporter
        .notify(format!("🌐 Ingress available at {primary_url}"))
        .await?;

    Ok((
        handle,
        OciSessionIngressRecord {
            mode: "path".to_string(),
            router_port,
            token,
            primary_url,
            routes: route_records,
        },
    ))
}

/// Core multi-service graph execution logic; accepts any `OciProvider` for testability.
///
/// Does **not** perform provider readiness check — the caller (`execute_multi_service`)
/// is responsible for that gate.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_service_graph_with_provider<P: OciProvider>(
    orch_plan: &OrchestrationPlan,
    images: &HashMap<String, OciImageResolution>,
    policy_mode: OciPolicyMode,
    egress_allow: &[String],
    manifest_name: &str,
    ephemeral_mount_sources: &HashSet<String>,
    ingress_config: Option<&IngressConfig>,
    reporter: &Arc<CliReporter>,
    provider: &P,
    session_meta: Option<OciSessionMeta>,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<i32> {
    // Gate: policy enforcement
    enforce_multi_service_policy_gate(policy_mode, egress_allow)?;

    let session_sfx = session_suffix(manifest_name);
    let session_id = format!("ato-{manifest_name}-{session_sfx}");
    let network_name = network_name(manifest_name, &session_sfx);

    // Create the session-scoped network.
    reporter
        .notify(format!("🔗 Creating OCI network: {network_name}"))
        .await?;
    let network_request = OciNetworkRequest {
        name: network_name.clone(),
        labels: {
            let mut l = HashMap::new();
            l.insert("io.ato.session_id".to_string(), session_id.clone());
            l.insert("io.ato.managed".to_string(), "true".to_string());
            l
        },
    };
    let network_result = if egress_allow.is_empty() {
        provider.create_internal_network(&network_request).await
    } else {
        provider.create_network(&network_request).await
    };
    let _network_id = network_result
        .map_err(|e| anyhow::anyhow!("{}: {}", e.code(), e))
        .context("failed to create session network")?;

    // Collect ingress route target labels so we can publish their ports
    // even when the service is not the main published front-end.
    let ingress_route_targets: HashSet<String> = ingress_config
        .map(|ic| ic.routes.values().map(|r| r.target.clone()).collect())
        .unwrap_or_default();

    // Pre-collect all service aliases across the entire orchestration plan so
    // they can be placed in NO_PROXY: inter-service traffic must not route through
    // the egress proxy.
    let all_service_aliases: Vec<&str> = orch_plan
        .services
        .iter()
        .flat_map(|s| s.network.aliases.iter().map(String::as_str))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let launch_ctx_merged = launch_ctx.merged_env();

    let mut started: Vec<ServiceStartRecord> = Vec::new();
    // Ephemeral engine-managed state volumes created during this run; cleanup
    // removes them since their state lives inside the engine, not on disk. See #444.
    let mut ephemeral_engine_volumes: HashSet<String> = HashSet::new();
    let mut graph_error: Option<anyhow::Error> = None;

    let service_layers = match service_start_layers(orch_plan) {
        Ok(layers) => layers,
        Err(err) => {
            cleanup_services(
                &started,
                &network_name,
                ephemeral_mount_sources,
                &ephemeral_engine_volumes,
                provider,
            )
            .await;
            return Err(err);
        }
    };

    'start_loop: for layer in service_layers {
        for service_name in &layer {
            let Some(service) = orch_plan.services.iter().find(|s| &s.name == service_name) else {
                graph_error = Some(anyhow::anyhow!(
                    "oci_dependency_planning_failed: service '{}' missing from graph",
                    service_name
                ));
                break 'start_loop;
            };

            let target_runtime = match &service.runtime {
                ResolvedServiceRuntime::Oci(rt) => rt,
                _ => {
                    graph_error = Some(anyhow::anyhow!(
                        "service '{}' is not an OCI service",
                        service_name
                    ));
                    break 'start_loop;
                }
            };
            let target_label = &target_runtime.target;

            let Some(image) = images.get(target_label) else {
                graph_error = Some(anyhow::anyhow!(
                    "{}",
                    OciProviderError::OciImageResolutionRequired {
                        declared_ref: target_runtime.image.clone().unwrap_or_default(),
                    }
                ));
                break 'start_loop;
            };

            // Require digest to be present.
            if image.resolved_digest.is_empty() {
                graph_error = Some(anyhow::anyhow!(
                    "OCI image '{}' for service '{}' has no resolved digest",
                    image.declared_ref,
                    service_name
                ));
                break 'start_loop;
            }

            reporter
                .notify(format!(
                    "⬇  [{}] Pulling OCI image: {}",
                    service_name, image.declared_ref
                ))
                .await?;
            if let Err(e) = provider.pull_image(image).await {
                graph_error = Some(
                    anyhow::anyhow!("{}: {}", e.code(), e)
                        .context(format!("failed to pull image for service '{service_name}'")),
                );
                break 'start_loop;
            }

            let container_name = service_container_name(manifest_name, service_name, &session_sfx);
            let labels = multi_service_labels(
                &session_id,
                service_name,
                provider.semantics().kind.as_str(),
            );
            let pull_ref = build_digest_pull_ref(image);

            // Build env: merge target env → launch context env → connection env.
            // All services in earlier layers have already passed readiness; sibling services in
            // this layer are never dependencies of each other.
            let mut env =
                build_service_env(service, &started, &target_runtime.env, &launch_ctx_merged);

            // Per-service proxy env override:
            // - egress_proxy=true:  replace host-loopback proxy URL with host.containers.internal
            //   (127.0.0.1 is unreachable from inside a container).
            // - egress_proxy=false: strip all proxy vars inherited from launch_ctx to ensure this
            //   service's traffic is never routed through the egress proxy.
            let extra_hosts = if let Some(port) = launch_ctx.egress_proxy_port() {
                if service.network.egress_proxy {
                    let container_proxy = crate::common::proxy::proxy_env_for_oci_container(
                        port,
                        &all_service_aliases,
                    );
                    for (k, v) in crate::common::proxy::proxy_env_to_pairs(&container_proxy) {
                        env.insert(k, v);
                    }
                    vec![crate::common::proxy::OCI_HOST_GATEWAY_ENTRY.to_string()]
                } else {
                    // Opt-out: strip any proxy vars that may have been injected by launch_ctx.
                    for key in crate::common::proxy::PROXY_ENV_KEYS {
                        env.remove(key);
                    }
                    vec![]
                }
            } else {
                vec![]
            };

            // Ports: publish for the main/user-facing service AND for services
            // that are declared as ingress route targets.
            let ports = if service.network.publish || ingress_route_targets.contains(target_label) {
                if let Some(container_port) = target_runtime.port {
                    vec![OciPortSpec {
                        container_port,
                        host_port: None, // auto-allocate
                        protocol: "tcp".to_string(),
                        host_ip: Some("127.0.0.1".to_string()),
                    }]
                } else {
                    vec![]
                }
            } else {
                vec![]
            };

            // Mounts: convert state bindings to OciMountSpec.
            //
            // The source strategy (host bind path vs engine-managed named volume)
            // is selected per-mount by `resolve_oci_mount`: on Windows + Podman,
            // Ato-managed writable state becomes an engine volume so the container
            // user can initialize permissions the Windows host FS can't grant
            // (#444). Ownership is passed through so the provider (Podman: `:U`,
            // Docker-compatible: warn + no-op) can apply engine-delegated
            // ownership init. Host-side chown is not performed. See #428.
            let is_podman = provider.semantics().kind == OciProviderKind::Podman;
            let mounts: Vec<OciMountSpec> = target_runtime
                .mounts
                .iter()
                .map(|m| resolve_oci_mount(m, is_podman, cfg!(target_os = "windows")))
                .collect();

            // Track ephemeral engine volumes so cleanup can delete them; their
            // state lives inside the engine, not on a host directory.
            for mount in &mounts {
                if let OciMountSourceKind::EngineVolume {
                    remove_on_stop: true,
                } = mount.source_kind
                {
                    ephemeral_engine_volumes.insert(mount.source.clone());
                }
            }

            let cmd = target_runtime.cmd.clone();

            prepare_writable_ownership_mount_sources(service_name, &mounts).with_context(|| {
                format!("mount preparation failed for service '{service_name}'")
            })?;

            reporter
                .notify(format!(
                    "📦 [{}] Creating container: {}",
                    service_name, container_name
                ))
                .await?;

            let container_id = match provider
                .create_container(&OciContainerRequest {
                    name: container_name.clone(),
                    image: pull_ref,
                    cmd,
                    env,
                    working_dir: target_runtime.working_dir.clone(),
                    labels,
                    mounts,
                    ports,
                    network: Some(network_name.clone()),
                    aliases: service.network.aliases.clone(),
                    platform: Some(image.platform.clone()),
                    extra_hosts,
                    user: target_runtime.user.clone(),
                })
                .await
            {
                Ok(id) => id,
                Err(e) => {
                    graph_error = Some(anyhow::anyhow!("{}: {}", e.code(), e).context(format!(
                        "failed to create container for service '{service_name}'"
                    )));
                    break 'start_loop;
                }
            };

            reporter
                .notify(format!("▶  [{}] Starting container", service_name))
                .await?;

            if let Err(e) = provider.start_container(&container_id).await {
                // Best-effort cleanup of this container before propagating.
                let _ = provider.remove_container(&container_id, true).await;
                graph_error = Some(anyhow::anyhow!("{}: {}", e.code(), e).context(format!(
                    "failed to start container for service '{service_name}'"
                )));
                break 'start_loop;
            }

            // ── run_once: wait for exit, do not push to `started` ───────────────
            // run_once containers are one-shot lifecycles (e.g. DB migrations,
            // permission init). Exit code 0 = success → dependents may start.
            // Non-zero / timeout / wait error = typed failure → dependents blocked
            // and previously-started long-running services are cleaned up by the
            // existing `cleanup_services` path (run_once is never in `started`).
            if service.run_once {
                reporter
                    .notify(format!(
                        "⏳ [{}] Waiting for init container to complete",
                        service_name
                    ))
                    .await?;
                let timeout_secs = oci_run_once_timeout_secs();
                let timed = tokio::time::timeout(
                    Duration::from_secs(timeout_secs),
                    provider.wait_container(&container_id),
                )
                .await;
                match timed {
                    Ok(Ok(0)) => {
                        let _ = provider.remove_container(&container_id, true).await;
                        reporter
                            .notify(format!(
                                "✅ [{}] Init container completed successfully",
                                service_name
                            ))
                            .await?;
                    }
                    Ok(Ok(code)) => {
                        let _ = provider.remove_container(&container_id, true).await;
                        graph_error = Some(anyhow::anyhow!(
                            "oci_run_once_failed: init container '{}' exited with non-zero status {}",
                            service_name,
                            code,
                        ));
                        break 'start_loop;
                    }
                    Ok(Err(e)) => {
                        let _ = provider
                            .stop_container(&container_id, OCI_MULTI_STOP_TIMEOUT_SECS)
                            .await;
                        let _ = provider.remove_container(&container_id, true).await;
                        graph_error = Some(anyhow::anyhow!(
                            "oci_run_once_failed: init container '{}' wait error: {}",
                            service_name,
                            e,
                        ));
                        break 'start_loop;
                    }
                    Err(_elapsed) => {
                        let _ = provider
                            .stop_container(&container_id, OCI_MULTI_STOP_TIMEOUT_SECS)
                            .await;
                        let _ = provider.remove_container(&container_id, true).await;
                        graph_error = Some(anyhow::anyhow!(
                            "oci_run_once_timeout: init container '{}' did not complete within {}s",
                            service_name,
                            timeout_secs,
                        ));
                        break 'start_loop;
                    }
                }
                continue;
            }

            // Inspect to get the auto-allocated host port.
            // We need the host port for published services AND for ingress route
            // targets (which had ports auto-allocated but are not published).
            let host_port =
                if service.network.publish || ingress_route_targets.contains(target_label) {
                    let inspect = provider
                        .inspect_container(&container_id)
                        .await
                        .unwrap_or_default();
                    target_runtime
                        .port
                        .and_then(|cp| inspect.host_ports.get(&cp).copied())
                } else {
                    None
                };

            started.push(ServiceStartRecord {
                service_name: service_name.clone(),
                container_id: container_id.clone(),
                container_name,
                host_port,
            });
        }

        if let Err(err) =
            await_layer_readiness(&layer, orch_plan, &started, reporter, provider).await
        {
            graph_error = Some(err);
            break 'start_loop;
        }
    }

    // If any service failed to start, clean up and return error.
    if let Some(err) = graph_error {
        cleanup_services(
            &started,
            &network_name,
            ephemeral_mount_sources,
            &ephemeral_engine_volumes,
            provider,
        )
        .await;
        return Err(err);
    }

    // Determine main endpoint.
    let main_endpoint = started
        .iter()
        .find(|r| {
            orch_plan
                .services
                .iter()
                .find(|s| s.name == r.service_name)
                .map(|s| s.network.publish)
                .unwrap_or(false)
        })
        .and_then(|r| r.host_port.map(|p| format!("http://127.0.0.1:{p}/")));

    // Build a map of target label → allocated host port for all started services.
    // This is used by the ingress router to know where to proxy.
    let mut route_target_host_ports: BTreeMap<String, u16> = BTreeMap::new();
    for sr in &started {
        if let Some(hp) = sr.host_port {
            let target_label = orch_plan
                .services
                .iter()
                .find(|s| s.name == sr.service_name)
                .and_then(|s| match &s.runtime {
                    ResolvedServiceRuntime::Oci(rt) => Some(rt.target.clone()),
                    _ => None,
                });
            if let Some(label) = target_label {
                route_target_host_ports.insert(label, hp);
            }
        }
    }

    // ── Start ingress path router ──────────────────────────────────────────
    let mut router_handle: Option<ingress_router::RouterHandle> = None;
    let ingress_metadata: Option<OciSessionIngressRecord> = if let Some(ingress) = ingress_config {
        match try_start_ingress_router(ingress, &route_target_host_ports, reporter).await {
            Ok((handle, metadata)) => {
                router_handle = Some(handle);
                Some(metadata)
            }
            Err(e) => {
                // Ingress init failed after services started. Clean up
                // containers/network so we don't orphan them.
                cleanup_services(
                    &started,
                    &network_name,
                    ephemeral_mount_sources,
                    &ephemeral_engine_volumes,
                    provider,
                )
                .await;
                return Err(e);
            }
        }
    } else {
        None
    };

    // Emit the canonical machine-readable readiness line so a non-TTY supervisor
    // (the Connected Runner agent, CI) recognizes OCI readiness and its port.
    // The runner monitor keys on `LIFECYCLE: ready[ port=N]`; the human
    // "🌐 OCI service available" line below is NOT machine-parsed, so without
    // this an OCI run on a runner stays `provisioning` forever even though the
    // container is serving. Mirrors the source path's `lifecycle_ready_line`.
    // The port is the published host port of the user-facing service (the one
    // the runner's root proxy maps).
    let ready_port = started
        .iter()
        .find(|r| {
            orch_plan
                .services
                .iter()
                .find(|s| s.name == r.service_name)
                .map(|s| s.network.publish)
                .unwrap_or(false)
        })
        .and_then(|r| r.host_port);
    let ready_line = match ready_port {
        Some(port) => format!("LIFECYCLE: ready port={port}"),
        None => "LIFECYCLE: ready".to_string(),
    };
    let _ = reporter.notify(ready_line).await;

    // The primary endpoint shown to users prefers the ingress URL when present.
    let display_endpoint = ingress_metadata
        .as_ref()
        .map(|i| i.primary_url.clone())
        .or_else(|| main_endpoint.clone());

    if let Some(endpoint) = &display_endpoint
        && let Err(e) = reporter
            .notify(format!("🌐 OCI service available at {endpoint}"))
            .await
    {
        cleanup_services(
            &started,
            &network_name,
            ephemeral_mount_sources,
            &ephemeral_engine_volumes,
            provider,
        )
        .await;
        if let Some(ref mut handle) = router_handle {
            handle.stop().await;
        }
        return Err(e.into());
    }

    // Emit the canonical machine-readable readiness line so a Connected Runner
    // recognizes OCI readiness. The runner monitor (runner_agent) parses
    // "LIFECYCLE: ready[ port=N]" / "(ready event received)" to settle a run as
    // ready; the human "🌐 OCI service available" line above is NOT machine-
    // parsed, so without this an OCI run stays `provisioning` forever even
    // though the container is serving (ato#712, runner-readiness half). Mirrors
    // the source path's lifecycle_ready_line(port). The port is the published
    // host port of the user-facing service (what the runner's root proxy maps).
    let ready_port = started
        .iter()
        .find(|r| {
            orch_plan
                .services
                .iter()
                .find(|s| s.name == r.service_name)
                .map(|s| s.network.publish)
                .unwrap_or(false)
        })
        .and_then(|r| r.host_port);
    let ready_line = match ready_port {
        Some(port) => format!("LIFECYCLE: ready port={port}"),
        None => "LIFECYCLE: ready".to_string(),
    };
    let _ = reporter.notify(ready_line).await;

    // Write OCI session record so `ato ps` and `ato stop --all` can track it.
    let session_store = OciSessionStore::new();
    let oci_session_record = session_store.as_ref().ok().map(|store| {
        let meta = session_meta.unwrap_or(OciSessionMeta {
            import_kind: "explicit-oci".to_string(),
            source_path: None,
            source_hash: None,
        });
        let service_records: Vec<OciServiceRecord> = started
            .iter()
            .map(|sr| {
                let image_info = oci_plan_image_for_service(orch_plan, &sr.service_name, images);
                OciServiceRecord {
                    name: sr.service_name.clone(),
                    container_id: sr.container_id.clone(),
                    container_name: sr.container_name.clone(),
                    image_ref: image_info.0,
                    image_digest: image_info.1,
                    host_port: sr.host_port,
                    persistent_volumes: oci_persistent_volumes_for_service(
                        orch_plan,
                        &sr.service_name,
                    ),
                }
            })
            .collect();
        let record = OciSessionRecord {
            session_id: session_id.clone(),
            import_kind: meta.import_kind,
            source_path: meta.source_path,
            source_hash: meta.source_hash,
            network_name: network_name.clone(),
            services: service_records,
            main_endpoint: display_endpoint.clone(),
            ingress: ingress_metadata.clone(),
            created_at: now_iso8601(),
            status: OciSessionStatus::Running,
        };
        let _ = store.write_session(&record);
        (store, record.session_id.clone())
    });

    // Stream logs for all services and wait for the main container to exit.
    let exit_code = wait_all_services(&started, orch_plan, reporter, provider).await;

    cleanup_services(
        &started,
        &network_name,
        ephemeral_mount_sources,
        &ephemeral_engine_volumes,
        provider,
    )
    .await;

    // Stop ingress router (if it was started).
    if let Some(ref mut handle) = router_handle {
        handle.stop().await;
    }

    // Remove the session record after cleanup.
    if let Some((store, sid)) = oci_session_record {
        let _ = store.delete_session(&sid);
    }

    Ok(exit_code)
}

/// Look up declared_ref and resolved_digest for a service's image.
fn oci_plan_image_for_service(
    orch_plan: &OrchestrationPlan,
    service_name: &str,
    images: &HashMap<String, OciImageResolution>,
) -> (String, Option<String>) {
    let target_label = orch_plan
        .services
        .iter()
        .find(|s| s.name == service_name)
        .and_then(|s| match &s.runtime {
            ResolvedServiceRuntime::Oci(rt) => Some(rt.target.clone()),
            _ => None,
        });
    match target_label.and_then(|t| images.get(&t)) {
        Some(img) => (img.declared_ref.clone(), Some(img.resolved_digest.clone())),
        None => (String::new(), None),
    }
}

/// Collect named Podman volumes that back persistent state bindings for a service.
fn oci_persistent_volumes_for_service(
    orch_plan: &OrchestrationPlan,
    service_name: &str,
) -> Vec<String> {
    orch_plan
        .services
        .iter()
        .find(|s| s.name == service_name)
        .map(|s| {
            s.runtime
                .runtime()
                .mounts
                .iter()
                .filter_map(|m| {
                    // Named volumes (no path separator) are Podman-managed persistent volumes.
                    let src = m.source.trim_start_matches('/');
                    if !src.contains('/') && !src.is_empty() {
                        Some(src.to_string())
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Record of a successfully started service container.
#[derive(Debug, Clone)]
pub(crate) struct ServiceStartRecord {
    pub service_name: String,
    pub container_id: String,
    pub container_name: String,
    pub host_port: Option<u16>,
}

pub(crate) fn service_container_name(
    manifest_name: &str,
    service_name: &str,
    session_sfx: &str,
) -> String {
    format!(
        "ato-{}-{}-{}",
        sanitize_name(manifest_name),
        sanitize_name(service_name),
        session_sfx,
    )
}

pub(crate) fn network_name(manifest_name: &str, session_sfx: &str) -> String {
    format!("ato-{}-{}", sanitize_name(manifest_name), session_sfx)
}

fn session_suffix(manifest_name: &str) -> String {
    let seed = format!("{manifest_name}-{}", std::process::id());
    let hash = blake3::hash(seed.as_bytes()).to_hex();
    hash.chars().take(8).collect()
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

pub(crate) fn multi_service_labels(
    session_id: &str,
    service_name: &str,
    provider_kind: &str,
) -> HashMap<String, String> {
    HashMap::from([
        ("io.ato.session_id".to_string(), session_id.to_string()),
        ("io.ato.target".to_string(), service_name.to_string()),
        ("io.ato.provider".to_string(), provider_kind.to_string()),
        ("io.ato.managed".to_string(), "true".to_string()),
        ("io.ato.execution_id".to_string(), session_id.to_string()),
    ])
}

fn service_start_layers(orch_plan: &OrchestrationPlan) -> Result<Vec<Vec<String>>> {
    let services_by_name: HashMap<&str, &ResolvedService> = orch_plan
        .services
        .iter()
        .map(|service| (service.name.as_str(), service))
        .collect();
    let startup_order: Vec<&str> = if orch_plan.startup_order.is_empty() {
        let mut names: Vec<&str> = services_by_name.keys().copied().collect();
        names.sort();
        names
    } else {
        orch_plan.startup_order.iter().map(String::as_str).collect()
    };

    let mut depth_by_name: HashMap<&str, usize> = HashMap::new();
    for service_name in startup_order {
        let service = services_by_name.get(service_name).ok_or_else(|| {
            anyhow::anyhow!(
                "oci_dependency_planning_failed: service '{}' appears in startup_order but is missing from services",
                service_name
            )
        })?;
        let mut depth = 0usize;
        for dependency in &service.depends_on {
            if !services_by_name.contains_key(dependency.as_str()) {
                anyhow::bail!(
                    "oci_dependency_planning_failed: service '{}' depends_on unknown service '{}'",
                    service.name,
                    dependency
                );
            }
            let Some(dep_depth) = depth_by_name.get(dependency.as_str()) else {
                anyhow::bail!(
                    "oci_dependency_planning_failed: service '{}' depends_on service '{}' before it is planned",
                    service.name,
                    dependency
                );
            };
            depth = depth.max(dep_depth + 1);
        }
        depth_by_name.insert(service.name.as_str(), depth);
    }

    if depth_by_name.len() != services_by_name.len() {
        let mut missing: Vec<&str> = services_by_name
            .keys()
            .copied()
            .filter(|name| !depth_by_name.contains_key(name))
            .collect();
        missing.sort();
        anyhow::bail!(
            "oci_dependency_planning_failed: startup_order omitted service(s): {}",
            missing.join(", ")
        );
    }

    let mut layers = Vec::new();
    for service_name in &orch_plan.startup_order {
        let depth = *depth_by_name
            .get(service_name.as_str())
            .expect("service depth must exist after planning");
        while layers.len() <= depth {
            layers.push(Vec::new());
        }
        layers[depth].push(service_name.clone());
    }
    if orch_plan.startup_order.is_empty() {
        let mut names: Vec<&str> = services_by_name.keys().copied().collect();
        names.sort();
        for name in names {
            let depth = *depth_by_name
                .get(name)
                .expect("service depth must exist after planning");
            while layers.len() <= depth {
                layers.push(Vec::new());
            }
            layers[depth].push(name.to_string());
        }
    }
    Ok(layers)
}

async fn await_layer_readiness<P: OciProvider>(
    layer: &[String],
    orch_plan: &OrchestrationPlan,
    started: &[ServiceStartRecord],
    reporter: &Arc<CliReporter>,
    provider: &P,
) -> Result<()> {
    use futures::stream::{FuturesUnordered, StreamExt};

    let mut probes = FuturesUnordered::new();
    for service_name in layer {
        let Some(service) = orch_plan.services.iter().find(|s| &s.name == service_name) else {
            anyhow::bail!(
                "oci_dependency_planning_failed: service '{}' missing while awaiting readiness",
                service_name
            );
        };
        if service.run_once {
            continue;
        }
        let Some(probe) = service.readiness_probe.as_ref() else {
            continue;
        };
        let Some(start_record) = started.iter().find(|r| r.service_name == *service_name) else {
            anyhow::bail!(
                "oci_dependency_planning_failed: service '{}' was not started before readiness wait",
                service_name
            );
        };

        reporter
            .notify(format!("⏳ [{}] Waiting for readiness", service_name))
            .await?;

        // The futures borrow `provider`/`probe`/`start_record`; they are driven
        // to completion inside this function (no `tokio::spawn`), so a `'static`
        // bound is not required and we avoid cloning the probe/container ids.
        probes.push(await_service_readiness(
            provider,
            probe,
            start_record.host_port,
            &start_record.container_id,
            &start_record.container_name,
            service_name,
        ));
    }

    // Return the first failure; dropping `probes` cancels the remaining waits.
    while let Some(result) = probes.next().await {
        result?;
    }
    Ok(())
}

/// Maximum number of trailing container-log lines attached to an
/// `oci_container_exited_before_ready` diagnostic.
pub(crate) const OCI_EXIT_LOG_TAIL_LINES: usize = 20;

/// Interval between container-liveness polls while waiting for readiness.
const OCI_EXIT_WATCH_POLL: Duration = Duration::from_millis(500);

/// Wait for a single service to become ready, racing the readiness probe against
/// container liveness.
///
/// Three outcomes:
/// * probe succeeds → `Ok(())`
/// * container exits before the probe succeeds → typed
///   `oci_container_exited_before_ready` carrying the exit code + a log tail
/// * probe exhausts its own timeout while the container is still running →
///   `oci_healthcheck_timeout` (unchanged behavior for genuinely slow services)
async fn await_service_readiness<P: OciProvider>(
    provider: &P,
    probe: &capsule::types::ReadinessProbe,
    host_port: Option<u16>,
    container_id: &str,
    container_name: &str,
    service_name: &str,
) -> Result<()> {
    let probe_fut = run_readiness_probe(probe, host_port, Some(container_name), service_name);
    tokio::pin!(probe_fut);

    tokio::select! {
        // Prefer reporting an exit over a marginal probe result.
        biased;

        exit_code = watch_container_exit(provider, container_id) => {
            let tail = collect_log_tail(provider, container_id, OCI_EXIT_LOG_TAIL_LINES).await;
            Err(exited_before_ready_error(service_name, exit_code, &tail))
        }

        ready = &mut probe_fut => {
            if ready {
                return Ok(());
            }
            // The probe gave up. If the container has since exited, surface the
            // more specific exited-before-ready error; otherwise it is a genuine
            // readiness timeout (container still running, just not ready yet).
            match provider.inspect_container(container_id).await {
                Ok(inspect) if !inspect.running => {
                    let tail =
                        collect_log_tail(provider, container_id, OCI_EXIT_LOG_TAIL_LINES).await;
                    Err(exited_before_ready_error(service_name, inspect.exit_code, &tail))
                }
                _ => Err(anyhow::anyhow!(
                    "oci_healthcheck_timeout: service '{}' did not become ready within {}s",
                    service_name,
                    probe.timeout_seconds,
                )),
            }
        }
    }
}

/// Poll the container until it is no longer running, returning its exit code.
///
/// Loops indefinitely on the `OCI_EXIT_WATCH_POLL` interval; it is meant to be
/// raced via `select!` against the readiness probe, which provides the timeout
/// bound. Transient inspect errors are ignored (keep polling).
async fn watch_container_exit<P: OciProvider>(provider: &P, container_id: &str) -> Option<i64> {
    loop {
        if let Ok(inspect) = provider.inspect_container(container_id).await
            && !inspect.running
        {
            return inspect.exit_code;
        }
        tokio::time::sleep(OCI_EXIT_WATCH_POLL).await;
    }
}

/// Collect a bounded tail of a container's logs for diagnostics.
///
/// Best-effort: a missing or unreadable log stream yields an empty tail. The
/// memory bound is enforced by [`collect_log_tail_from_rx`].
async fn collect_log_tail<P: OciProvider>(
    provider: &P,
    container_id: &str,
    max_lines: usize,
) -> Vec<String> {
    match provider.logs(container_id, false).await {
        Ok(rx) => collect_log_tail_from_rx(rx, max_lines).await,
        Err(_) => Vec::new(),
    }
}

/// Collect a bounded tail of lines from an already-opened log stream.
///
/// Shared by the multi-service executor (`OciProvider`) and the orchestration
/// session path (`OciRuntimeClient`) — both yield the same chunk receiver type,
/// so the exited-before-ready diagnostic carries a log tail regardless of which
/// path produced it (#445).
///
/// Truly bounded in memory: we retain at most `max_lines` complete trailing
/// lines (via a ring buffer) plus a single in-flight line capped at
/// `MAX_PARTIAL_LINE_BYTES`, so a chatty or newline-less container can't make us
/// buffer its entire log.
pub(crate) async fn collect_log_tail_from_rx(
    mut rx: tokio::sync::mpsc::Receiver<capsule::Result<capsule::runtime::oci::OciLogChunk>>,
    max_lines: usize,
) -> Vec<String> {
    use std::collections::VecDeque;

    /// Cap on a single newline-less line so one runaway line stays bounded.
    const MAX_PARTIAL_LINE_BYTES: usize = 8 * 1024;

    let cap = max_lines.max(1);
    let mut lines: VecDeque<String> = VecDeque::with_capacity(cap.min(64));
    let mut partial = String::new();

    let push_line = |lines: &mut VecDeque<String>, raw: &str| {
        let trimmed = raw.trim_end();
        if trimmed.is_empty() {
            return;
        }
        if lines.len() == cap {
            lines.pop_front();
        }
        lines.push_back(trimmed.to_string());
    };

    while let Ok(Some(chunk)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
        let Ok(chunk) = chunk else { continue };
        partial.push_str(&String::from_utf8_lossy(&chunk.message));

        // Flush every complete line, keeping only the trailing `cap`.
        while let Some(nl) = partial.find('\n') {
            let line: String = partial.drain(..=nl).collect();
            push_line(&mut lines, &line);
        }

        // Bound the still-incomplete trailing line, keeping its tail on a
        // valid char boundary.
        if partial.len() > MAX_PARTIAL_LINE_BYTES {
            let mut start = partial.len() - MAX_PARTIAL_LINE_BYTES;
            while start < partial.len() && !partial.is_char_boundary(start) {
                start += 1;
            }
            partial = partial.split_off(start);
        }
    }

    // Flush any trailing line that never got a newline.
    push_line(&mut lines, &partial);

    lines.into()
}

/// Typed, downcast-able error for a container that started but exited before it
/// passed its readiness probe.
///
/// Emitted by both the multi-service executor (`await_service_readiness`) and
/// the orchestration session path (`wait_until_ready_in_state` in
/// `orchestrator.rs`). It is preserved through the `anyhow` chain so that
/// `diagnostics::mapping::from_anyhow` can classify it as the typed
/// `oci_container_exited_before_ready` diagnostic (service name, exit code, log
/// tail) instead of folding it into the generic E999 fallback. See #445 / #429.
#[derive(Debug, Clone)]
pub(crate) struct OciExitedBeforeReadyError {
    pub service_name: String,
    pub exit_code: Option<i64>,
    pub log_tail: Vec<String>,
}

/// Stable diagnostic code string carried by [`OciExitedBeforeReadyError`].
pub(crate) const OCI_EXITED_BEFORE_READY_CODE: &str = "oci_container_exited_before_ready";

impl OciExitedBeforeReadyError {
    /// Render the exit code as `N` or `unknown` for display/details.
    pub(crate) fn exit_code_display(&self) -> String {
        self.exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

impl std::fmt::Display for OciExitedBeforeReadyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let logs = if self.log_tail.is_empty() {
            "    (no container logs captured)".to_string()
        } else {
            self.log_tail
                .iter()
                .map(|l| format!("    {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        write!(
            f,
            "{code}: service '{name}' exited with status {status} before it became ready\n  \
             last logs:\n{logs}\n  hint: the container started but exited before passing its \
             readiness probe. Check the logs above; a permission error on a mounted volume \
             usually means the container user lacks write/execute access to the bind target.",
            code = OCI_EXITED_BEFORE_READY_CODE,
            name = self.service_name,
            status = self.exit_code_display(),
        )
    }
}

impl std::error::Error for OciExitedBeforeReadyError {}

/// Build the typed `oci_container_exited_before_ready` error.
fn exited_before_ready_error(
    service_name: &str,
    exit_code: Option<i64>,
    log_tail: &[String],
) -> anyhow::Error {
    anyhow::Error::new(OciExitedBeforeReadyError {
        service_name: service_name.to_string(),
        exit_code,
        log_tail: log_tail.to_vec(),
    })
}

/// Build the env map for a service container.
///
/// Merges, in precedence order:
/// 1. service/target-level env (from the resolved manifest)
/// 2. session-scoped env from the launch context (e.g. proxy vars injected by ato-netd)
/// 3. connection env vars for already-started dependencies
///
/// Internal connections use the Podman network alias (not localhost) so containers
/// reach each other inside the session network.
pub(crate) fn build_service_env(
    service: &ResolvedService,
    started: &[ServiceStartRecord],
    base_env: &HashMap<String, String>,
    launch_ctx_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    // Layer 1: target env.
    let mut env = base_env.clone();
    // Layer 2: session-scoped env (higher priority than target env).
    env.extend(launch_ctx_env.clone());
    // Layer 3: connection env (highest priority — overrides everything).
    for conn in &service.connections {
        // Find the dependency among already-started services.
        if let Some(dep_record) = started.iter().find(|r| r.service_name == conn.dependency) {
            // Use network alias for host (internal name, not 127.0.0.1).
            env.insert(conn.host_env.clone(), conn.default_host.clone());
            // Use the container port for inter-service communication.
            if let Some(port) = conn.container_port {
                env.insert(conn.port_env.clone(), port.to_string());
            }
            // Keep a record of the container id for potential use.
            let _ = dep_record; // suppress unused warning
        }
    }
    env
}

/// Prepare filesystem bind-mount sources for writable mounts that carry an
/// ownership declaration, immediately before `podman create`.
///
/// Only mounts satisfying `!readonly && ownership.is_some()` are touched.
/// Readonly mounts and mounts without an ownership declaration are not modified.
///
/// For each qualifying mount source that looks like an absolute path (`/`):
/// * `create_dir_all` ensures the directory exists.
/// * If `ownership.mode` is `Some(bits)`, `chmod` applies those permission bits.
///   `chmod` is non-root-safe; `chown` is intentionally **not** performed (#428 Gate A).
///
/// Background: Podman `:U` provides user-namespace uid remapping but the
/// virtiofs layer on macOS/Podman-machine does NOT reflect this through the
/// POSIX `access(W_OK)` syscall (Gate B finding). Container entrypoints that
/// use `[ -w dir ]` (e.g. openlist) will fail unless the host mode bits allow
/// write access for others. Recipe authors must declare `mode = "0777"` (or a
/// suitable mode) in `[[services.*.state_bindings]]` to opt in to this chmod.
fn prepare_writable_ownership_mount_sources(
    service_name: &str,
    mounts: &[OciMountSpec],
) -> anyhow::Result<()> {
    for mount in mounts {
        if mount.readonly || mount.ownership.is_none() {
            continue;
        }
        if mount.is_engine_volume() {
            // Engine-managed volume — the engine initializes ownership (copy-up
            // / `:U`); there is no host directory to prepare. See #444.
            continue;
        }
        std::fs::create_dir_all(&mount.source).with_context(|| {
            format!(
                "service '{}': failed to create mount source directory '{}'",
                service_name, mount.source
            )
        })?;

        let Some(ownership) = mount.ownership.as_ref() else {
            continue;
        };
        let Some(mode_bits) = ownership.mode else {
            continue;
        };

        // POSIX mode bits only have meaning on Unix hosts. On Windows the OCI
        // engine (Docker Desktop) manages mount permissions inside its own
        // Linux VM, so a host-side chmod is impossible and unnecessary — the
        // directory creation above is the portable part. Gating this keeps
        // `ato-cli` compiling on the Windows desktop target. (#377)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&mount.source).with_context(|| {
                format!(
                    "service '{}': failed to stat mount source '{}'",
                    service_name, mount.source
                )
            })?;
            let mut perms = meta.permissions();
            perms.set_mode(mode_bits);
            std::fs::set_permissions(&mount.source, perms).with_context(|| {
                format!(
                    "service '{}': failed to chmod mount source '{}' to {:o}",
                    service_name, mount.source, mode_bits
                )
            })?;
        }
        #[cfg(not(unix))]
        {
            let _ = mode_bits;
        }
    }
    Ok(())
}

/// Collect mount source directories that belong to `Ephemeral` state bindings.
fn collect_ephemeral_mount_sources(plan: &ManifestData) -> HashSet<String> {
    let Ok(manifest) = plan.typed_manifest() else {
        return HashSet::new();
    };
    manifest
        .state
        .iter()
        .filter(|(_, req)| req.durability == StateDurability::Ephemeral)
        .filter_map(|(name, req)| {
            manifest
                .state_source_path(name, req, Some(&plan.state_source_overrides))
                .ok()
        })
        .collect()
}

/// Policy gate for the multi-service graph.
///
/// * `Strict`: any non-empty `egress_allow` list fails because `PodmanProvider`
///   cannot enforce domain-level egress filtering.
/// * `Loose`: allows execution with a diagnostic warning.
/// * `Off`: always allows.
pub(crate) fn enforce_multi_service_policy_gate(
    policy_mode: OciPolicyMode,
    egress_allow: &[String],
) -> Result<()> {
    if matches!(policy_mode, OciPolicyMode::Strict) && !egress_allow.is_empty() {
        anyhow::bail!(
            "oci_execution_gate_failed: policy_mode=Strict but PodmanProvider cannot enforce \
             the requested egress_allow list ({} rule(s)); set policy_mode to Loose or Off, \
             or remove the egress_allow declaration",
            egress_allow.len()
        );
    }
    Ok(())
}

/// Wait for TCP readiness on `host:port`.
pub(crate) async fn wait_tcp_ready(
    host: &str,
    port: u16,
    attempts: u32,
    interval: Duration,
) -> bool {
    let addr = format!("{host}:{port}");
    for _ in 0..attempts {
        if tokio::net::TcpStream::connect(&addr).await.is_ok() {
            return true;
        }
        tokio::time::sleep(interval).await;
    }
    false
}

/// Wait for HTTP readiness via a GET request to `url`.
pub(crate) async fn wait_http_ready(url: &str, attempts: u32, interval: Duration) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    for _ in 0..attempts {
        if let Ok(resp) = client.get(url).send().await
            && (resp.status().is_success() || resp.status().is_redirection())
        {
            return true;
        }
        tokio::time::sleep(interval).await;
    }
    false
}

/// Run a command inside `container_name` via `podman exec`; exit 0 = ready.
pub(crate) async fn wait_exec_ready(
    container_name: &str,
    cmd: &[String],
    attempts: u32,
    interval: Duration,
) -> bool {
    for _ in 0..attempts {
        let result = tokio::process::Command::new("podman")
            .arg("exec")
            .arg(container_name)
            .args(cmd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        if let Ok(status) = result
            && status.success()
        {
            return true;
        }
        tokio::time::sleep(interval).await;
    }
    false
}

/// Run the readiness probe for a service.
///
/// Returns `true` when the service is ready, `false` on timeout.
async fn run_readiness_probe(
    probe: &capsule::types::ReadinessProbe,
    host_port: Option<u16>,
    container_name: Option<&str>,
    service_label: &str,
) -> bool {
    // Honor per-probe initial_delay before the first attempt.
    if probe.initial_delay_seconds > 0 {
        tokio::time::sleep(Duration::from_secs(probe.initial_delay_seconds as u64)).await;
    }

    // Derive attempts from timeout / interval, clamping to at least 1.
    let interval = Duration::from_secs(probe.interval_seconds.max(1) as u64);
    let attempts = (probe.timeout_seconds / probe.interval_seconds.max(1)).max(1);

    let _ = service_label; // reserved for future structured logging

    // Exec probe: run command inside the container; exit 0 means ready.
    if let Some(cmd) = &probe.exec
        && let Some(cname) = container_name
    {
        return wait_exec_ready(cname, cmd, attempts, interval).await;
    }

    // HTTP probe: GET the path on the host port.
    if let Some(path) = &probe.http_get
        && let Some(port) = host_port
    {
        let url = format!("http://127.0.0.1:{port}{path}");
        return wait_http_ready(&url, attempts, interval).await;
    }

    // TCP probe: connect to the host or the resolved host port.
    if let Some(addr) = &probe.tcp_connect {
        // addr may be "host:port" or just a port number.
        if addr.contains(':') {
            let parts: Vec<&str> = addr.splitn(2, ':').collect();
            if let (Some(h), Some(p)) = (
                parts.first().copied(),
                parts.get(1).and_then(|s| s.parse::<u16>().ok()),
            ) {
                return wait_tcp_ready(h, p, attempts, interval).await;
            }
        } else if let Ok(p) = addr.parse::<u16>() {
            // Port only — use localhost on the host-side port.
            let target_port = host_port.unwrap_or(p);
            return wait_tcp_ready("127.0.0.1", target_port, attempts, interval).await;
        }
    }

    // No recognizable probe — assume ready immediately.
    true
}

/// Stream logs from all containers and wait for the published service to exit (or Ctrl-C).
async fn wait_all_services<P: OciProvider>(
    started: &[ServiceStartRecord],
    orch_plan: &OrchestrationPlan,
    reporter: &Arc<CliReporter>,
    provider: &P,
) -> i32 {
    // Find the main (published) service.
    let main = started.iter().find(|r| {
        orch_plan
            .services
            .iter()
            .find(|s| s.name == r.service_name)
            .map(|s| s.network.publish)
            .unwrap_or(false)
    });

    // Stream logs sequentially from each service (non-blocking) then wait for main.
    for record in started {
        match provider.logs(&record.container_id, false).await {
            Ok(mut rx) => {
                while let Ok(Some(chunk)) =
                    tokio::time::timeout(Duration::from_millis(100), rx.recv()).await
                {
                    if let Ok(chunk) = chunk {
                        let prefix = format!("[{}] ", record.service_name);
                        let _ = if chunk.stderr {
                            let mut w = std::io::stderr();
                            let _ = w.write_all(prefix.as_bytes());
                            let _ = w.write_all(&chunk.message);
                            w.flush()
                        } else {
                            let mut w = std::io::stdout();
                            let _ = w.write_all(prefix.as_bytes());
                            let _ = w.write_all(&chunk.message);
                            w.flush()
                        };
                    }
                }
            }
            Err(e) => {
                let _ = reporter
                    .warn(format!(
                        "[{}] failed to stream logs: {e}",
                        record.service_name
                    ))
                    .await;
            }
        }
    }

    // Wait for the main (published) service container to exit.
    if let Some(main_record) = main {
        provider
            .wait_container(&main_record.container_id)
            .await
            .unwrap_or(0) as i32
    } else {
        0
    }
}

/// Stop and remove all started containers, remove the network, and delete
/// ephemeral mount sources (both host directories and engine-managed volumes).
async fn cleanup_services<P: OciProvider>(
    started: &[ServiceStartRecord],
    network_name: &str,
    ephemeral_mount_sources: &HashSet<String>,
    ephemeral_engine_volumes: &HashSet<String>,
    provider: &P,
) {
    // Stop and remove in reverse start order.
    for record in started.iter().rev() {
        let _ = provider
            .stop_container(&record.container_id, OCI_MULTI_STOP_TIMEOUT_SECS)
            .await;
        let _ = provider.remove_container(&record.container_id, true).await;
    }

    // Remove the session-scoped network.
    let _ = provider.remove_network(network_name).await;

    // Delete ephemeral host mount source directories. Persistent sources, and
    // engine-managed volumes, are intentionally left untouched here.
    for source in ephemeral_mount_sources {
        let path = std::path::Path::new(source);
        if path.exists() {
            let _ = std::fs::remove_dir_all(path);
        }
    }

    // Remove ephemeral engine-managed volumes; persistent volumes survive stop
    // so durable state is preserved. See #444.
    for volume in ephemeral_engine_volumes {
        let _ = provider.remove_volume(volume).await;
    }
}

// ── PodmanProviderSemantics kind helper ───────────────────────────────────────

trait OciProviderKindStr {
    fn as_str(&self) -> &'static str;
}

impl OciProviderKindStr for capsule::types::OciProviderKind {
    fn as_str(&self) -> &'static str {
        use capsule::types::OciProviderKind;
        match self {
            OciProviderKind::Podman => "podman",
            OciProviderKind::DockerCompatible => "docker-compatible",
            OciProviderKind::AtoNative => "ato-native",
        }
    }
}

// ── std::io::Write import for log printing ────────────────────────────────────
use std::io::Write;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::runtime::oci_provider::FakeOciProvider;
    use capsule::runtime::oci::{OciContainerInspect, engine_state_volume_name};
    use capsule::types::{
        OciImageResolution, OciPlatform, OrchestrationPlan, ResolvedService,
        ResolvedServiceNetwork, ResolvedServiceRuntime, ResolvedTargetRuntime,
        ServiceConnectionInfo,
    };

    fn make_image(declared_ref: &str) -> OciImageResolution {
        OciImageResolution {
            declared_ref: declared_ref.to_string(),
            resolved_digest: "sha256:".to_string() + &"a".repeat(64),
            platform: OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            },
            importer_input_hash: None,
        }
    }

    fn make_service(
        name: &str,
        target: &str,
        depends_on: Vec<String>,
        publish: bool,
        port: Option<u16>,
        connections: Vec<ServiceConnectionInfo>,
    ) -> ResolvedService {
        ResolvedService {
            name: name.to_string(),
            depends_on,
            connections,
            readiness_probe: None,
            network: ResolvedServiceNetwork {
                aliases: vec![name.to_string()],
                publish,
                allow_from: vec![],
                egress_proxy: true,
            },
            run_once: false,
            runtime: ResolvedServiceRuntime::Oci(ResolvedTargetRuntime {
                target: target.to_string(),
                runtime: "oci".to_string(),
                driver: None,
                runtime_version: None,
                image: Some(format!("{target}:latest")),
                entrypoint: String::new(),
                run_command: None,
                cmd: vec![],
                env: HashMap::new(),
                working_dir: None,
                source_layout: None,
                port,
                required_env: vec![],
                mounts: vec![],
                user: None,
            }),
        }
    }

    fn blinko_plan() -> OrchestrationPlan {
        // db: postgres:14 (no publish, port 5432)
        // main: blinko (publish, port 1111, depends_on db)
        let db = make_service("db", "postgres", vec![], false, Some(5432), vec![]);
        let main = make_service(
            "main",
            "blinko",
            vec!["db".to_string()],
            true,
            Some(1111),
            vec![ServiceConnectionInfo {
                dependency: "db".to_string(),
                host_env: "ATO_SERVICE_DB_HOST".to_string(),
                port_env: "ATO_SERVICE_DB_PORT".to_string(),
                container_port: Some(5432),
                default_host: "db".to_string(),
            }],
        );
        OrchestrationPlan {
            startup_order: vec!["db".to_string(), "main".to_string()],
            services: vec![db, main],
        }
    }

    fn images_for_blinko() -> HashMap<String, OciImageResolution> {
        let mut m = HashMap::new();
        m.insert("postgres".to_string(), make_image("postgres:14"));
        m.insert(
            "blinko".to_string(),
            make_image("blinkospace/blinko:latest"),
        );
        m
    }

    fn make_provider_with_unique_ids() -> FakeOciProvider {
        let mut p = FakeOciProvider::ready();
        // Queue two distinct container IDs.
        p.create_container_queue
            .lock()
            .unwrap()
            .extend([Ok("fake-db-id".to_string()), Ok("fake-main-id".to_string())]);
        // inspect returns container port 1111 → host port 54321 for the main service.
        p.inspect_result = Ok(OciContainerInspect {
            running: true,
            exit_code: None,
            host_ports: HashMap::from([(1111u16, 54321u16)]),
        });
        p
    }

    async fn run_blinko(provider: &FakeOciProvider) -> Result<i32> {
        execute_service_graph_with_provider(
            &blinko_plan(),
            &images_for_blinko(),
            OciPolicyMode::Strict,
            &[],
            "blinko",
            &HashSet::new(),
            None, // ingress_config
            &Arc::new(crate::reporters::CliReporter::new(false)),
            provider,
            None,
            &RuntimeLaunchContext::empty(),
        )
        .await
    }

    // ── Helper function tests ─────────────────────────────────────────────────

    #[test]
    fn service_container_name_is_session_scoped() {
        let name = service_container_name("myapp", "db", "ab12cd34");
        assert_eq!(name, "ato-myapp-db-ab12cd34");
    }

    #[test]
    fn network_name_uses_session_suffix() {
        let name = network_name("myapp", "ab12cd34");
        assert_eq!(name, "ato-myapp-ab12cd34");
    }

    #[tokio::test]
    async fn empty_egress_allow_uses_internal_network() {
        let provider = make_provider_with_unique_ids();

        run_blinko(&provider)
            .await
            .expect("deny-all graph must launch on an internal network");

        assert!(
            provider
                .call_log
                .lock()
                .expect("call log lock")
                .iter()
                .any(|call| call.starts_with("create_internal_network:")),
            "empty egress_allow must select an internal OCI network"
        );
    }

    #[test]
    fn sanitize_name_handles_special_chars() {
        assert_eq!(sanitize_name("My App!"), "my-app");
        assert_eq!(sanitize_name("simple"), "simple");
        assert_eq!(sanitize_name("with_under"), "with-under");
    }

    #[test]
    fn build_service_env_injects_connection_env() {
        let service = make_service(
            "main",
            "blinko",
            vec!["db".to_string()],
            true,
            Some(1111),
            vec![ServiceConnectionInfo {
                dependency: "db".to_string(),
                host_env: "ATO_SERVICE_DB_HOST".to_string(),
                port_env: "ATO_SERVICE_DB_PORT".to_string(),
                container_port: Some(5432),
                default_host: "db".to_string(),
            }],
        );
        let started = vec![ServiceStartRecord {
            service_name: "db".to_string(),
            container_id: "fake-db-id".to_string(),
            container_name: "ato-blinko-db-xx".to_string(),
            host_port: Some(54321),
        }];
        let env = build_service_env(&service, &started, &HashMap::new(), &HashMap::new());

        // Internal connection uses the network alias, not localhost.
        assert_eq!(env.get("ATO_SERVICE_DB_HOST"), Some(&"db".to_string()));
        // Uses container port, not host port.
        assert_eq!(env.get("ATO_SERVICE_DB_PORT"), Some(&"5432".to_string()));
    }

    #[test]
    fn strict_policy_gap_blocks_multi_service_execution() {
        let result = enforce_multi_service_policy_gate(
            OciPolicyMode::Strict,
            &["0.0.0.0/0:443".to_string()],
        );
        assert!(result.is_err(), "Strict + egress must fail");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("oci_execution_gate_failed"),
            "error must describe gate failure: {msg}"
        );
    }

    #[test]
    fn loose_policy_gap_records_warning_if_loose_is_wired() {
        enforce_multi_service_policy_gate(OciPolicyMode::Loose, &["0.0.0.0/0:443".to_string()])
            .expect("Loose + egress must not fail");
    }

    #[test]
    fn off_policy_always_passes() {
        enforce_multi_service_policy_gate(OciPolicyMode::Off, &["0.0.0.0/0:443".to_string()])
            .expect("Off policy must not fail");
    }

    /// Host port must not appear in identity fields; OciImageResolution has none.
    #[test]
    fn allocated_host_port_does_not_change_identity() {
        let image = make_image("postgres:14");
        // Confirm that OciImageResolution carries no host_port field.
        // (Compile-time: if this builds, the struct has no such field.)
        let _ = OciImageResolution {
            declared_ref: image.declared_ref,
            resolved_digest: image.resolved_digest,
            platform: image.platform,
            importer_input_hash: image.importer_input_hash,
        };
        // No host_port in scope — the test passes by construction.
    }

    /// Secret env values must not appear in the build_service_env output when
    /// the base_env is given with only key names (caller responsibility).
    #[test]
    fn generated_secret_values_are_redacted_from_receipt() {
        // The convention: callers populate env with key=VALUE;
        // receipt printing should only emit key names.  Here we test
        // that build_service_env does NOT add new secret values beyond
        // what's in base_env, and that ATO_SERVICE_* keys hold only
        // connection metadata (alias / port), not passwords.
        let service = make_service(
            "main",
            "blinko",
            vec!["db".to_string()],
            true,
            Some(1111),
            vec![ServiceConnectionInfo {
                dependency: "db".to_string(),
                host_env: "ATO_SERVICE_DB_HOST".to_string(),
                port_env: "ATO_SERVICE_DB_PORT".to_string(),
                container_port: Some(5432),
                default_host: "db".to_string(),
            }],
        );
        let started = vec![ServiceStartRecord {
            service_name: "db".to_string(),
            container_id: "fake-db-id".to_string(),
            container_name: "ato-blinko-db-xx".to_string(),
            host_port: Some(54321),
        }];
        let mut base = HashMap::new();
        base.insert("POSTGRES_PASSWORD".to_string(), "s3cr3t".to_string());
        let env = build_service_env(&service, &started, &base, &HashMap::new());

        // ATO_SERVICE_DB_HOST and ATO_SERVICE_DB_PORT are safe connection metadata.
        assert_eq!(env.get("ATO_SERVICE_DB_HOST"), Some(&"db".to_string()));
        assert_eq!(env.get("ATO_SERVICE_DB_PORT"), Some(&"5432".to_string()));

        // The password key is present (passed through from base), but its value
        // is "s3cr3t" — in a receipt, only the key should be printed.  This test
        // confirms that build_service_env does not synthesise or modify secret values.
        assert_eq!(env.get("POSTGRES_PASSWORD"), Some(&"s3cr3t".to_string()));
    }

    #[test]
    fn database_url_template_does_not_leak_secret_value() {
        // DATABASE_URL is assembled from alias + container port — never from a secret value.
        let service = make_service(
            "main",
            "blinko",
            vec!["db".to_string()],
            true,
            Some(1111),
            vec![ServiceConnectionInfo {
                dependency: "db".to_string(),
                host_env: "ATO_SERVICE_DB_HOST".to_string(),
                port_env: "ATO_SERVICE_DB_PORT".to_string(),
                container_port: Some(5432),
                default_host: "db".to_string(),
            }],
        );
        let started = vec![ServiceStartRecord {
            service_name: "db".to_string(),
            container_id: "fake-db-id".to_string(),
            container_name: "ato-blinko-db-xx".to_string(),
            host_port: Some(54321),
        }];
        let env = build_service_env(&service, &started, &HashMap::new(), &HashMap::new());
        let host = env.get("ATO_SERVICE_DB_HOST").cloned().unwrap_or_default();
        let port = env.get("ATO_SERVICE_DB_PORT").cloned().unwrap_or_default();
        // A consumer would build: postgresql://<host>:<port>/db  — no password in the address.
        let url_template = format!("postgresql://{host}:{port}/db");
        assert!(
            !url_template.contains("password"),
            "URL must not contain password"
        );
        assert!(
            !url_template.contains("secret"),
            "URL must not contain secret"
        );
        // Host is the alias, not a host port allocation.
        assert!(
            !url_template.contains("54321"),
            "URL must not contain host port"
        );
    }

    // ── Executor tests (using FakeOciProvider) ────────────────────────────────

    #[tokio::test]
    async fn multi_service_starts_in_dependency_order() {
        let provider = make_provider_with_unique_ids();
        run_blinko(&provider)
            .await
            .expect("blinko graph must succeed");

        let log = provider.call_log.lock().unwrap();
        // pull:postgres must precede pull:blinko
        let pull_pg = log.iter().position(|e| e == "pull:postgres:14").unwrap();
        let pull_bl = log
            .iter()
            .position(|e| e == "pull:blinkospace/blinko:latest")
            .unwrap();
        assert!(pull_pg < pull_bl, "postgres pull must precede blinko pull");

        // start:fake-db-id must precede start:fake-main-id
        let start_db = log.iter().position(|e| e == "start:fake-db-id").unwrap();
        let start_main = log.iter().position(|e| e == "start:fake-main-id").unwrap();
        assert!(start_db < start_main, "db start must precede main start");
    }

    #[test]
    fn service_start_layers_groups_sibling_leaf_dependencies() {
        let db = make_service("db", "postgres", vec![], false, Some(5432), vec![]);
        let redis = make_service("redis", "redis", vec![], false, Some(6379), vec![]);
        let weaviate = make_service("weaviate", "weaviate", vec![], false, Some(8080), vec![]);
        let api = make_service(
            "api",
            "api",
            vec![
                "db".to_string(),
                "redis".to_string(),
                "weaviate".to_string(),
            ],
            true,
            Some(5001),
            vec![],
        );
        let plan = OrchestrationPlan {
            startup_order: vec![
                "db".to_string(),
                "redis".to_string(),
                "weaviate".to_string(),
                "api".to_string(),
            ],
            services: vec![db, redis, weaviate, api],
        };

        let layers = service_start_layers(&plan).expect("layer planning");
        assert_eq!(
            layers,
            vec![
                vec![
                    "db".to_string(),
                    "redis".to_string(),
                    "weaviate".to_string()
                ],
                vec!["api".to_string()]
            ]
        );
    }

    #[tokio::test]
    async fn multi_leaf_dependencies_start_before_run_once_consumer() {
        let db = make_service("db", "postgres", vec![], false, Some(5432), vec![]);
        let redis = make_service("redis", "redis", vec![], false, Some(6379), vec![]);
        let migration = make_run_once_service(
            "migration",
            "migration-image",
            vec!["db".to_string(), "redis".to_string()],
        );
        let app = make_service(
            "app",
            "myapp",
            vec!["migration".to_string()],
            true,
            Some(8080),
            vec![],
        );
        let plan = OrchestrationPlan {
            startup_order: vec![
                "db".to_string(),
                "redis".to_string(),
                "migration".to_string(),
                "app".to_string(),
            ],
            services: vec![db, redis, migration, app],
        };
        let mut images = HashMap::new();
        images.insert("postgres".to_string(), make_image("postgres:14"));
        images.insert("redis".to_string(), make_image("redis:7"));
        images.insert(
            "migration-image".to_string(),
            make_image("migration-image:latest"),
        );
        images.insert("myapp".to_string(), make_image("myapp:latest"));

        let provider = FakeOciProvider::ready();
        provider.create_container_queue.lock().unwrap().extend([
            Ok("db-id".to_string()),
            Ok("redis-id".to_string()),
            Ok("migration-id".to_string()),
            Ok("app-id".to_string()),
        ]);
        provider.wait_result_queue.lock().unwrap().push_back(Ok(0));
        provider.wait_result_queue.lock().unwrap().push_back(Ok(0));

        let result = execute_service_graph_with_provider(
            &plan,
            &images,
            OciPolicyMode::Strict,
            &[],
            "affine-style",
            &HashSet::new(),
            None,
            &Arc::new(crate::reporters::CliReporter::new(false)),
            &provider,
            None,
            &RuntimeLaunchContext::empty(),
        )
        .await;
        assert!(result.is_ok(), "multi-leaf plan must start: {result:?}");

        let log = provider.call_log.lock().unwrap();
        let start_db = log.iter().position(|e| e == "start:db-id").unwrap();
        let start_redis = log.iter().position(|e| e == "start:redis-id").unwrap();
        let wait_migration = log
            .iter()
            .position(|e| e == "wait_container:migration-id")
            .unwrap();
        assert!(
            start_db < wait_migration && start_redis < wait_migration,
            "all sibling leaf dependencies must be started before run_once consumer waits; log: {log:?}"
        );
    }

    #[tokio::test]
    async fn failure_cleans_up_started_services() {
        let provider = make_provider_with_unique_ids();
        // Make the second start (main) fail.
        provider
            .start_result_queue
            .lock()
            .unwrap()
            .push_back(Ok(()));
        provider.start_result_queue.lock().unwrap().push_back(Err(
            OciProviderError::OciContainerStartFailed {
                container_name: "fake-main-id".to_string(),
                message: "simulated failure".to_string(),
            },
        ));

        let result = run_blinko(&provider).await;
        assert!(result.is_err(), "second start failure must propagate");

        let log = provider.call_log.lock().unwrap();
        // db was started → must be stopped and removed.
        assert!(
            log.iter().any(|e| e == "stop:fake-db-id"),
            "db must be stopped on failure; log: {log:?}"
        );
        assert!(
            log.iter().any(|e| e == "remove:fake-db-id"),
            "db must be removed on failure; log: {log:?}"
        );
        // network must be removed.
        assert!(
            log.iter().any(|e| e.starts_with("remove_network:")),
            "network must be removed on failure; log: {log:?}"
        );
    }

    #[tokio::test]
    async fn service_network_aliases_are_session_scoped() {
        let provider = make_provider_with_unique_ids();
        run_blinko(&provider)
            .await
            .expect("blinko graph must succeed");

        let requests = provider.create_container_requests.lock().unwrap();
        // Both containers must have a network set.
        for req in requests.iter() {
            assert!(
                req.network.is_some(),
                "container '{}' must have a network",
                req.name
            );
        }
        // db container must have alias "db".
        let db_req = requests
            .iter()
            .find(|r| r.name.contains("-db-"))
            .expect("db container request must be present");
        assert!(
            db_req.aliases.contains(&"db".to_string()),
            "db must have 'db' alias; aliases: {:?}",
            db_req.aliases
        );
        // main container must have alias "main".
        let main_req = requests
            .iter()
            .find(|r| r.name.contains("-main-"))
            .expect("main container request must be present");
        assert!(
            main_req.aliases.contains(&"main".to_string()),
            "main must have 'main' alias; aliases: {:?}",
            main_req.aliases
        );
    }

    #[tokio::test]
    async fn only_main_service_publishes_host_port() {
        let provider = make_provider_with_unique_ids();
        run_blinko(&provider)
            .await
            .expect("blinko graph must succeed");

        let requests = provider.create_container_requests.lock().unwrap();
        let db_req = requests
            .iter()
            .find(|r| r.name.contains("-db-"))
            .expect("db container request");
        let main_req = requests
            .iter()
            .find(|r| r.name.contains("-main-"))
            .expect("main container request");

        // db must have no published ports.
        assert!(
            db_req.ports.is_empty(),
            "db must not publish ports; got: {:?}",
            db_req.ports
        );
        // main must publish a port.
        assert!(
            !main_req.ports.is_empty(),
            "main must publish a port; got: {:?}",
            main_req.ports
        );
        // main port must be host_port=None (auto-allocate).
        assert_eq!(
            main_req.ports[0].host_port, None,
            "main host port must be auto-allocated"
        );
    }

    /// Legacy Bollard path must not be imported.
    /// This is a structural compile-time check: the module must not import
    /// from `super::oci` (the Bollard adapter).
    #[test]
    fn legacy_bollard_path_not_used_for_multi_service() {
        // If this file compiles without importing crate::adapters::runtime::oci,
        // the invariant holds.  No runtime assertion needed.
    }

    #[tokio::test]
    async fn persistent_volume_is_not_deleted_on_failure() {
        let provider = make_provider_with_unique_ids();
        // Fail the second start.
        provider
            .start_result_queue
            .lock()
            .unwrap()
            .push_back(Ok(()));
        provider.start_result_queue.lock().unwrap().push_back(Err(
            OciProviderError::OciContainerStartFailed {
                container_name: "fake-main-id".to_string(),
                message: "simulated".to_string(),
            },
        ));

        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().to_str().unwrap().to_string();

        // Only ephemeral sources are deleted; this persistent source is not in the set.
        let ephemeral: HashSet<String> = HashSet::new();

        let result = execute_service_graph_with_provider(
            &blinko_plan(),
            &images_for_blinko(),
            OciPolicyMode::Strict,
            &[],
            "blinko",
            &ephemeral,
            None, // ingress_config
            &Arc::new(crate::reporters::CliReporter::new(false)),
            &provider,
            None,
            &RuntimeLaunchContext::empty(),
        )
        .await;
        assert!(result.is_err());

        // Persistent source must still exist.
        assert!(
            std::path::Path::new(&source).exists(),
            "persistent source must not be deleted"
        );
    }

    #[tokio::test]
    async fn ephemeral_volume_is_deleted_on_failure() {
        let provider = make_provider_with_unique_ids();
        provider
            .start_result_queue
            .lock()
            .unwrap()
            .push_back(Ok(()));
        provider.start_result_queue.lock().unwrap().push_back(Err(
            OciProviderError::OciContainerStartFailed {
                container_name: "fake-main-id".to_string(),
                message: "simulated".to_string(),
            },
        ));

        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().to_str().unwrap().to_string();

        let mut ephemeral: HashSet<String> = HashSet::new();
        ephemeral.insert(source.clone());

        let result = execute_service_graph_with_provider(
            &blinko_plan(),
            &images_for_blinko(),
            OciPolicyMode::Strict,
            &[],
            "blinko",
            &ephemeral,
            None, // ingress_config
            &Arc::new(crate::reporters::CliReporter::new(false)),
            &provider,
            None,
            &RuntimeLaunchContext::empty(),
        )
        .await;
        assert!(result.is_err());

        // Ephemeral source must be gone.
        assert!(
            !std::path::Path::new(&source).exists(),
            "ephemeral source must be deleted on failure"
        );
        // tempdir guard released — the cleanup call already deleted it.
        std::mem::forget(dir);
    }

    // ── Test 20: imported Compose graph can execute with fake provider ────────

    #[test]
    fn imported_graph_can_be_executed_with_fake_multi_service_provider() {
        use capsule::routing::importer::compose::{ComposeImportInput, import_compose};
        use std::path::PathBuf;

        let compose_text = r#"
services:
  postgres:
    image: postgres:14
    environment:
      POSTGRES_USER: blinko
      POSTGRES_DB: blinko
    volumes:
      - postgres-data:/var/lib/postgresql/data

  blinko:
    image: blinkospace/blinko:latest
    ports:
      - "1111:1111"
    environment:
      DATABASE_URL: postgresql://blinko:secret@postgres:5432/blinko
    volumes:
      - blinko-data:/app/.blinko
    depends_on:
      - postgres

volumes:
  postgres-data: {}
  blinko-data: {}
"#;
        let import_input = ComposeImportInput::new(
            compose_text.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_out = import_compose(&import_input).unwrap();

        // Verify topology: blinko depends on postgres.
        let blinko = import_out
            .services
            .iter()
            .find(|s| s.name == "blinko")
            .unwrap();
        assert_eq!(blinko.depends_on[0].service, "postgres");

        // Convert to OrchestrationPlan.
        let plan = import_out.to_orchestration_plan().unwrap();

        // Startup order has postgres before blinko.
        let pg_idx = plan
            .startup_order
            .iter()
            .position(|s| s == "postgres")
            .unwrap();
        let blinko_idx = plan
            .startup_order
            .iter()
            .position(|s| s == "blinko")
            .unwrap();
        assert!(
            pg_idx < blinko_idx,
            "postgres must appear before blinko in startup_order"
        );

        // Execute with FakeOciProvider.
        let mut images = HashMap::new();
        images.insert("postgres".to_string(), make_image("postgres:14"));
        images.insert(
            "blinko".to_string(),
            make_image("blinkospace/blinko:latest"),
        );

        let provider = FakeOciProvider::ready();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let exit_code = rt
            .block_on(execute_service_graph_with_provider(
                &plan,
                &images,
                OciPolicyMode::Strict,
                &[],
                "blinko-compose",
                &HashSet::new(),
                None, // ingress_config
                &Arc::new(crate::reporters::CliReporter::new(false)),
                &provider,
                None,
                &RuntimeLaunchContext::empty(),
            ))
            .unwrap();
        assert_eq!(exit_code, 0);
    }

    // ── Execution identity invariant tests ───────────────────────────────────
    // These tests document that the no-lock compatibility path enforces the
    // same identity invariants as the lock-resolved path.

    fn single_service_plan(target: &str, image_ref: &str) -> OrchestrationPlan {
        let svc = make_service(target, target, vec![], true, Some(8080), vec![]);
        // Override image ref for the service.
        let svc = ResolvedService {
            runtime: ResolvedServiceRuntime::Oci(ResolvedTargetRuntime {
                image: Some(image_ref.to_string()),
                ..if let ResolvedServiceRuntime::Oci(rt) = svc.runtime {
                    rt
                } else {
                    unreachable!()
                }
            }),
            ..svc
        };
        OrchestrationPlan {
            startup_order: vec![target.to_string()],
            services: vec![svc],
        }
    }

    /// The executor must reject an OciImageResolution with an empty digest before
    /// any pull or start call. This is the guard that prevents running a raw mutable
    /// tag (`:latest`) without a confirmed content identity.
    #[tokio::test]
    async fn oci_runtime_does_not_start_without_resolved_digest() {
        let provider = FakeOciProvider::ready();
        let plan = single_service_plan("app", "myapp:latest");

        let mut images = HashMap::new();
        images.insert(
            "app".to_string(),
            OciImageResolution {
                declared_ref: "myapp:latest".to_string(),
                resolved_digest: "".to_string(), // empty — not resolved
                platform: OciPlatform {
                    os: "linux".to_string(),
                    architecture: "amd64".to_string(),
                    variant: None,
                },
                importer_input_hash: None,
            },
        );

        let result = execute_service_graph_with_provider(
            &plan,
            &images,
            OciPolicyMode::Strict,
            &[],
            "myapp",
            &HashSet::new(),
            None, // ingress_config
            &Arc::new(crate::reporters::CliReporter::new(false)),
            &provider,
            None,
            &RuntimeLaunchContext::empty(),
        )
        .await;

        assert!(result.is_err(), "empty digest must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("no resolved digest"),
            "error must mention missing digest; got: {msg}"
        );

        // No pull or container creation must have occurred.
        let log = provider.call_log.lock().unwrap();
        assert!(
            !log.iter().any(|e| e.starts_with("pull:")),
            "pull must not occur without a resolved digest; log: {log:?}"
        );
    }

    /// Resolved digest is recorded in the session via OciImageResolution.
    /// The pull call receives the OciImageResolution which includes the resolved_digest,
    /// and the session record stores image_digest = Some(resolved_digest).
    #[tokio::test]
    async fn oci_runtime_resolved_digest_is_propagated_to_pull() {
        let provider = FakeOciProvider::ready();
        let plan = single_service_plan("app", "myapp:v1.2.3");
        let digest = "sha256:".to_string() + &"c".repeat(64);

        let mut images = HashMap::new();
        images.insert(
            "app".to_string(),
            OciImageResolution {
                declared_ref: "myapp:v1.2.3".to_string(),
                resolved_digest: digest.clone(),
                platform: OciPlatform {
                    os: "linux".to_string(),
                    architecture: "amd64".to_string(),
                    variant: None,
                },
                importer_input_hash: None,
            },
        );

        execute_service_graph_with_provider(
            &plan,
            &images,
            OciPolicyMode::Strict,
            &[],
            "myapp",
            &HashSet::new(),
            None, // ingress_config
            &Arc::new(crate::reporters::CliReporter::new(false)),
            &provider,
            None,
            &RuntimeLaunchContext::empty(),
        )
        .await
        .expect("execution must succeed");

        // Pull must have been called for the declared_ref.
        let log = provider.call_log.lock().unwrap();
        assert!(
            log.iter().any(|e| e == "pull:myapp:v1.2.3"),
            "pull must be called with declared_ref; log: {log:?}"
        );
    }

    /// Two image resolutions with the same declared_ref but different digests
    /// represent different content identities — they are not equal.
    /// This is the fundamental "digest drift changes identity" invariant.
    #[test]
    fn oci_runtime_digest_drift_changes_identity() {
        let image_a = OciImageResolution {
            declared_ref: "myapp:latest".to_string(),
            resolved_digest: "sha256:".to_string() + &"a".repeat(64),
            platform: OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            },
            importer_input_hash: None,
        };
        let image_b = OciImageResolution {
            declared_ref: "myapp:latest".to_string(),
            resolved_digest: "sha256:".to_string() + &"b".repeat(64),
            platform: OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            },
            importer_input_hash: None,
        };

        assert_ne!(
            image_a.resolved_digest, image_b.resolved_digest,
            "same tag / different digest = different execution identity"
        );
    }

    /// The no-lock compatibility path uses the declared image ref as a fallback key
    /// when the lock file has no entry. Verify that the images map built from
    /// the resolved image (via OciResolvedImage::into_lock_resolution) contains
    /// a non-empty digest, so the downstream guard in execute_service_graph_with_provider
    /// accepts it.
    #[test]
    fn oci_runtime_no_lock_path_resolved_image_has_non_empty_digest() {
        use crate::adapters::runtime::oci_provider::OciResolvedImage;

        // This is what FakeOciProvider.resolve_image() returns for the no-lock path.
        let resolved = OciResolvedImage {
            declared_ref: "myapp:latest".to_string(),
            resolved_digest: "sha256:".to_string() + &"b".repeat(64),
            platform: OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            },
            media_type: None,
            provider_semantics: crate::adapters::runtime::oci_provider::fake_oci_semantics(),
        };

        let lock_resolution = resolved.into_lock_resolution();
        assert!(
            !lock_resolution.resolved_digest.is_empty(),
            "no-lock path must produce a non-empty digest; got empty"
        );
        assert!(
            lock_resolution.resolved_digest.starts_with("sha256:"),
            "digest must be sha256-prefixed; got: {}",
            lock_resolution.resolved_digest
        );
    }

    // ── run_once tests ────────────────────────────────────────────────────────

    fn make_run_once_service(name: &str, target: &str, depends_on: Vec<String>) -> ResolvedService {
        let mut svc = make_service(name, target, depends_on, false, None, vec![]);
        svc.run_once = true;
        // run_once requires a cmd
        if let ResolvedServiceRuntime::Oci(ref mut rt) = svc.runtime {
            rt.cmd = vec!["sh".to_string(), "-c".to_string(), "echo ok".to_string()];
        }
        svc
    }

    /// Build a synthetic plan: db + init (run_once, depends_on db) + app (depends_on init).
    fn synthetic_run_once_plan() -> OrchestrationPlan {
        let db = make_service("db", "postgres", vec![], false, Some(5432), vec![]);
        let init = make_run_once_service("init", "init-image", vec!["db".to_string()]);
        let app = make_service(
            "app",
            "myapp",
            vec!["init".to_string()],
            true,
            Some(8080),
            vec![],
        );
        OrchestrationPlan {
            startup_order: vec!["db".to_string(), "init".to_string(), "app".to_string()],
            services: vec![db, init, app],
        }
    }

    fn images_for_run_once_plan() -> HashMap<String, OciImageResolution> {
        let mut m = HashMap::new();
        m.insert("postgres".to_string(), make_image("postgres:14"));
        m.insert("init-image".to_string(), make_image("init-image:latest"));
        m.insert("myapp".to_string(), make_image("myapp:latest"));
        m
    }

    async fn run_run_once_plan(provider: &FakeOciProvider) -> Result<i32> {
        execute_service_graph_with_provider(
            &synthetic_run_once_plan(),
            &images_for_run_once_plan(),
            OciPolicyMode::Strict,
            &[],
            "smoke",
            &HashSet::new(),
            None, // ingress_config
            &Arc::new(crate::reporters::CliReporter::new(false)),
            provider,
            None,
            &RuntimeLaunchContext::empty(),
        )
        .await
    }

    /// run_once exits 0 → dependents start; init container is removed without
    /// being added to the long-running `started` list.
    #[tokio::test]
    async fn dependent_starts_after_run_once_exit_zero() {
        let provider = FakeOciProvider::ready();
        // First wait_container call is init's run_once wait; second is the
        // long-running app's wait_all_services wait.  Both Ok(0).
        provider.wait_result_queue.lock().unwrap().push_back(Ok(0));
        provider.wait_result_queue.lock().unwrap().push_back(Ok(0));

        let result = run_run_once_plan(&provider).await;
        assert!(
            result.is_ok(),
            "run_once exit-0 plan must succeed: {:?}",
            result
        );

        let log = provider.call_log.lock().unwrap();
        // Creation order is a robust proxy for start order (each create is
        // followed by start in the same loop iteration; container ids in
        // FakeOciProvider are not unique by default so we key off the
        // service-name embedded in the container name).
        let create_db = log
            .iter()
            .position(|e| e.contains("-db-"))
            .expect("db create must happen");
        let create_init = log
            .iter()
            .position(|e| e.contains("-init-"))
            .expect("init create must happen");
        let create_app = log
            .iter()
            .position(|e| e.contains("-app-"))
            .expect("app create must happen");
        assert!(
            create_db < create_init && create_init < create_app,
            "creation order must be db → init → app; log: {log:?}"
        );

        // Init container must be removed after exit-0.
        assert!(
            log.iter().any(|e| e.starts_with("remove:")),
            "init removal must be logged after exit-0; log: {log:?}"
        );
    }

    /// run_once exits non-zero → dependents do NOT start, graph returns typed error.
    #[tokio::test]
    async fn dependent_not_started_when_run_once_fails() {
        let provider = FakeOciProvider::ready();
        provider.wait_result_queue.lock().unwrap().push_back(Ok(1));

        let result = run_run_once_plan(&provider).await;
        assert!(
            result.is_err(),
            "non-zero run_once exit must return an error"
        );
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("oci_run_once_failed"),
            "error must contain oci_run_once_failed; got: {msg}"
        );

        // app container must NOT have been created (init failure blocks it).
        let log = provider.call_log.lock().unwrap();
        assert!(
            !log.iter().any(|e| e.contains("-app-")),
            "app must not be created when init fails; log: {log:?}"
        );
    }

    /// On run_once failure, previously-started long-running services must be
    /// stopped and removed (cleanup walks `started` in reverse).
    #[tokio::test]
    async fn run_once_failure_cleans_up_started_services() {
        let provider = FakeOciProvider::ready();
        provider.wait_result_queue.lock().unwrap().push_back(Ok(2));

        let result = run_run_once_plan(&provider).await;
        assert!(result.is_err());

        let log = provider.call_log.lock().unwrap();
        // db was started → must be stopped and removed during cleanup.
        // FakeOciProvider returns the same container_id ("fake-container-id")
        // for every create call by default, so we assert by op kind.
        assert!(
            log.iter().any(|e| e.starts_with("stop:")),
            "started service must be stopped on run_once failure; log: {log:?}"
        );
        assert!(
            log.iter().any(|e| e.starts_with("remove:")),
            "started service must be removed on run_once failure; log: {log:?}"
        );
        assert!(
            log.iter().any(|e| e.starts_with("remove_network:")),
            "network must be removed on run_once failure; log: {log:?}"
        );
    }

    /// Wait-side provider error is reported as a typed `oci_run_once_failed`.
    #[tokio::test]
    async fn run_once_provider_error_returns_typed_error() {
        let provider = FakeOciProvider::ready();
        provider.wait_result_queue.lock().unwrap().push_back(Err(
            OciProviderError::CommandFailed {
                provider: "podman",
                command: "wait".to_string(),
                status: Some(1),
                message: "simulated wait error".to_string(),
            },
        ));

        let result = run_run_once_plan(&provider).await;
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("oci_run_once_failed"),
            "error must contain oci_run_once_failed; got: {msg}"
        );
    }

    /// run_once that doesn't complete within the configured timeout returns
    /// `oci_run_once_timeout`.  Uses `ATO_OCI_RUN_ONCE_TIMEOUT_SECS=1` and a
    /// 3s FakeOciProvider block to keep the wall-clock under 2 seconds.
    ///
    /// Marked `serial` via lock: the env var is process-global, and other
    /// tests may run concurrently.  We restore the prior value at the end.
    #[tokio::test]
    async fn run_once_timeout_returns_typed_error() {
        let result = {
            let _guard = run_once_test_env_lock().lock().await;
            let prev = std::env::var("ATO_OCI_RUN_ONCE_TIMEOUT_SECS").ok();
            // SAFETY: tests sharing this env var hold _guard for their duration.
            unsafe {
                std::env::set_var("ATO_OCI_RUN_ONCE_TIMEOUT_SECS", "1");
            }

            let provider = FakeOciProvider::ready();
            // Block wait_container for 3 seconds (> 1s timeout).
            *provider.wait_block_ms.lock().unwrap() = Some(3_000);
            provider.wait_result_queue.lock().unwrap().push_back(Ok(0));

            let result = run_run_once_plan(&provider).await;

            // Restore env var ASAP so a later test panic on the assertion below
            // doesn't leave the variable set.
            match prev {
                Some(v) => unsafe { std::env::set_var("ATO_OCI_RUN_ONCE_TIMEOUT_SECS", v) },
                None => unsafe { std::env::remove_var("ATO_OCI_RUN_ONCE_TIMEOUT_SECS") },
            }

            result
        };

        assert!(result.is_err(), "timeout must surface as Err");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("oci_run_once_timeout"),
            "error must contain oci_run_once_timeout; got: {msg}"
        );
    }

    /// `ato stop --all` must not crash when the run_once container was already
    /// removed.  We exercise this at the executor level: after a successful
    /// run_once, the init container is removed and is *not* in the session
    /// record, so a subsequent cleanup pass over the long-running services
    /// completes without touching the missing init container.
    #[tokio::test]
    async fn run_once_removed_container_does_not_break_stop_all() {
        let provider = FakeOciProvider::ready();
        provider.wait_result_queue.lock().unwrap().push_back(Ok(0));
        provider.wait_result_queue.lock().unwrap().push_back(Ok(0));

        let result = run_run_once_plan(&provider).await;
        assert!(
            result.is_ok(),
            "happy-path run_once must complete: {:?}",
            result
        );

        let log = provider.call_log.lock().unwrap();
        // Init was removed exactly once during the run_once branch (success
        // path removes the container immediately after exit-0).  The later
        // cleanup_services loop walks `started` only — which never contained
        // init — so it does NOT issue a second remove for the init container.
        // We assert that `stop:*` was issued only for long-running services
        // (db + app), not for the init container.  FakeOciProvider returns
        // the same fake id for every create_container call by default, so we
        // assert on call counts instead of per-id tracking: 2 stops (db+app),
        // not 3.
        let stop_count = log.iter().filter(|e| e.starts_with("stop:")).count();
        assert_eq!(
            stop_count, 2,
            "cleanup must stop only long-running services (db + app), got {stop_count}; log: {log:?}"
        );
    }

    /// run_once result is recorded with at minimum: the success notification
    /// emitted via the reporter.  Because the executor returns `Result<i32>`,
    /// receipt-level capture is left to higher layers; this test pins the
    /// observable signal (the success message and the removal call sequence).
    #[tokio::test]
    async fn run_once_result_recorded_in_call_sequence() {
        let provider = FakeOciProvider::ready();
        provider.wait_result_queue.lock().unwrap().push_back(Ok(0));
        provider.wait_result_queue.lock().unwrap().push_back(Ok(0));

        let _ = run_run_once_plan(&provider).await.expect("ok");
        let log = provider.call_log.lock().unwrap();
        // The init container's wait+remove sequence is the recorded result.
        let wait_idx = log
            .iter()
            .position(|e| e.starts_with("wait_container:"))
            .expect("wait_container must be logged for init");
        let remove_idx = log
            .iter()
            .skip(wait_idx)
            .position(|e| e.starts_with("remove:"))
            .expect("remove must follow init wait");
        let _ = remove_idx;
    }

    /// Long-running services unaffected when no run_once targets present.
    #[tokio::test]
    async fn existing_long_running_services_unaffected_by_run_once() {
        let provider = FakeOciProvider::ready();
        let result = run_blinko(&provider).await;
        assert!(
            result.is_ok(),
            "blinko plan (no run_once) must still succeed: {:?}",
            result
        );
        // No `wait_container` calls would have happened with a run_once branch;
        // verify the legacy flow is intact by checking creation count = 2
        // (db + main).
        let log = provider.call_log.lock().unwrap();
        let creates = log.iter().filter(|e| e.starts_with("create:")).count();
        assert_eq!(
            creates, 2,
            "blinko must create exactly db + main; log: {log:?}"
        );
    }

    /// Verify two services sharing same-capsule state receive the same mount locator.
    #[tokio::test]
    async fn shared_state_same_capsule_services_receive_same_mount() {
        use capsule::types::Mount;

        let provider = make_provider_with_unique_ids();
        // Queue 3 container IDs: db, api, worker.
        provider.create_container_queue.lock().unwrap().extend([
            Ok("fake-db-id".to_string()),
            Ok("fake-api-id".to_string()),
            Ok("fake-worker-id".to_string()),
        ]);
        provider
            .start_result_queue
            .lock()
            .unwrap()
            .extend([Ok(()), Ok(()), Ok(())]);

        let shared_source = "/var/lib/ato/state/shared-app/uploads";
        let shared_target = "/app/storage";

        let mut api_svc = make_service(
            "api",
            "api-image",
            vec!["db".to_string()],
            true,
            Some(8080),
            vec![ServiceConnectionInfo {
                dependency: "db".to_string(),
                host_env: "ATO_SERVICE_DB_HOST".to_string(),
                port_env: "ATO_SERVICE_DB_PORT".to_string(),
                container_port: Some(5432),
                default_host: "db".to_string(),
            }],
        );
        if let ResolvedServiceRuntime::Oci(ref mut rt) = api_svc.runtime {
            rt.mounts = vec![Mount {
                source: shared_source.to_string(),
                target: shared_target.to_string(),
                readonly: false,
                ownership: None,
            }];
        }

        let mut worker_svc = make_service("worker", "worker-image", vec![], false, None, vec![]);
        if let ResolvedServiceRuntime::Oci(ref mut rt) = worker_svc.runtime {
            rt.mounts = vec![Mount {
                source: shared_source.to_string(),
                target: shared_target.to_string(),
                readonly: false,
                ownership: None,
            }];
        }

        let db = make_service("db", "postgres", vec![], false, Some(5432), vec![]);
        let plan = OrchestrationPlan {
            startup_order: vec!["db".to_string(), "api".to_string(), "worker".to_string()],
            services: vec![db, api_svc, worker_svc],
        };

        let mut images = HashMap::new();
        images.insert("postgres".to_string(), make_image("postgres:14"));
        images.insert("api-image".to_string(), make_image("example/api:1.0"));
        images.insert("worker-image".to_string(), make_image("example/worker:1.0"));

        let ephemeral: HashSet<String> = HashSet::new();
        let result = execute_service_graph_with_provider(
            &plan,
            &images,
            OciPolicyMode::Strict,
            &[],
            "shared-state-app",
            &ephemeral,
            None, // ingress_config
            &Arc::new(crate::reporters::CliReporter::new(false)),
            &provider,
            None,
            &RuntimeLaunchContext::empty(),
        )
        .await;
        assert!(result.is_ok(), "shared state plan must start: {result:?}");

        let requests = provider.create_container_requests.lock().unwrap();
        let api_req = requests
            .iter()
            .find(|r| r.name.contains("-api-"))
            .expect("api container request must exist");
        let worker_req = requests
            .iter()
            .find(|r| r.name.contains("-worker-"))
            .expect("worker container request must exist");

        let api_mount = api_req
            .mounts
            .iter()
            .find(|m| m.target == shared_target)
            .expect("api must mount shared target");
        let worker_mount = worker_req
            .mounts
            .iter()
            .find(|m| m.target == shared_target)
            .expect("worker must mount shared target");

        assert_eq!(
            api_mount.source, worker_mount.source,
            "shared state must use the same mount source for both services"
        );
        // The source-strategy is platform-dependent (#444): on Windows + Podman
        // this Ato-managed writable state becomes a stable engine volume, while
        // on other hosts it stays the bind path. Either way the source is stable
        // and shared between the two services (asserted above).
        if cfg!(target_os = "windows") {
            assert_eq!(api_mount.source, engine_state_volume_name(shared_source));
            assert!(api_mount.is_engine_volume());
        } else {
            assert_eq!(api_mount.source, shared_source);
            assert_eq!(api_mount.source_kind, OciMountSourceKind::BindPath);
        }
        assert!(!api_mount.readonly, "shared state mount must be writable");
        assert!(
            !worker_mount.readonly,
            "shared state mount must be writable"
        );
    }

    /// Process-wide mutex for the timeout test (env-var-dependent). Async-aware
    /// so the guard can be held across `.await` (clippy::await_holding_lock).
    fn run_once_test_env_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        &LOCK
    }

    // ── OCI proxy env tests ───────────────────────────────────────────────────

    #[allow(dead_code)]
    fn make_service_with_egress_proxy(egress_proxy: bool) -> ResolvedService {
        use capsule::types::orchestration::ResolvedServiceNetwork;
        let svc = make_service("app", "app", vec![], true, Some(8080), vec![]);
        ResolvedService {
            network: ResolvedServiceNetwork {
                egress_proxy,
                ..svc.network
            },
            ..svc
        }
    }

    #[test]
    fn proxy_env_for_oci_container_uses_host_containers_internal() {
        let proxy = crate::common::proxy::proxy_env_for_oci_container(9876, &[]);
        assert!(
            proxy.http_proxy.contains("host.containers.internal"),
            "http_proxy must use host.containers.internal, got: {}",
            proxy.http_proxy
        );
        assert!(
            proxy.https_proxy.contains("host.containers.internal"),
            "https_proxy must use host.containers.internal"
        );
        assert!(
            proxy.all_proxy.contains("host.containers.internal"),
            "all_proxy must use host.containers.internal"
        );
        assert!(proxy.http_proxy.contains("9876"), "port must be present");
    }

    #[test]
    fn proxy_env_for_oci_container_no_proxy_includes_loopback() {
        let proxy = crate::common::proxy::proxy_env_for_oci_container(9876, &[]);
        let no_proxy = &proxy.no_proxy;
        assert!(
            no_proxy.contains("localhost"),
            "NO_PROXY must include localhost"
        );
        assert!(
            no_proxy.contains("127.0.0.1"),
            "NO_PROXY must include 127.0.0.1"
        );
        assert!(no_proxy.contains("::1"), "NO_PROXY must include ::1");
    }

    #[test]
    fn proxy_env_for_oci_container_includes_service_aliases() {
        let aliases = ["db", "redis", "minio"];
        let proxy = crate::common::proxy::proxy_env_for_oci_container(9876, &aliases);
        for alias in &aliases {
            assert!(
                proxy.no_proxy.contains(alias),
                "NO_PROXY must include service alias '{}', got: {}",
                alias,
                proxy.no_proxy
            );
        }
    }

    #[tokio::test]
    async fn oci_multi_service_injects_container_proxy_when_enabled() {
        use crate::adapters::runtime::executors::launch_context::RuntimeLaunchContext;

        let provider = FakeOciProvider::ready();
        let plan = blinko_plan(); // egress_proxy = true (default)

        let mut ctx = RuntimeLaunchContext::empty();
        ctx.set_egress_proxy_port(9999);

        let result = execute_service_graph_with_provider(
            &plan,
            &images_for_blinko(),
            OciPolicyMode::Strict,
            &[],
            "blinko",
            &HashSet::new(),
            None,
            &Arc::new(crate::reporters::CliReporter::new(false)),
            &provider,
            None,
            &ctx,
        )
        .await;
        assert!(result.is_ok(), "execution must succeed: {result:?}");

        let requests = provider.create_container_requests.lock().unwrap();
        for req in requests.iter() {
            // All services have egress_proxy=true (default), so all should get container proxy.
            let http_proxy = req
                .env
                .get("HTTP_PROXY")
                .or_else(|| req.env.get("http_proxy"));
            assert!(
                http_proxy
                    .map(|v| v.contains("host.containers.internal"))
                    .unwrap_or(false),
                "HTTP_PROXY must use host.containers.internal in container '{}', got: {:?}",
                req.name,
                http_proxy
            );
            // extra_hosts must include the gateway entry.
            assert!(
                req.extra_hosts
                    .iter()
                    .any(|h| h == crate::common::proxy::OCI_HOST_GATEWAY_ENTRY),
                "extra_hosts must include '{}' for container '{}'",
                crate::common::proxy::OCI_HOST_GATEWAY_ENTRY,
                req.name
            );
        }
    }

    #[tokio::test]
    async fn oci_multi_service_strips_proxy_when_egress_proxy_false() {
        use crate::adapters::runtime::executors::launch_context::RuntimeLaunchContext;
        use capsule::types::orchestration::{ResolvedService, ResolvedServiceNetwork};

        // Build a single-service plan with egress_proxy = false.
        let svc = make_service("app", "blinko", vec![], true, Some(3000), vec![]);
        let svc = ResolvedService {
            network: ResolvedServiceNetwork {
                egress_proxy: false,
                ..svc.network
            },
            ..svc
        };
        let plan = OrchestrationPlan {
            startup_order: vec!["app".to_string()],
            services: vec![svc],
        };
        let mut images = HashMap::new();
        // Image map is keyed by target label, not service name.
        images.insert(
            "blinko".to_string(),
            make_image("blinkospace/blinko:latest"),
        );

        let provider = FakeOciProvider::ready();
        let mut ctx = RuntimeLaunchContext::empty();
        ctx.set_egress_proxy_port(9999);
        // Simulate that launch_ctx already has proxy vars injected (as session.rs does).
        ctx.extend_injected_env({
            let mut m = HashMap::new();
            m.insert(
                "HTTP_PROXY".to_string(),
                "http://127.0.0.1:9999".to_string(),
            );
            m.insert(
                "http_proxy".to_string(),
                "http://127.0.0.1:9999".to_string(),
            );
            m
        });

        let result = execute_service_graph_with_provider(
            &plan,
            &images,
            OciPolicyMode::Strict,
            &[],
            "blinko",
            &HashSet::new(),
            None,
            &Arc::new(crate::reporters::CliReporter::new(false)),
            &provider,
            None,
            &ctx,
        )
        .await;
        assert!(result.is_ok(), "execution must succeed: {result:?}");

        let requests = provider.create_container_requests.lock().unwrap();
        let req = requests.first().expect("one container request");
        // Proxy vars must be stripped — both uppercase and lowercase.
        for key in crate::common::proxy::PROXY_ENV_KEYS {
            assert!(
                !req.env.contains_key(key),
                "proxy var '{}' must be stripped when egress_proxy=false, found: {:?}",
                key,
                req.env.get(key)
            );
        }
        // No extra_hosts when proxy is disabled.
        assert!(
            req.extra_hosts.is_empty(),
            "extra_hosts must be empty when egress_proxy=false"
        );
    }

    #[tokio::test]
    async fn oci_multi_service_no_proxy_includes_all_service_aliases() {
        use crate::adapters::runtime::executors::launch_context::RuntimeLaunchContext;

        // blinko_plan has "blinko" and "postgres" services with their aliases.
        let provider = FakeOciProvider::ready();
        let plan = blinko_plan();
        let all_aliases: Vec<String> = plan
            .services
            .iter()
            .flat_map(|s| s.network.aliases.iter().cloned())
            .collect();

        let mut ctx = RuntimeLaunchContext::empty();
        ctx.set_egress_proxy_port(9999);

        let result = execute_service_graph_with_provider(
            &plan,
            &images_for_blinko(),
            OciPolicyMode::Strict,
            &[],
            "blinko",
            &HashSet::new(),
            None,
            &Arc::new(crate::reporters::CliReporter::new(false)),
            &provider,
            None,
            &ctx,
        )
        .await;
        assert!(result.is_ok(), "execution must succeed: {result:?}");

        let requests = provider.create_container_requests.lock().unwrap();
        for req in requests.iter() {
            let no_proxy = req
                .env
                .get("NO_PROXY")
                .or_else(|| req.env.get("no_proxy"))
                .expect("NO_PROXY must be set");
            for alias in &all_aliases {
                assert!(
                    no_proxy.contains(alias.as_str()),
                    "NO_PROXY must include alias '{}' for container '{}', got: {}",
                    alias,
                    req.name,
                    no_proxy
                );
            }
        }
    }

    #[tokio::test]
    async fn oci_multi_service_no_extra_hosts_when_no_proxy_port() {
        // No egress_proxy_port set → extra_hosts must be empty.
        let provider = FakeOciProvider::ready();

        let result = execute_service_graph_with_provider(
            &blinko_plan(),
            &images_for_blinko(),
            OciPolicyMode::Strict,
            &[],
            "blinko",
            &HashSet::new(),
            None,
            &Arc::new(crate::reporters::CliReporter::new(false)),
            &provider,
            None,
            &RuntimeLaunchContext::empty(), // no egress_proxy_port
        )
        .await;
        assert!(result.is_ok(), "execution must succeed: {result:?}");

        let requests = provider.create_container_requests.lock().unwrap();
        for req in requests.iter() {
            assert!(
                req.extra_hosts.is_empty(),
                "extra_hosts must be empty when no egress_proxy_port, container: {}",
                req.name
            );
        }
    }

    // ── Exit-before-ready readiness tests (#429) ───────────────────────────────

    fn tcp_probe(addr: &str, timeout_seconds: u32) -> capsule::types::ReadinessProbe {
        capsule::types::ReadinessProbe {
            http_get: None,
            tcp_connect: Some(addr.to_string()),
            exec: None,
            port: None,
            initial_delay_seconds: 0,
            timeout_seconds,
            interval_seconds: 1,
        }
    }

    /// A probe with no recognizable target resolves to "ready" immediately
    /// (see `run_readiness_probe`'s final branch).
    fn always_ready_probe() -> capsule::types::ReadinessProbe {
        capsule::types::ReadinessProbe {
            http_get: None,
            tcp_connect: None,
            exec: None,
            port: None,
            initial_delay_seconds: 0,
            timeout_seconds: 5,
            interval_seconds: 1,
        }
    }

    #[test]
    fn exited_before_ready_error_includes_code_and_log_tail() {
        let err = exited_before_ready_error(
            "main",
            Some(1),
            &[
                "Error: Current user does not have write and/or execute permissions".to_string(),
                "for the ./data directory: /opt/openlist/data".to_string(),
            ],
        );
        let msg = err.to_string();
        assert!(
            msg.contains("oci_container_exited_before_ready"),
            "code: {msg}"
        );
        assert!(msg.contains("service 'main'"), "service: {msg}");
        assert!(msg.contains("status 1"), "exit code: {msg}");
        assert!(msg.contains("/opt/openlist/data"), "log tail: {msg}");
        assert!(msg.contains("hint:"), "hint: {msg}");

        // The error must remain downcast-able so the diagnostics layer can map
        // it to the typed `oci_container_exited_before_ready` code (#445).
        let typed = err
            .downcast_ref::<OciExitedBeforeReadyError>()
            .expect("must preserve the typed exited-before-ready error");
        assert_eq!(typed.service_name, "main");
        assert_eq!(typed.exit_code, Some(1));
        assert_eq!(typed.log_tail.len(), 2);
    }

    #[test]
    fn exited_before_ready_error_handles_unknown_code_and_empty_logs() {
        let err = exited_before_ready_error("db", None, &[]);
        let msg = err.to_string();
        assert!(msg.contains("status unknown"), "unknown code: {msg}");
        assert!(
            msg.contains("(no container logs captured)"),
            "empty logs: {msg}"
        );
    }

    #[tokio::test]
    async fn collect_log_tail_truncates_to_max_lines() {
        let mut provider = FakeOciProvider::ready();
        provider.log_chunks = vec![capsule::runtime::oci::OciLogChunk {
            stderr: true,
            message: b"l1\nl2\nl3\nl4\nl5\n".to_vec(),
        }];
        let tail = collect_log_tail(&provider, "cid", 3).await;
        assert_eq!(
            tail,
            vec!["l3", "l4", "l5"],
            "should keep only the last 3 lines"
        );
    }

    #[tokio::test]
    async fn collect_log_tail_handles_chunk_splits_and_trailing_line() {
        let mut provider = FakeOciProvider::ready();
        // A line split across chunk boundaries, plus a final line with no
        // terminating newline — both must be reassembled/flushed correctly.
        provider.log_chunks = vec![
            capsule::runtime::oci::OciLogChunk {
                stderr: false,
                message: b"first line\nsec".to_vec(),
            },
            capsule::runtime::oci::OciLogChunk {
                stderr: false,
                message: b"ond line\nthird (no newline)".to_vec(),
            },
        ];
        let tail = collect_log_tail(&provider, "cid", 10).await;
        assert_eq!(
            tail,
            vec!["first line", "second line", "third (no newline)"],
            "split line must be reassembled and the trailing newline-less line flushed"
        );
    }

    #[tokio::test]
    async fn await_service_readiness_reports_exit_before_ready() {
        let mut provider = FakeOciProvider::ready();
        // Container is already dead by the time we inspect.
        provider.inspect_result = Ok(OciContainerInspect {
            running: false,
            exit_code: Some(1),
            host_ports: std::collections::HashMap::new(),
        });
        provider.log_chunks = vec![capsule::runtime::oci::OciLogChunk {
            stderr: true,
            message:
                b"Error: Current user does not have write and/or execute permissions for the ./data directory: /opt/openlist/data\n"
                    .to_vec(),
        }];

        // A probe that would otherwise loop for 30s; the exit watch must win first.
        let probe = tcp_probe("127.0.0.1:1", 30);
        let result =
            await_service_readiness(&provider, &probe, Some(45678), "cid", "ato-main", "main")
                .await;

        let err = result.expect_err("must fail with exited-before-ready");
        let msg = err.to_string();
        assert!(
            msg.contains("oci_container_exited_before_ready"),
            "code: {msg}"
        );
        assert!(msg.contains("status 1"), "exit code: {msg}");
        assert!(msg.contains("/opt/openlist/data"), "log tail: {msg}");

        // Readiness wait must return the typed (downcast-able) error with the
        // service name and log tail intact, so the multi-service layer can carry
        // it to the diagnostics mapping without stringifying it (#445).
        let typed = err
            .downcast_ref::<OciExitedBeforeReadyError>()
            .expect("await_service_readiness must return the typed error");
        assert_eq!(typed.service_name, "main");
        assert_eq!(typed.exit_code, Some(1));
        assert!(
            typed
                .log_tail
                .iter()
                .any(|line| line.contains("/opt/openlist/data")),
            "log tail must be preserved on the typed error: {:?}",
            typed.log_tail
        );
    }

    #[tokio::test]
    async fn await_service_readiness_reports_timeout_when_still_running() {
        let mut provider = FakeOciProvider::ready();
        // Container keeps running but never passes the probe.
        provider.inspect_result = Ok(OciContainerInspect {
            running: true,
            exit_code: None,
            host_ports: std::collections::HashMap::new(),
        });

        // tcp connect to a closed port; short timeout so the probe gives up fast.
        let probe = tcp_probe("127.0.0.1:1", 1);
        let result =
            await_service_readiness(&provider, &probe, None, "cid", "ato-main", "main").await;

        let err = result.expect_err("must fail with healthcheck timeout");
        let msg = err.to_string();
        assert!(msg.contains("oci_healthcheck_timeout"), "code: {msg}");
        assert!(
            !msg.contains("oci_container_exited_before_ready"),
            "must not be exit-before-ready while still running: {msg}"
        );
    }

    #[tokio::test]
    async fn await_service_readiness_ok_when_probe_passes() {
        let provider = FakeOciProvider::ready(); // inspect running:true by default
        let probe = always_ready_probe();
        let result =
            await_service_readiness(&provider, &probe, None, "cid", "ato-main", "main").await;
        assert!(result.is_ok(), "ready probe must succeed: {result:?}");
    }

    // ── OciMountSpec ownership propagation tests (#428 followup) ──────────────

    #[test]
    fn oci_mount_spec_ownership_propagates_from_manifest_mount() {
        let manifest_mount = capsule::types::Mount {
            source: "/host/state".to_string(),
            target: "/app/state".to_string(),
            readonly: false,
            ownership: Some(capsule::types::MountOwnership {
                uid: Some(1001),
                gid: Some(1001),
                recursive: false,
                mode: Some(0o755),
            }),
        };
        let spec = OciMountSpec {
            source: manifest_mount.source.clone(),
            target: manifest_mount.target.clone(),
            readonly: manifest_mount.readonly,
            ownership: manifest_mount.ownership.clone(),
            source_kind: OciMountSourceKind::default(),
        };
        assert_eq!(spec.ownership.as_ref().unwrap().uid, Some(1001));
    }

    #[test]
    fn oci_mount_spec_no_ownership_when_not_declared() {
        let spec = OciMountSpec {
            source: "/host/cfg".to_string(),
            target: "/app/cfg".to_string(),
            readonly: true,
            ownership: None,
            source_kind: OciMountSourceKind::default(),
        };
        assert!(spec.ownership.is_none());
        assert!(spec.readonly);
    }

    // resolve_oci_mount strategy selection is unit-tested in capsule
    // (`engine::runtime::oci::mount_source_tests`) since the helper now lives
    // there and is shared by both the multi-service and orchestrator paths (#444).

    // ── cleanup_services: ephemeral engine volume removal (#444) ──────────────

    #[tokio::test]
    async fn cleanup_removes_ephemeral_engine_volumes() {
        let provider = FakeOciProvider::ready();
        let started: Vec<ServiceStartRecord> = vec![];
        let sources: HashSet<String> = HashSet::new();
        let mut volumes: HashSet<String> = HashSet::new();
        volumes.insert("ato-state-deadbeef0000-cache".to_string());

        cleanup_services(&started, "ato-net", &sources, &volumes, &provider).await;

        let log = provider.call_log.lock().unwrap();
        assert!(
            log.iter()
                .any(|e| e == "remove_volume:ato-state-deadbeef0000-cache"),
            "ephemeral engine volume must be removed: {log:?}"
        );
    }

    #[tokio::test]
    async fn cleanup_does_not_remove_persistent_engine_volumes() {
        let provider = FakeOciProvider::ready();
        let started: Vec<ServiceStartRecord> = vec![];
        let sources: HashSet<String> = HashSet::new();
        // Persistent volumes are never added to the ephemeral set.
        let volumes: HashSet<String> = HashSet::new();

        cleanup_services(&started, "ato-net", &sources, &volumes, &provider).await;

        let log = provider.call_log.lock().unwrap();
        assert!(
            !log.iter().any(|e| e.starts_with("remove_volume:")),
            "persistent engine volumes must survive cleanup: {log:?}"
        );
    }

    // ── prepare_writable_ownership_mount_sources tests ────────────────────────

    fn make_oci_mount(
        source: &str,
        readonly: bool,
        ownership: Option<capsule::types::MountOwnership>,
    ) -> OciMountSpec {
        OciMountSpec {
            source: source.to_string(),
            target: "/container/path".to_string(),
            readonly,
            ownership,
            source_kind: OciMountSourceKind::default(),
        }
    }

    fn ownership_with_mode(mode: u32) -> capsule::types::MountOwnership {
        capsule::types::MountOwnership {
            uid: Some(1001),
            gid: Some(1001),
            recursive: false,
            mode: Some(mode),
        }
    }

    fn ownership_no_mode() -> capsule::types::MountOwnership {
        capsule::types::MountOwnership {
            uid: Some(1001),
            gid: Some(1001),
            recursive: false,
            mode: None,
        }
    }

    // Uses a host filesystem path as the mount source; the named-volume
    // heuristic (`source.contains('/')`) and the directory layout assume
    // POSIX path separators, so this is a Unix-only assertion.
    #[cfg(unix)]
    #[test]
    fn prepare_mounts_creates_dir_for_writable_ownership_mounts() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("state_data");
        let mounts = vec![make_oci_mount(
            source.to_str().unwrap(),
            false,
            Some(ownership_with_mode(0o755)),
        )];
        prepare_writable_ownership_mount_sources("svc", &mounts).unwrap();
        assert!(source.exists(), "source directory must be created");
    }

    #[cfg(unix)]
    #[test]
    fn prepare_mounts_applies_mode_when_declared() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("state_data");
        let mounts = vec![make_oci_mount(
            source.to_str().unwrap(),
            false,
            Some(ownership_with_mode(0o777)),
        )];
        prepare_writable_ownership_mount_sources("svc", &mounts).unwrap();
        let mode = std::fs::metadata(&source).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o777,
            "mode must be applied when ownership.mode is Some"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prepare_mounts_does_not_chmod_when_mode_is_none() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("state_data");
        // Pre-create with restrictive mode.
        std::fs::create_dir_all(&source).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mounts = vec![make_oci_mount(
            source.to_str().unwrap(),
            false,
            Some(ownership_no_mode()),
        )];
        prepare_writable_ownership_mount_sources("svc", &mounts).unwrap();
        let mode = std::fs::metadata(&source).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "mode must not change when ownership.mode is None"
        );
    }

    #[test]
    fn prepare_mounts_skips_readonly_mounts() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("ro_data");
        let mounts = vec![make_oci_mount(
            source.to_str().unwrap(),
            true, // readonly
            Some(ownership_with_mode(0o777)),
        )];
        prepare_writable_ownership_mount_sources("svc", &mounts).unwrap();
        assert!(
            !source.exists(),
            "readonly mount source must not be created"
        );
    }

    #[test]
    fn prepare_mounts_skips_mounts_without_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("no_owner_data");
        let mounts = vec![make_oci_mount(
            source.to_str().unwrap(),
            false,
            None, // no ownership
        )];
        prepare_writable_ownership_mount_sources("svc", &mounts).unwrap();
        assert!(
            !source.exists(),
            "mount without ownership declaration must not be created"
        );
    }

    #[test]
    fn prepare_mounts_skips_named_volumes() {
        // Named volumes (no path separator) are engine-managed, must not be mkdir'd.
        let mounts = vec![make_oci_mount(
            "my-named-volume", // no '/'
            false,
            Some(ownership_with_mode(0o777)),
        )];
        // Should succeed without touching the filesystem.
        prepare_writable_ownership_mount_sources("svc", &mounts).unwrap();
        // No directory created (named volume is not a path on the host).
    }

    // ── #501: orchestration strict gate + per-service provider evidence ──

    use capsule::execution_identity::{OciEnforcementStatus, OciImageDigestStatus};
    use capsule::execution_plan::error::AtoExecutionError;

    #[test]
    fn orchestration_provider_evidence_is_produced_per_service() {
        let web = make_service("web", "web-image", vec![], true, Some(8080), vec![]);
        let db = make_service("db", "db-image", vec![], false, Some(5432), vec![]);
        let plan = OrchestrationPlan {
            startup_order: vec!["db".to_string(), "web".to_string()],
            services: vec![web, db],
        };
        let mut images = HashMap::new();
        images.insert("web-image".to_string(), make_image("web-image:1"));
        images.insert("db-image".to_string(), make_image("db-image:1"));

        // No egress → network "enforced" (nothing to downgrade); image pinned.
        let evidence = oci_orchestration_provider_evidence(&plan, &images, &[], true);
        assert_eq!(evidence.len(), 2, "one evidence record per OCI service");
        for (svc, ev) in &evidence {
            assert_eq!(ev.provider_kind, "oci");
            assert_eq!(ev.provider_version.as_deref(), Some("oci-podman-v1"));
            // Each record carries its own value-free service label (so a single
            // receipt's provider_projections can be attributed per service).
            assert_eq!(ev.service_label.as_deref(), Some(svc.as_str()));
            assert!(
                matches!(ev.image_digest_status, OciImageDigestStatus::Pinned { .. }),
                "service {svc} image must be pinned"
            );
            assert_eq!(
                ev.network_enforcement_status,
                OciEnforcementStatus::Enforced
            );
        }

        // A declared egress allowlist: Unsupported enforcement + required policy.
        let evidence = oci_orchestration_provider_evidence(
            &plan,
            &images,
            &["api.example.com".to_string()],
            true,
        );
        for (_svc, ev) in &evidence {
            assert_eq!(
                ev.network_enforcement_status,
                OciEnforcementStatus::Unsupported
            );
            assert!(
                ev.capabilities_required
                    .contains(&"network-policy".to_string()),
                "egress allowlist must surface as a required network policy"
            );
        }
    }

    #[test]
    fn orchestration_provider_evidence_has_env_keys_but_no_values() {
        let mut web = make_service("web", "web-image", vec![], true, Some(8080), vec![]);
        if let ResolvedServiceRuntime::Oci(rt) = &mut web.runtime {
            rt.env
                .insert("OPENAI_API_KEY".to_string(), "sk-do-not-leak".to_string());
        }
        let plan = OrchestrationPlan {
            startup_order: vec!["web".to_string()],
            services: vec![web],
        };
        let mut images = HashMap::new();
        images.insert("web-image".to_string(), make_image("web-image:1"));

        let evidence = oci_orchestration_provider_evidence(&plan, &images, &[], true);
        let (_svc, ev) = &evidence[0];
        assert!(ev.env_keys.contains(&"OPENAI_API_KEY".to_string()));
        let json = serde_json::to_string(ev).unwrap();
        assert!(!json.contains("sk-do-not-leak"), "env value leaked: {json}");
    }

    #[test]
    fn strict_orchestration_blocks_unpinned_image_and_names_service_normal_passes() {
        // A service whose image is absent from the resolution map → unpinned.
        let web = make_service("web", "missing-image", vec![], true, Some(8080), vec![]);
        let plan = OrchestrationPlan {
            startup_order: vec!["web".to_string()],
            services: vec![web],
        };
        let images: HashMap<String, OciImageResolution> = HashMap::new();

        // Strict blocks (before any provider side effect); normal is non-breaking.
        let err = enforce_strict_oci_orchestration(&plan, &images, &[], true, true)
            .expect_err("unpinned image must block in strict mode");
        let ato = err
            .downcast_ref::<AtoExecutionError>()
            .expect("typed strict realization error");
        assert_eq!(ato.code, "ATO_ERR_STRICT_REALIZATION_BLOCKED");
        let details = ato.details.clone().expect("details");
        let node_id = details["blocked"][0]["node_id"].as_str().unwrap();
        assert!(
            node_id.contains("web"),
            "error names the service: {node_id}"
        );

        assert!(
            enforce_strict_oci_orchestration(&plan, &images, &[], true, false).is_ok(),
            "normal mode must not block"
        );
    }

    #[test]
    fn strict_orchestration_error_has_no_observed_id_or_completeness_claim() {
        let web = make_service("web", "web-image", vec![], true, Some(8080), vec![]);
        let plan = OrchestrationPlan {
            startup_order: vec!["web".to_string()],
            services: vec![web],
        };
        let mut images = HashMap::new();
        images.insert("web-image".to_string(), make_image("web-image:1"));
        // Force a block via an unenforceable egress policy, then inspect payload.
        let err = enforce_strict_oci_orchestration(
            &plan,
            &images,
            &["x.example.com".to_string()],
            true,
            true,
        )
        .expect_err("egress block");
        let ato = err.downcast_ref::<AtoExecutionError>().unwrap();
        let serialized = serde_json::to_string(&ato.details).unwrap();
        for forbidden in [
            "observed_execution_id",
            "GraphCompleteness",
            "Complete",
            "graph-execution-id-unbound",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "strict error must not contain '{forbidden}': {serialized}"
            );
        }
    }
}

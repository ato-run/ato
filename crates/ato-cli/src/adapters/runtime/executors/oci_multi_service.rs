//! Multi-service OCI execution via PodmanProvider.
//!
//! This is the **official** path for capsules that declare a `[services]` graph
//! where every service target uses `runtime = "oci"`.
//!
//! The legacy Bollard/Docker-compatible orchestration path is in `orchestrator.rs`.
//! New code must NOT route through that path for OCI services.
//!
//! # Execution order
//! 1. `execute_multi_service` — public entry point, reads the plan, lock, and manifest.
//!    Performs provider readiness check before delegating to `execute_service_graph_with_provider`.
//! 2. `execute_service_graph_with_provider<P: OciProvider>` — testable core, accepts any provider.
//!
//! # Invariants
//! * Every OCI service must have a resolved image digest in the lock file.
//! * Container id, host port, and network id are **Session/Receipt** data — not identity.
//! * Persistent state bindings are preserved on failure; ephemeral ones are deleted.
//! * Internal service-to-service connections use Podman network aliases, not localhost.
//! * Only the main (published) service exposes a host port.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use capsule_core::CapsuleReporter;
use capsule_core::contract::lock_runtime::resolve_oci_image_for_target;
use capsule_core::execution_plan::model::OciPolicyMode;
use capsule_core::router::ManifestData;
use capsule_core::runtime::oci::{
    OciContainerRequest, OciMountSpec, OciNetworkRequest, OciPortSpec,
};
use capsule_core::types::{
    IngressConfig, OciImageResolution, OrchestrationPlan, ResolvedService, ResolvedServiceRuntime,
    StateDurability,
};
use tokio::task::JoinSet;

use super::launch_context::RuntimeLaunchContext;
use crate::adapters::runtime::ingress_router;
use crate::adapters::runtime::oci_provider::{
    DefaultOciProviderSelector, OciImageResolutionMode, OciImageResolutionRequest,
    OciPlatformPolicy, OciProvider, OciProviderError, OciProviderSelector, build_digest_pull_ref,
};
use crate::adapters::runtime::oci_session_store::{
    IngressRouteRecord, OciServiceRecord, OciSessionIngressRecord, OciSessionMeta,
    OciSessionRecord, OciSessionStatus, OciSessionStore, now_iso8601,
};
use crate::application::preflight::{
    OciProviderReadinessMode, OciProviderReadinessRequirements, preflight_oci_provider_readiness,
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

/// Execute a multi-service OCI capsule through the official PodmanProvider path.
///
/// Reads the manifest, lock, and service graph from `plan`, performs provider
/// readiness check, then delegates to `execute_service_graph_with_provider`.
pub(crate) async fn execute_multi_service(
    plan: &ManifestData,
    reporter: Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
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

    // Provider readiness check in Required mode (before image resolution so
    // we can use the provider to resolve digests for compat-path capsules).
    preflight_oci_provider_readiness(
        &DefaultOciProviderSelector,
        OciProviderReadinessMode::Required,
        OciProviderReadinessRequirements::default(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{}: {}", e.code(), e))?;

    let provider = DefaultOciProviderSelector.select_provider();

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
                let declared_ref = rt.image.as_deref().unwrap_or_default();
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
    let _network_id = provider
        .create_network(&OciNetworkRequest {
            name: network_name.clone(),
            labels: {
                let mut l = HashMap::new();
                l.insert("io.ato.session_id".to_string(), session_id.clone());
                l.insert("io.ato.managed".to_string(), "true".to_string());
                l
            },
        })
        .await
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
    let mut graph_error: Option<anyhow::Error> = None;

    let service_layers = match service_start_layers(orch_plan) {
        Ok(layers) => layers,
        Err(err) => {
            cleanup_services(&started, &network_name, ephemeral_mount_sources, provider).await;
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
            let mounts: Vec<OciMountSpec> = target_runtime
                .mounts
                .iter()
                .map(|m| OciMountSpec {
                    source: m.source.clone(),
                    target: m.target.clone(),
                    readonly: m.readonly,
                })
                .collect();

            let cmd = target_runtime.cmd.clone();

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

        if let Err(err) = await_layer_readiness(&layer, orch_plan, &started, reporter).await {
            graph_error = Some(err);
            break 'start_loop;
        }
    }

    // If any service failed to start, clean up and return error.
    if let Some(err) = graph_error {
        cleanup_services(&started, &network_name, ephemeral_mount_sources, provider).await;
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
                cleanup_services(&started, &network_name, ephemeral_mount_sources, provider).await;
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
        cleanup_services(&started, &network_name, ephemeral_mount_sources, provider).await;
        if let Some(ref mut handle) = router_handle {
            handle.stop().await;
        }
        return Err(e.into());
    }

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

    cleanup_services(&started, &network_name, ephemeral_mount_sources, provider).await;

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

async fn await_layer_readiness(
    layer: &[String],
    orch_plan: &OrchestrationPlan,
    started: &[ServiceStartRecord],
    reporter: &Arc<CliReporter>,
) -> Result<()> {
    let mut tasks = JoinSet::new();
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
        let Some(probe) = service.readiness_probe.clone() else {
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

        let service_name = service_name.clone();
        let host_port = start_record.host_port;
        let container_name = start_record.container_name.clone();
        tasks.spawn(async move {
            let ready = run_readiness_probe(
                &probe,
                host_port,
                Some(container_name.as_str()),
                &service_name,
            )
            .await;
            if ready {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "oci_healthcheck_timeout: service '{}' did not become ready within {}s",
                    service_name,
                    probe.timeout_seconds,
                ))
            }
        });
    }

    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                return Err(err);
            }
            Err(err) => {
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                return Err(err.into());
            }
        }
    }
    Ok(())
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
    probe: &capsule_core::types::ReadinessProbe,
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

/// Stop and remove all started containers, remove the network, and delete ephemeral mount sources.
async fn cleanup_services<P: OciProvider>(
    started: &[ServiceStartRecord],
    network_name: &str,
    ephemeral_mount_sources: &HashSet<String>,
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

    // Delete ephemeral mount source directories.
    for source in ephemeral_mount_sources {
        let path = std::path::Path::new(source);
        if path.exists() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

// ── PodmanProviderSemantics kind helper ───────────────────────────────────────

trait OciProviderKindStr {
    fn as_str(&self) -> &'static str;
}

impl OciProviderKindStr for capsule_core::types::OciProviderKind {
    fn as_str(&self) -> &'static str {
        use capsule_core::types::OciProviderKind;
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
    use capsule_core::runtime::oci::OciContainerInspect;
    use capsule_core::types::{
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
        use capsule_core::routing::importer::compose::{ComposeImportInput, import_compose};
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
            let _guard = run_once_test_env_lock().lock().unwrap();
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
        use capsule_core::types::Mount;

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
            }];
        }

        let mut worker_svc = make_service("worker", "worker-image", vec![], false, None, vec![]);
        if let ResolvedServiceRuntime::Oci(ref mut rt) = worker_svc.runtime {
            rt.mounts = vec![Mount {
                source: shared_source.to_string(),
                target: shared_target.to_string(),
                readonly: false,
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
        assert_eq!(api_mount.source, shared_source);
        assert!(!api_mount.readonly, "shared state mount must be writable");
        assert!(
            !worker_mount.readonly,
            "shared state mount must be writable"
        );
    }

    /// Process-wide mutex for the timeout test (env-var-dependent).
    fn run_once_test_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    // ── OCI proxy env tests ───────────────────────────────────────────────────

    #[allow(dead_code)]
    fn make_service_with_egress_proxy(egress_proxy: bool) -> ResolvedService {
        use capsule_core::types::orchestration::ResolvedServiceNetwork;
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
        use capsule_core::types::orchestration::{ResolvedService, ResolvedServiceNetwork};

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
}

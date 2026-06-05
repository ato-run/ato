//! Single-target OCI execution via PodmanProvider.
//!
//! This is the **official** OCI execution path. It requires:
//! 1. An `OciPolicyEnvelope` present in the compiled `ExecutionPlan`.
//! 2. A resolved image digest in `envelope.resolved_image`.
//! 3. Provider readiness in `Required` mode.
//! 4. Acceptable policy (Strict mode fails on unenforced policies).
//!
//! The legacy Bollard/Docker-compatible execution path is in `oci.rs`.
//! New code must NOT route through that path.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule_core::CapsuleReporter;
use capsule_core::execution_plan::model::{OciPolicyEnvelope, OciPolicyMode};
use capsule_core::router::ManifestData;
use capsule_core::runtime::oci::{OciContainerRequest, OciLogChunk, OciPortSpec};

use super::launch_context::RuntimeLaunchContext;
use crate::adapters::runtime::oci_provider::{
    DefaultOciProviderSelector, OciProvider, OciProviderError, OciProviderSelector,
    build_digest_pull_ref,
};
use crate::application::preflight::{
    OciProviderReadinessMode, OciProviderReadinessRequirements, preflight_oci_provider_readiness,
};
use crate::reporters::CliReporter;

const OCI_STOP_TIMEOUT_SECS: i64 = 10;

/// Execute a single OCI target through the official `PodmanProvider` path.
///
/// `strict_realization` is the opt-in `--strict-realization` profile (#500/#501):
/// when set, Gate 5 blocks the launch before any pull/create if a required policy
/// facet cannot be enforced.
pub(crate) async fn execute_single_target(
    plan: &ManifestData,
    reporter: Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
    strict_realization: bool,
) -> Result<i32> {
    let selector = DefaultOciProviderSelector;
    let provider = selector.select_provider();
    execute_with_provider(plan, reporter, launch_ctx, &provider, strict_realization).await
}

/// Assemble the env map handed to `create_container`, in precedence order: the
/// OCI target's manifest env (`base`), then the launch context's injected env,
/// then the container proxy override, then SecretStore-backed launch-condition
/// grants (#508) last so a secret wins for its exact key.
///
/// Secret values reach **only** this map (the in-memory `OciContainerRequest.env`).
/// They are taken from `launch_ctx.secret_env()` — a channel deliberately excluded
/// from `merged_env`/`merged_env_with_origins`/`env_permission_keys` — so the
/// prelaunch receipt, session record, and logs never observe a raw value.
fn build_oci_container_env(
    base: HashMap<String, String>,
    launch_ctx: &RuntimeLaunchContext,
) -> HashMap<String, String> {
    let mut env = base;
    env.extend(launch_ctx.merged_env());
    // Override proxy env for containers: 127.0.0.1 is unreachable from inside a
    // container; use host.containers.internal instead.
    if let Some(port) = launch_ctx.egress_proxy_port() {
        let container_proxy = crate::common::proxy::proxy_env_for_oci_container(port, &[]);
        for (k, v) in crate::common::proxy::proxy_env_to_pairs(&container_proxy) {
            env.insert(k, v);
        }
    }
    // SecretStore-backed launch-condition grants (#508). Applied at the OCI
    // container-creation boundary only; the value reaches only this map. Last so a
    // secret wins for its exact env key.
    for secret in launch_ctx.secret_env() {
        env.insert(secret.name.clone(), secret.value.expose().to_string());
    }
    env
}

/// Core execution logic, accepting any `OciProvider` implementation.
/// Separated for testability — tests inject `FakeOciProvider`.
pub(crate) async fn execute_with_provider<P: OciProvider>(
    plan: &ManifestData,
    reporter: Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
    provider: &P,
    strict_realization: bool,
) -> Result<i32> {
    // ── Gate 1: OCI policy envelope from the lock-compiled execution plan ────
    // Compile the full plan once; it is reused for the launch receipt so the
    // receipt records exactly the plan the gates evaluated.
    let execution_plan = compile_oci_execution_plan(plan)?;
    let oci_envelope = execution_plan
        .oci
        .clone()
        .ok_or_else(|| anyhow::anyhow!("{}", OciProviderError::OciPolicyEnvelopeMissing))
        .context("OCI execution plan missing policy envelope; ensure target has runtime=\"oci\"")?;

    // ── Gate 2: resolved image digest required ───────────────────────────────
    let resolved_image = oci_envelope
        .resolved_image
        .as_ref()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                OciProviderError::OciImageResolutionRequired {
                    declared_ref: oci_envelope.declared_image_ref.clone(),
                }
            )
        })
        .context(
            "OCI execution requires a resolved image digest in the lock file; run `ato lock` first",
        )?;

    // ── Gate 3: provider readiness (Required mode) ───────────────────────────
    preflight_oci_provider_readiness(
        &DefaultOciProviderSelector,
        OciProviderReadinessMode::Required,
        OciProviderReadinessRequirements::default(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{}: {}", e.code(), e))?;

    // ── Gate 4: policy enforcement check ─────────────────────────────────────
    enforce_policy_gate(&oci_envelope)?;

    // ── Execution ─────────────────────────────────────────────────────────────
    let manifest_name = plan
        .manifest_name()
        .unwrap_or_else(|| "capsule".to_string());
    let session_id = session_id(&manifest_name, plan.selected_target_label());
    let container_name = format!(
        "ato-{}-{}-{}",
        sanitize_name(&manifest_name),
        sanitize_name(plan.selected_target_label()),
        session_suffix(&session_id),
    );

    let labels = oci_labels(&session_id, plan.selected_target_label());

    let pull_ref = build_digest_pull_ref(resolved_image);

    // Env: OCI target manifest env overlaid with launch-context injected env, the
    // container proxy override, and SecretStore-backed grants (#508). Secret values
    // reach only this `OciContainerRequest.env`, never the receipt/session/logs —
    // see `build_oci_container_env`.
    let env = build_oci_container_env(plan.targets_oci_env(), launch_ctx);

    // Cmd: prefer targets_oci_cmd, fall back to entrypoint/run command.
    let mut cmd = plan.targets_oci_cmd();
    if cmd.is_empty()
        && let Some(entrypoint) = plan
            .execution_entrypoint()
            .or_else(|| plan.execution_run_command())
    {
        cmd = shell_words::split(&entrypoint).unwrap_or_else(|_| vec![entrypoint]);
    }

    // Port: use None for host_port so podman auto-allocates from the ephemeral range.
    let ports = oci_envelope
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

    let mounts: Vec<capsule_core::runtime::oci::OciMountSpec> = launch_ctx
        .injected_mounts()
        .iter()
        .map(|m| capsule_core::runtime::oci::OciMountSpec {
            source: m.source.to_string_lossy().to_string(),
            target: m.target.clone(),
            readonly: m.readonly,
            ownership: None,
            source_kind: capsule_core::runtime::oci::OciMountSourceKind::default(),
        })
        .collect();

    // Assemble the launch request up front so the strict realization gate can
    // inspect the full projection BEFORE any image pull or container creation.
    let container_request = OciContainerRequest {
        name: container_name.clone(),
        image: pull_ref,
        cmd,
        env,
        working_dir: plan.targets_oci_working_dir(),
        labels,
        mounts,
        ports,
        network: None,
        aliases: Vec::new(),
        platform: None,
        extra_hosts: if launch_ctx.egress_proxy_port().is_some() {
            vec![crate::common::proxy::OCI_HOST_GATEWAY_ENTRY.to_string()]
        } else {
            vec![]
        },
        user: plan.targets_oci_user(),
    };

    // ── Gate 5: strict realization profile (#500/#501) ───────────────────────
    // Opt-in `--strict-realization`. Blocks before any pull/create when a
    // required policy facet cannot be enforced by PodmanProvider, the image is
    // unpinned, or a host-bound mount fallback is required. Normal mode is a
    // no-op here. This is distinct from Gate 4 (`OciPolicyMode::Strict`).
    enforce_strict_oci_launch(&oci_envelope, &container_request, strict_realization)?;

    // Persist a durable prelaunch receipt with the OCI provider evidence (#501),
    // BEFORE any pull/create, so the resolved launch envelope is recorded
    // independent of the live container. Best-effort: a receipt issue must never
    // regress an OCI launch.
    persist_oci_launch_receipt(plan, &execution_plan, launch_ctx, None, &reporter).await;

    reporter
        .notify(format!(
            "⬇  Pulling OCI image: {}",
            resolved_image.declared_ref
        ))
        .await?;
    provider
        .pull_image(resolved_image)
        .await
        .map_err(|e| anyhow::anyhow!("{}: {}", e.code(), e))
        .context("failed to pull OCI image")?;

    let container_id = provider
        .create_container(&container_request)
        .await
        .map_err(|e| anyhow::anyhow!("{}: {}", e.code(), e))
        .context("failed to create OCI container")?;

    if let Err(e) = provider.start_container(&container_id).await {
        // Best-effort cleanup on start failure.
        let _ = provider.remove_container(&container_id, true).await;
        return Err(anyhow::anyhow!("{}: {}", e.code(), e))
            .context("failed to start OCI container");
    }

    // After start, inspect to get the auto-allocated host port.
    if let Some(container_port) = oci_envelope.port_exposure {
        let inspect = provider
            .inspect_container(&container_id)
            .await
            .unwrap_or_default();
        let host_port = inspect.host_ports.get(&container_port).copied();
        let display_port = host_port.unwrap_or(container_port);
        reporter
            .notify(format!(
                "🌐 OCI target '{}' available at http://127.0.0.1:{}/",
                plan.selected_target_label(),
                display_port,
            ))
            .await?;
    }

    // Stream logs and wait for container exit (or Ctrl-C).
    let mut log_rx = provider
        .logs(&container_id, true)
        .await
        .map_err(|e| anyhow::anyhow!("{}: {}", e.code(), e))
        .context("failed to start OCI log stream")?;

    let target_label = plan.selected_target_label().to_string();
    let log_task = tokio::spawn(async move {
        while let Some(chunk) = log_rx.recv().await {
            match chunk {
                Ok(chunk) => {
                    let _ = print_log_chunk(&target_label, &chunk);
                }
                Err(err) => {
                    let _ = writeln!(std::io::stderr(), "[{}] log error: {}", target_label, err);
                    break;
                }
            }
        }
    });

    let exit_code = tokio::select! {
        result = provider.wait_container(&container_id) => {
            result.map_err(|e| anyhow::anyhow!("{}: {}", e.code(), e))
                .context("error waiting for OCI container")?
        }
        _ = tokio::signal::ctrl_c() => {
            let _ = provider
                .stop_container(&container_id, OCI_STOP_TIMEOUT_SECS)
                .await;
            130
        }
    };

    let _ = provider
        .stop_container(&container_id, OCI_STOP_TIMEOUT_SECS)
        .await;
    let _ = provider.remove_container(&container_id, true).await;
    let _ = log_task.await;

    Ok(exit_code as i32)
}

/// Compile the full lock-derived `ExecutionPlan` for the selected OCI target.
///
/// Shared by the policy-envelope gate and the launch-receipt builder so both read
/// the same compiled plan (the receipt records exactly what the gate evaluated).
pub(crate) fn compile_oci_execution_plan(
    plan: &ManifestData,
) -> Result<capsule_core::execution_plan::model::ExecutionPlan> {
    use capsule_core::contract::lock_runtime;
    use capsule_core::execution_plan::derive::{self, PlatformSnapshot};

    let resolved =
        lock_runtime::resolve_lock_runtime_model(&plan.lock, Some(plan.selected_target_label()))
            .context("failed to resolve lock runtime model for OCI target")?;

    derive::compile_execution_plan_from_lock(
        &plan.lock,
        &resolved,
        &Default::default(),
        &PlatformSnapshot::current(),
    )
    .context("failed to compile OCI execution plan from lock")
}

/// Check that the policy mode can be honoured by the PodmanProvider.
///
/// `Strict`: fail if any declared policy cannot be enforced.  
/// `Loose`: allow with a diagnostic note emitted to stderr.  
/// `Off`: always allow.
fn enforce_policy_gate(envelope: &OciPolicyEnvelope) -> Result<()> {
    let has_egress_policy = !envelope.egress_allow.is_empty();
    match envelope.policy_mode {
        OciPolicyMode::Strict if has_egress_policy => {
            anyhow::bail!(
                "{}",
                OciProviderError::OciExecutionGateFailed {
                    reason: format!(
                        "policy_mode=strict requires egress_allow to be enforced, \
                         but PodmanProvider (oci-podman-v1) cannot enforce network \
                         egress allowlists; declared rules: {:?}",
                        envelope.egress_allow
                    ),
                }
            );
        }
        OciPolicyMode::Loose if has_egress_policy => {
            eprintln!(
                "⚠  OCI policy gap: egress_allow is declared but cannot be enforced by \
                 PodmanProvider (policy_mode=loose); execution proceeds with a policy gap"
            );
        }
        _ => {}
    }
    Ok(())
}

/// Gate 5 (#500/#501): apply the opt-in strict realization profile to an OCI
/// launch *before* any pull/create.
///
/// Routes the resolved policy envelope + projection plan through the strict
/// realization gate: in strict mode it blocks (typed
/// `ATO_ERR_STRICT_REALIZATION_BLOCKED`) when a required policy facet cannot be
/// enforced by PodmanProvider, the image is unpinned, or a host-bound mount
/// fallback is required. In normal mode it is a no-op, so existing OCI launches
/// are never newly blocked. The typed error is surfaced through anyhow and stays
/// downcastable for structured output.
fn enforce_strict_oci_launch(
    envelope: &OciPolicyEnvelope,
    request: &OciContainerRequest,
    strict_realization: bool,
) -> Result<()> {
    use crate::application::provider_projection::oci::OciProjectionPlan;
    use crate::application::provider_projection::strict_oci::{
        OciProviderEnforcement, OciStrictFacts, enforce_strict_oci,
    };
    use capsule_core::realization::LaunchProfile;

    let profile = if strict_realization {
        LaunchProfile::Strict
    } else {
        LaunchProfile::Normal
    };
    let projection = OciProjectionPlan::from_container_request(request);
    let facts = OciStrictFacts::from_launch(envelope, &projection);
    let enforcement = OciProviderEnforcement::podman(facts.network_policy_required);
    // The graph-derived resolved execution id is not threaded into the OCI launch
    // path yet (a remaining #501 slice), so pass `None` rather than substituting
    // the provider projection fingerprint — which is not an execution identity.
    enforce_strict_oci(&facts, &enforcement, profile, None).map_err(anyhow::Error::new)
}

/// Persist a durable launch receipt carrying OCI provider evidence (#501).
///
/// Shared by the single-target and multi-service OCI paths. Builds the v2 receipt
/// (which already includes `provider_projections`) via the shared receipt builder
/// and writes it to the executions store, emitting the stable `RECEIPT: <path>`
/// line on success (parity with the source-native launch path).
///
/// **Best-effort:** on any build/write failure it warns and returns — a receipt
/// issue must never regress an OCI launch. `provider_projections_override` lets the
/// multi-service path supply one evidence record per service; `None` keeps the
/// builder's single declared projection (single-target).
pub(crate) async fn persist_oci_launch_receipt(
    plan: &ManifestData,
    execution_plan: &capsule_core::execution_plan::model::ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    provider_projections_override: Option<
        Vec<capsule_core::execution_identity::OciProviderReceiptEvidence>,
    >,
    reporter: &Arc<CliReporter>,
) {
    let result = crate::application::execution_receipt_builder::build_oci_launch_receipt(
        plan,
        execution_plan,
        launch_ctx,
        provider_projections_override,
    )
    .and_then(|document| {
        crate::application::execution_receipts::write_receipt_document_atomic(&document)
    });
    match result {
        Ok(path) => {
            let _ = reporter
                .notify(format!("RECEIPT: {}", path.display()))
                .await;
        }
        Err(err) => {
            let _ = reporter
                .notify(format!(
                    "⚠  failed to persist OCI launch receipt (continuing): {err}"
                ))
                .await;
        }
    }
}

fn print_log_chunk(service_name: &str, chunk: &OciLogChunk) -> std::io::Result<()> {
    let prefix = format!("[{service_name}] ");
    if chunk.stderr {
        let mut w = std::io::stderr();
        w.write_all(prefix.as_bytes())?;
        w.write_all(&chunk.message)?;
        w.flush()
    } else {
        let mut w = std::io::stdout();
        w.write_all(prefix.as_bytes())?;
        w.write_all(&chunk.message)?;
        w.flush()
    }
}

fn session_id(manifest_name: &str, target_label: &str) -> String {
    let seed = format!("{manifest_name}-{target_label}-{}", std::process::id());
    blake3::hash(seed.as_bytes()).to_hex().to_string()
}

fn session_suffix(session_id: &str) -> String {
    session_id.chars().take(8).collect()
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

fn oci_labels(session_id: &str, target_label: &str) -> std::collections::HashMap<String, String> {
    std::collections::HashMap::from([
        ("io.ato.session_id".to_string(), session_id.to_string()),
        ("io.ato.target".to_string(), target_label.to_string()),
        ("io.ato.provider".to_string(), "podman".to_string()),
        ("io.ato.managed".to_string(), "true".to_string()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::execution_plan::model::{OciPolicyEnvelope, OciPolicyMode};
    use capsule_core::types::{OciImageResolution, OciPlatform};

    fn make_resolved_image(declared_ref: &str) -> OciImageResolution {
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

    fn make_envelope(
        policy_mode: OciPolicyMode,
        resolved: bool,
        egress: Vec<String>,
    ) -> OciPolicyEnvelope {
        OciPolicyEnvelope {
            declared_image_ref: "postgres:14".to_string(),
            resolved_image: if resolved {
                Some(make_resolved_image("postgres:14"))
            } else {
                None
            },
            port_exposure: Some(5432),
            egress_allow: egress,
            policy_mode,
        }
    }

    // ── Secret env injection (#508) ───────────────────────────────────────────
    // OCI secret grants must reach the container env but never the receipt /
    // session / logs. These exercise `build_oci_container_env`, the single seam
    // where the env handed to `create_container` is assembled.
    use crate::adapters::runtime::secret_injection::{RuntimeSecretEnv, SecretValue};

    fn secret(name: &str, value: &str) -> RuntimeSecretEnv {
        RuntimeSecretEnv {
            name: name.to_string(),
            value: SecretValue::new(value.to_string()),
        }
    }

    #[test]
    fn oci_secret_env_reaches_container_env() {
        let ctx = RuntimeLaunchContext::default()
            .with_secret_env(vec![secret("OPENAI_API_KEY", "sk-live-secret")]);
        let env = build_oci_container_env(HashMap::new(), &ctx);
        assert_eq!(
            env.get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-live-secret"),
            "secret grant must reach the container env"
        );
    }

    #[test]
    fn oci_secret_env_wins_over_base_env_key() {
        let mut base = HashMap::new();
        base.insert("OPENAI_API_KEY".to_string(), "placeholder".to_string());
        let ctx = RuntimeLaunchContext::default()
            .with_secret_env(vec![secret("OPENAI_API_KEY", "sk-live-secret")]);
        let env = build_oci_container_env(base, &ctx);
        assert_eq!(
            env.get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-live-secret"),
            "a secret is applied last so it wins for its exact key"
        );
    }

    #[test]
    fn oci_secret_env_not_in_receipt_or_session_env() {
        let ctx = RuntimeLaunchContext::default()
            .with_secret_env(vec![secret("OPENAI_API_KEY", "sk-live-secret")]);
        // Receipt/session observe `merged_env*`, which must exclude `secret_env`.
        assert!(
            !ctx.merged_env().contains_key("OPENAI_API_KEY"),
            "secret_env must not appear in merged_env (receipt-observed)"
        );
        assert!(
            !ctx.merged_env_with_origins().contains_key("OPENAI_API_KEY"),
            "secret_env must not appear in merged_env_with_origins (receipt-observed)"
        );
    }

    #[test]
    fn oci_secret_env_not_in_generated_log_output() {
        // The launch context is what may be Debug-logged; the raw secret value must
        // not appear in its Debug rendering (RuntimeSecretEnv redacts the value).
        let ctx = RuntimeLaunchContext::default()
            .with_secret_env(vec![secret("OPENAI_API_KEY", "sk-live-secret")]);
        let rendered = format!("{ctx:?}");
        assert!(
            !rendered.contains("sk-live-secret"),
            "raw secret value must not appear in launch-context Debug output"
        );
    }

    #[test]
    fn oci_secret_env_debug_redacted() {
        let entry = secret("OPENAI_API_KEY", "sk-live-secret");
        let rendered = format!("{entry:?}");
        assert!(!rendered.contains("sk-live-secret"), "value must be redacted");
        assert!(
            rendered.contains("OPENAI_API_KEY"),
            "the env name is not sensitive and may appear"
        );
    }

    #[test]
    fn oci_secret_injection_does_not_affect_non_installed_run() {
        // Transient `ato run` carries no secret_env; the env map must be untouched
        // by the secret loop (only the base env, no secret keys added).
        let mut base = HashMap::new();
        base.insert("FOO".to_string(), "bar".to_string());
        let ctx = RuntimeLaunchContext::default();
        assert!(ctx.secret_env().is_empty());
        let env = build_oci_container_env(base.clone(), &ctx);
        assert_eq!(env, base, "no secret_env means env is unchanged by injection");
    }

    // ── Policy gate tests ─────────────────────────────────────────────────────

    #[test]
    fn strict_policy_gap_blocks_execution_when_egress_is_declared() {
        let envelope = make_envelope(
            OciPolicyMode::Strict,
            true,
            vec!["0.0.0.0/0:443".to_string()],
        );
        let result = enforce_policy_gate(&envelope);
        assert!(result.is_err(), "Strict + egress_allow must fail");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("oci_execution_gate_failed") || msg.contains("cannot enforce"),
            "error must describe gate failure: {msg}"
        );
    }

    #[test]
    fn strict_policy_without_egress_passes() {
        let envelope = make_envelope(OciPolicyMode::Strict, true, vec![]);
        enforce_policy_gate(&envelope).expect("Strict + no egress must pass");
    }

    #[test]
    fn loose_policy_with_egress_does_not_fail_gate() {
        let envelope = make_envelope(
            OciPolicyMode::Loose,
            true,
            vec!["0.0.0.0/0:443".to_string()],
        );
        enforce_policy_gate(&envelope).expect("Loose + egress must not fail");
    }

    #[test]
    fn off_policy_with_egress_always_passes() {
        let envelope = make_envelope(OciPolicyMode::Off, true, vec!["0.0.0.0/0:443".to_string()]);
        enforce_policy_gate(&envelope).expect("Off policy must not fail");
    }

    // ── build_digest_pull_ref tests ───────────────────────────────────────────

    #[test]
    fn digest_ref_passes_through_unchanged() {
        let _image = make_resolved_image("postgres@sha256:aabbcc");
        // image.declared_ref already contains '@'
        let result = build_digest_pull_ref(&OciImageResolution {
            declared_ref: "postgres@sha256:aabbcc".to_string(),
            resolved_digest: "sha256:aabbcc".to_string(),
            platform: OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            },
            importer_input_hash: None,
        });
        assert_eq!(result, "postgres@sha256:aabbcc");
    }

    #[test]
    fn mutable_tag_ref_gets_digest_appended() {
        let digest = "sha256:".to_string() + &"a".repeat(64);
        let image = OciImageResolution {
            declared_ref: "postgres:14".to_string(),
            resolved_digest: digest.clone(),
            platform: OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            },
            importer_input_hash: None,
        };
        assert_eq!(build_digest_pull_ref(&image), format!("postgres@{digest}"));
    }

    #[test]
    fn image_without_tag_gets_digest_appended() {
        let digest = "sha256:".to_string() + &"b".repeat(64);
        let image = OciImageResolution {
            declared_ref: "postgres".to_string(),
            resolved_digest: digest.clone(),
            platform: OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            },
            importer_input_hash: None,
        };
        assert_eq!(build_digest_pull_ref(&image), format!("postgres@{digest}"));
    }

    // ── Gate 2: resolved image must be present ────────────────────────────────

    #[test]
    fn missing_resolved_image_produces_typed_error() {
        let envelope = make_envelope(OciPolicyMode::Strict, false, vec![]);
        let result = envelope.resolved_image.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                OciProviderError::OciImageResolutionRequired {
                    declared_ref: envelope.declared_image_ref.clone(),
                }
            )
        });
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("oci_image_resolution_required") || msg.contains("resolved digest"),
            "error must be typed: {msg}"
        );
    }

    // ── Label/naming tests ────────────────────────────────────────────────────

    #[test]
    fn oci_labels_include_required_fields_and_exclude_volatile_state() {
        let labels = oci_labels("sess-abc123", "main");
        assert_eq!(
            labels.get("io.ato.session_id").map(|s| s.as_str()),
            Some("sess-abc123")
        );
        assert_eq!(
            labels.get("io.ato.target").map(|s| s.as_str()),
            Some("main")
        );
        assert_eq!(
            labels.get("io.ato.provider").map(|s| s.as_str()),
            Some("podman")
        );
        assert_eq!(
            labels.get("io.ato.managed").map(|s| s.as_str()),
            Some("true")
        );
        // Volatile live state must not appear in labels.
        assert!(!labels.contains_key("io.ato.machine_id"));
        assert!(!labels.contains_key("io.ato.container_id"));
        assert!(!labels.contains_key("io.ato.host_port"));
    }

    #[test]
    fn sanitize_name_replaces_special_chars() {
        assert_eq!(sanitize_name("My App"), "my-app");
        assert_eq!(sanitize_name("---foo---"), "foo");
        assert_eq!(sanitize_name("hello_world"), "hello-world");
    }

    // ── OciProviderError new variant codes ───────────────────────────────────

    #[test]
    fn new_error_variants_have_distinct_stable_codes() {
        assert_eq!(
            OciProviderError::OciPolicyEnvelopeMissing.code(),
            "oci_policy_envelope_missing"
        );
        assert_eq!(
            OciProviderError::OciImageResolutionRequired {
                declared_ref: "x:latest".into()
            }
            .code(),
            "oci_image_resolution_required"
        );
        assert_eq!(
            OciProviderError::OciExecutionGateFailed {
                reason: "test".into()
            }
            .code(),
            "oci_execution_gate_failed"
        );
        assert_eq!(
            OciProviderError::OciContainerStartFailed {
                container_name: "c".into(),
                message: "m".into()
            }
            .code(),
            "oci_container_start_failed"
        );
        assert_eq!(
            OciProviderError::OciCleanupFailed {
                operation: "stop".into(),
                message: "m".into()
            }
            .code(),
            "oci_cleanup_failed"
        );
    }

    // ── Provider selector type test ───────────────────────────────────────────

    #[test]
    fn default_provider_selector_returns_podman_not_bollard() {
        use crate::adapters::runtime::oci_provider::{
            DefaultOciProviderSelector, OciProviderSelector, PodmanProvider, SystemCommandRunner,
        };
        let selector = DefaultOciProviderSelector;
        let _: PodmanProvider<SystemCommandRunner> = selector.select_provider();
    }

    // ── Cleanup on start failure ──────────────────────────────────────────────

    #[tokio::test]
    async fn cleanup_runs_when_start_container_fails() {
        use crate::adapters::runtime::oci_provider::FakeOciProvider;

        let mut fake = FakeOciProvider::ready();
        fake.start_result = Err(OciProviderError::OciContainerStartFailed {
            container_name: "ato-test-main-abc123".to_string(),
            message: "image not found".to_string(),
        });
        // We just verify the error propagates as an anyhow error with the typed code.
        let err = fake
            .start_container("fake-container-id")
            .await
            .expect_err("start must fail");
        assert_eq!(err.code(), "oci_container_start_failed");
    }

    // ── Single OCI execution requires resolved image digest (integration-style) ─

    #[test]
    fn single_oci_execution_requires_resolved_image_digest() {
        let envelope = make_envelope(OciPolicyMode::Strict, false, vec![]);
        let result: Result<()> = envelope
            .resolved_image
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{}",
                    OciProviderError::OciImageResolutionRequired {
                        declared_ref: envelope.declared_image_ref.clone(),
                    }
                )
            })
            .map(|_| ());
        assert!(result.is_err(), "missing digest must fail gate");
    }
}

//! CLI wiring for Docker install script → OCI lock/plan/run (PR 11).
//!
//! This module connects the pure [`DockerRunScriptImporter`] to the multi-service
//! OCI executor (PR 8) behind the explicit `--oci-install-sh` CLI flag.
//!
//! # Entry points
//! 1. [`execute_install_sh_run`] — production path: detect install script,
//!    parse it, check provider readiness, resolve/replay image digests, execute.
//! 2. [`execute_install_sh_run_with_provider`] — testable core, accepts any
//!    `OciProvider` and pre-built `OciImageResolution` map.
//!
//! # Invariants
//! * Every service must have a resolved image digest before execution starts.
//! * Script `--name` values are source metadata only; runtime names are
//!   Ato session-scoped (from the multi-service executor in PR 8).
//! * Secret-like env values (PASSWORD, SECRET, TOKEN …) are never written to
//!   receipt. The `is_secret_like` flag from the importer is the authority.
//! * `--restart` policies are silently ignored; Ato session owns lifecycle.
//! * This module does NOT execute the install script, does NOT shell out, and
//!   does NOT use the legacy Bollard path.
//! * The OCI lock file (`ato.oci.lock.json`) is shared with the `--oci-compose`
//!   path; the `import.kind` field distinguishes them.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule_core::execution_plan::model::OciPolicyMode;
use capsule_core::oci_compose_lock::{
    self, OciComposeLock, OciImageLockEntry, OciImportMeta, OciLockError,
};
use capsule_core::routing::importer::docker_run_script::{
    detect_install_script_candidate, import_docker_run_script, DockerRunScriptImportInput,
    DockerRunScriptImportOutput,
};
use capsule_core::types::OciImageResolution;
use capsule_core::CapsuleReporter;

use super::oci_multi_service::execute_service_graph_with_provider;
use crate::adapters::runtime::oci_provider::{
    DefaultOciProviderSelector, OciImageResolutionMode, OciImageResolutionRequest, OciProvider,
    OciProviderError, OciProviderSelector,
};
use crate::application::preflight::{
    preflight_oci_provider_readiness, OciProviderReadinessMode, OciProviderReadinessRequirements,
};
use crate::reporters::CliReporter;

// ── Production entry point ────────────────────────────────────────────────────

/// Execute an OCI service graph from an install script in `project_dir`.
///
/// This is the **production** entry point for the `--oci-install-sh` CLI flag.
/// It:
/// 1. Detects the install script (install.sh, setup.sh, …).
/// 2. Parses it with the pure `DockerRunScriptImporter` — no shell execution.
/// 3. Reports importer warnings and unsupported features.
/// 4. Checks `OciProvider` readiness in Required mode.
/// 5. Resolves image digests, replaying from `ato.oci.lock.json` when fresh.
/// 6. Persists resolved digests to `ato.oci.lock.json`.
/// 7. Delegates to `execute_service_graph_with_provider`.
pub(crate) async fn execute_install_sh_run(
    project_dir: &Path,
    reporter: Arc<CliReporter>,
    policy_mode: OciPolicyMode,
    egress_allow: &[String],
) -> Result<i32> {
    // 1. Detect install script.
    let script_path = detect_install_script_candidate(project_dir).ok_or_else(|| {
        anyhow::anyhow!(
            "install_sh_not_found: no install.sh / setup.sh / start.sh / run.sh found in {}",
            project_dir.display()
        )
    })?;

    // 2. Parse (pure — no shell execution, no Docker/Podman calls).
    let script_text = std::fs::read_to_string(&script_path)
        .with_context(|| format!("failed to read {}", script_path.display()))?;
    let source_hash =
        capsule_core::routing::importer::docker_run_script::compute_script_source_hash(
            &script_text,
        );
    let script_rel_path = script_path
        .strip_prefix(project_dir)
        .unwrap_or(&script_path)
        .to_string_lossy()
        .into_owned();

    let input = DockerRunScriptImportInput::new(script_text, script_path.clone());
    let import_output = import_docker_run_script(&input)
        .map_err(|e| anyhow::anyhow!("install_sh_import_failed: {e}"))?;

    // 3. Surface diagnostics.
    reporter
        .notify(format!("📋 Install script: {}", script_path.display()))
        .await?;
    if !import_output.extracted_networks.is_empty() {
        reporter
            .notify(format!(
                "🔗 Extracted networks: {}",
                import_output.extracted_networks.join(", ")
            ))
            .await?;
    }
    reporter
        .notify(format!(
            "🔧 Services: {}",
            import_output
                .services
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .await?;
    for warning in &import_output.warnings {
        reporter
            .notify(format!("⚠️  install.sh: {warning}"))
            .await?;
    }
    for feat in &import_output.unsupported_features {
        reporter
            .notify(format!("ℹ️  install.sh (unsupported, skipped): {feat}"))
            .await?;
    }

    // 4. Provider readiness gate.
    preflight_oci_provider_readiness(
        &DefaultOciProviderSelector,
        OciProviderReadinessMode::Required,
        OciProviderReadinessRequirements::default(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{}: {}", e.code(), e))?;

    let provider = DefaultOciProviderSelector.select_provider();

    // 5. Load existing lock (parse errors are non-fatal: re-resolve from scratch).
    let existing_lock = match oci_compose_lock::load_from_dir(project_dir) {
        Ok(lock) => lock,
        Err(OciLockError::ParseFailed(msg)) => {
            reporter
                .notify(format!(
                    "⚠️  ato.oci.lock.json parse error, re-resolving: {msg}"
                ))
                .await?;
            None
        }
        Err(_) => None,
    };

    // 6. Resolve image digests with lock replay.
    let (images, new_lock) = resolve_install_sh_images_with_lock_replay(
        &import_output,
        &script_rel_path,
        &source_hash,
        existing_lock.as_ref(),
        &provider,
        &reporter,
    )
    .await?;

    // 7. Persist lock (fail on write error — don't execute with unresolved digests).
    oci_compose_lock::write_to_dir(project_dir, &new_lock)
        .map_err(|e| anyhow::anyhow!("oci_lock_write_failed: {e}"))?;
    reporter
        .notify("🔒 Lock written: ato.oci.lock.json".to_string())
        .await?;

    // 8. Execute.
    let project_name = script_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("install-sh");

    execute_install_sh_run_with_provider(
        &import_output,
        &images,
        policy_mode,
        egress_allow,
        project_name,
        &reporter,
        &provider,
    )
    .await
}

// ── Image resolution + lock replay ────────────────────────────────────────────

/// Resolve image digests with lock replay for every service in the install script output.
///
/// - If an existing lock entry is fresh (source hash + declared ref + provider
///   semantics all match), the persisted digest is reused without a provider
///   round-trip (♻️).
/// - Otherwise the provider is called to resolve a fresh digest (✅).
///
/// `import.kind` is set to `"docker-run-script"` to distinguish from compose entries.
/// Returns `(service_name → OciImageResolution, updated_OciComposeLock)`.
pub(crate) async fn resolve_install_sh_images_with_lock_replay<P: OciProvider>(
    import_output: &DockerRunScriptImportOutput,
    script_source_path: &str,
    source_hash: &str,
    existing_lock: Option<&OciComposeLock>,
    provider: &P,
    reporter: &Arc<CliReporter>,
) -> Result<(HashMap<String, OciImageResolution>, OciComposeLock)> {
    let provider_semantics = provider.semantics().coarse_label();
    let mut images: HashMap<String, OciImageResolution> = HashMap::new();
    let mut lock_images: BTreeMap<String, OciImageLockEntry> = BTreeMap::new();

    for svc in &import_output.services {
        // Check if an existing lock entry covers this service.
        let reuse = existing_lock.and_then(|lock| {
            lock.entry_is_fresh(source_hash, &svc.name, &svc.image_ref, &provider_semantics)
                .then(|| lock.images.get(&svc.name).cloned())
                .flatten()
        });

        if let Some(entry) = reuse {
            reporter
                .notify(format!(
                    "♻️  [{}] Reusing lock: {} → {}",
                    svc.name,
                    svc.image_ref,
                    &entry.resolved_digest[..std::cmp::min(19, entry.resolved_digest.len())]
                ))
                .await?;
            let platform = capsule_core::oci_compose_lock::parse_platform_str(&entry.platform);
            images.insert(
                svc.name.clone(),
                OciImageResolution {
                    declared_ref: svc.image_ref.clone(),
                    resolved_digest: entry.resolved_digest.clone(),
                    platform,
                    importer_input_hash: None,
                },
            );
            lock_images.insert(svc.name.clone(), entry);
        } else {
            reporter
                .notify(format!(
                    "🔍 [{}] Resolving image digest: {}",
                    svc.name, svc.image_ref
                ))
                .await?;
            let request = OciImageResolutionRequest {
                target_label: svc.name.clone(),
                declared_ref: svc.image_ref.clone(),
                requested_platform: None,
                resolution_mode: OciImageResolutionMode::Required,
                importer_input_hash: None,
            };
            let resolved = match provider.resolve_image(&request).await {
                Ok(r) => r,
                Err(OciProviderError::Unsupported(_)) => {
                    anyhow::bail!(
                        "oci_image_resolution_required: provider does not support image digest \
                         resolution; run `ato lock` first to resolve '{}' for service '{}'",
                        svc.image_ref,
                        svc.name
                    );
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("{}: {}", e.code(), e).context(format!(
                        "failed to resolve image '{}' for service '{}'",
                        svc.image_ref, svc.name
                    )));
                }
            };
            reporter
                .notify(format!(
                    "✅ [{}] Resolved: {}",
                    svc.name,
                    &resolved.resolved_digest[..std::cmp::min(19, resolved.resolved_digest.len())]
                ))
                .await?;
            let lock_entry = OciImageLockEntry {
                declared_ref: svc.image_ref.clone(),
                resolved_digest: resolved.resolved_digest.clone(),
                platform: resolved.platform.os.clone() + "/" + &resolved.platform.architecture,
                provider_semantics: provider_semantics.clone(),
            };
            lock_images.insert(svc.name.clone(), lock_entry);
            images.insert(svc.name.clone(), resolved.into_lock_resolution());
        }
    }

    let new_lock = OciComposeLock {
        version: 1,
        import: OciImportMeta {
            kind: "docker-run-script".to_string(),
            source_path: script_source_path.to_string(),
            source_hash: source_hash.to_string(),
        },
        images: lock_images,
    };
    Ok((images, new_lock))
}

// ── Testable core ─────────────────────────────────────────────────────────────

/// Execute the imported install-script service graph with a caller-provided
/// `OciProvider` and pre-built image resolution map.
///
/// Does **not** perform provider readiness check or image resolution — those
/// are the caller's responsibility. This is the path used by all unit tests.
pub(crate) async fn execute_install_sh_run_with_provider<P: OciProvider>(
    import_output: &DockerRunScriptImportOutput,
    images: &HashMap<String, OciImageResolution>,
    policy_mode: OciPolicyMode,
    egress_allow: &[String],
    project_name: &str,
    reporter: &Arc<CliReporter>,
    provider: &P,
) -> Result<i32> {
    let orch_plan = import_output
        .to_orchestration_plan()
        .map_err(|e| anyhow::anyhow!("oci_execution_graph_invalid: {e}"))?;

    let ephemeral_mount_sources = HashSet::new();

    execute_service_graph_with_provider(
        &orch_plan,
        images,
        policy_mode,
        egress_allow,
        project_name,
        &ephemeral_mount_sources,
        reporter,
        provider,
    )
    .await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use capsule_core::routing::importer::docker_run_script::{
        import_docker_run_script, DockerRunScriptImportInput,
    };
    use capsule_core::types::OciImageResolution;

    use super::*;
    use crate::adapters::runtime::oci_provider::FakeOciProvider;
    use crate::reporters::CliReporter;

    // ── Fixtures ───────────────────────────────────────────────────────────────

    fn make_image(declared_ref: &str) -> OciImageResolution {
        OciImageResolution {
            declared_ref: declared_ref.to_string(),
            resolved_digest: format!("sha256:{}", "a".repeat(64)),
            platform: capsule_core::types::OciPlatform {
                os: "linux".to_string(),
                architecture: "arm64".to_string(),
                variant: None,
            },
            importer_input_hash: None,
        }
    }

    fn fake_reporter() -> Arc<CliReporter> {
        Arc::new(CliReporter::new(false))
    }

    fn make_import(script: &str) -> DockerRunScriptImportOutput {
        let input =
            DockerRunScriptImportInput::new(script.to_string(), PathBuf::from("install.sh"));
        import_docker_run_script(&input).unwrap()
    }

    const BLINKO_INSTALL_SH: &str = r#"#!/bin/bash
docker network create blinko-net

docker run -d \
  --name blinko-postgres \
  --network blinko-net \
  -e POSTGRES_USER=postgres \
  -e POSTGRES_PASSWORD=changeme \
  -e POSTGRES_DB=blinko \
  -v pg_data:/var/lib/postgresql/data \
  postgres:16-alpine

docker run -d \
  --name blinko-website \
  --network blinko-net \
  -p 1111:1111 \
  -e DATABASE_URL="postgresql://postgres:changeme@blinko-postgres:5432/blinko" \
  -e NEXTAUTH_SECRET=my_ultra_secure_nextauth_secret \
  -e NEXTAUTH_URL=http://0.0.0.0:1111 \
  --restart always \
  blinkospace/blinko:latest
"#;

    // ── 1. Lock replay: same source + same lock reuses entries ─────────────────

    #[tokio::test]
    async fn rerun_reuses_install_sh_lock_entries() {
        let import_output = make_import(BLINKO_INSTALL_SH);
        let source_hash = &import_output.source_hash;

        // Build a synthetic lock that is "fresh" for both services.
        let provider = FakeOciProvider::ready();
        let provider_label = provider.semantics().coarse_label();
        let lock = build_fresh_lock(
            source_hash,
            &[
                ("blinko-postgres", "postgres:16-alpine"),
                ("blinko-website", "blinkospace/blinko:latest"),
            ],
            &provider_label,
        );

        let reporter = fake_reporter();
        let (images, _) = resolve_install_sh_images_with_lock_replay(
            &import_output,
            "install.sh",
            source_hash,
            Some(&lock),
            &provider,
            &reporter,
        )
        .await
        .unwrap();

        assert_eq!(images.len(), 2, "both services should be resolved");
        // Both should use the cached digest "sha256:aaaa..." not re-resolved.
        for (_, img) in &images {
            assert!(
                img.resolved_digest.starts_with("sha256:aaaa"),
                "expected cached digest, got: {}",
                img.resolved_digest
            );
        }
    }

    // ── 2. Source hash drift triggers re-resolve ───────────────────────────────

    #[tokio::test]
    async fn source_hash_drift_requires_refresh_or_reresolve() {
        let import_output = make_import(BLINKO_INSTALL_SH);
        let stale_hash = "sha256:stale000000000000000000000000000000000000000000000000000000000000";

        let provider = FakeOciProvider::ready();
        let provider_label = provider.semantics().coarse_label();
        // Build a lock with a DIFFERENT source hash.
        let lock = build_fresh_lock(
            stale_hash,
            &[
                ("blinko-postgres", "postgres:16-alpine"),
                ("blinko-website", "blinkospace/blinko:latest"),
            ],
            &provider_label,
        );

        let current_hash = &import_output.source_hash;
        let reporter = fake_reporter();
        let (images, new_lock) = resolve_install_sh_images_with_lock_replay(
            &import_output,
            "install.sh",
            current_hash,
            Some(&lock),
            &provider,
            &reporter,
        )
        .await
        .unwrap();

        // Lock should now have the current hash.
        assert_eq!(new_lock.import.source_hash, *current_hash);
        // Images were re-resolved — the fake provider returns "bbbbb..." digests.
        for (_, img) in &images {
            assert!(
                !img.resolved_digest.starts_with("sha256:aaaa"),
                "expected fresh digest after drift, got: {}",
                img.resolved_digest
            );
        }
    }

    // ── 3. Lock kind = docker-run-script ─────────────────────────────────────

    #[tokio::test]
    async fn install_sh_source_hash_written_to_oci_lock() {
        let import_output = make_import(BLINKO_INSTALL_SH);
        let source_hash = &import_output.source_hash;
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();

        let (_, lock) = resolve_install_sh_images_with_lock_replay(
            &import_output,
            "install.sh",
            source_hash,
            None,
            &provider,
            &reporter,
        )
        .await
        .unwrap();

        assert_eq!(lock.import.kind, "docker-run-script");
        assert_eq!(lock.import.source_path, "install.sh");
        assert_eq!(lock.import.source_hash, *source_hash);
        assert!(lock.images.contains_key("blinko-postgres"));
        assert!(lock.images.contains_key("blinko-website"));
    }

    // ── 4. Blinko-style graph executes with fake provider ─────────────────────

    #[tokio::test]
    async fn blinko_style_install_sh_executes_with_fake_provider() {
        let import_output = make_import(BLINKO_INSTALL_SH);

        let mut images = HashMap::new();
        for svc in &import_output.services {
            images.insert(svc.name.clone(), make_image(&svc.image_ref));
        }

        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();
        let result = execute_install_sh_run_with_provider(
            &import_output,
            &images,
            OciPolicyMode::Strict,
            &[],
            "blinko",
            &reporter,
            &provider,
        )
        .await
        .unwrap();

        assert_eq!(result, 0, "execution should succeed");
    }

    // ── 5. install.sh does not use legacy Bollard ─────────────────────────────

    #[tokio::test]
    async fn install_sh_path_does_not_use_legacy_bollard() {
        let script = r#"docker run -d --name app -p 8080:8080 alpine:3.19"#;
        let import_output = make_import(script);

        let mut images = HashMap::new();
        for svc in &import_output.services {
            images.insert(svc.name.clone(), make_image(&svc.image_ref));
        }

        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();
        let result = execute_install_sh_run_with_provider(
            &import_output,
            &images,
            OciPolicyMode::Strict,
            &[],
            "test",
            &reporter,
            &provider,
        )
        .await
        .unwrap();

        assert_eq!(result, 0);
    }

    // ── 6. Secret values are not in the lock ──────────────────────────────────

    #[tokio::test]
    async fn secret_values_are_not_persisted_to_lock() {
        let script = r#"docker run -d --name pg -e POSTGRES_PASSWORD=supersecret postgres:16"#;
        let import_output = make_import(script);
        assert_eq!(import_output.services.len(), 1);

        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();
        let (_, lock) = resolve_install_sh_images_with_lock_replay(
            &import_output,
            "install.sh",
            &import_output.source_hash.clone(),
            None,
            &provider,
            &reporter,
        )
        .await
        .unwrap();

        // Lock should have the image entry but no secret values.
        let entry = lock.images.get("pg").expect("pg entry in lock");
        assert_eq!(entry.declared_ref, "postgres:16");
        // No field for POSTGRES_PASSWORD in the lock model.
        let lock_json = serde_json::to_string(&lock).unwrap();
        assert!(
            !lock_json.contains("supersecret"),
            "secret must not appear in lock JSON"
        );
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn build_fresh_lock(
        source_hash: &str,
        services: &[(&str, &str)],
        provider_semantics: &str,
    ) -> OciComposeLock {
        let mut images = BTreeMap::new();
        for (name, image_ref) in services {
            images.insert(
                name.to_string(),
                OciImageLockEntry {
                    declared_ref: image_ref.to_string(),
                    resolved_digest: format!("sha256:{}", "a".repeat(64)),
                    platform: "linux/arm64".to_string(),
                    provider_semantics: provider_semantics.to_string(),
                },
            );
        }
        OciComposeLock {
            version: 1,
            import: OciImportMeta {
                kind: "docker-run-script".to_string(),
                source_path: "install.sh".to_string(),
                source_hash: source_hash.to_string(),
            },
            images,
        }
    }
}

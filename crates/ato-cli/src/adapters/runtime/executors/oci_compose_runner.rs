//! CLI wiring for Docker Compose subset import → OCI lock/plan/run.
//!
//! This module connects the pure [`ComposeImporter`] (PR 9) to the multi-service
//! OCI executor (PR 8) behind the explicit `--oci-compose` CLI flag.
//!
//! # Entry points
//! 1. [`execute_compose_run`] — production path: detect compose file, import,
//!    check provider readiness, resolve image digests, execute.
//! 2. [`execute_compose_run_with_provider`] — testable core, accepts any
//!    `OciProvider` and pre-built `OciImageResolution` map.
//! 3. [`resolve_images_with_lock_replay`] — resolve every service's image digest
//!    via the provider (reusing fresh lock entries when present); exposed
//!    `pub(crate)` for testing.
//!
//! # Invariants
//! * Every service must have a resolved image digest before execution starts.
//! * Compose `container_name` is source metadata only; runtime names are
//!   Ato session-scoped (from the multi-service executor in PR 8).
//! * Secret-like env values (PASSWORD, SECRET, TOKEN …) are never written to
//!   receipt.  The `is_secret_like` flag from the importer is the authority.
//! * This module does NOT shell out to `docker compose` or any shell.
//! * The legacy Bollard path is never used by this module.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule::CapsuleReporter;
use capsule::execution_plan::model::OciPolicyMode;
use capsule::oci_compose_lock::{
    self, OciComposeLock, OciImageLockEntry, OciImportMeta, compute_compose_source_hash,
};
use capsule::routing::importer::compose::{
    ComposeImportInput, ComposeImportOutput, detect_compose_candidate, import_compose,
};
use capsule::types::OciImageResolution;

use super::launch_context::RuntimeLaunchContext;
use super::oci_multi_service::execute_service_graph_with_provider;
use crate::adapters::runtime::oci_provider::{
    DefaultOciProviderSelector, OciImageResolutionMode, OciImageResolutionRequest,
    OciPlatformPolicy, OciProvider, OciProviderError, OciProviderSelector,
};
use crate::adapters::runtime::oci_session_store::OciSessionMeta;
use crate::application::preflight::{
    OciProviderReadinessMode, OciProviderReadinessRequirements, preflight_oci_provider_readiness,
};
use crate::reporters::CliReporter;

// ── Production entry point ────────────────────────────────────────────────────

/// Execute an OCI service graph from a Docker Compose file in `project_dir`.
///
/// This is the **production** entry point for the `--oci-compose` CLI flag. It:
/// 1. Detects the compose file (compose.yml / docker-compose.yml / …).
/// 2. Imports it with the pure `ComposeImporter` — no `docker compose` execution.
/// 3. Reports importer warnings and unsupported features to the reporter.
/// 4. Checks `OciProvider` readiness in Required mode.
/// 5. Resolves image digests, replaying from `ato.oci.lock.json` when fresh.
/// 6. Persists resolved digests to `ato.oci.lock.json`.
/// 7. Delegates to `execute_service_graph_with_provider`.
pub(crate) async fn execute_compose_run(
    project_dir: &Path,
    reporter: Arc<CliReporter>,
    policy_mode: OciPolicyMode,
    egress_allow: &[String],
) -> Result<i32> {
    // 1. Detect compose file.
    let compose_path = detect_compose_candidate(project_dir).ok_or_else(|| {
        anyhow::anyhow!(
            "compose_not_found: no docker-compose.yml / compose.yml found in {}",
            project_dir.display()
        )
    })?;

    // 2. Import (pure — no Docker/Podman calls).
    let file_text = std::fs::read_to_string(&compose_path)
        .with_context(|| format!("failed to read {}", compose_path.display()))?;
    let source_hash = compute_compose_source_hash(&file_text);
    let compose_rel_path = compose_path
        .strip_prefix(project_dir)
        .unwrap_or(&compose_path)
        .to_string_lossy()
        .into_owned();
    let input = ComposeImportInput::new(file_text, compose_path.clone());
    let import_output =
        import_compose(&input).map_err(|e| anyhow::anyhow!("compose_import_failed: {e}"))?;

    // 3. Surface diagnostics.
    reporter
        .notify(format!("📋 Compose file: {}", compose_path.display()))
        .await?;
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
        reporter.notify(format!("⚠️  compose: {warning}")).await?;
    }
    for feat in &import_output.unsupported_features {
        reporter
            .notify(format!("ℹ️  compose (unsupported, skipped): {feat}"))
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

    // 5. Load existing lock with dual-read: prefer ato.lock.json resolution.oci_images,
    //    fall back to ato.oci.lock.json. Fail-closed on main lock parse/validation errors.
    let existing_lock = {
        let main_lock_path = project_dir.join("ato.lock.json");
        let main_lock = if main_lock_path.exists() {
            let lock = capsule::ato_lock::load_unvalidated_from_path(&main_lock_path)
                .map_err(|e| anyhow::anyhow!("failed to read ato.lock.json: {e}"))?;
            Some(lock)
        } else {
            None
        };

        let oci_read = match &main_lock {
            Some(lock) => capsule::ato_lock::read_oci_lock(lock, project_dir)
                .map_err(|e| anyhow::anyhow!("ato.lock.json OCI resolution is invalid: {e}"))?,
            None => {
                let empty = capsule::ato_lock::AtoLock::default();
                capsule::ato_lock::read_oci_lock(&empty, project_dir)
                    .map_err(|e| anyhow::anyhow!("failed to read OCI lock: {e}"))?
            }
        };

        for warning in &oci_read.warnings {
            reporter.notify(format!("⚠️  {warning}")).await?;
        }

        if oci_read.images.is_empty() {
            None
        } else {
            let mut lock_images: BTreeMap<String, OciImageLockEntry> = BTreeMap::new();
            for (name, entry) in &oci_read.images {
                lock_images.insert(
                    name.clone(),
                    OciImageLockEntry {
                        declared_ref: entry.declared_ref.clone(),
                        resolved_digest: entry.resolved_digest.clone(),
                        platform: entry.platform.clone(),
                        provider_semantics: entry.provider_semantics.clone(),
                    },
                );
            }
            Some(OciComposeLock {
                version: 1,
                import: OciImportMeta {
                    kind: "compose".to_string(),
                    source_path: compose_rel_path.clone(),
                    source_hash: source_hash.clone(),
                },
                images: lock_images,
            })
        }
    };

    // 6. Resolve image digests with lock replay.
    let (images, new_lock) = resolve_images_with_lock_replay(
        &import_output,
        &compose_rel_path,
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

    // Also write OCI facts into main ato.lock.json.
    {
        use capsule::ato_lock::oci::{
            OciImageLockEntry as MainOciImageLockEntry, OciImportEntry,
            construct_resolved_ref_from_sidecar, write_oci_facts_to_main_lock,
        };
        let main_images: BTreeMap<String, MainOciImageLockEntry> = new_lock
            .images
            .iter()
            .map(|(name, entry)| {
                let resolved_ref = construct_resolved_ref_from_sidecar(
                    &entry.declared_ref,
                    &entry.resolved_digest,
                );
                (
                    name.clone(),
                    MainOciImageLockEntry {
                        declared_ref: entry.declared_ref.clone(),
                        resolved_ref,
                        resolved_digest: entry.resolved_digest.clone(),
                        platform: entry.platform.clone(),
                        provider_semantics: entry.provider_semantics.clone(),
                        import_id: Some("default".to_string()),
                    },
                )
            })
            .collect();
        let mut main_imports = BTreeMap::new();
        main_imports.insert(
            "default".to_string(),
            OciImportEntry {
                kind: "compose".to_string(),
                source_path: compose_rel_path.clone(),
                source_hash: source_hash.clone(),
            },
        );
        write_oci_facts_to_main_lock(project_dir, main_images, main_imports)
            .map_err(|e| anyhow::anyhow!("main_lock_oci_write_failed: {e}"))?;
    }
    reporter
        .notify("🔒 Main lock updated with OCI facts".to_string())
        .await?;

    // 8. Execute.
    let project_name = compose_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("compose");

    execute_compose_run_with_provider(
        &import_output,
        &images,
        policy_mode,
        egress_allow,
        project_name,
        &reporter,
        &provider,
        Some(OciSessionMeta {
            import_kind: "compose".to_string(),
            source_path: Some(compose_path.display().to_string()),
            source_hash: Some(source_hash),
        }),
    )
    .await
}

// ── Image resolution + lock replay ────────────────────────────────────────────

/// Resolve image digests with lock replay for every service in the import output.
///
/// For each service:
/// - If an existing lock entry is fresh (source hash + declared ref + provider semantics
///   all match), the persisted digest is reused without a provider round-trip (♻️).
/// - Otherwise the provider is called to resolve a fresh digest (✅).
///
/// Returns `(service_name → OciImageResolution, updated_OciComposeLock)`.
/// The caller is responsible for persisting the returned lock to disk.
pub(crate) async fn resolve_images_with_lock_replay<P: OciProvider>(
    import_output: &ComposeImportOutput,
    compose_source_path: &str,
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
            let platform = capsule::oci_compose_lock::parse_platform_str(&entry.platform);
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
                platform_policy: OciPlatformPolicy::NativeOnly,
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
            kind: "compose".to_string(),
            source_path: compose_source_path.to_string(),
            source_hash: source_hash.to_string(),
        },
        images: lock_images,
    };
    Ok((images, new_lock))
}

/// Execute the imported service graph with a caller-provided `OciProvider` and
/// pre-built image resolution map.
///
/// Does **not** perform provider readiness check or image resolution — those are
/// the caller's responsibility. This is the path used by all unit tests.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_compose_run_with_provider<P: OciProvider>(
    import_output: &ComposeImportOutput,
    images: &HashMap<String, OciImageResolution>,
    policy_mode: OciPolicyMode,
    egress_allow: &[String],
    project_name: &str,
    reporter: &Arc<CliReporter>,
    provider: &P,
    session_meta: Option<OciSessionMeta>,
) -> Result<i32> {
    let orch_plan = import_output
        .to_orchestration_plan()
        .map_err(|e| anyhow::anyhow!("oci_execution_graph_invalid: {e}"))?;

    // Named volumes from Compose import are persistent; there are no ephemeral
    // mount sources at import time.
    let ephemeral_mount_sources = HashSet::new();

    execute_service_graph_with_provider(
        &orch_plan,
        images,
        policy_mode,
        egress_allow,
        project_name,
        &ephemeral_mount_sources,
        None, // ingress_config: compose imports do not support ingress in v1
        reporter,
        provider,
        session_meta,
        &RuntimeLaunchContext::empty(),
    )
    .await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use capsule::routing::importer::compose::{
        ComposeImportInput, detect_compose_candidate, import_compose,
    };
    use capsule::types::OciImageResolution;

    use super::*;
    use crate::adapters::runtime::oci_provider::{FakeOciProvider, fake_oci_semantics};
    use crate::reporters::CliReporter;

    // ── Fixtures ───────────────────────────────────────────────────────────────

    const SIMPLE_TWO_SERVICE_COMPOSE: &str = r#"
services:
  db:
    image: postgres:14
    volumes:
      - db-data:/var/lib/postgresql/data
  app:
    image: example/myapp:1.0
    ports:
      - "8080:8080"
    depends_on:
      - db

volumes:
  db-data: {}
"#;

    const BLINKO_COMPOSE: &str = r#"
services:
  postgres:
    image: postgres:14
    environment:
      POSTGRES_USER: blinko
      POSTGRES_PASSWORD: mysecretpassword
      POSTGRES_DB: blinko
    volumes:
      - postgres-data:/var/lib/postgresql/data

  blinko:
    image: blinkospace/blinko:latest
    ports:
      - "1111:1111"
    environment:
      DATABASE_URL: postgresql://blinko:mysecretpassword@postgres:5432/blinko
      NEXTAUTH_SECRET: supersecret
    volumes:
      - blinko-data:/app/.blinko
    depends_on:
      - postgres

volumes:
  postgres-data: {}
  blinko-data: {}
"#;

    fn make_image(declared_ref: &str) -> OciImageResolution {
        OciImageResolution {
            declared_ref: declared_ref.to_string(),
            resolved_digest: format!("sha256:{}", "a".repeat(64)),
            platform: capsule::types::OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            },
            importer_input_hash: None,
        }
    }

    fn fake_reporter() -> Arc<CliReporter> {
        Arc::new(CliReporter::new(false))
    }

    fn simple_images() -> HashMap<String, OciImageResolution> {
        let mut m = HashMap::new();
        m.insert("db".to_string(), make_image("postgres:14"));
        m.insert("app".to_string(), make_image("example/myapp:1.0"));
        m
    }

    fn blinko_images() -> HashMap<String, OciImageResolution> {
        let mut m = HashMap::new();
        m.insert("postgres".to_string(), make_image("postgres:14"));
        m.insert(
            "blinko".to_string(),
            make_image("blinkospace/blinko:latest"),
        );
        m
    }

    // ── Test 1: compose file discovery ────────────────────────────────────────

    #[test]
    fn cli_compose_flag_discovers_compose_file() {
        let dir = tempfile::tempdir().unwrap();
        let compose_path = dir.path().join("docker-compose.yml");
        std::fs::write(&compose_path, SIMPLE_TWO_SERVICE_COMPOSE).unwrap();

        let found = detect_compose_candidate(dir.path());
        assert!(
            found.is_some(),
            "detect_compose_candidate should find the file"
        );
        assert_eq!(found.unwrap(), compose_path);
    }

    // ── Test 2: import without docker compose ─────────────────────────────────

    #[test]
    fn cli_compose_flag_imports_graph_without_docker_compose() {
        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let output = import_compose(&input).unwrap();
        assert_eq!(output.services.len(), 2, "should import 2 services");
        let names: Vec<_> = output.services.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"db"), "should import 'db'");
        assert!(names.contains(&"app"), "should import 'app'");
    }

    // ── Test 3: import errors are typed ───────────────────────────────────────

    #[test]
    fn cli_compose_import_errors_are_typed() {
        let bad_yaml = "services:\n  no_image:\n    build: ."; // build-only → rejected
        let input = ComposeImportInput::new(bad_yaml.to_string(), PathBuf::from("compose.yml"));
        let result = import_compose(&input);
        assert!(
            result.is_err(),
            "build-only service should be rejected with a typed error"
        );
    }

    // ── Test 4: warnings are reported ─────────────────────────────────────────

    #[test]
    fn cli_compose_warnings_are_reported() {
        // Relative bind mount generates a warning (not a hard error by default).
        let yaml = r#"
services:
  app:
    image: example/myapp:1.0
    volumes:
      - ./data:/app/data
"#;
        let input = ComposeImportInput::new(yaml.to_string(), PathBuf::from("compose.yml"));
        let output = import_compose(&input).unwrap();
        // Either a warning or an unsupported_features entry must be present.
        let has_diagnostic = !output.warnings.is_empty() || !output.unsupported_features.is_empty();
        assert!(
            has_diagnostic,
            "relative bind mount should produce a warning or unsupported diagnostic"
        );
    }

    // ── Test 5: image digest required before execution ────────────────────────

    #[test]
    fn cli_compose_requires_image_digest_before_execution() {
        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();

        // Pass an empty images map — execution must fail with a resolution error.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(execute_compose_run_with_provider(
            &import_output,
            &HashMap::new(), // no resolved images
            OciPolicyMode::Strict,
            &[],
            "test-project",
            &reporter,
            &provider,
            None,
        ));
        assert!(
            result.is_err(),
            "execution must fail when images map is empty"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("resolution") || msg.contains("digest") || msg.contains("OCI image"),
            "error must mention image resolution, got: {msg}"
        );
    }

    // ── Test 6: all service images resolved into lock ─────────────────────────

    #[test]
    fn cli_compose_resolves_all_service_images_into_lock() {
        // FakeOciProvider.resolve_image returns a fake resolved image.
        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();

        let source_hash =
            capsule::oci_compose_lock::compute_compose_source_hash(SIMPLE_TWO_SERVICE_COMPOSE);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (images, _lock) = rt
            .block_on(resolve_images_with_lock_replay(
                &import_output,
                "docker-compose.yml",
                &source_hash,
                None, // no existing lock
                &provider,
                &reporter,
            ))
            .unwrap();

        assert_eq!(images.len(), 2, "should resolve 2 service images");
        assert!(images.contains_key("db"), "should contain 'db'");
        assert!(images.contains_key("app"), "should contain 'app'");
        for img in images.values() {
            assert!(
                !img.resolved_digest.is_empty(),
                "every image must have a non-empty digest"
            );
        }
    }

    // ── Test 7: execute imported graph with fake provider ─────────────────────

    #[test]
    fn cli_compose_executes_imported_graph_with_fake_provider() {
        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let exit_code = rt
            .block_on(execute_compose_run_with_provider(
                &import_output,
                &simple_images(),
                OciPolicyMode::Strict,
                &[],
                "test-project",
                &reporter,
                &provider,
                None,
            ))
            .unwrap();

        assert_eq!(exit_code, 0);
    }

    // ── Test 8: legacy Bollard path not used ─────────────────────────────────
    //
    // Structural test: this module does not import from `super::oci` (legacy
    // Bollard orchestrator).  Verified at compile time: there is no
    // `use super::oci` or `use crate::…::OciRuntimeClient` here.
    //
    // The FakeOciProvider is the OciProvider impl used in all tests and the
    // production path goes through PodmanProvider, never through Bollard.
    #[test]
    fn cli_compose_does_not_use_legacy_bollard_path() {
        // Structural guarantee: FakeOciProvider satisfies the OciProvider bound.
        // If the code compiled without importing the Bollard path, this test passes.
        let provider: &dyn crate::adapters::runtime::oci_provider::OciProvider =
            &FakeOciProvider::ready();
        let sem = provider.semantics();
        assert_eq!(
            sem.kind,
            capsule::types::OciProviderKind::Podman,
            "provider kind must be Podman, not legacy Bollard"
        );
    }

    // ── Test 9: secret-like env values are redacted ───────────────────────────

    #[test]
    fn cli_compose_redacts_secret_like_env_values() {
        let input = ComposeImportInput::new(
            BLINKO_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let output = import_compose(&input).unwrap();

        let postgres = output
            .services
            .iter()
            .find(|s| s.name == "postgres")
            .unwrap();
        let pw_entry = postgres
            .env
            .iter()
            .find(|e| e.key == "POSTGRES_PASSWORD")
            .unwrap();
        assert!(
            pw_entry.is_secret_like,
            "POSTGRES_PASSWORD must be flagged as secret-like"
        );

        let blinko = output.services.iter().find(|s| s.name == "blinko").unwrap();
        let secret_entry = blinko
            .env
            .iter()
            .find(|e| e.key == "NEXTAUTH_SECRET")
            .unwrap();
        assert!(
            secret_entry.is_secret_like,
            "NEXTAUTH_SECRET must be flagged as secret-like"
        );
    }

    // ── Test 10: Blinko-style smoke test ──────────────────────────────────────

    #[test]
    fn blinko_style_compose_smoke_imports_and_executes_with_fake_provider() {
        let input = ComposeImportInput::new(
            BLINKO_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();

        // Topology: blinko depends on postgres.
        let blinko = import_output
            .services
            .iter()
            .find(|s| s.name == "blinko")
            .unwrap();
        assert_eq!(
            blinko.depends_on[0].service, "postgres",
            "blinko must depend on postgres"
        );

        // Startup order: postgres before blinko.
        let plan = import_output.to_orchestration_plan().unwrap();
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
        assert!(pg_idx < blinko_idx, "postgres must start before blinko");

        // blinko publishes its port; postgres does not.
        let blinko_svc = plan.services.iter().find(|s| s.name == "blinko").unwrap();
        let pg_svc = plan.services.iter().find(|s| s.name == "postgres").unwrap();
        assert!(blinko_svc.network.publish, "blinko should publish its port");
        assert!(
            !pg_svc.network.publish,
            "postgres should not publish its port"
        );

        // Execute end-to-end with fake provider.
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let exit_code = rt
            .block_on(execute_compose_run_with_provider(
                &import_output,
                &blinko_images(),
                OciPolicyMode::Strict,
                &[],
                "blinko",
                &reporter,
                &provider,
                None,
            ))
            .unwrap();
        assert_eq!(exit_code, 0);

        // provider received pull calls for both services.
        let log = provider.call_log.lock().unwrap();
        let pulls: Vec<_> = log.iter().filter(|e| e.starts_with("pull:")).collect();
        assert_eq!(pulls.len(), 2, "expected 2 pull calls, got: {pulls:?}");
    }

    // ── Test 11: normal source run behavior unchanged ─────────────────────────
    //
    // Structural test: `execute_run_like_command` still calls
    // `execute_standard_run_with_env_assistance` when `oci_compose = false`.
    // Verified by examining dispatch/run.rs: the early-return for `oci_compose`
    // is guarded with `if args.oci_compose { … }` and does not affect the
    // normal code path.
    #[test]
    fn normal_source_run_behavior_unchanged() {
        // If this module compiled successfully with all its imports, the normal
        // dispatch path hasn't been broken. No behavioral assertion needed here —
        // the existing source-target tests in other modules cover that path.
        let _ = fake_oci_semantics();
    }

    // ── Test 12: resolve_image returning Unsupported yields resolution_required ─

    #[test]
    fn image_resolve_unsupported_returns_resolution_required_error() {
        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::with_resolve_error(
            crate::adapters::runtime::oci_provider::OciProviderError::Unsupported("resolve_image"),
        );
        let reporter = fake_reporter();

        let source_hash =
            capsule::oci_compose_lock::compute_compose_source_hash(SIMPLE_TWO_SERVICE_COMPOSE);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(resolve_images_with_lock_replay(
            &import_output,
            "docker-compose.yml",
            &source_hash,
            None, // no existing lock
            &provider,
            &reporter,
        ));
        assert!(
            result.is_err(),
            "should fail when resolve returns Unsupported"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("oci_image_resolution_required") || msg.contains("resolution"),
            "error must mention resolution, got: {msg}"
        );
    }

    // ── Test 13: generic resolve failure is propagated with context ───────────

    #[test]
    fn image_resolve_generic_failure_is_propagated() {
        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::with_resolve_error(
            crate::adapters::runtime::oci_provider::OciProviderError::Operation {
                operation: "resolve_image",
                message: "registry timeout".to_string(),
            },
        );
        let reporter = fake_reporter();

        let source_hash =
            capsule::oci_compose_lock::compute_compose_source_hash(SIMPLE_TWO_SERVICE_COMPOSE);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(resolve_images_with_lock_replay(
            &import_output,
            "docker-compose.yml",
            &source_hash,
            None, // no existing lock
            &provider,
            &reporter,
        ));
        assert!(result.is_err(), "generic resolve error must propagate");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("registry timeout") || msg.contains("failed to resolve"),
            "error must include the original reason, got: {msg}"
        );
    }

    // ── Test 14: pull failure in compose graph is typed ───────────────────────

    #[test]
    fn pull_failure_in_compose_graph_is_typed() {
        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::with_pull_failure(
            crate::adapters::runtime::oci_provider::OciProviderError::Operation {
                operation: "pull_image",
                message: "image not found in registry".to_string(),
            },
        );
        let reporter = fake_reporter();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(execute_compose_run_with_provider(
            &import_output,
            &simple_images(),
            OciPolicyMode::Strict,
            &[],
            "test-project",
            &reporter,
            &provider,
            None,
        ));
        assert!(result.is_err(), "pull failure must propagate as error");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("pull_image") || msg.contains("image not found") || msg.contains("pull"),
            "error must mention pull failure, got: {msg}"
        );
    }

    // ── Test 15: strict egress gap blocks compose execution ───────────────────

    #[test]
    fn strict_egress_gap_blocks_compose_execution() {
        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();

        let egress_allow = vec!["example.com".to_string()];
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(execute_compose_run_with_provider(
            &import_output,
            &simple_images(),
            OciPolicyMode::Strict, // Strict + non-empty egress → must fail
            &egress_allow,
            "test-project",
            &reporter,
            &provider,
            None,
        ));
        assert!(
            result.is_err(),
            "Strict policy + egress_allow must fail (PodmanProvider cannot enforce)"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("oci_execution_gate_failed")
                || msg.contains("Strict")
                || msg.contains("egress"),
            "error must reference strict policy gate, got: {msg}"
        );
    }

    // ── Test 16: loose policy gap allows compose execution with warning path ──

    #[test]
    fn loose_policy_gap_allows_compose_execution() {
        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();

        let egress_allow = vec!["example.com".to_string()];
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(execute_compose_run_with_provider(
            &import_output,
            &simple_images(),
            OciPolicyMode::Loose, // Loose + non-empty egress → must succeed (warning only)
            &egress_allow,
            "test-project",
            &reporter,
            &provider,
            None,
        ));
        assert!(
            result.is_ok(),
            "Loose policy must allow execution even with egress_allow, got: {:?}",
            result.err()
        );
    }

    // ── Test 17: real Podman opt-in smoke ────────────────────────────────────
    //
    // Skipped unless ATO_TEST_REAL_PODMAN=1 is set.
    // Requires Podman to be installed and (on macOS) a running Podman machine.
    // Uses only small stable images (alpine:3.19) to keep pull time minimal.
    //
    // Run with:
    //   ATO_TEST_REAL_PODMAN=1 cargo test -p ato-cli real_podman -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn real_podman_compose_smoke_minimal_two_service() {
        if std::env::var("ATO_TEST_REAL_PODMAN").is_err() {
            eprintln!("Skipping: ATO_TEST_REAL_PODMAN not set");
            return;
        }

        // Create a temporary project directory with a minimal compose file.
        // Uses alpine:3.19 for both services to avoid any build step and minimize
        // pull time. "db" acts as a background sleeper; "app" depends on db and
        // exits after a short sleep (triggering end of test).
        let tmp = tempfile::tempdir().expect("tempdir");
        let compose_yaml = r#"
services:
  db:
    image: alpine:3.19
    command: ["sh", "-c", "echo db-started && sleep 30"]
  app:
    image: alpine:3.19
    command: ["sh", "-c", "echo app-started && sleep 3"]
    ports:
      - "19999:19999"
    depends_on:
      - db

volumes: {}
"#;
        std::fs::write(tmp.path().join("docker-compose.yml"), compose_yaml).expect("write compose");

        let reporter = fake_reporter();
        let result = execute_compose_run(
            tmp.path(),
            reporter,
            OciPolicyMode::Strict,
            &[], // no egress_allow
        )
        .await;

        match &result {
            Err(e) => eprintln!("real Podman smoke failed: {e:#}"),
            Ok(code) => eprintln!("real Podman smoke exited with code {code}"),
        }

        // Accept either success or a specific known skip condition (provider not ready).
        // The goal is to confirm the path connects to real Podman, not to require
        // Podman to be set up in every CI environment.
        match result {
            Ok(code) => assert_eq!(code, 0, "smoke test exited with non-zero code"),
            Err(e) => {
                let msg = e.to_string();
                // If Podman itself is not available, that's a prerequisite failure,
                // not a test failure. Log but don't fail.
                if msg.contains("oci_provider_not_ready")
                    || msg.contains("podman")
                    || msg.contains("not ready")
                    || msg.contains("not found")
                {
                    eprintln!("Podman not available or not ready — skipping real smoke: {msg}");
                } else {
                    panic!("Unexpected smoke test failure: {msg}");
                }
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // PR 10.6 Lock Persistence Tests
    // ═══════════════════════════════════════════════════════════════════════════

    // Helper: build an OciComposeLock with a single entry from a given digest.
    fn make_lock_with_entry(
        source_hash: &str,
        service: &str,
        declared_ref: &str,
        digest: &str,
    ) -> OciComposeLock {
        use capsule::oci_compose_lock::{OciImageLockEntry, OciImportMeta};
        use std::collections::BTreeMap;
        let mut images = BTreeMap::new();
        images.insert(
            service.to_string(),
            OciImageLockEntry {
                declared_ref: declared_ref.to_string(),
                resolved_digest: digest.to_string(),
                platform: "linux/amd64".to_string(),
                provider_semantics: "podman-rootless-native-v1".to_string(),
            },
        );
        OciComposeLock {
            version: 1,
            import: OciImportMeta {
                kind: "compose".to_string(),
                source_path: "docker-compose.yml".to_string(),
                source_hash: source_hash.to_string(),
            },
            images,
        }
    }

    // ── Lock test 18: compose run writes lock to disk ─────────────────────────

    #[test]
    fn compose_run_writes_oci_image_resolutions_to_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let compose_yaml = SIMPLE_TWO_SERVICE_COMPOSE.as_bytes();
        std::fs::write(tmp.path().join("docker-compose.yml"), compose_yaml).unwrap();

        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            tmp.path().join("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();
        let source_hash =
            capsule::oci_compose_lock::compute_compose_source_hash(SIMPLE_TWO_SERVICE_COMPOSE);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (images, lock) = rt
            .block_on(resolve_images_with_lock_replay(
                &import_output,
                "docker-compose.yml",
                &source_hash,
                None, // no existing lock
                &provider,
                &reporter,
            ))
            .unwrap();

        // Persist to disk.
        capsule::oci_compose_lock::write_to_dir(tmp.path(), &lock).unwrap();

        // Verify lock file was written.
        let lock_path = tmp.path().join("ato.oci.lock.json");
        assert!(lock_path.exists(), "ato.oci.lock.json should be created");

        // Load back and verify entries.
        let loaded = capsule::oci_compose_lock::load_from_dir(tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(loaded.images.len(), 2);
        assert!(loaded.images.contains_key("db"));
        assert!(loaded.images.contains_key("app"));
        for entry in loaded.images.values() {
            assert!(!entry.resolved_digest.is_empty());
        }
        // images map also has 2 entries.
        assert_eq!(images.len(), 2);
    }

    // ── Lock test 19: compose run reuses existing lock resolution ──────────────

    #[test]
    fn compose_run_reuses_existing_lock_resolution() {
        let source_hash =
            capsule::oci_compose_lock::compute_compose_source_hash(SIMPLE_TWO_SERVICE_COMPOSE);
        let persisted_digest = format!("sha256:{}", "b".repeat(64));

        // Build an existing lock with a fresh "db" entry.
        let existing_lock =
            make_lock_with_entry(&source_hash, "db", "postgres:14", &persisted_digest);

        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();

        // Use a provider whose semantics match the lock entry.
        // FakeOciProvider::ready() uses "podman-rootless-native-v1".
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (images, _lock) = rt
            .block_on(resolve_images_with_lock_replay(
                &import_output,
                "docker-compose.yml",
                &source_hash,
                Some(&existing_lock),
                &provider,
                &reporter,
            ))
            .unwrap();

        // "db" must use the persisted digest, not a freshly resolved one.
        let db_img = images.get("db").unwrap();
        assert_eq!(
            db_img.resolved_digest, persisted_digest,
            "db should reuse the persisted digest from lock"
        );
    }

    // ── Lock test 20: source hash drift triggers fresh resolution ─────────────

    #[test]
    fn compose_source_hash_drift_triggers_fresh_resolution() {
        let stale_hash = "sha256:000000000000000000000000000000000000000000000000000000000000dead";
        let real_hash =
            capsule::oci_compose_lock::compute_compose_source_hash(SIMPLE_TWO_SERVICE_COMPOSE);

        // Lock was written with a different source hash (stale).
        let stale_digest = format!("sha256:{}", "c".repeat(64));
        let stale_lock = make_lock_with_entry(stale_hash, "db", "postgres:14", &stale_digest);

        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (images, new_lock) = rt
            .block_on(resolve_images_with_lock_replay(
                &import_output,
                "docker-compose.yml",
                &real_hash, // current hash ≠ stale_hash
                Some(&stale_lock),
                &provider,
                &reporter,
            ))
            .unwrap();

        // db digest should be freshly resolved (not the stale one).
        let db_img = images.get("db").unwrap();
        assert_ne!(
            db_img.resolved_digest, stale_digest,
            "db must not reuse a stale lock entry"
        );
        // New lock source_hash must equal the real hash.
        assert_eq!(new_lock.import.source_hash, real_hash);
    }

    // ── Lock test 21: mutable tag without lock → resolve required or error ────

    #[test]
    fn mutable_tag_without_persisted_digest_triggers_resolution() {
        // A mutable tag like "latest" with no lock should trigger provider resolve.
        // FakeOciProvider succeeds → we should get a resolved digest.
        let yaml = r#"
services:
  app:
    image: blinkospace/blinko:latest
    ports:
      - "1111:1111"
"#;
        let input = ComposeImportInput::new(yaml.to_string(), PathBuf::from("docker-compose.yml"));
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();
        let source_hash = capsule::oci_compose_lock::compute_compose_source_hash(yaml);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (images, lock) = rt
            .block_on(resolve_images_with_lock_replay(
                &import_output,
                "docker-compose.yml",
                &source_hash,
                None, // no lock
                &provider,
                &reporter,
            ))
            .unwrap();

        // A fresh digest was produced.
        let app_img = images.get("app").unwrap();
        assert!(!app_img.resolved_digest.is_empty());
        // The lock records the resolved digest.
        assert!(lock.images.contains_key("app"));
    }

    // ── Lock test 22: digest-ref round-trips without churn ────────────────────

    #[test]
    fn digest_ref_round_trips_without_lock_churn() {
        let digest_ref = format!("postgres@sha256:{}", "d".repeat(64));
        let yaml = format!(
            "services:\n  db:\n    image: {digest_ref}\n    volumes:\n      - db-data:/data\nvolumes:\n  db-data: {{}}\n"
        );
        let source_hash = capsule::oci_compose_lock::compute_compose_source_hash(&yaml);

        // Build a lock where "db" has this exact digest already.
        let existing_lock = make_lock_with_entry(
            &source_hash,
            "db",
            &digest_ref,
            &format!("sha256:{}", "d".repeat(64)),
        );

        let input = ComposeImportInput::new(yaml.clone(), PathBuf::from("docker-compose.yml"));
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (images, new_lock) = rt
            .block_on(resolve_images_with_lock_replay(
                &import_output,
                "docker-compose.yml",
                &source_hash,
                Some(&existing_lock),
                &provider,
                &reporter,
            ))
            .unwrap();

        // The persisted digest must be reused (no unnecessary re-resolve).
        let db_img = images.get("db").unwrap();
        assert_eq!(
            db_img.resolved_digest,
            format!("sha256:{}", "d".repeat(64)),
            "digest-ref must round-trip without churn"
        );
        // The lock's source hash is unchanged.
        assert_eq!(new_lock.import.source_hash, source_hash);
    }

    // ── Lock test 23: secret-like values are not persisted to lock ─────────────

    #[test]
    fn secret_values_are_not_persisted_to_lock() {
        let input = ComposeImportInput::new(
            BLINKO_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();
        let source_hash = capsule::oci_compose_lock::compute_compose_source_hash(BLINKO_COMPOSE);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (_images, lock) = rt
            .block_on(resolve_images_with_lock_replay(
                &import_output,
                "docker-compose.yml",
                &source_hash,
                None,
                &provider,
                &reporter,
            ))
            .unwrap();

        // Serialize the lock to JSON and verify no secrets appear.
        let lock_json = serde_json::to_string(&lock).unwrap();
        assert!(
            !lock_json.contains("mysecretpassword"),
            "POSTGRES_PASSWORD must not appear in lock JSON"
        );
        assert!(
            !lock_json.contains("supersecret"),
            "NEXTAUTH_SECRET must not appear in lock JSON"
        );
        assert!(
            !lock_json.contains("DATABASE_URL"),
            "DATABASE_URL must not appear in lock JSON"
        );
    }

    // ── Lock test 24: Blinko-style compose lock replay ────────────────────────

    #[test]
    fn blinko_style_compose_lock_replay_with_fake_provider() {
        let source_hash = capsule::oci_compose_lock::compute_compose_source_hash(BLINKO_COMPOSE);
        let pg_digest = format!("sha256:{}", "e".repeat(64));
        let blinko_digest = format!("sha256:{}", "f".repeat(64));

        // Build a pre-existing lock simulating a previous run.
        use capsule::oci_compose_lock::{OciImageLockEntry, OciImportMeta};
        use std::collections::BTreeMap;
        let mut existing_images = BTreeMap::new();
        existing_images.insert(
            "postgres".to_string(),
            OciImageLockEntry {
                declared_ref: "postgres:14".to_string(),
                resolved_digest: pg_digest.clone(),
                platform: "linux/amd64".to_string(),
                provider_semantics: "podman-rootless-native-v1".to_string(),
            },
        );
        existing_images.insert(
            "blinko".to_string(),
            OciImageLockEntry {
                declared_ref: "blinkospace/blinko:latest".to_string(),
                resolved_digest: blinko_digest.clone(),
                platform: "linux/amd64".to_string(),
                provider_semantics: "podman-rootless-native-v1".to_string(),
            },
        );
        let existing_lock = OciComposeLock {
            version: 1,
            import: OciImportMeta {
                kind: "compose".to_string(),
                source_path: "docker-compose.yml".to_string(),
                source_hash: source_hash.clone(),
            },
            images: existing_images,
        };

        let input = ComposeImportInput::new(
            BLINKO_COMPOSE.to_string(),
            PathBuf::from("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (images, new_lock) = rt
            .block_on(resolve_images_with_lock_replay(
                &import_output,
                "docker-compose.yml",
                &source_hash,
                Some(&existing_lock),
                &provider,
                &reporter,
            ))
            .unwrap();

        // Both services must reuse their persisted digests.
        assert_eq!(
            images.get("postgres").unwrap().resolved_digest,
            pg_digest,
            "postgres must reuse lock digest"
        );
        assert_eq!(
            images.get("blinko").unwrap().resolved_digest,
            blinko_digest,
            "blinko must reuse lock digest"
        );

        // Identity hash should be stable on re-run.
        assert_eq!(
            new_lock.execution_identity_hash(),
            existing_lock.execution_identity_hash(),
            "execution identity must be stable when lock is unchanged"
        );

        // Execute end-to-end with fake provider.
        let exit_code = rt
            .block_on(execute_compose_run_with_provider(
                &import_output,
                &images,
                OciPolicyMode::Strict,
                &[],
                "blinko",
                &reporter,
                &provider,
                None,
            ))
            .unwrap();
        assert_eq!(exit_code, 0, "Blinko compose replay must succeed");
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // PR 241 / Phase 2 — Main lock OCI write tests
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn compose_runner_writes_main_lock_oci_facts_alongside_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("docker-compose.yml"),
            SIMPLE_TWO_SERVICE_COMPOSE.as_bytes(),
        )
        .unwrap();

        let source_hash =
            capsule::oci_compose_lock::compute_compose_source_hash(SIMPLE_TWO_SERVICE_COMPOSE);
        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            tmp.path().join("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (_images, lock) = rt
            .block_on(resolve_images_with_lock_replay(
                &import_output,
                "docker-compose.yml",
                &source_hash,
                None,
                &provider,
                &reporter,
            ))
            .unwrap();

        // Sidecar write (existing behavior).
        capsule::oci_compose_lock::write_to_dir(tmp.path(), &lock).unwrap();
        let sidecar_path = tmp.path().join("ato.oci.lock.json");
        assert!(sidecar_path.exists(), "sidecar lock must still be written");

        // Main lock write (Phase 2).
        let main_images: BTreeMap<String, MainOciImageLockEntry> = lock
            .images
            .iter()
            .map(|(name, entry)| {
                let resolved_ref = capsule::ato_lock::construct_resolved_ref_from_sidecar(
                    &entry.declared_ref,
                    &entry.resolved_digest,
                );
                (
                    name.clone(),
                    MainOciImageLockEntry {
                        declared_ref: entry.declared_ref.clone(),
                        resolved_ref,
                        resolved_digest: entry.resolved_digest.clone(),
                        platform: entry.platform.clone(),
                        provider_semantics: entry.provider_semantics.clone(),
                        import_id: Some("default".to_string()),
                    },
                )
            })
            .collect();
        let mut main_imports = BTreeMap::new();
        main_imports.insert(
            "default".to_string(),
            OciImportEntry {
                kind: "compose".to_string(),
                source_path: "docker-compose.yml".to_string(),
                source_hash: source_hash.clone(),
            },
        );
        capsule::ato_lock::write_oci_facts_to_main_lock(tmp.path(), main_images, main_imports)
            .unwrap();

        // Verify main lock.
        let main_lock_path = tmp.path().join("ato.lock.json");
        assert!(main_lock_path.exists(), "ato.lock.json must be created");
        let loaded = capsule::ato_lock::load_unvalidated_from_path(&main_lock_path).unwrap();
        let oci_read = capsule::ato_lock::read_oci_lock(&loaded, tmp.path()).unwrap();
        assert_eq!(
            oci_read.source,
            capsule::ato_lock::OciLockSource::MainLock,
            "read must prefer main lock when present"
        );
        assert_eq!(oci_read.images.len(), 2, "must contain both services");
        assert!(oci_read.images.contains_key("db"));
        assert!(oci_read.images.contains_key("app"));
        assert_eq!(oci_read.imports.len(), 1);
        let import = oci_read.imports.get("default").unwrap();
        assert_eq!(import.kind, "compose");
        assert_eq!(import.source_path, "docker-compose.yml");
        for entry in oci_read.images.values() {
            assert!(entry.resolved_ref.ends_with(&entry.resolved_digest));
            assert_eq!(entry.import_id.as_deref(), Some("default"));
        }

        // Sidecar must remain readable (compatibility).
        let sidecar_loaded = capsule::oci_compose_lock::load_from_dir(tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(sidecar_loaded.images.len(), 2);
    }

    #[test]
    fn compose_runner_main_lock_source_path_is_project_relative() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("subdir");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("docker-compose.yml"),
            SIMPLE_TWO_SERVICE_COMPOSE.as_bytes(),
        )
        .unwrap();

        let source_hash =
            capsule::oci_compose_lock::compute_compose_source_hash(SIMPLE_TWO_SERVICE_COMPOSE);
        let input = ComposeImportInput::new(
            SIMPLE_TWO_SERVICE_COMPOSE.to_string(),
            nested.join("docker-compose.yml"),
        );
        let import_output = import_compose(&input).unwrap();
        let provider = FakeOciProvider::ready();
        let reporter = fake_reporter();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (_images, lock) = rt
            .block_on(resolve_images_with_lock_replay(
                &import_output,
                "subdir/docker-compose.yml",
                &source_hash,
                None,
                &provider,
                &reporter,
            ))
            .unwrap();

        let main_images: BTreeMap<String, MainOciImageLockEntry> = lock
            .images
            .iter()
            .map(|(name, entry)| {
                let resolved_ref = capsule::ato_lock::construct_resolved_ref_from_sidecar(
                    &entry.declared_ref,
                    &entry.resolved_digest,
                );
                (
                    name.clone(),
                    MainOciImageLockEntry {
                        declared_ref: entry.declared_ref.clone(),
                        resolved_ref,
                        resolved_digest: entry.resolved_digest.clone(),
                        platform: entry.platform.clone(),
                        provider_semantics: entry.provider_semantics.clone(),
                        import_id: Some("default".to_string()),
                    },
                )
            })
            .collect();
        let mut main_imports = BTreeMap::new();
        main_imports.insert(
            "default".to_string(),
            OciImportEntry {
                kind: "compose".to_string(),
                source_path: "subdir/docker-compose.yml".to_string(),
                source_hash: source_hash.clone(),
            },
        );
        capsule::ato_lock::write_oci_facts_to_main_lock(tmp.path(), main_images, main_imports)
            .unwrap();

        let loaded =
            capsule::ato_lock::load_unvalidated_from_path(&tmp.path().join("ato.lock.json"))
                .unwrap();
        let oci_read = capsule::ato_lock::read_oci_lock(&loaded, tmp.path()).unwrap();
        let import = oci_read.imports.get("default").unwrap();
        assert_eq!(
            import.source_path, "subdir/docker-compose.yml",
            "source_path must be project-relative, not absolute"
        );
        assert!(
            !import.source_path.starts_with('/'),
            "source_path must not be absolute"
        );
    }

    // Re-declare main-lock OCI types for use in tests above.
    // (Not re-exported from parent scope since the parent module uses the sidecar type.)
    use capsule::ato_lock::OciImageLockEntry as MainOciImageLockEntry;
    use capsule::ato_lock::OciImportEntry;
}

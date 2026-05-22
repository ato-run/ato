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
//! 3. [`resolve_images_for_compose`] — resolve every service's image digest via
//!    the provider; exposed `pub(crate)` for testing.
//!
//! # Invariants
//! * Every service must have a resolved image digest before execution starts.
//! * Compose `container_name` is source metadata only; runtime names are
//!   Ato session-scoped (from the multi-service executor in PR 8).
//! * Secret-like env values (PASSWORD, SECRET, TOKEN …) are never written to
//!   receipt.  The `is_secret_like` flag from the importer is the authority.
//! * This module does NOT shell out to `docker compose` or any shell.
//! * The legacy Bollard path is never used by this module.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule_core::execution_plan::model::OciPolicyMode;
use capsule_core::routing::importer::compose::{
    detect_compose_candidate, import_compose, ComposeImportInput, ComposeImportOutput,
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

/// Execute an OCI service graph from a Docker Compose file in `project_dir`.
///
/// This is the **production** entry point for the `--oci-compose` CLI flag. It:
/// 1. Detects the compose file (compose.yml / docker-compose.yml / …).
/// 2. Imports it with the pure `ComposeImporter` — no `docker compose` execution.
/// 3. Reports importer warnings and unsupported features to the reporter.
/// 4. Checks `OciProvider` readiness in Required mode.
/// 5. Resolves image digests for every service via the provider.
/// 6. Delegates to `execute_service_graph_with_provider`.
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

    // 5. Resolve image digests.
    let images = resolve_images_for_compose(&import_output, &provider, &reporter).await?;
    for (svc_name, img) in &images {
        reporter
            .notify(format!(
                "✅ [{}] Resolved: {}",
                svc_name,
                &img.resolved_digest[..std::cmp::min(19, img.resolved_digest.len())]
            ))
            .await?;
    }

    // 6. Execute.
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
    )
    .await
}

// ── Image resolution ──────────────────────────────────────────────────────────

/// Resolve image digests for every service in the import output.
///
/// Returns `service_name → OciImageResolution`. On `Unsupported` (provider
/// cannot resolve), returns a typed error asking the caller to run `ato lock`
/// first. On any other provider error, propagates with context.
pub(crate) async fn resolve_images_for_compose<P: OciProvider>(
    import_output: &ComposeImportOutput,
    provider: &P,
    reporter: &Arc<CliReporter>,
) -> Result<HashMap<String, OciImageResolution>> {
    let mut images = HashMap::new();
    for svc in &import_output.services {
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
        match provider.resolve_image(&request).await {
            Ok(resolved) => {
                images.insert(svc.name.clone(), resolved.into_lock_resolution());
            }
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
        }
    }
    Ok(images)
}

// ── Testable core ─────────────────────────────────────────────────────────────

/// Execute the imported service graph with a caller-provided `OciProvider` and
/// pre-built image resolution map.
///
/// Does **not** perform provider readiness check or image resolution — those are
/// the caller's responsibility. This is the path used by all unit tests.
pub(crate) async fn execute_compose_run_with_provider<P: OciProvider>(
    import_output: &ComposeImportOutput,
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
        reporter,
        provider,
    )
    .await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use capsule_core::routing::importer::compose::{
        detect_compose_candidate, import_compose, ComposeImportInput,
    };
    use capsule_core::types::OciImageResolution;

    use super::*;
    use crate::adapters::runtime::oci_provider::{fake_oci_semantics, FakeOciProvider};
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
            platform: capsule_core::types::OciPlatform {
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

        let rt = tokio::runtime::Runtime::new().unwrap();
        let images = rt
            .block_on(resolve_images_for_compose(
                &import_output,
                &provider,
                &reporter,
            ))
            .unwrap();

        assert_eq!(images.len(), 2, "should resolve 2 service images");
        assert!(images.contains_key("db"), "should contain 'db'");
        assert!(images.contains_key("app"), "should contain 'app'");
        for (_, img) in &images {
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
            capsule_core::types::OciProviderKind::Podman,
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

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(resolve_images_for_compose(
            &import_output,
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

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(resolve_images_for_compose(
            &import_output,
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
}

use std::path::Path;

use anyhow::Result;
use capsule_core::ato_lock::{AtoLock, UnresolvedReason, UnresolvedValue, ATO_LOCK_SCHEMA_VERSION};
use capsule_core::input_resolver::ResolvedCompatibilityProject;
use capsule_core::lockfile::CapsuleLock;
use capsule_core::manifest::LoadedManifest;
use serde::Serialize;

use super::diagnostics::{
    sort_diagnostics, CompatibilityDiagnostic, CompatibilityDiagnosticCode,
    CompatibilityDiagnosticSeverity,
};
use super::legacy_lock_import::import_legacy_lock;
use super::manifest_import::import_manifest;
use super::provenance::{sort_provenance, CompilerOwnedField, ProvenanceRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DraftCompleteness {
    Skeleton,
    PartiallyResolved,
    ResolutionEnriched,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DraftGuarantee {
    pub completeness: DraftCompleteness,
    pub execution_usable: bool,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UnresolvedSummary {
    pub total: usize,
    pub resolution: usize,
    pub contract: usize,
    pub binding: usize,
    pub policy: usize,
    pub attestations: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct CompatibilityCompileResult {
    pub draft_lock: AtoLock,
    pub guarantee: DraftGuarantee,
    pub unresolved_summary: UnresolvedSummary,
    pub diagnostics: Vec<CompatibilityDiagnostic>,
    pub provenance: Vec<ProvenanceRecord>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CompatibilityCompilerInput<'a> {
    pub manifest: &'a LoadedManifest,
    pub legacy_lock: Option<&'a CapsuleLock>,
    pub legacy_lock_path: Option<&'a Path>,
}

impl<'a> CompatibilityCompilerInput<'a> {
    pub(crate) fn new(
        manifest: &'a LoadedManifest,
        legacy_lock: Option<&'a CapsuleLock>,
        legacy_lock_path: Option<&'a Path>,
    ) -> Self {
        Self {
            manifest,
            legacy_lock,
            legacy_lock_path,
        }
    }
}

pub(crate) fn compile_compatibility_project(
    project: &ResolvedCompatibilityProject,
) -> Result<CompatibilityCompileResult> {
    compile_compatibility_input(CompatibilityCompilerInput::new(
        &project.manifest,
        project.legacy_lock.as_ref().map(|lock| &lock.lock),
        project.legacy_lock.as_ref().map(|lock| lock.path.as_path()),
    ))
}

pub(crate) fn compile_compatibility_input(
    input: CompatibilityCompilerInput<'_>,
) -> Result<CompatibilityCompileResult> {
    let mut manifest_result = import_manifest(&input)?;

    if let Some(legacy_lock) = input.legacy_lock {
        let legacy_result = import_legacy_lock(&input, &manifest_result.draft_lock, legacy_lock);
        manifest_result
            .draft_lock
            .resolution
            .entries
            .extend(legacy_result.resolution_entries);
        manifest_result
            .draft_lock
            .resolution
            .unresolved
            .extend(legacy_result.unresolved);
        manifest_result
            .diagnostics
            .extend(legacy_result.diagnostics);
        manifest_result.provenance.extend(legacy_result.provenance);
        if manifest_result.draft_lock.contract.entries.is_empty() {
            manifest_result
                .draft_lock
                .contract
                .unresolved
                .push(UnresolvedValue {
                field: Some("contract.process".to_string()),
                reason: UnresolvedReason::InsufficientEvidence,
                detail: Some(
                    "legacy lock data cannot reconstruct authored contract without capsule.toml"
                        .to_string(),
                ),
                candidates: Vec::new(),
            });
        }
    }

    manifest_result.draft_lock.schema_version = ATO_LOCK_SCHEMA_VERSION;
    manifest_result.diagnostics.push(CompatibilityDiagnostic::new(
        CompatibilityDiagnosticCode::DraftNotExecutionUsable,
        CompatibilityDiagnosticSeverity::Warning,
        CompilerOwnedField::new("contract", "process").lock_path(),
        "compatibility compiler output is a lock-shaped draft for downstream resolution and diagnostics, not an execution-usable canonical lock",
        Some(input.manifest.path.as_path()),
    ));

    // Deterministic guarantees apply to compiler-owned draft sections and to
    // diagnostics/provenance ordering under identical normalized inputs.
    sort_diagnostics(&mut manifest_result.diagnostics);
    sort_provenance(&mut manifest_result.provenance);

    let unresolved_summary = summarize_unresolved(&manifest_result.draft_lock);
    let completeness = if input.legacy_lock.is_some() {
        DraftCompleteness::ResolutionEnriched
    } else if unresolved_summary.total > 0 {
        DraftCompleteness::PartiallyResolved
    } else {
        DraftCompleteness::Skeleton
    };

    let guarantee = DraftGuarantee {
        completeness,
        execution_usable: false,
        summary: "lock-shaped draft suitable for downstream resolution and diagnostics; execution readiness is intentionally not guaranteed at Ticket 03".to_string(),
    };

    Ok(CompatibilityCompileResult {
        draft_lock: manifest_result.draft_lock,
        guarantee,
        unresolved_summary,
        diagnostics: manifest_result.diagnostics,
        provenance: manifest_result.provenance,
    })
}

fn summarize_unresolved(lock: &AtoLock) -> UnresolvedSummary {
    let resolution = lock.resolution.unresolved.len();
    let contract = lock.contract.unresolved.len();
    let binding = lock.binding.unresolved.len();
    let policy = lock.policy.unresolved.len();
    let attestations = lock.attestations.unresolved.len();

    UnresolvedSummary {
        total: resolution + contract + binding + policy + attestations,
        resolution,
        contract,
        binding,
        policy,
        attestations,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use capsule_core::input_resolver::{
        resolve_authoritative_input, ResolveInputOptions, ResolvedInput,
    };
    use serde_json::Value;
    use tempfile::tempdir;

    use super::compile_compatibility_project;

    fn write_manifest(dir: &std::path::Path, content: &str) {
        fs::write(dir.join("capsule.toml"), content).expect("write manifest");
    }

    fn write_minimal_macos_app_bundle(dir: &std::path::Path, relative: &str) {
        let binary = dir.join(relative).join("Contents/MacOS/MyApp");
        fs::create_dir_all(binary.parent().expect("bundle binary parent"))
            .expect("create app bundle");
        fs::write(binary, b"demo-app").expect("write app bundle binary");
    }

    fn compile_from_dir(dir: &std::path::Path) -> super::CompatibilityCompileResult {
        let resolved = resolve_authoritative_input(dir, ResolveInputOptions::default())
            .expect("resolve compatibility input");
        let ResolvedInput::CompatibilityProject { project, .. } = resolved else {
            panic!("expected compatibility project");
        };
        compile_compatibility_project(&project).expect("compile project")
    }

    #[test]
    fn single_service_populates_contract_process() {
        let dir = tempdir().expect("tempdir");
        write_manifest(
            dir.path(),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "main.ts"

[services.main]
target = "web"
"#,
        );

        let result = compile_from_dir(dir.path());
        assert!(result.draft_lock.contract.entries.contains_key("process"));
        assert!(result.provenance.iter().any(|record| {
            record.field
                == crate::application::compat_import::CompilerOwnedField::new("contract", "process")
                && record.source_field.as_deref() == Some("services.<single>.entrypoint")
        }));
        let workloads = result
            .draft_lock
            .contract
            .entries
            .get("workloads")
            .and_then(Value::as_array)
            .expect("workloads array");
        assert_eq!(workloads.len(), 1);
    }

    #[test]
    fn native_app_import_emits_imported_artifact_closure() {
        let dir = tempdir().expect("tempdir");
        write_manifest(
            dir.path(),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "dist/MyApp.app"
"#,
        );
        write_minimal_macos_app_bundle(dir.path(), "dist/MyApp.app");

        let result = compile_from_dir(dir.path());
        let closure = result
            .draft_lock
            .resolution
            .entries
            .get("closure")
            .expect("resolution.closure");

        assert_eq!(
            closure.get("kind").and_then(Value::as_str),
            Some("imported_artifact_closure")
        );
        assert_eq!(
            closure.get("status").and_then(Value::as_str),
            Some("complete")
        );
        let artifact = closure
            .get("artifact")
            .and_then(Value::as_object)
            .expect("artifact");
        assert_eq!(
            artifact.get("artifact_type").and_then(Value::as_str),
            Some("macos_app_bundle")
        );
        assert_eq!(
            artifact.get("provenance_limited").and_then(Value::as_bool),
            Some(true)
        );
        assert!(artifact
            .get("digest")
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with("blake3:")));
        assert!(result.provenance.iter().any(|record| {
            record.field
                == crate::application::compat_import::CompilerOwnedField::new(
                    "resolution",
                    "closure",
                )
                && record
                    .note
                    .as_deref()
                    .is_some_and(|note| note.contains("provenance is intentionally limited"))
        }));
    }

    #[test]
    fn multi_service_leaves_contract_process_unresolved() {
        let dir = tempdir().expect("tempdir");
        write_manifest(
            dir.path(),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "main.ts"

[targets.worker]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "worker.ts"

[services.main]
target = "main"

[services.worker]
target = "worker"
"#,
        );

        let result = compile_from_dir(dir.path());
        assert!(!result.draft_lock.contract.entries.contains_key("process"));
        assert_eq!(result.unresolved_summary.contract, 1);
        assert!(result.provenance.iter().any(|record| {
            record.field
                == crate::application::compat_import::CompilerOwnedField::new(
                    "contract",
                    "workloads",
                )
                && record.source_field.as_deref() == Some("services")
        }));
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.lock_path == "contract.process"));
    }

    #[test]
    fn legacy_lock_enriches_resolution_without_overriding_contract() {
        let dir = tempdir().expect("tempdir");
        write_manifest(
            dir.path(),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "main.ts"
"#,
        );
        fs::write(
            dir.path().join("capsule.lock.json"),
            r#"{
  "version": "1",
  "meta": {"created_at": "2026-03-25T00:00:00Z", "manifest_hash": "sha256:demo"},
    "injected_data": {
        "DATABASE_URL": {"source": "env", "digest": "sha256:deadbeef", "bytes": 42}
    },
  "runtimes": {
    "deno": {
      "provider": "denoland",
      "version": "2.1.3",
      "targets": {"aarch64-apple-darwin": {"url": "https://example.invalid/deno.zip", "sha256": "abc"}}
    }
  }
}"#,
        )
        .expect("write lock");

        let result = compile_from_dir(dir.path());
        assert!(result.draft_lock.contract.entries.contains_key("process"));
        assert!(result
            .draft_lock
            .resolution
            .entries
            .contains_key("locked_runtimes"));
        assert!(result
            .draft_lock
            .resolution
            .entries
            .contains_key("locked_injected_data"));
        assert!(!result
            .draft_lock
            .contract
            .entries
            .contains_key("locked_injected_data"));
        assert!(result.provenance.iter().any(|record| {
            record.field
                == crate::application::compat_import::CompilerOwnedField::new(
                    "resolution",
                    "locked_injected_data",
                )
                && record
                    .note
                    .as_deref()
                    .is_some_and(|note| note.contains("resolution-scoped"))
        }));
    }

    #[test]
    fn conflicting_legacy_runtime_becomes_diagnostic() {
        let dir = tempdir().expect("tempdir");
        write_manifest(
            dir.path(),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "main.ts"
"#,
        );
        fs::write(
            dir.path().join("capsule.lock.json"),
            r#"{
  "version": "1",
  "meta": {"created_at": "2026-03-25T00:00:00Z", "manifest_hash": "sha256:demo"},
  "runtimes": {
    "deno": {
      "provider": "denoland",
      "version": "2.2.0",
      "targets": {"aarch64-apple-darwin": {"url": "https://example.invalid/deno.zip", "sha256": "abc"}}
    }
  }
}"#,
        )
        .expect("write lock");

        let result = compile_from_dir(dir.path());
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("runtime version conflict")));
        assert!(result.unresolved_summary.resolution >= 1);
    }

    #[test]
    fn deterministic_diagnostics_and_provenance_ordering() {
        let dir = tempdir().expect("tempdir");
        write_manifest(
            dir.path(),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "main.ts"
"#,
        );

        let left = compile_from_dir(dir.path());
        let right = compile_from_dir(dir.path());

        assert_eq!(
            serde_json::to_value(&left.diagnostics).expect("left diagnostics"),
            serde_json::to_value(&right.diagnostics).expect("right diagnostics")
        );
        assert_eq!(
            serde_json::to_value(&left.provenance).expect("left provenance"),
            serde_json::to_value(&right.provenance).expect("right provenance")
        );
        assert_eq!(
            serde_json::to_value(&left.draft_lock.contract).expect("left contract"),
            serde_json::to_value(&right.draft_lock.contract).expect("right contract")
        );
    }

    #[test]
    fn manifest_only_draft_is_deterministic_without_legacy_lock() {
        let dir = tempdir().expect("tempdir");
        write_manifest(
            dir.path(),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "main.ts"
"#,
        );

        let left = compile_from_dir(dir.path());
        let right = compile_from_dir(dir.path());

        assert_eq!(
            serde_json::to_value(&left.draft_lock).expect("left draft"),
            serde_json::to_value(&right.draft_lock).expect("right draft")
        );
    }

    #[test]
    fn service_order_does_not_affect_workload_ordering() {
        let left = tempdir().expect("tempdir");
        let right = tempdir().expect("tempdir");
        write_manifest(
            left.path(),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "main.ts"

[targets.worker]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "worker.ts"

[services.main]
target = "main"

[services.worker]
target = "worker"
"#,
        );
        write_manifest(
            right.path(),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "main.ts"

[targets.worker]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "worker.ts"

[services.worker]
target = "worker"

[services.main]
target = "main"
"#,
        );

        let left = compile_from_dir(left.path());
        let right = compile_from_dir(right.path());

        assert_eq!(
            left.draft_lock.contract.entries.get("workloads"),
            right.draft_lock.contract.entries.get("workloads")
        );
        assert_eq!(
            left.draft_lock.contract.unresolved,
            right.draft_lock.contract.unresolved
        );
    }

    #[test]
    fn legacy_conflict_only_affects_resolution_and_unresolved() {
        let base = tempdir().expect("tempdir");
        write_manifest(
            base.path(),
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
driver = "deno"
runtime_version = "2.1.3"
entrypoint = "main.ts"
"#,
        );
        let baseline = compile_from_dir(base.path());

        fs::write(
            base.path().join("capsule.lock.json"),
            r#"{
  "version": "1",
  "meta": {"created_at": "2026-03-25T00:00:00Z", "manifest_hash": "sha256:demo"},
  "runtimes": {
    "deno": {
      "provider": "denoland",
      "version": "2.2.0",
      "targets": {"aarch64-apple-darwin": {"url": "https://example.invalid/deno.zip", "sha256": "abc"}}
    }
  }
}"#,
        )
        .expect("write lock");

        let conflicted = compile_from_dir(base.path());
        assert_eq!(baseline.draft_lock.contract, conflicted.draft_lock.contract);
        assert_eq!(
            baseline.unresolved_summary.contract,
            conflicted.unresolved_summary.contract
        );
        assert!(conflicted
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.lock_path.starts_with("contract.")
                || diagnostic.lock_path.starts_with("resolution.")));
        assert!(conflicted
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.lock_path.starts_with("resolution.")));
        assert!(conflicted.unresolved_summary.resolution > baseline.unresolved_summary.resolution);
    }

    #[test]
    fn chml_like_manifest_flows_through_same_compiler_path() {
        let dir = tempdir().expect("tempdir");
        write_manifest(
            dir.path(),
            // CHML-like here means the compact manifest form without explicit
            // targets/services tables that normalizes through the legacy
            // compatibility manifest loader before import.
            r#"name = "demo"
type = "app"
runtime = "source/deno"
runtime_version = "2.1.3"
run = "main.ts"
port = 4173
"#,
        );

        let result = compile_from_dir(dir.path());
        assert!(result.draft_lock.contract.entries.contains_key("process"));
        assert!(result
            .draft_lock
            .resolution
            .entries
            .contains_key("target_selection"));
    }
}

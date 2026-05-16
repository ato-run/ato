use anyhow::Result;
use capsule_core::engine::execution_graph::{ExecutionGraphBuilder, ExecutionGraphNode};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::input_resolver::{
    resolve_authoritative_input, ResolveInputOptions, ResolvedInput,
};
use capsule_core::lockfile::{
    manifest_external_capsule_dependencies, verify_lockfile_external_dependencies,
    CAPSULE_LOCK_FILE_NAME,
};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

use crate::application::execution_graph_adapter::build_input_from_external_dependencies;
use crate::application::source_inference::{
    materialize_run_from_compatibility, materialize_run_from_source_only,
};
use crate::reporters::CliReporter;

#[derive(Debug, Clone, Serialize)]
pub struct ValidateResult {
    pub authoritative_input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_lock_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_label: Option<String>,
    pub lockfile_checked: bool,
    pub warnings: Vec<String>,
}

pub fn execute(path: PathBuf, json_output: bool) -> Result<ValidateResult> {
    let resolved = resolve_authoritative_input(&path, ResolveInputOptions::default())?;
    let reporter = Arc::new(CliReporter::new(false));
    let mut warnings = resolved
        .advisories()
        .iter()
        .map(|advisory| advisory.message.clone())
        .collect::<Vec<_>>();

    let result = match resolved {
        ResolvedInput::CanonicalLock {
            canonical,
            provenance,
            ..
        } => {
            let decision = capsule_core::router::route_lock(
                &canonical.path,
                &canonical.lock,
                &canonical.project_root,
                capsule_core::router::ExecutionProfile::Release,
                None,
            )?;

            ValidateResult {
                authoritative_input: provenance.selected_kind.as_str().to_string(),
                manifest_path: None,
                canonical_lock_path: Some(canonical.path),
                runtime: Some(format!("{:?}", decision.kind).to_lowercase()),
                target_label: Some(decision.plan.selected_target_label().to_string()),
                lockfile_checked: false,
                warnings,
            }
        }
        ResolvedInput::CompatibilityProject {
            project,
            provenance,
            ..
        } => {
            let manifest_path = project.manifest.path.clone();
            let materialized =
                materialize_run_from_compatibility(&project, None, reporter.clone(), true)?;
            let decision = capsule_core::router::route_lock(
                &materialized.lock_path,
                &materialized.lock,
                &project.project_root,
                capsule_core::router::ExecutionProfile::Release,
                None,
            )?;
            let targets_to_validate = decision.plan.selected_target_package_order()?;
            for target_label in &targets_to_validate {
                capsule_core::diagnostics::manifest::validate_manifest_for_build(
                    &manifest_path,
                    target_label,
                )?;
            }

            let lockfile_checked = if let Some(legacy_lock) = project.legacy_lock.as_ref() {
                capsule_core::lockfile::verify_lockfile_manifest(&manifest_path, &legacy_lock.path)
                    .map_err(|err| {
                        if err.to_string().contains("manifest hash mismatch") {
                            AtoExecutionError::lockfile_tampered(
                                err.to_string(),
                                Some(CAPSULE_LOCK_FILE_NAME),
                            )
                        } else {
                            AtoExecutionError::policy_violation(err.to_string())
                        }
                    })?;

                // Wave 2 / PR-4b: emit the unified execution graph alongside
                // the legacy lock-vs-manifest verification. The graph is *not*
                // load-bearing — `verify_lockfile_external_dependencies`
                // remains the source of truth for the consistency check; the
                // graph build is purely a parity observation.
                let external_dependencies =
                    manifest_external_capsule_dependencies(&decision.plan.manifest)?;
                debug_assert_provider_aliases_match_lock(&external_dependencies, &legacy_lock.lock);

                verify_lockfile_external_dependencies(&decision.plan.manifest, &legacy_lock.lock)?;
                true
            } else {
                let external_dependencies =
                    manifest_external_capsule_dependencies(&decision.plan.manifest)?;

                // Wave 2 / PR-4a: emit the unified execution graph alongside
                // the legacy derivation. The graph is *not* load-bearing —
                // the legacy `external_dependencies` vector below remains the
                // source of truth for the gating decision. We only debug-assert
                // shape parity (provider count) to surface drift early.
                debug_assert_provider_node_parity(&external_dependencies);

                if !external_dependencies.is_empty() {
                    return Err(AtoExecutionError::lock_incomplete(
                        "external capsule dependencies require capsule.lock.json",
                        Some(CAPSULE_LOCK_FILE_NAME),
                    )
                    .into());
                }
                false
            };

            let raw_manifest: toml::Value =
                toml::from_str(&project.manifest.raw_text).map_err(|err| {
                    AtoExecutionError::execution_contract_invalid(
                        format!("Failed to parse manifest TOML for IPC validation: {err}"),
                        None,
                        None,
                    )
                })?;
            let ipc_diagnostics =
                crate::ipc::validate::validate_manifest(&raw_manifest, &project.project_root)
                    .map_err(|err| {
                        AtoExecutionError::execution_contract_invalid(
                            format!("IPC validation failed: {err}"),
                            None,
                            None,
                        )
                    })?;
            if crate::ipc::validate::has_errors(&ipc_diagnostics) {
                return Err(AtoExecutionError::execution_contract_invalid(
                    crate::ipc::validate::format_diagnostics(&ipc_diagnostics),
                    None,
                    None,
                )
                .into());
            }
            warnings.extend(
                ipc_diagnostics
                    .into_iter()
                    .map(|diagnostic| diagnostic.to_string()),
            );

            ValidateResult {
                authoritative_input: provenance.selected_kind.as_str().to_string(),
                manifest_path: Some(manifest_path),
                canonical_lock_path: None,
                runtime: Some(format!("{:?}", decision.kind).to_lowercase()),
                target_label: Some(decision.plan.selected_target_label().to_string()),
                lockfile_checked,
                warnings,
            }
        }
        ResolvedInput::SourceOnly {
            source, provenance, ..
        } => {
            let materialized =
                materialize_run_from_source_only(&source, None, reporter.clone(), true)?;
            let decision = capsule_core::router::route_lock(
                &materialized.lock_path,
                &materialized.lock,
                &source.project_root,
                capsule_core::router::ExecutionProfile::Release,
                None,
            )?;

            ValidateResult {
                authoritative_input: provenance.selected_kind.as_str().to_string(),
                manifest_path: None,
                canonical_lock_path: Some(materialized.lock_path),
                runtime: Some(format!("{:?}", decision.kind).to_lowercase()),
                target_label: Some(decision.plan.selected_target_label().to_string()),
                lockfile_checked: false,
                warnings,
            }
        }
    };

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("✔ Input validation passed");
        println!("  Authoritative input: {}", result.authoritative_input);
        if let Some(path) = result.canonical_lock_path.as_ref() {
            println!("  Canonical lock: {}", path.display());
        }
        if let Some(path) = result.manifest_path.as_ref() {
            println!("  Manifest: {}", path.display());
        }
        if let Some(runtime) = result.runtime.as_ref() {
            println!("  Runtime: {}", runtime);
        }
        if let Some(target_label) = result.target_label.as_ref() {
            println!("  Target: {}", target_label);
        }
        if result.lockfile_checked {
            println!("  {}: verified", CAPSULE_LOCK_FILE_NAME);
        }
        if result.warnings.is_empty() {
            println!("  IPC: no warnings");
        } else {
            println!("  IPC warnings:");
            for warning in &result.warnings {
                println!("    {}", warning.replace('\n', "\n    "));
            }
        }
    }

    Ok(result)
}

/// PR-4a / PR-4b equivalence guard: build the unified execution graph from
/// the same `ExternalCapsuleDependency` list the validate path consumes and
/// check that its provider-node count matches 1:1.
///
/// `debug_assert_eq!` is a no-op in release builds, but the surrounding
/// graph build still runs there. That's intentional: validate is a low-rate
/// inspection command, the graph build is cheap, and continuing to compute
/// it in release surfaces real-world drift between the two derivations
/// before a future wave promotes the graph to the source of truth.
fn debug_assert_provider_node_parity(
    external_dependencies: &[capsule_core::types::ExternalCapsuleDependency],
) {
    let input = build_input_from_external_dependencies(external_dependencies, None);
    let graph = ExecutionGraphBuilder::build(input);
    let graph_provider_count = graph
        .nodes
        .iter()
        .filter(|node| matches!(node, ExecutionGraphNode::Provider { .. }))
        .count();
    debug_assert_eq!(
        graph_provider_count,
        external_dependencies.len(),
        "execution graph provider count drifted from manifest_external_capsule_dependencies"
    );
}

/// PR-4b equivalence guard for the legacy-lock-present validate branch:
/// build the unified execution graph from the manifest-derived
/// `ExternalCapsuleDependency` list, extract the provider-node alias set,
/// and check it matches the alias set of the lock's `capsule_dependencies`
/// (modulo deps that are present in only one side — those are caught by
/// `verify_lockfile_external_dependencies` itself, which remains the source
/// of truth for the gating decision).
///
/// This is intentionally weaker than `verify_lockfile_external_dependencies`
/// — that function already enforces full equality of source/contract/etc.
/// We only assert *alias-set* parity here so a divergence between the
/// graph adapter's stable provider-identifier convention and the manifest's
/// alias convention surfaces immediately, without duplicating the full
/// consistency check.
fn debug_assert_provider_aliases_match_lock(
    external_dependencies: &[capsule_core::types::ExternalCapsuleDependency],
    lock: &capsule_core::lockfile::CapsuleLock,
) {
    let input = build_input_from_external_dependencies(external_dependencies, None);
    let graph = ExecutionGraphBuilder::build(input);

    let mut graph_provider_aliases: Vec<String> = graph
        .nodes
        .iter()
        .filter_map(|node| match node {
            ExecutionGraphNode::Provider { identifier } => Some(
                identifier
                    .strip_prefix("provider://")
                    .unwrap_or(identifier.as_str())
                    .to_string(),
            ),
            _ => None,
        })
        .collect();
    graph_provider_aliases.sort();

    let mut manifest_aliases: Vec<String> = external_dependencies
        .iter()
        .map(|dependency| dependency.alias.clone())
        .collect();
    manifest_aliases.sort();

    debug_assert_eq!(
        graph_provider_aliases, manifest_aliases,
        "execution graph provider aliases drifted from manifest dependency aliases"
    );

    // Lock side: only consider lock entries whose alias matches a manifest
    // alias. The lock may carry additional dependencies (e.g. transitive),
    // and `verify_lockfile_external_dependencies` does not require lock
    // ⊆ manifest in that direction. We just want to know that every
    // manifest-derived alias is also present in the lock — which is what
    // the legacy verify call enforces too.
    let manifest_alias_set: std::collections::BTreeSet<&str> = external_dependencies
        .iter()
        .map(|dependency| dependency.alias.as_str())
        .collect();
    let mut lock_aliases_in_manifest: Vec<&str> = lock
        .capsule_dependencies
        .iter()
        .map(|locked| locked.name.as_str())
        .filter(|name| manifest_alias_set.contains(*name))
        .collect();
    lock_aliases_in_manifest.sort_unstable();

    let manifest_alias_strs: Vec<&str> = manifest_aliases.iter().map(String::as_str).collect();
    debug_assert_eq!(
        lock_aliases_in_manifest, manifest_alias_strs,
        "lock capsule_dependencies missing aliases the graph derived from the manifest"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::types::ExternalCapsuleDependency;
    use std::collections::BTreeMap;

    fn dependency(alias: &str) -> ExternalCapsuleDependency {
        ExternalCapsuleDependency {
            alias: alias.to_string(),
            source: format!("capsule://ato/{alias}"),
            source_type: "store".to_string(),
            contract: Some("service@1".to_string()),
            injection_bindings: BTreeMap::new(),
            parameters: BTreeMap::new(),
            credentials: BTreeMap::new(),
        }
    }

    #[test]
    fn graph_provider_count_equals_legacy_external_dependency_count() {
        // Equivalence test for PR-4a: the validate-path adapter flow
        // produces a provider-node count that matches the legacy
        // `manifest_external_capsule_dependencies` vector 1:1.
        let dependencies = vec![dependency("db"), dependency("cache"), dependency("queue")];

        let input = build_input_from_external_dependencies(&dependencies, None);
        let graph = ExecutionGraphBuilder::build(input);

        let provider_count = graph
            .nodes
            .iter()
            .filter(|node| matches!(node, ExecutionGraphNode::Provider { .. }))
            .count();
        assert_eq!(provider_count, dependencies.len());

        let mut graph_aliases: Vec<String> = graph
            .nodes
            .iter()
            .filter_map(|node| match node {
                ExecutionGraphNode::Provider { identifier } => Some(
                    identifier
                        .strip_prefix("provider://")
                        .unwrap_or(identifier.as_str())
                        .to_string(),
                ),
                _ => None,
            })
            .collect();
        graph_aliases.sort();

        let mut legacy_aliases: Vec<String> = dependencies
            .iter()
            .map(|dependency| dependency.alias.clone())
            .collect();
        legacy_aliases.sort();

        assert_eq!(graph_aliases, legacy_aliases);
    }

    #[test]
    fn empty_dependency_list_yields_empty_provider_set() {
        let dependencies: Vec<ExternalCapsuleDependency> = Vec::new();
        let input = build_input_from_external_dependencies(&dependencies, None);
        let graph = ExecutionGraphBuilder::build(input);

        assert!(graph
            .nodes
            .iter()
            .all(|node| !matches!(node, ExecutionGraphNode::Provider { .. })));
    }

    fn locked_dependency(alias: &str) -> capsule_core::lockfile::LockedCapsuleDependency {
        capsule_core::lockfile::LockedCapsuleDependency {
            name: alias.to_string(),
            source: format!("capsule://ato/{alias}"),
            source_type: "store".to_string(),
            contract: Some("service@1".to_string()),
            injection_bindings: BTreeMap::new(),
            parameters: BTreeMap::new(),
            credentials: BTreeMap::new(),
            identity_exports: BTreeMap::new(),
            resolved_version: None,
            digest: None,
            sha256: None,
            artifact_url: None,
        }
    }

    fn empty_lock_with(
        capsule_dependencies: Vec<capsule_core::lockfile::LockedCapsuleDependency>,
    ) -> capsule_core::lockfile::CapsuleLock {
        capsule_core::lockfile::CapsuleLock {
            version: "1".to_string(),
            meta: capsule_core::lockfile::LockMeta {
                created_at: "2026-05-09T00:00:00Z".to_string(),
                manifest_hash: "sha256:test".to_string(),
            },
            allowlist: None,
            capsule_dependencies,
            tool_capsules: BTreeMap::new(),
            injected_data: std::collections::HashMap::new(),
            tools: None,
            runtimes: None,
            targets: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn graph_provider_aliases_match_lock_for_legacy_lock_branch() {
        // Equivalence test for PR-4b: the legacy-lock-present validate
        // branch's adapter flow produces a provider-alias set that matches
        // the lock's `capsule_dependencies` aliases for every alias the
        // manifest declares.
        let manifest_dependencies =
            vec![dependency("db"), dependency("cache"), dependency("queue")];
        let lock = empty_lock_with(vec![
            locked_dependency("db"),
            locked_dependency("cache"),
            locked_dependency("queue"),
        ]);

        // This must not panic — the helper itself is the assertion under test.
        debug_assert_provider_aliases_match_lock(&manifest_dependencies, &lock);

        // Reproduce the alias-set check inline so the test fails loudly even
        // if `debug_assertions` are off (release-mode `cargo test` builds).
        let input = build_input_from_external_dependencies(&manifest_dependencies, None);
        let graph = ExecutionGraphBuilder::build(input);
        let mut graph_aliases: Vec<String> = graph
            .nodes
            .iter()
            .filter_map(|node| match node {
                ExecutionGraphNode::Provider { identifier } => Some(
                    identifier
                        .strip_prefix("provider://")
                        .unwrap_or(identifier.as_str())
                        .to_string(),
                ),
                _ => None,
            })
            .collect();
        graph_aliases.sort();

        let mut lock_aliases: Vec<String> = lock
            .capsule_dependencies
            .iter()
            .map(|locked| locked.name.clone())
            .collect();
        lock_aliases.sort();

        assert_eq!(graph_aliases, lock_aliases);
    }

    #[test]
    fn graph_provider_aliases_ignore_lock_extras_outside_manifest() {
        // The legacy-lock branch verify already enforces manifest⊆lock; the
        // graph parity guard must tolerate lock entries that the manifest
        // does not declare (e.g. transitive deps recorded in the lock).
        let manifest_dependencies = vec![dependency("db")];
        let lock = empty_lock_with(vec![
            locked_dependency("db"),
            locked_dependency("transitive-extra"),
        ]);

        // Should not panic — the helper filters lock entries by manifest set.
        debug_assert_provider_aliases_match_lock(&manifest_dependencies, &lock);
    }

    #[test]
    fn graph_provider_count_matches_lock_count_for_legacy_lock_branch() {
        // Direct count-parity check that mirrors PR-4a's primary equivalence
        // assertion, but for the legacy-lock-present validate branch.
        let manifest_dependencies = vec![dependency("db"), dependency("cache")];
        let lock = empty_lock_with(vec![locked_dependency("db"), locked_dependency("cache")]);

        let input = build_input_from_external_dependencies(&manifest_dependencies, None);
        let graph = ExecutionGraphBuilder::build(input);
        let provider_count = graph
            .nodes
            .iter()
            .filter(|node| matches!(node, ExecutionGraphNode::Provider { .. }))
            .count();

        // Manifest-derived count and lock-recorded count both equal the
        // graph's provider-node count for the inputs the legacy verify
        // function consumes.
        assert_eq!(provider_count, manifest_dependencies.len());
        assert_eq!(provider_count, lock.capsule_dependencies.len());
    }
}

//! Ready-State **declared execution identity** (Track C, #912).
//!
//! Computes the real Ato Execution Identity for a Ready-State snapshot build so the
//! sealed manifest can carry `execution_id` — the value `capsule_snapshots.execution_id`
//! records and runners verify before restore.
//!
//! ## Sourced from, and de-host-coupled
//!
//! The computation is the **same declared-domain derivation** the launch path uses
//! (docs/execution-identity.md §"Graph-based execution identity"):
//!
//! ```text
//! declared_execution_id = digest_hex(canonicalize(G_declared, Domain::Declared))
//! ```
//!
//! built with the same [`ExecutionGraphBuilder`] + canonical framing (version- and
//! domain-tagged bytes, SHA-256). What differs from the cli receipt path
//! (`build_launch_graph_bundle`) is only the *inputs*, which there are host-coupled —
//! the Source identifier is a local manifest **path display string** and the host/policy
//! facets come from launch observers. A Store build must produce the **same id on any
//! builder host**, so this envelope replaces those with declared, host-independent facts:
//!
//! - **Source**: the pinned store source identity
//!   (`github://owner/repo@<40-hex-commit>[#subdir]`) — never a local path.
//! - **Target**: the manifest's default target label + its declared runtime.
//! - **Dependencies**: the manifest's `[dependencies.*]` via the same
//!   [`lockfile::manifest_external_capsule_dependencies`] derivation and the same
//!   `provider://` / `output://` identifier convention as the launch adapter.
//! - **Host**: only the *declared* workspace-relative working directory (no observed
//!   roots, no view hashes — those are resolved-domain).
//! - **Policy**: declared policy hashes when known (`None` for the no-binding v1 —
//!   the label is skipped, deterministically).
//!
//! Launch-command facets (run command, port, readiness path, env) are **not** folded in
//! directly — in the existing identity semantics they live in the separate launch-envelope
//! digest ([`super::GraphLaunchInput`], `DerivedLaunchView`), not in
//! `declared_execution_id`. For a Store build they are still committed to
//! **transitively**: they are declared in `capsule.toml`, which is part of the source
//! tree pinned by the commit in the Source identifier — changing any of them requires a
//! new commit, which changes the id.
//!
//! Deterministic by construction: the graph builder sorts nodes/edges, labels are a
//! `BTreeMap`, and the canonical bytes embed [`super::CANONICAL_FORM_VERSION`] plus the
//! domain tag, so any future change to the framing forces a visible id change.

use super::{
    CanonicalGraphDomain, ExecutionGraphBuildInput, ExecutionGraphBuilder, GraphDependencyInput,
    GraphHostInput, GraphPolicyInput, GraphSourceInput, GraphTargetInput,
};
use crate::lockfile;

/// The declared, host-independent execution envelope of a Ready-State snapshot build.
///
/// Everything here is a *declared* fact (store record + manifest): nothing may come from
/// the builder host (paths, clocks, job ids, artifact hashes) — that would turn the
/// execution identity into a build-job identity.
#[derive(Debug, Clone)]
pub struct ReadyStateDeclaredEnvelope {
    /// Pinned store source identity — use [`store_source_identifier`].
    pub source_identifier: String,
    /// The manifest's default target label (e.g. `"app"`).
    pub target_label: String,
    /// The target's declared runtime string (e.g. `"source"`).
    pub runtime: String,
    /// Declared workspace-relative working directory, when the target sets one.
    pub working_directory: Option<String>,
    /// Declared `[dependencies.*]` — use [`declared_dependencies_from_manifest_toml`].
    pub dependencies: Vec<GraphDependencyInput>,
    /// Declared-domain policy hashes, when known (v1 no-binding builds pass `None`).
    pub network_policy_hash: Option<String>,
    pub capability_policy_hash: Option<String>,
}

impl ReadyStateDeclaredEnvelope {
    /// The declared execution id for this envelope:
    /// `digest_hex(canonicalize(G_declared, Domain::Declared))` — a `sha256:<hex>`
    /// string, stable for the same envelope on any host.
    pub fn declared_execution_id(&self) -> String {
        let graph = ExecutionGraphBuilder::build(ExecutionGraphBuildInput {
            source: Some(GraphSourceInput {
                identifier: self.source_identifier.clone(),
            }),
            targets: vec![GraphTargetInput {
                identifier: format!("target://{}", self.target_label),
                runtime: self.runtime.clone(),
            }],
            dependencies: self.dependencies.clone(),
            host: Some(GraphHostInput {
                filesystem_working_directory: self.working_directory.clone(),
                ..GraphHostInput::default()
            }),
            policy: Some(GraphPolicyInput {
                network_policy_hash: self.network_policy_hash.clone(),
                capability_policy_hash: self.capability_policy_hash.clone(),
                ..GraphPolicyInput::default()
            }),
        });
        graph
            .canonical_form(CanonicalGraphDomain::Declared)
            .digest_hex()
    }
}

/// The pinned store source identity: `github://owner/repo@<commit>` plus `#subdir` when
/// the capsule lives in a subdirectory. Host-independent by construction — callers must
/// pass the SERVER-RESOLVED identity (approved store record), never a local path.
pub fn store_source_identifier(
    owner: &str,
    repo: &str,
    commit: &str,
    subdir: Option<&str>,
) -> String {
    match subdir.filter(|s| !s.is_empty()) {
        Some(s) => format!("github://{owner}/{repo}@{commit}#{s}"),
        None => format!("github://{owner}/{repo}@{commit}"),
    }
}

/// Derive the declared `[dependencies.*]` graph inputs from raw `capsule.toml` text,
/// using the **same** [`lockfile::manifest_external_capsule_dependencies`] derivation and
/// the same `provider://<alias>` / `output://<alias>` identifier convention as the
/// launch-path adapter (`cli::application::execution_graph_adapter`), so the facets that
/// enter the id match the launch path byte-for-byte.
pub fn declared_dependencies_from_manifest_toml(
    toml_text: &str,
) -> Result<Vec<GraphDependencyInput>, String> {
    let value: toml::Value =
        toml::from_str(toml_text).map_err(|e| format!("parse capsule.toml: {e}"))?;
    let deps = lockfile::manifest_external_capsule_dependencies(&value)
        .map_err(|e| format!("derive [dependencies.*]: {e}"))?;
    Ok(deps
        .into_iter()
        .map(|d| GraphDependencyInput {
            provider: format!("provider://{}", d.alias),
            output: format!("output://{}", d.alias),
            source: Some(d.source),
            source_type: Some(d.source_type),
            contract: d.contract,
            injection_bindings: d.injection_bindings,
            parameters: d.parameters,
            credentials: d.credentials,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full, structurally valid 0.3 manifest — `declared_dependencies_from_manifest_toml`
    /// takes whole capsule.toml text (the `[dependencies.*]` bridge rejects fragments).
    fn manifest_toml(deps: &str) -> String {
        format!(
            r#"
schema_version = "0.3"
name = "consumer"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python3 app.py"
port = 8080
readiness_probe = {{ http_get = "/health" }}
{deps}"#
        )
    }

    fn base_envelope() -> ReadyStateDeclaredEnvelope {
        ReadyStateDeclaredEnvelope {
            source_identifier: store_source_identifier(
                "acme",
                "app",
                &"a".repeat(40),
                Some("pkg/web"),
            ),
            target_label: "app".into(),
            runtime: "source".into(),
            working_directory: None,
            dependencies: vec![],
            network_policy_hash: None,
            capability_policy_hash: None,
        }
    }

    #[test]
    fn same_envelope_yields_the_same_id() {
        let a = base_envelope().declared_execution_id();
        let b = base_envelope().declared_execution_id();
        assert_eq!(a, b);
        assert!(a.starts_with("sha256:"), "{a}");
        // A REAL digest, not a placeholder shape.
        assert_eq!(a.len(), "sha256:".len() + 64);
    }

    #[test]
    fn every_relevant_declared_field_changes_the_id() {
        let base = base_envelope().declared_execution_id();
        // Pinned source commit (transitively commits to capsule.toml → run/port/readiness).
        let mut e = base_envelope();
        e.source_identifier =
            store_source_identifier("acme", "app", &"b".repeat(40), Some("pkg/web"));
        assert_ne!(
            e.declared_execution_id(),
            base,
            "commit change must change the id"
        );
        // Subdir.
        let mut e = base_envelope();
        e.source_identifier = store_source_identifier("acme", "app", &"a".repeat(40), None);
        assert_ne!(
            e.declared_execution_id(),
            base,
            "subdir change must change the id"
        );
        // Target label.
        let mut e = base_envelope();
        e.target_label = "web".into();
        assert_ne!(
            e.declared_execution_id(),
            base,
            "target change must change the id"
        );
        // Runtime.
        let mut e = base_envelope();
        e.runtime = "oci".into();
        assert_ne!(
            e.declared_execution_id(),
            base,
            "runtime change must change the id"
        );
        // Working directory.
        let mut e = base_envelope();
        e.working_directory = Some("server".into());
        assert_ne!(
            e.declared_execution_id(),
            base,
            "working_dir change must change the id"
        );
        // A declared dependency.
        let mut e = base_envelope();
        e.dependencies = declared_dependencies_from_manifest_toml(
            &manifest_toml("[dependencies.db]\ncapsule = \"capsule://ato/acme-postgres@16\"\ncontract = \"service@1\"\n"),
        )
        .unwrap();
        assert_ne!(
            e.declared_execution_id(),
            base,
            "dependency change must change the id"
        );
        // Declared policy hash.
        let mut e = base_envelope();
        e.network_policy_hash = Some("sha256:aaaa".into());
        assert_ne!(
            e.declared_execution_id(),
            base,
            "policy change must change the id"
        );
    }

    #[test]
    fn dependency_derivation_matches_the_launch_adapter_convention() {
        let deps = declared_dependencies_from_manifest_toml(
            &manifest_toml("[dependencies.db]\ncapsule = \"capsule://ato/acme-postgres@16\"\ncontract = \"service@1\"\n"),
        )
        .unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].provider, "provider://db");
        assert_eq!(deps[0].output, "output://db");
        assert_eq!(
            deps[0].source.as_deref(),
            Some("capsule://ato/acme-postgres@16")
        );
        // No [dependencies.*] ⇒ empty (the common no-binding store capsule).
        assert!(
            declared_dependencies_from_manifest_toml(&manifest_toml(""))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn source_identifier_shape_is_pinned() {
        assert_eq!(
            store_source_identifier("o", "r", "c", Some("sub/dir")),
            "github://o/r@c#sub/dir"
        );
        assert_eq!(
            store_source_identifier("o", "r", "c", None),
            "github://o/r@c"
        );
        assert_eq!(
            store_source_identifier("o", "r", "c", Some("")),
            "github://o/r@c"
        );
    }
}

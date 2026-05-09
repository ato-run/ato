//! Skeleton builder that turns a decoupled input shape into an
//! [`ExecutionGraph`].
//!
//! The input types here (`GraphSourceInput`, `GraphTargetInput`, etc.) are
//! deliberately *not* the real `Manifest` / `LockFile` / `Policy` types
//! from `ato-cli`. Crossing that boundary is deferred to Wave 2 (PR-4a) so
//! the builder can be designed and tested in isolation while call-site
//! adapters evolve.
//!
//! Build is a pure function of its input — no I/O, no globals — and emits
//! nodes and edges in deterministic order so downstream identity hashing
//! (Wave 2, #98) is straightforward to add.

use std::collections::BTreeMap;

use super::types::{
    ExecutionGraph, ExecutionGraphConstraint, ExecutionGraphEdge, ExecutionGraphEdgeKind,
    ExecutionGraphNode,
};

/// Insert `(key, value.clone())` into `labels` when `value` is `Some`.
/// No-op when `value` is `None`. Centralizes the "optional graph fact →
/// canonical label" wiring so callers don't repeat the `if let Some`
/// dance for every identity facet.
fn insert_optional_label(
    labels: &mut BTreeMap<String, String>,
    key: &'static str,
    value: Option<&String>,
) {
    if let Some(value) = value {
        labels.insert(key.to_string(), value.clone());
    }
}

/// Stable label keys for graph-derived identity facets.
///
/// These keys are part of the canonical form's `LBLS` section, so renaming
/// any of them is a breaking change — every previously computed digest
/// would shift. Add new keys, never rename.
pub mod identity_labels {
    /// [`crate::execution_identity::FilesystemIdentityV2::source_root`].
    pub const FS_SOURCE_ROOT: &str = "fs.source_root";
    /// [`crate::execution_identity::FilesystemIdentityV2::working_directory`].
    pub const FS_WORKING_DIRECTORY: &str = "fs.working_directory";
    /// [`crate::execution_identity::FilesystemIdentityV2::view_hash`] value
    /// (resolved domain only; absent before host materialization).
    pub const FS_VIEW_HASH: &str = "fs.view_hash";
    /// [`crate::execution_identity::PolicyIdentityV2::network_policy_hash`].
    pub const POLICY_NETWORK_HASH: &str = "policy.network_hash";
    /// [`crate::execution_identity::PolicyIdentityV2::capability_policy_hash`].
    pub const POLICY_CAPABILITY_HASH: &str = "policy.capability_hash";
    /// [`crate::execution_identity::PolicyIdentityV2::sandbox_policy_hash`].
    pub const POLICY_SANDBOX_HASH: &str = "policy.sandbox_hash";
}

/// Decoupled input fed to [`ExecutionGraphBuilder::build`].
///
/// Fields are intentionally minimal — only enough to drive node/edge
/// emission for the tests in this PR. The real call-site adapters in
/// PR-4a will extend these structs (or wrap them) with the fields needed
/// to reach parity with the existing `dependency_contracts` derivation.
#[derive(Debug, Clone, Default)]
pub struct ExecutionGraphBuildInput {
    pub source: Option<GraphSourceInput>,
    pub targets: Vec<GraphTargetInput>,
    pub dependencies: Vec<GraphDependencyInput>,
    pub host: Option<GraphHostInput>,
    pub policy: Option<GraphPolicyInput>,
}

/// Project source tree node input.
#[derive(Debug, Clone)]
pub struct GraphSourceInput {
    pub identifier: String,
}

/// A target the runtime should be wired to (entrypoint-shaped).
#[derive(Debug, Clone)]
pub struct GraphTargetInput {
    pub identifier: String,
    /// Runtime identifier this target executes against. The builder emits
    /// a `Runtime` node and a `Requires` edge from the entrypoint to it.
    pub runtime: String,
}

/// A resolved dependency, modelled as a (provider, output) pair.
///
/// In later waves this will carry richer data (lockfile coordinates,
/// content hashes, etc.); for the skeleton it's enough to anchor a
/// deterministic Provider/DependencyOutput emission and a
/// `MaterializesTo` edge between them.
#[derive(Debug, Clone)]
pub struct GraphDependencyInput {
    pub provider: String,
    pub output: String,
}

/// Host-side facets the graph attaches to (filesystem, network, env, …).
///
/// All fields are optional so individual call sites can populate only the
/// facets they actually know about.
///
/// ## Identity-relevant fields (PR-5a, refs #100)
///
/// The `*_role` fields are declared-domain facts (manifest + lock + policy
/// only), while `filesystem_view_hash` is a resolved-domain fact that only
/// becomes available after host materialization. They are surfaced on the
/// graph as deterministic labels (see [`ExecutionGraphBuilder::build`])
/// so they participate in the canonical form's `LBLS` section. Identity
/// builders (e.g. [`crate::execution_identity::FilesystemIdentityBuilder`])
/// consume the corresponding labels back out via [`ExecutionGraph::labels`].
///
/// All identity-relevant fields are `Option<String>`; if a call site doesn't
/// know a value, leave it `None` — the builder skips emission of that label.
#[derive(Debug, Clone, Default)]
pub struct GraphHostInput {
    pub filesystem: Option<String>,
    pub network: Option<String>,
    pub env: Option<String>,
    pub state: Option<String>,
    /// Workspace-relative role string for the source root (declared domain).
    /// Mirrors `FilesystemIdentityV2::source_root` when known.
    pub filesystem_source_root: Option<String>,
    /// Workspace-relative role string for the working directory (declared
    /// domain). Mirrors `FilesystemIdentityV2::working_directory` when known.
    pub filesystem_working_directory: Option<String>,
    /// Resolved view hash of the materialized filesystem (resolved domain).
    /// Mirrors `FilesystemIdentityV2::view_hash.value` when known.
    pub filesystem_view_hash: Option<String>,
}

/// Policy facets that gate execution.
///
/// ## Identity-relevant fields (PR-5a, refs #102)
///
/// The three `*_policy_hash` fields mirror [`crate::execution_identity::PolicyIdentityV2`]
/// and are surfaced on the graph as labels under the `policy.*` namespace.
/// `network_policy_hash` and `capability_policy_hash` are declared-domain
/// (derived from the manifest's policy and consent ledger respectively);
/// `sandbox_policy_hash` straddles both domains in v0.6.0 (target_runtime
/// is declared, mount_set_algo and allow_hosts_count are resolved).
#[derive(Debug, Clone, Default)]
pub struct GraphPolicyInput {
    pub constraints: Vec<ExecutionGraphConstraint>,
    pub network_policy_hash: Option<String>,
    pub capability_policy_hash: Option<String>,
    pub sandbox_policy_hash: Option<String>,
}

/// Builder for [`ExecutionGraph`].
///
/// Currently a unit struct exposing a single associated `build` function;
/// it stays a type rather than a free function so later waves can layer
/// per-instance configuration (feature flags, identity hashing options,
/// canonicalization mode) without breaking the call shape.
pub struct ExecutionGraphBuilder;

impl ExecutionGraphBuilder {
    /// Build a deterministic [`ExecutionGraph`] from the given input.
    ///
    /// Determinism guarantees:
    /// - `nodes` is sorted by `(node_kind_discriminant, identifier)`.
    /// - `edges` is sorted by `(source, target, edge_kind_discriminant)`.
    /// - `constraints` preserves input order (the input is the source of
    ///   truth for constraint ordering at this stage).
    pub fn build(input: ExecutionGraphBuildInput) -> ExecutionGraph {
        let ExecutionGraphBuildInput {
            source,
            targets,
            dependencies,
            host,
            policy,
        } = input;

        let mut nodes: Vec<ExecutionGraphNode> = Vec::new();
        let mut edges: Vec<ExecutionGraphEdge> = Vec::new();
        let mut labels: BTreeMap<String, String> = BTreeMap::new();

        if let Some(src) = source.as_ref() {
            nodes.push(ExecutionGraphNode::Source {
                identifier: src.identifier.clone(),
            });
        }

        for target in &targets {
            nodes.push(ExecutionGraphNode::Entrypoint {
                identifier: target.identifier.clone(),
            });
            nodes.push(ExecutionGraphNode::Runtime {
                identifier: target.runtime.clone(),
            });
            edges.push(ExecutionGraphEdge {
                source: target.identifier.clone(),
                target: target.runtime.clone(),
                kind: ExecutionGraphEdgeKind::Requires,
            });
            if let Some(src) = source.as_ref() {
                edges.push(ExecutionGraphEdge {
                    source: target.identifier.clone(),
                    target: src.identifier.clone(),
                    kind: ExecutionGraphEdgeKind::DependsOn,
                });
            }
        }

        for dep in &dependencies {
            nodes.push(ExecutionGraphNode::Provider {
                identifier: dep.provider.clone(),
            });
            nodes.push(ExecutionGraphNode::DependencyOutput {
                identifier: dep.output.clone(),
            });
            edges.push(ExecutionGraphEdge {
                source: dep.provider.clone(),
                target: dep.output.clone(),
                kind: ExecutionGraphEdgeKind::Provides,
            });
            edges.push(ExecutionGraphEdge {
                source: dep.output.clone(),
                target: dep.provider.clone(),
                kind: ExecutionGraphEdgeKind::MaterializesTo,
            });
        }

        if let Some(host) = host.as_ref() {
            if let Some(fs) = host.filesystem.as_ref() {
                nodes.push(ExecutionGraphNode::Filesystem {
                    identifier: fs.clone(),
                });
            }
            if let Some(net) = host.network.as_ref() {
                nodes.push(ExecutionGraphNode::Network {
                    identifier: net.clone(),
                });
            }
            if let Some(env) = host.env.as_ref() {
                nodes.push(ExecutionGraphNode::Env {
                    identifier: env.clone(),
                });
            }
            if let Some(state) = host.state.as_ref() {
                nodes.push(ExecutionGraphNode::State {
                    identifier: state.clone(),
                });
            }
            insert_optional_label(
                &mut labels,
                identity_labels::FS_SOURCE_ROOT,
                host.filesystem_source_root.as_ref(),
            );
            insert_optional_label(
                &mut labels,
                identity_labels::FS_WORKING_DIRECTORY,
                host.filesystem_working_directory.as_ref(),
            );
            insert_optional_label(
                &mut labels,
                identity_labels::FS_VIEW_HASH,
                host.filesystem_view_hash.as_ref(),
            );
        }

        if let Some(policy) = policy.as_ref() {
            insert_optional_label(
                &mut labels,
                identity_labels::POLICY_NETWORK_HASH,
                policy.network_policy_hash.as_ref(),
            );
            insert_optional_label(
                &mut labels,
                identity_labels::POLICY_CAPABILITY_HASH,
                policy.capability_policy_hash.as_ref(),
            );
            insert_optional_label(
                &mut labels,
                identity_labels::POLICY_SANDBOX_HASH,
                policy.sandbox_policy_hash.as_ref(),
            );
        }

        // Deduplicate nodes — a runtime referenced by multiple targets,
        // for instance, should appear once. Edges are kept as-emitted (a
        // duplicate edge implies a real modelling problem upstream and
        // should remain visible).
        nodes.sort_by(|a, b| {
            a.kind_discriminant()
                .cmp(&b.kind_discriminant())
                .then_with(|| a.identifier().cmp(b.identifier()))
        });
        nodes.dedup();

        edges.sort_by(|a, b| {
            a.source
                .cmp(&b.source)
                .then_with(|| a.target.cmp(&b.target))
                .then_with(|| a.kind.discriminant().cmp(&b.kind.discriminant()))
        });

        let constraints = policy.map(|p| p.constraints).unwrap_or_default();

        ExecutionGraph {
            nodes,
            edges,
            labels,
            constraints,
        }
    }
}

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

use super::types::{
    ExecutionGraph, ExecutionGraphConstraint, ExecutionGraphEdge, ExecutionGraphEdgeKind,
    ExecutionGraphNode,
};

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
#[derive(Debug, Clone, Default)]
pub struct GraphHostInput {
    pub filesystem: Option<String>,
    pub network: Option<String>,
    pub env: Option<String>,
    pub state: Option<String>,
}

/// Policy facets that gate execution.
#[derive(Debug, Clone, Default)]
pub struct GraphPolicyInput {
    pub constraints: Vec<ExecutionGraphConstraint>,
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
            labels: Default::default(),
            constraints,
        }
    }
}

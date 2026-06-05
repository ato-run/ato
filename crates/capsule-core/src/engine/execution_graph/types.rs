//! Value types for the [`ExecutionGraph`].
//!
//! See [`super`] for module-level context. These types are deliberately
//! minimal and stringly-typed; richer fields land in later waves.

use std::collections::BTreeMap;

/// A node in an [`ExecutionGraph`].
///
/// Each variant carries an `identifier` string. Identifiers are opaque to
/// the graph itself — the convention for choosing them (e.g. how a
/// `Provider` is keyed) lives with the call-site adapters that build the
/// graph in later waves.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExecutionGraphNode {
    Source { identifier: String },
    Runtime { identifier: String },
    DependencyOutput { identifier: String },
    ToolCapsule { identifier: String },
    Service { identifier: String },
    Provider { identifier: String },
    Bridge { identifier: String },
    Env { identifier: String },
    Filesystem { identifier: String },
    Network { identifier: String },
    State { identifier: String },
    Entrypoint { identifier: String },
    Process { identifier: String },
    RuntimeInstance { identifier: String },
    BridgeCapability { identifier: String },
}

impl ExecutionGraphNode {
    /// Stable discriminant for deterministic ordering.
    ///
    /// Kept module-private so the numeric values can shift as new node
    /// kinds are added without breaking external callers.
    pub(super) fn kind_discriminant(&self) -> u8 {
        match self {
            Self::Source { .. } => 0,
            Self::Runtime { .. } => 1,
            Self::DependencyOutput { .. } => 2,
            Self::ToolCapsule { .. } => 3,
            Self::Service { .. } => 4,
            Self::Provider { .. } => 5,
            Self::Bridge { .. } => 6,
            Self::Env { .. } => 7,
            Self::Filesystem { .. } => 8,
            Self::Network { .. } => 9,
            Self::State { .. } => 10,
            Self::Entrypoint { .. } => 11,
            Self::Process { .. } => 12,
            Self::RuntimeInstance { .. } => 13,
            Self::BridgeCapability { .. } => 14,
        }
    }

    pub fn identifier(&self) -> &str {
        match self {
            Self::Source { identifier }
            | Self::Runtime { identifier }
            | Self::DependencyOutput { identifier }
            | Self::ToolCapsule { identifier }
            | Self::Service { identifier }
            | Self::Provider { identifier }
            | Self::Bridge { identifier }
            | Self::Env { identifier }
            | Self::Filesystem { identifier }
            | Self::Network { identifier }
            | Self::State { identifier }
            | Self::Entrypoint { identifier }
            | Self::Process { identifier }
            | Self::RuntimeInstance { identifier }
            | Self::BridgeCapability { identifier } => identifier,
        }
    }

    /// Stable, serialization-friendly label for the node kind.
    ///
    /// Unlike [`Self::kind_discriminant`] (an internal ordering aid whose
    /// numeric values may shift), this label is part of the receipt contract:
    /// it is what a [`super::super::execution_identity::NodeReceipt`] records as
    /// its `kind`. Keep the strings stable.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Source { .. } => "source",
            Self::Runtime { .. } => "runtime",
            Self::DependencyOutput { .. } => "dependency-output",
            Self::ToolCapsule { .. } => "tool-capsule",
            Self::Service { .. } => "service",
            Self::Provider { .. } => "provider",
            Self::Bridge { .. } => "bridge",
            Self::Env { .. } => "env",
            Self::Filesystem { .. } => "filesystem",
            Self::Network { .. } => "network",
            Self::State { .. } => "state",
            Self::Entrypoint { .. } => "entrypoint",
            Self::Process { .. } => "process",
            Self::RuntimeInstance { .. } => "runtime-instance",
            Self::BridgeCapability { .. } => "bridge-capability",
        }
    }
}

/// Kind of relationship represented by an [`ExecutionGraphEdge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecutionGraphEdgeKind {
    DependsOn,
    MaterializesTo,
    Provides,
    Requires,
    ConnectsTo,
    Grants,
    Mounts,
    Injects,
    StartsBefore,
    Observes,
}

impl ExecutionGraphEdgeKind {
    pub(super) fn discriminant(self) -> u8 {
        match self {
            Self::DependsOn => 0,
            Self::MaterializesTo => 1,
            Self::Provides => 2,
            Self::Requires => 3,
            Self::ConnectsTo => 4,
            Self::Grants => 5,
            Self::Mounts => 6,
            Self::Injects => 7,
            Self::StartsBefore => 8,
            Self::Observes => 9,
        }
    }

    /// Stable, serialization-friendly label for the edge kind. Part of the
    /// receipt contract (an [`super::super::execution_identity::EdgeReceipt`]
    /// records this as its `kind`); keep the strings stable.
    pub fn kind_label(self) -> &'static str {
        match self {
            Self::DependsOn => "depends-on",
            Self::MaterializesTo => "materializes-to",
            Self::Provides => "provides",
            Self::Requires => "requires",
            Self::ConnectsTo => "connects-to",
            Self::Grants => "grants",
            Self::Mounts => "mounts",
            Self::Injects => "injects",
            Self::StartsBefore => "starts-before",
            Self::Observes => "observes",
        }
    }
}

/// A directed edge between two nodes in an [`ExecutionGraph`].
///
/// `source` and `target` are node identifiers (matching the `identifier`
/// field of an [`ExecutionGraphNode`] variant). The graph does not enforce
/// referential integrity at this stage — that lives in canonicalization,
/// which is Wave 2 work.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExecutionGraphEdge {
    pub source: String,
    pub target: String,
    pub kind: ExecutionGraphEdgeKind,
}

/// Placeholder constraint shape.
///
/// The constraint vocabulary is intentionally unspecified in this skeleton;
/// later waves will replace `kind: String` with a structured enum once the
/// canonicalization design (#98) firms up the constraint catalogue.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExecutionGraphConstraint {
    pub kind: String,
    pub target: String,
}

/// The graph value produced by [`super::ExecutionGraphBuilder::build`].
///
/// Node and edge collections are emitted in deterministic order (see
/// builder docs). `labels` is a sorted-by-construction `BTreeMap`, so its
/// iteration order is also stable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionGraph {
    pub nodes: Vec<ExecutionGraphNode>,
    pub edges: Vec<ExecutionGraphEdge>,
    pub labels: BTreeMap<String, String>,
    pub constraints: Vec<ExecutionGraphConstraint>,
}

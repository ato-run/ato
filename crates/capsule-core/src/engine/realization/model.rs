//! Output model for the Capsule Realization Contract (#498-A).
//!
//! These types are the *typed answer* to a single question:
//!
//! > Can a resolved Capsule reconstruct an equivalent launch envelope, or
//! > does it fail with a typed explanation of what is missing?
//!
//! They are deliberately a pure data model — no I/O, no provider handles, no
//! runtime evidence. A [`RealizationContract`] is produced by
//! [`super::classify`] from typed node facts, or by [`super::bundle`] from a
//! [`crate::engine::execution_graph::LaunchGraphBundle`] plus host/provider
//! evidence.
//!
//! ## Identity boundary (#501)
//!
//! The contract's only identity field is [`RealizationContract::resolved_execution_id`],
//! which is always the graph-derived resolved execution id. A container id,
//! process id, image digest, or rendered `docker`/`podman run` string is
//! **never** identity here — when such a string appears it is carried as
//! [`RealizationEvidence::DerivedProjectionCommand`], i.e. evidence derived
//! *from* the graph, not the source of truth.

use serde::{Deserialize, Serialize};

/// The kind of node being classified.
///
/// This is the realization-contract taxonomy, not the
/// [`crate::engine::execution_graph::ExecutionGraphNode`] taxonomy. The two
/// overlap but are not identical: the realization view splits concerns the
/// raw graph keeps merged (e.g. a policy facet becomes a distinct
/// [`Self::NetworkPolicy`] / [`Self::CapabilityPolicy`] node), and #498-A only
/// classifies the subset that can be grounded from today's launch bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RealizationNodeKind {
    /// Project source tree.
    Source,
    /// Language/runtime toolchain the entrypoint executes against.
    Runtime,
    /// A pinned runtime tool binary (keyed by lockfile `binary_sha256`).
    RuntimeTool,
    /// A dependency derivation (the build that produces a dependency output).
    DependencyDerivation,
    /// A concrete dependency output artifact.
    DependencyOutput,
    /// The materialized filesystem view (mounts/overlays) presented to launch.
    FilesystemView,
    /// The closed set of environment values required at launch.
    EnvClosure,
    /// A capability/sandbox policy facet (mount readonly, dropped caps, …).
    CapabilityPolicy,
    /// A network policy facet (deny-by-default, allow-list, …).
    NetworkPolicy,
    /// A binding to persistent/prior state.
    StateBinding,
    /// The launch entrypoint (argv + cwd).
    Entrypoint,
    /// A provider projection of the resolved capsule (e.g. OCI). The rendered
    /// invocation is evidence, never identity.
    ProviderProjection,
}

/// Typed classification of whether a single node can be realized.
///
/// The three "bound" statuses ([`Self::HostBound`], [`Self::StateBound`],
/// [`Self::PolicyDowngraded`]) are **not** failures: a capsule can still be
/// [`RealizationResult::Realized`] while depending on host conditions,
/// persistent state, or a provider that cannot fully enforce a policy. They
/// exist so those facts stay *visible* rather than being silently absorbed
/// into a clean `Verified`. Only [`Self::Unavailable`] forces an
/// [`RealizationResult::Unrealizable`] result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RealizationStatus {
    /// Re-derivable: materializing it would reconstruct the node, and there is
    /// declared evidence (a hash/identity) backing that claim.
    Materializable,
    /// Already present and checked against its declared identity (local
    /// artifact, runtime, source, or state that exists and matches).
    Verified,
    /// Depends on a host-specific condition (an absolute host path, a host
    /// fingerprint). Realizable, but not host-portable.
    HostBound,
    /// Depends on persistent state or a prior binding that lives outside the
    /// resolved graph.
    StateBound,
    /// A required node cannot be reconstructed or verified.
    Unavailable,
    /// A policy is required but the projecting provider cannot fully enforce
    /// it. The downgrade is surfaced rather than reported as clean.
    PolicyDowngraded,
    /// Classification could not be decided from available facts.
    Unknown,
}

impl RealizationStatus {
    /// Whether this status, on a node the contract treats as required, makes
    /// the overall result [`RealizationResult::Unrealizable`].
    ///
    /// Only [`Self::Unavailable`] is fail-closed here. Strict fail-closed for
    /// `PolicyDowngraded`/`HostBound` is a *profile* decision and is out of
    /// scope for #498-A (it lands with strict-profile launch policy, #500).
    pub fn is_blocking(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

/// Redacted, receipt-safe representation of a rendered provider invocation.
///
/// This holds the *shape* of an OCI/provider argv — flags and structural
/// tokens — with every value reduced to a `<redacted>` placeholder. It
/// deliberately never holds the raw command: an OCI invocation can embed env
/// values, tokens, DB URLs, or absolute paths, and a [`RealizationContract`]
/// is declared serde-ready and is the input the future receipt writeback
/// (#493) will persist. Fixing the redaction boundary here — in the core model
/// — means a raw command can never reach a serialized contract by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactedProjectionCommand {
    /// Renderer label, e.g. `"podman-create"`, `"docker-run"`. Caller-supplied
    /// and value-free.
    pub renderer: String,
    /// Argv tokens with all values redacted: flags are preserved, every
    /// positional or `KEY=VALUE` token becomes `<redacted>`, and a long flag
    /// carrying an inline value (`--name=app`) becomes `--name=<redacted>`.
    pub argv_shape: Vec<String>,
    /// Always `true` once built via [`Self::from_argv`]; present so a reader
    /// can assert the evidence is redacted before persisting it.
    pub redacted: bool,
}

impl RedactedProjectionCommand {
    /// Placeholder substituted for every redacted value.
    pub const PLACEHOLDER: &'static str = "<redacted>";

    /// Build redacted evidence from a renderer label and a raw argv. The raw
    /// argv is consumed here and never stored: only the value-free shape
    /// survives.
    pub fn from_argv(renderer: impl Into<String>, argv: &[String]) -> Self {
        Self {
            renderer: renderer.into(),
            argv_shape: argv.iter().map(|token| redact_token(token)).collect(),
            redacted: true,
        }
    }
}

/// Reduce a single argv token to its value-free shape. A bare flag survives; a
/// flag with an inline value keeps the flag name only; everything else (a
/// positional argument or a `KEY=VALUE` assignment) is fully redacted, since it
/// may carry an env value, token, URL, or path.
fn redact_token(token: &str) -> String {
    let placeholder = RedactedProjectionCommand::PLACEHOLDER;
    if let Some(rest) = token.strip_prefix("--") {
        match rest.split_once('=') {
            Some((key, _value)) => format!("--{key}={placeholder}"),
            None => token.to_string(),
        }
    } else if token.starts_with('-') && token.len() > 1 {
        match token.split_once('=') {
            Some((key, _value)) => format!("{key}={placeholder}"),
            None => token.to_string(),
        }
    } else {
        placeholder.to_string()
    }
}

/// A typed piece of evidence backing a node's status.
///
/// Evidence never contains resolved secret values. The
/// [`Self::DerivedProjectionCommand`] variant is the explicit home for a
/// rendered provider invocation (e.g. an OCI `podman run …`): it is derived
/// *from* the graph and recorded as **redacted** evidence
/// ([`RedactedProjectionCommand`]), never the raw command and never promoted to
/// identity (#501).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "evidence", rename_all = "kebab-case")]
pub enum RealizationEvidence {
    /// A declared content/identity hash that makes a node materializable.
    DeclaredHash { label: String, hash: String },
    /// A present artifact verified against its declared identity (the declared
    /// and materialized hashes both exist and match).
    VerifiedArtifact { label: String, hash: String },
    /// A present artifact whose materialized hash does **not** match the
    /// declared identity. Content/identity hashes are safe to record. This
    /// makes the node `Unavailable`, never `Verified`.
    HashMismatch {
        label: String,
        declared: String,
        actual: String,
    },
    /// A host-specific binding (e.g. an absolute host path role).
    HostBinding { detail: String },
    /// A persistent-state binding reference (never a secret value).
    StateBinding { reference: String },
    /// A gap between a required policy and provider enforcement.
    PolicyEnforcementGap { policy: String, detail: String },
    /// A rendered provider invocation, derived from the graph and redacted.
    /// Evidence only — explicitly **not** identity (#501), and never the raw
    /// command (#498-A review).
    DerivedProjectionCommand {
        provider: String,
        command: RedactedProjectionCommand,
    },
    /// Free-form note for facts that do not yet have a typed variant.
    Note { detail: String },
}

/// Why a resolved capsule cannot reconstruct an equivalent launch envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum UnrealizableReason {
    /// A required immutable input node cannot be reconstructed or verified.
    MissingImmutableInput {
        node_id: String,
        node_kind: RealizationNodeKind,
    },
    /// A required immutable input is present but its materialized hash does not
    /// match its declared identity, so it cannot be trusted.
    MismatchedImmutableInput {
        node_id: String,
        node_kind: RealizationNodeKind,
    },
    /// A runtime tool's `binary_sha256` is not populated, so it cannot be
    /// verified. Until cross-platform lockfile population lands it must be
    /// `Unavailable`, never `Verified` (#473).
    RuntimeToolBinaryHashUnavailable { node_id: String },
    /// A required dependency output has no content hash to materialize from.
    MissingDependencyOutput { node_id: String },
    /// A required persistent-state binding is missing and no creation policy
    /// exists to establish it.
    MissingStateBinding { node_id: String },
    /// The launch requires an environment value that is not declared/closed.
    UndeclaredEnvRequired { node_id: String },
}

/// The overall verdict of a [`RealizationContract`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum RealizationResult {
    /// The resolved capsule can reconstruct an equivalent launch envelope.
    Realized,
    /// It cannot, with one or more typed explanations.
    Unrealizable { reasons: Vec<UnrealizableReason> },
}

impl RealizationResult {
    pub fn is_realized(&self) -> bool {
        matches!(self, Self::Realized)
    }
}

/// Per-node classification result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizationNodeStatus {
    pub node_id: String,
    pub node_kind: RealizationNodeKind,
    pub status: RealizationStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<RealizationEvidence>,
}

/// Whether an edge's endpoints can both be realized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RealizationEdgeState {
    /// Both endpoints are realizable (neither is `Unavailable`).
    Connectable,
    /// At least one endpoint is `Unavailable`, so the relationship cannot hold.
    BrokenByUnavailableEndpoint,
}

/// Per-edge classification result. The edge `kind` is carried as a string so
/// the contract stays decoupled from the
/// [`crate::engine::execution_graph::ExecutionGraphEdgeKind`] vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizationEdgeStatus {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub state: RealizationEdgeState,
}

/// The typed Capsule Realization Contract for one resolved capsule.
///
/// `resolved_execution_id` is the sole identity field and is always the
/// graph-derived resolved execution id — never a container id, pid, or
/// rendered command string (#501).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizationContract {
    pub resolved_execution_id: String,
    pub node_statuses: Vec<RealizationNodeStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_statuses: Vec<RealizationEdgeStatus>,
    pub result: RealizationResult,
}

impl RealizationContract {
    /// Iterator over node statuses with the given status value.
    pub fn nodes_with_status(
        &self,
        status: RealizationStatus,
    ) -> impl Iterator<Item = &RealizationNodeStatus> {
        self.node_statuses
            .iter()
            .filter(move |n| n.status == status)
    }

    /// Whether any node carries the given status.
    pub fn has_status(&self, status: RealizationStatus) -> bool {
        self.nodes_with_status(status).next().is_some()
    }
}

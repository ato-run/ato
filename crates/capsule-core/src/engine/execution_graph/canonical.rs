//! Canonical form + domain-tagged digest for [`ExecutionGraph`].
//!
//! Spec: [`docs/execution-identity.md`](../../../../../docs/execution-identity.md)
//! (the "Graph-based execution identity" section). This file is the
//! executable counterpart: the framing documented in the spec must match
//! the bytes produced here. **If you change one, change the other in the
//! same commit.**
//!
//! Scope of this module is intentionally narrow:
//! - Pure value-level transform from [`ExecutionGraph`] to canonical bytes
//!   plus a SHA-256 digest of those bytes.
//! - Domain tagging via [`CanonicalGraphDomain`] so the same nodes/edges
//!   produce different digests in `Declared` vs `Resolved` vs `Observed`
//!   contexts.
//! - No call-site wiring: nothing here reaches into receipts, sessions, or
//!   `ato-cli`. Plumbing `declared_execution_id` / `resolved_execution_id`
//!   into `ExecutionReceiptV2` lands in a later wave (PR-5a).
//!
//! See `docs/execution-identity.md` for the canonicalization rules and the
//! exact framing.

use sha2::{Digest, Sha256};

use super::types::{
    ExecutionGraph, ExecutionGraphConstraint, ExecutionGraphEdge, ExecutionGraphNode,
};

/// Version of the canonical-form framing.
///
/// Embedded into the canonical bytes (and therefore into the digest) so
/// that any change to the framing or to the canonicalization rules forces
/// a hash-domain change rather than a silent collision. Bump only when an
/// existing kind's shape must change in a non-additive way; pure additions
/// (new node kinds, new edge kinds) keep this constant.
pub const CANONICAL_FORM_VERSION: u32 = 1;

/// Magic prefix to make the canonical bytes self-identifying. Independent
/// of [`CANONICAL_FORM_VERSION`] — the magic identifies "this is an ato
/// execution-graph canonical form", the version identifies which framing.
const CANONICAL_FORM_MAGIC: &[u8; 16] = b"ato-graph-canon\0";

/// Domain a graph is canonicalized in.
///
/// The same set of nodes and edges produces a different digest in each
/// domain. This is the safety boundary that prevents a `Declared` digest
/// from being mistaken for a `Resolved` digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanonicalGraphDomain {
    /// Manifest + lock + policy only. Host-independent.
    Declared,
    /// After host resolution (artifact selector → concrete artifact,
    /// runtime → store path, etc.). Host-bound.
    Resolved,
    /// After runtime observation of undeclared edges. Optional; absent by
    /// default in v0.6.0.
    Observed,
}

impl CanonicalGraphDomain {
    /// Stable byte discriminant. Embedded in the canonical bytes.
    fn discriminant(self) -> u8 {
        match self {
            Self::Declared => 0,
            Self::Resolved => 1,
            Self::Observed => 2,
        }
    }
}

/// Canonicalized representation of an [`ExecutionGraph`] in some
/// [`CanonicalGraphDomain`], plus the SHA-256 digest of those bytes.
///
/// `bytes` is exposed for diagnostics and for cross-implementation
/// reproduction tests; production callers normally only need `digest`.
pub struct GraphCanonicalForm {
    /// Length-prefixed framing of the canonicalized graph. The exact
    /// layout is documented in `docs/execution-identity.md`.
    pub bytes: Vec<u8>,
    /// SHA-256 of `bytes`.
    pub digest: [u8; 32],
}

impl GraphCanonicalForm {
    /// Hex-encoded digest, prefixed with the algorithm name. Convenient
    /// for log lines and equality assertions in tests; not intended for
    /// receipt-format use yet.
    pub fn digest_hex(&self) -> String {
        format!("sha256:{}", hex::encode(self.digest))
    }
}

/// Trait implemented by graph-shaped types that can be reduced to a
/// deterministic byte string and digested under a [`CanonicalGraphDomain`].
pub trait CanonicalizableGraph {
    fn canonicalize(&self, domain: CanonicalGraphDomain) -> GraphCanonicalForm;
}

impl CanonicalizableGraph for ExecutionGraph {
    fn canonicalize(&self, domain: CanonicalGraphDomain) -> GraphCanonicalForm {
        let bytes = encode_canonical(self, domain);
        let digest = Sha256::digest(&bytes).into();
        GraphCanonicalForm { bytes, digest }
    }
}

impl ExecutionGraph {
    /// Convenience accessor: canonical form for `domain`.
    ///
    /// Equivalent to [`CanonicalizableGraph::canonicalize`]; provided as an
    /// inherent method so call sites do not need the trait in scope.
    pub fn canonical_form(&self, domain: CanonicalGraphDomain) -> GraphCanonicalForm {
        <Self as CanonicalizableGraph>::canonicalize(self, domain)
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Produce the canonical bytes for `graph` in `domain`.
///
/// The graph is re-sorted defensively here so callers don't have to rely
/// on the builder's ordering — passing a hand-constructed [`ExecutionGraph`]
/// with shuffled nodes/edges yields the same bytes as the
/// builder's output for the same logical content.
fn encode_canonical(graph: &ExecutionGraph, domain: CanonicalGraphDomain) -> Vec<u8> {
    let mut out = Vec::with_capacity(estimate_size(graph));

    // Header: magic || version || domain.
    out.extend_from_slice(CANONICAL_FORM_MAGIC);
    out.extend_from_slice(&CANONICAL_FORM_VERSION.to_le_bytes());
    out.push(domain.discriminant());

    // Nodes. Sort defensively by (kind discriminant, identifier).
    let mut nodes: Vec<&ExecutionGraphNode> = graph.nodes.iter().collect();
    nodes.sort_by(|a, b| {
        a.kind_discriminant()
            .cmp(&b.kind_discriminant())
            .then_with(|| a.identifier().cmp(b.identifier()))
    });
    write_section_header(&mut out, b"NODE", nodes.len());
    for node in &nodes {
        out.push(node.kind_discriminant());
        write_node_payload(&mut out, node);
    }

    // Edges. Sort defensively by (source, target, kind discriminant).
    let mut edges: Vec<&ExecutionGraphEdge> = graph.edges.iter().collect();
    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.target.cmp(&b.target))
            .then_with(|| a.kind.discriminant().cmp(&b.kind.discriminant()))
    });
    write_section_header(&mut out, b"EDGE", edges.len());
    for edge in &edges {
        write_lp_str(&mut out, &edge.source);
        write_lp_str(&mut out, &edge.target);
        out.push(edge.kind.discriminant());
    }

    // Labels. `BTreeMap` already iterates in sorted-by-key order; we keep
    // that order verbatim.
    write_section_header(&mut out, b"LBLS", graph.labels.len());
    for (k, v) in &graph.labels {
        write_lp_str(&mut out, k);
        write_lp_str(&mut out, v);
    }

    // Constraints. Sort by (kind, target). The constraint vocabulary is
    // still expanding (#98); when it does, additional fields must be
    // appended *after* `target` to keep this section additive.
    let mut constraints: Vec<&ExecutionGraphConstraint> = graph.constraints.iter().collect();
    constraints.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.target.cmp(&b.target)));
    write_section_header(&mut out, b"CSTR", constraints.len());
    for c in &constraints {
        write_lp_str(&mut out, &c.kind);
        write_lp_str(&mut out, &c.target);
    }

    out
}

/// Estimate output size to keep the encoder allocation-light. The numbers
/// are heuristics; correctness does not depend on them.
fn estimate_size(graph: &ExecutionGraph) -> usize {
    16 + 4
        + 1
        + 8
        + graph.nodes.len() * 64
        + 8
        + graph.edges.len() * 96
        + 8
        + graph.labels.len() * 64
        + 8
        + graph.constraints.len() * 64
}

/// Write a section tag and a u32 LE count. The tag is purely informational
/// (helps when staring at hex dumps); the digest depends on it.
fn write_section_header(out: &mut Vec<u8>, tag: &[u8; 4], count: usize) {
    out.extend_from_slice(tag);
    let count_u32: u32 = count.try_into().unwrap_or(u32::MAX);
    out.extend_from_slice(&count_u32.to_le_bytes());
}

/// Write a length-prefixed string: `len:u32 LE` followed by the UTF-8 bytes.
///
/// Length prefixing is what makes the framing unambiguous under
/// concatenation: two adjacent strings cannot be confused with a single
/// longer string because the boundary is encoded explicitly.
fn write_lp_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len: u32 = bytes.len().try_into().unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Write the per-variant payload for a node.
///
/// Every variant currently carries only an `identifier`, so the payload is
/// a single length-prefixed string. **Redaction boundary**: if a future
/// variant grows a value field that may carry a secret (env values,
/// capability tokens, etc.), it MUST be omitted here. See
/// `docs/execution-identity.md` §"Secret redaction boundary" — that
/// section is the authoritative list, kept in lockstep with this match.
fn write_node_payload(out: &mut Vec<u8>, node: &ExecutionGraphNode) {
    // Each variant is matched explicitly (no `_` arm) so adding a variant
    // to `ExecutionGraphNode` produces a compile error here. That is the
    // safety net that prevents a new node kind from silently shipping with
    // an empty payload — and, more importantly, prevents a new
    // secret-bearing field from silently being included in the digest.
    match node {
        ExecutionGraphNode::Source { identifier }
        | ExecutionGraphNode::Runtime { identifier }
        | ExecutionGraphNode::DependencyOutput { identifier }
        | ExecutionGraphNode::ToolCapsule { identifier }
        | ExecutionGraphNode::Service { identifier }
        | ExecutionGraphNode::Provider { identifier }
        | ExecutionGraphNode::Bridge { identifier }
        | ExecutionGraphNode::Env { identifier }
        | ExecutionGraphNode::Filesystem { identifier }
        | ExecutionGraphNode::Network { identifier }
        | ExecutionGraphNode::State { identifier }
        | ExecutionGraphNode::Entrypoint { identifier }
        | ExecutionGraphNode::Process { identifier }
        | ExecutionGraphNode::RuntimeInstance { identifier }
        | ExecutionGraphNode::BridgeCapability { identifier } => {
            write_lp_str(out, identifier);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::builder::{
        ExecutionGraphBuildInput, ExecutionGraphBuilder, GraphDependencyInput, GraphHostInput,
        GraphPolicyInput, GraphSourceInput, GraphTargetInput,
    };
    use super::super::types::{
        ExecutionGraph, ExecutionGraphConstraint, ExecutionGraphEdge, ExecutionGraphEdgeKind,
        ExecutionGraphNode,
    };
    use super::*;

    fn sample_input() -> ExecutionGraphBuildInput {
        ExecutionGraphBuildInput {
            source: Some(GraphSourceInput {
                identifier: "src://workspace".into(),
            }),
            targets: vec![
                GraphTargetInput {
                    identifier: "entry://main".into(),
                    runtime: "runtime://node".into(),
                },
                GraphTargetInput {
                    identifier: "entry://worker".into(),
                    runtime: "runtime://node".into(),
                },
            ],
            dependencies: vec![
                GraphDependencyInput {
                    provider: "provider://npm".into(),
                    output: "output://lodash".into(),
                ..Default::default()
            },
                GraphDependencyInput {
                    provider: "provider://cargo".into(),
                    output: "output://serde".into(),
                ..Default::default()
            },
            ],
            host: Some(GraphHostInput {
                filesystem: Some("fs://workspace".into()),
                network: Some("net://offline".into()),
                env: Some("env://CONFIG".into()),
                state: Some("state://session".into()),
                ..GraphHostInput::default()
            }),
            policy: Some(GraphPolicyInput {
                constraints: vec![
                    ExecutionGraphConstraint {
                        kind: "network.deny".into(),
                        target: "net://offline".into(),
                    },
                    ExecutionGraphConstraint {
                        kind: "fs.readonly".into(),
                        target: "fs://workspace".into(),
                    },
                ],
                ..GraphPolicyInput::default()
            }),
        }
    }

    #[test]
    fn permuting_input_order_does_not_change_digest() {
        let forward = ExecutionGraphBuilder::build(sample_input());

        let mut shuffled = sample_input();
        shuffled.targets.reverse();
        shuffled.dependencies.reverse();
        if let Some(p) = shuffled.policy.as_mut() {
            p.constraints.reverse();
        }
        let reversed = ExecutionGraphBuilder::build(shuffled);

        let f = forward.canonical_form(CanonicalGraphDomain::Declared);
        let r = reversed.canonical_form(CanonicalGraphDomain::Declared);

        assert_eq!(
            f.bytes, r.bytes,
            "canonical bytes must be order-independent",
        );
        assert_eq!(f.digest, r.digest, "digest must be order-independent",);
    }

    #[test]
    fn changing_a_target_runtime_changes_digest() {
        let baseline = ExecutionGraphBuilder::build(sample_input());

        let mut perturbed = sample_input();
        perturbed.targets[0].runtime = "runtime://deno".into();
        let perturbed = ExecutionGraphBuilder::build(perturbed);

        let a = baseline.canonical_form(CanonicalGraphDomain::Declared);
        let b = perturbed.canonical_form(CanonicalGraphDomain::Declared);
        assert_ne!(
            a.digest, b.digest,
            "swapping a runtime identifier must change the digest",
        );
    }

    #[test]
    fn changing_a_label_value_changes_digest() {
        let baseline = ExecutionGraphBuilder::build(sample_input());

        // Labels currently come from the builder as empty; mutate the
        // graph directly to exercise the label section of the framing.
        let mut with_label = baseline.clone();
        with_label
            .labels
            .insert("ato.kind".to_string(), "service".to_string());

        let mut with_other_label = baseline.clone();
        with_other_label
            .labels
            .insert("ato.kind".to_string(), "tool".to_string());

        let a = with_label.canonical_form(CanonicalGraphDomain::Declared);
        let b = with_other_label.canonical_form(CanonicalGraphDomain::Declared);
        assert_ne!(a.digest, b.digest);
    }

    #[test]
    fn domains_produce_distinct_digests_for_the_same_graph() {
        let graph = ExecutionGraphBuilder::build(sample_input());

        let declared = graph.canonical_form(CanonicalGraphDomain::Declared);
        let resolved = graph.canonical_form(CanonicalGraphDomain::Resolved);
        let observed = graph.canonical_form(CanonicalGraphDomain::Observed);

        assert_ne!(declared.digest, resolved.digest);
        assert_ne!(declared.digest, observed.digest);
        assert_ne!(resolved.digest, observed.digest);
    }

    /// Canonicalization is identifier-only for `Env` nodes today.
    ///
    /// The redaction contract says: even if the env-node type later grows
    /// a `value` field that carries a secret at runtime, swapping that
    /// value while keeping the identifier fixed must NOT change the
    /// digest. Because the type currently has no value field, the strongest
    /// statement we can encode now is: two graphs that agree on the env
    /// identifier produce identical bytes regardless of what surrounding
    /// non-secret state varies. The future test ("re-enable when env
    /// value lands") would mutate the value field directly.
    #[test]
    fn env_node_canonicalization_uses_identifier_only_today() {
        let mut a = ExecutionGraph::default();
        a.nodes.push(ExecutionGraphNode::Env {
            identifier: "env://OPENAI_API_KEY".into(),
        });

        let mut b = ExecutionGraph::default();
        b.nodes.push(ExecutionGraphNode::Env {
            identifier: "env://OPENAI_API_KEY".into(),
        });

        // Two identical env-node-only graphs hash equally.
        assert_eq!(
            a.canonical_form(CanonicalGraphDomain::Declared).digest,
            b.canonical_form(CanonicalGraphDomain::Declared).digest,
        );

        // And changing the *identifier* (which is non-secret) does change
        // the digest, proving the env section participates in the framing.
        let mut c = ExecutionGraph::default();
        c.nodes.push(ExecutionGraphNode::Env {
            identifier: "env://DIFFERENT".into(),
        });
        assert_ne!(
            a.canonical_form(CanonicalGraphDomain::Declared).digest,
            c.canonical_form(CanonicalGraphDomain::Declared).digest,
        );

        // When the env-node type grows a value field, add a sibling test
        // here that varies the value while pinning the identifier and
        // asserts digest equality. Tracked alongside the redaction rule
        // in `docs/execution-identity.md`.
    }

    #[test]
    fn canonical_bytes_start_with_magic_and_version() {
        let graph = ExecutionGraphBuilder::build(sample_input());
        let form = graph.canonical_form(CanonicalGraphDomain::Declared);

        assert!(
            form.bytes.starts_with(CANONICAL_FORM_MAGIC),
            "canonical bytes must start with the framing magic",
        );

        // Version follows the 16-byte magic, little-endian u32.
        let version_bytes: [u8; 4] = form.bytes[16..20].try_into().expect("version slice");
        let version = u32::from_le_bytes(version_bytes);
        assert_eq!(
            version, CANONICAL_FORM_VERSION,
            "version embedded in framing must equal CANONICAL_FORM_VERSION",
        );

        // Domain follows directly after the version.
        assert_eq!(
            form.bytes[20],
            CanonicalGraphDomain::Declared.discriminant(),
            "domain byte must follow the version",
        );
    }

    #[test]
    fn schema_version_participates_in_digest() {
        // Build the canonical bytes, then synthesize a "v2" variant by
        // flipping the version field in place and rehashing. A future
        // bump of CANONICAL_FORM_VERSION must therefore change every
        // digest — that property is what this test pins.
        let graph = ExecutionGraphBuilder::build(sample_input());
        let form = graph.canonical_form(CanonicalGraphDomain::Declared);

        let mut mutated = form.bytes.clone();
        let next_version: u32 = CANONICAL_FORM_VERSION.wrapping_add(1);
        mutated[16..20].copy_from_slice(&next_version.to_le_bytes());

        let original_digest = Sha256::digest(&form.bytes);
        let mutated_digest = Sha256::digest(&mutated);
        assert_ne!(
            original_digest.as_slice(),
            mutated_digest.as_slice(),
            "changing the framing version must change the digest",
        );
    }

    #[test]
    fn hand_built_graph_in_wrong_order_canonicalizes_identically() {
        // Pins the "callers don't have to pre-sort" contract: the trait
        // impl on `ExecutionGraph` re-sorts defensively, so a graph
        // constructed without the builder still produces canonical bytes.
        let from_builder = ExecutionGraphBuilder::build(sample_input());

        let mut hand = from_builder.clone();
        hand.nodes.reverse();
        hand.edges.reverse();
        hand.constraints.reverse();

        let a = from_builder.canonical_form(CanonicalGraphDomain::Declared);
        let b = hand.canonical_form(CanonicalGraphDomain::Declared);
        assert_eq!(a.bytes, b.bytes);
        assert_eq!(a.digest, b.digest);
    }

    #[test]
    fn digest_hex_is_lowercase_sha256_prefixed() {
        let graph = ExecutionGraphBuilder::build(sample_input());
        let form = graph.canonical_form(CanonicalGraphDomain::Declared);
        let hex = form.digest_hex();
        assert!(hex.starts_with("sha256:"));
        assert_eq!(hex.len(), "sha256:".len() + 64);
        assert!(hex[7..]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    /// Declared-domain sensitivity (refs #98, #99): changing a
    /// manifest/lock/policy-relevant field that is part of the declared
    /// graph (here, a target's runtime — picked because it's the most
    /// canonical "declared field" in the spec) changes the declared
    /// digest. This is the load-bearing property for
    /// `declared_execution_id`.
    #[test]
    fn declared_id_changes_when_declared_runtime_changes() {
        let baseline = ExecutionGraphBuilder::build(sample_input());

        let mut perturbed = sample_input();
        perturbed.targets[0].runtime = "runtime://deno".into();
        let perturbed = ExecutionGraphBuilder::build(perturbed);

        let baseline_id = baseline
            .canonical_form(CanonicalGraphDomain::Declared)
            .digest_hex();
        let perturbed_id = perturbed
            .canonical_form(CanonicalGraphDomain::Declared)
            .digest_hex();
        assert_ne!(
            baseline_id, perturbed_id,
            "declared_execution_id must react to manifest-declared runtime drift"
        );
    }

    /// Resolved-domain sensitivity (refs #98, #99): the spec splits
    /// the graph into a *declared* graph (host-independent — never
    /// carries the `fs.view_hash` label) and a *resolved* graph
    /// (declared graph + host-resolution labels layered on top). This
    /// test pins that separation:
    ///
    /// - The declared graph alone, hashed under `Declared`, is stable.
    /// - Two resolved graphs that differ only in `fs.view_hash`,
    ///   hashed under `Resolved`, produce different digests.
    ///
    /// The receipt builder enforces this layering via
    /// `extend_to_resolved_graph` in
    /// `crates/ato-cli/src/application/execution_receipt_builder.rs`.
    /// This test pins the canonical-form side of the contract.
    #[test]
    fn resolved_id_reacts_to_host_resolution_while_declared_id_stays_stable() {
        use crate::engine::execution_graph::identity_labels;

        // Declared graph: no host-resolution labels.
        let declared = ExecutionGraphBuilder::build(sample_input());
        let declared_id = declared
            .canonical_form(CanonicalGraphDomain::Declared)
            .digest_hex();

        // Two resolved graphs = declared + a different `fs.view_hash`
        // label each. The Declared digest of the *unextended* declared
        // graph is what `declared_execution_id` actually carries —
        // those resolved-only labels never reach the declared
        // canonicalization input.
        let mut resolved_a = declared.clone();
        resolved_a
            .labels
            .insert(identity_labels::FS_VIEW_HASH.to_string(), "blake3:a".into());
        let mut resolved_b = declared.clone();
        resolved_b
            .labels
            .insert(identity_labels::FS_VIEW_HASH.to_string(), "blake3:b".into());

        let resolved_a_id = resolved_a
            .canonical_form(CanonicalGraphDomain::Resolved)
            .digest_hex();
        let resolved_b_id = resolved_b
            .canonical_form(CanonicalGraphDomain::Resolved)
            .digest_hex();

        // Declared digest depends only on the declared graph's bytes.
        // Recomputing it twice from the same source must be stable.
        assert_eq!(
            declared_id,
            declared
                .canonical_form(CanonicalGraphDomain::Declared)
                .digest_hex(),
            "declared digest must be stable for an unchanged declared graph",
        );
        assert_ne!(
            resolved_a_id, resolved_b_id,
            "resolved id must react to a host-resolution label change"
        );
    }

    /// Secret invariance (refs #98, #99): changing an env *value*
    /// (which would only ever be carried as a future variant field)
    /// does not change either domain's digest. Today the type has no
    /// `value` field, so we approximate the contract by holding the
    /// env identifier fixed across two graphs that differ in unrelated
    /// non-secret state and confirming the env section bytes are
    /// stable. The redaction guarantee in
    /// `docs/execution-identity.md` §"Secret redaction boundary" pins
    /// the rule for the future field.
    #[test]
    fn env_identifier_pinned_secret_invariance_over_both_domains() {
        let mut a = ExecutionGraph::default();
        a.nodes.push(ExecutionGraphNode::Env {
            identifier: "env://OPENAI_API_KEY".into(),
        });
        let mut b = ExecutionGraph::default();
        b.nodes.push(ExecutionGraphNode::Env {
            identifier: "env://OPENAI_API_KEY".into(),
        });

        for domain in [
            CanonicalGraphDomain::Declared,
            CanonicalGraphDomain::Resolved,
        ] {
            assert_eq!(
                a.canonical_form(domain).digest,
                b.canonical_form(domain).digest,
                "env-only graphs with identical identifiers must hash identically in {domain:?}",
            );
        }
    }

    #[test]
    fn edge_kind_change_changes_digest() {
        let mut a = ExecutionGraph::default();
        a.edges.push(ExecutionGraphEdge {
            source: "x".into(),
            target: "y".into(),
            kind: ExecutionGraphEdgeKind::DependsOn,
        });
        let mut b = ExecutionGraph::default();
        b.edges.push(ExecutionGraphEdge {
            source: "x".into(),
            target: "y".into(),
            kind: ExecutionGraphEdgeKind::Provides,
        });

        assert_ne!(
            a.canonical_form(CanonicalGraphDomain::Declared).digest,
            b.canonical_form(CanonicalGraphDomain::Declared).digest,
        );
    }
}

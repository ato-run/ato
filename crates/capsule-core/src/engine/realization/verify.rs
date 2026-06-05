//! Resolved-graph materialization verifier (#499-A).
//!
//! The [`super::classify`] layer (#498-A) decides whether a resolved capsule
//! *can* be realized from typed facts. This module is the layer that lets a
//! node legitimately claim [`RealizationStatus::Verified`]: it compares a
//! declared/expected content identity against the actual materialized identity
//! and returns a typed result — [`MaterializationVerificationResult::Verified`],
//! [`MaterializationVerificationResult::Mismatch`], or
//! [`MaterializationVerificationResult::Unavailable`].
//!
//! ## Scope (#499-A)
//!
//! The core judgment is a **pure** function over expected/actual hashes
//! ([`verify_materialization`]); a small optional adapter
//! ([`verify_materialization_with_provider`]) fills the actual hash from a
//! [`MaterializedHashProvider`]. This module does **not** stop a launch
//! (strict fail-closed is #500), observe a running process, compute drift, or
//! populate runtime-tool `binary_sha256` (#469). It changes no launch
//! behavior; it only produces typed verification facts and maps them into the
//! #498 realization contract.
//!
//! ## Redaction boundary (#498-A review, #501)
//!
//! Evidence and results never carry a raw host path or a secret. A node may
//! carry a transient `materialized_path` *input* used to compute a hash, but it
//! is reduced to a role label in [`MaterializationVerificationEvidence::RedactedPath`]
//! and never serialized. Content hashes are safe and are the only values that
//! survive into the (serde-ready) output.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::model::{RealizationNodeKind, RealizationStatus, UnrealizableReason};

/// The class of resolved object whose materialized identity is being verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaterializedNodeSource {
    RuntimeBinary,
    RuntimeTool,
    DependencyOutput,
    BuildArtifact,
    FilesystemView,
    SourceTree,
}

impl MaterializedNodeSource {
    /// A value-free role label for redacted path evidence — never a filename or
    /// host path.
    fn role_label(self) -> &'static str {
        match self {
            Self::RuntimeBinary => "runtime-binary",
            Self::RuntimeTool => "runtime-tool",
            Self::DependencyOutput => "dependency-output",
            Self::BuildArtifact => "build-artifact",
            Self::FilesystemView => "filesystem-view",
            Self::SourceTree => "source-tree",
        }
    }
}

/// One resolved object to verify.
///
/// `materialized_path` is a transient *input* only: this struct is not
/// serializable and never reaches the output. The verifier reduces it to a
/// role label; the raw path (which may contain a home directory, project name,
/// or secret-bearing filename) is discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedNodeInput {
    pub node_id: String,
    pub node_kind: RealizationNodeKind,
    /// Declared/expected content identity (e.g. `sha256:…`). `None` ⇒ nothing
    /// to verify against.
    pub expected_hash: Option<String>,
    /// Actual materialized content identity. `None` ⇒ the object was not
    /// materialized (or its hash was not computed).
    pub actual_hash: Option<String>,
    /// Whether this object is a required immutable input. The verifier itself
    /// returns a result for every node regardless of this flag; aggregation and
    /// strict handling of required vs optional nodes is the caller's
    /// responsibility (#498 `classify` / strict profile #500).
    pub required: bool,
    pub source: MaterializedNodeSource,
    /// Optional raw path the actual hash was computed from. Transient; redacted
    /// to a role label, never stored or serialized.
    pub materialized_path: Option<String>,
}

/// A batch of objects to verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializationVerificationRequest {
    pub nodes: Vec<MaterializedNodeInput>,
}

/// Why a materialized object could not be verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaterializationUnavailableReason {
    /// No declared/expected hash exists to verify against.
    MissingExpectedHash,
    /// The declared/expected identity is not a well-formed content hash, so it
    /// is rejected rather than trusted as one (#499-A review).
    InvalidExpectedHashIdentity,
    /// The actual materialized identity is not a well-formed content hash.
    InvalidActualHashIdentity,
    /// An expected hash exists but the object was not materialized.
    MissingMaterializedObject,
    /// A runtime tool's `binary_sha256` is not populated yet (#469/#473). Until
    /// population lands this is `Unavailable`, never `Verified`.
    RuntimeToolBinaryHashUnpopulated,
    /// The actual hash could not be computed (e.g. the hash provider failed).
    HashComputationUnavailable,
    /// This node kind/source is not supported by the verifier. Reserved for
    /// future source kinds; not emitted by the current verifier (every
    /// [`MaterializedNodeSource`] is handled).
    UnsupportedNodeKind,
}

/// The typed outcome of verifying one object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum MaterializationVerificationResult {
    /// Expected and actual content identity both exist and match.
    Verified,
    /// Both exist but differ. Content hashes only — safe to persist.
    Mismatch { expected: String, actual: String },
    /// Cannot be verified, with a typed reason.
    Unavailable {
        reason: MaterializationUnavailableReason,
    },
}

impl MaterializationVerificationResult {
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified)
    }
}

/// Receipt-safe evidence backing a verification. Never a raw path or secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "evidence", rename_all = "kebab-case")]
pub enum MaterializationVerificationEvidence {
    /// A hash comparison was performed with the named algorithm (derived from
    /// the hash prefix, e.g. `sha256`).
    HashCompared { algorithm: String },
    /// A materialized path was involved, reduced to a value-free role label.
    RedactedPath { label: String },
    /// Free-form note for facts without a typed variant. Verifier-generated;
    /// never echoes caller input.
    Note { detail: String },
}

/// The verification result for one object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializationVerification {
    pub node_id: String,
    pub node_kind: RealizationNodeKind,
    pub result: MaterializationVerificationResult,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<MaterializationVerificationEvidence>,
}

/// Verify a batch of resolved objects. Pure: compares expected vs actual.
pub fn verify_materialization(
    request: MaterializationVerificationRequest,
) -> Vec<MaterializationVerification> {
    request.nodes.iter().map(verify_node).collect()
}

/// Verify a single resolved object. Pure.
pub fn verify_node(node: &MaterializedNodeInput) -> MaterializationVerification {
    let mut evidence = Vec::new();
    if node.materialized_path.is_some() {
        // Reduce the raw path to a role label; the path itself is discarded.
        evidence.push(MaterializationVerificationEvidence::RedactedPath {
            label: node.source.role_label().to_string(),
        });
    }

    // Validate hashes *before* anything is copied into a result. A caller that
    // mistakenly passes a raw path or `KEY=VALUE` is rejected with a typed
    // reason, so an unvalidated value can never be echoed into a persisted
    // `Mismatch`/reason or evidence (#499-A review).
    let result = match node.expected_hash.as_deref() {
        // No expected hash. A runtime tool reports the #469/#473-specific reason
        // so a missing `binary_sha256` is never silently treated as verified.
        None => MaterializationVerificationResult::Unavailable {
            reason: if node.source == MaterializedNodeSource::RuntimeTool {
                MaterializationUnavailableReason::RuntimeToolBinaryHashUnpopulated
            } else {
                MaterializationUnavailableReason::MissingExpectedHash
            },
        },
        Some(expected) if !is_content_hash(expected) => {
            MaterializationVerificationResult::Unavailable {
                reason: MaterializationUnavailableReason::InvalidExpectedHashIdentity,
            }
        }
        Some(expected) => match node.actual_hash.as_deref() {
            None => MaterializationVerificationResult::Unavailable {
                reason: MaterializationUnavailableReason::MissingMaterializedObject,
            },
            Some(actual) if !is_content_hash(actual) => {
                MaterializationVerificationResult::Unavailable {
                    reason: MaterializationUnavailableReason::InvalidActualHashIdentity,
                }
            }
            Some(actual) => {
                // Both are validated content hashes — safe to record.
                evidence.push(MaterializationVerificationEvidence::HashCompared {
                    algorithm: hash_algorithm(expected),
                });
                if expected == actual {
                    MaterializationVerificationResult::Verified
                } else {
                    MaterializationVerificationResult::Mismatch {
                        expected: expected.to_string(),
                        actual: actual.to_string(),
                    }
                }
            }
        },
    };

    MaterializationVerification {
        node_id: node.node_id.clone(),
        node_kind: node.node_kind,
        result,
        evidence,
    }
}

/// Whether `value` is a well-formed `algo:digest` content hash. This is the
/// guard that keeps a caller's mistake — a raw host path, an env assignment, a
/// secret — out of any persisted result (#499-A review): such values are not
/// content hashes and are rejected before they can be echoed. Shared with the
/// strict gate (#500), which uses it both to reject invalid identities and to
/// decide whether an identity is safe to summarize into an error payload.
pub(crate) fn is_content_hash(value: &str) -> bool {
    let Some((algo, digest)) = value.split_once(':') else {
        return false;
    };
    !algo.is_empty()
        && !digest.is_empty()
        && algo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && digest
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-' || c == '_' || c == '.')
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('=')
}

/// Derive a coarse algorithm label from a validated hash's `algo:` prefix.
fn hash_algorithm(hash: &str) -> String {
    match hash.split_once(':') {
        Some((algo, _)) if !algo.is_empty() => algo.to_string(),
        _ => "unspecified".to_string(),
    }
}

// ---------------------------------------------------------------------------
// #498 realization mapping
// ---------------------------------------------------------------------------

/// Map a materialization result onto the #498 [`RealizationStatus`]. A
/// `Mismatch` or `Unavailable` both make the node `Unavailable` — only a real
/// match yields `Verified`.
pub fn materialization_result_to_realization_status(
    result: &MaterializationVerificationResult,
) -> RealizationStatus {
    match result {
        MaterializationVerificationResult::Verified => RealizationStatus::Verified,
        MaterializationVerificationResult::Mismatch { .. }
        | MaterializationVerificationResult::Unavailable { .. } => RealizationStatus::Unavailable,
    }
}

/// Map a materialization result onto a typed [`UnrealizableReason`], or `None`
/// when the object verified. Carries content hashes only — never a path or
/// secret.
pub fn materialization_result_to_unrealizable_reason(
    node_id: &str,
    node_kind: RealizationNodeKind,
    result: &MaterializationVerificationResult,
) -> Option<UnrealizableReason> {
    match result {
        MaterializationVerificationResult::Verified => None,
        MaterializationVerificationResult::Mismatch { expected, actual } => {
            Some(UnrealizableReason::MismatchedImmutableInput {
                node_id: node_id.to_string(),
                node_kind,
                expected: expected.clone(),
                actual: actual.clone(),
            })
        }
        MaterializationVerificationResult::Unavailable { reason } => Some(match reason {
            MaterializationUnavailableReason::RuntimeToolBinaryHashUnpopulated => {
                UnrealizableReason::RuntimeToolBinaryHashUnavailable {
                    node_id: node_id.to_string(),
                }
            }
            MaterializationUnavailableReason::InvalidExpectedHashIdentity
            | MaterializationUnavailableReason::InvalidActualHashIdentity => {
                UnrealizableReason::InvalidImmutableInputIdentity {
                    node_id: node_id.to_string(),
                    node_kind,
                }
            }
            MaterializationUnavailableReason::MissingExpectedHash
            | MaterializationUnavailableReason::MissingMaterializedObject
            | MaterializationUnavailableReason::HashComputationUnavailable
            | MaterializationUnavailableReason::UnsupportedNodeKind => {
                UnrealizableReason::MissingImmutableInput {
                    node_id: node_id.to_string(),
                    node_kind,
                }
            }
        }),
    }
}

// ---------------------------------------------------------------------------
// Optional hash-provider adapter (small seam; concrete fs provider lands later)
// ---------------------------------------------------------------------------

/// Failure modes for computing a content hash from a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializationHashError {
    NotFound,
    Io(String),
    Unsupported,
}

/// Computes a content hash for a materialized object. The concrete
/// filesystem-backed implementation (SHA-256/blake3 over a real path) is
/// deliberately out of scope for #499-A; this trait fixes the seam so a later
/// PR can supply it without reshaping the verifier.
pub trait MaterializedHashProvider {
    fn hash_file(&self, path: &Path) -> Result<String, MaterializationHashError>;
}

/// Verify a batch, filling each node's actual hash from `provider` when the
/// node has a `materialized_path` but no `actual_hash`. A provider error maps
/// to [`MaterializationUnavailableReason::HashComputationUnavailable`]; the raw
/// path is never surfaced.
pub fn verify_materialization_with_provider<P: MaterializedHashProvider>(
    provider: &P,
    request: MaterializationVerificationRequest,
) -> Vec<MaterializationVerification> {
    request
        .nodes
        .into_iter()
        .map(|mut node| {
            if node.actual_hash.is_none() {
                if let Some(path) = node.materialized_path.as_deref() {
                    match provider.hash_file(Path::new(path)) {
                        Ok(hash) => node.actual_hash = Some(hash),
                        Err(_) => {
                            // Compute failed — typed Unavailable, path redacted.
                            return MaterializationVerification {
                                node_id: node.node_id.clone(),
                                node_kind: node.node_kind,
                                result: MaterializationVerificationResult::Unavailable {
                                    reason:
                                        MaterializationUnavailableReason::HashComputationUnavailable,
                                },
                                evidence: vec![
                                    MaterializationVerificationEvidence::RedactedPath {
                                        label: node.source.role_label().to_string(),
                                    },
                                ],
                            };
                        }
                    }
                }
            }
            verify_node(&node)
        })
        .collect()
}

#[cfg(test)]
mod tests;

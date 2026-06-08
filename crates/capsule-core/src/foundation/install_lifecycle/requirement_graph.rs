//! Deterministic requirement-graph compiler + profile normalization
//! (RFC: Ato Resource Namespace §"Relationship to Application Requirement Graph";
//! #581 wave 3A).
//!
//! This is the first *real* compiler that replaces the opaque minimal
//! placeholder PR #585 persisted. It turns the facts genuinely available at
//! install time into a typed, deterministic [`RequirementGraphSnapshot`] and an
//! explicit [`RequirementGraphCompleteness`].
//!
//! # Hard rules
//!
//! - **No fabrication.** A runtime / entrypoint / network / secret / storage /
//!   state node is emitted only when the corresponding fact is genuinely known.
//!   When a fact is absent, no node is invented; instead a typed completeness
//!   reason is recorded.
//! - **Deterministic hash.** The graph id is a constant (not revision-derived),
//!   so `graph_hash` reflects the *requirements* (artifact content via the
//!   artifact-output node, profile via the profile-defaults node, manifest facts
//!   when present) — never a timestamp, host, temp path, session id, port, route,
//!   log cursor, observed status, or secret value. Collections are sorted before
//!   emission so the hash is order-independent of the input order.
//! - **Completeness is explicit.** The standard install path is `Partial` (no
//!   parsed manifest yet) and is never presented as `Complete`.
//!
//! This wave does **not** generate launch templates, binding assignment sets, or
//! a compatibility index, and does not select runners — it only compiles the
//! requirement-graph snapshot persisted at install time.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::hashing::canonical_hash;
use super::ids::InstallRevisionId;
use super::records::{
    RequirementGraph, RequirementGraphCompleteness, RequirementGraphCompletenessReason,
    RequirementGraphEdge, RequirementGraphNode, RequirementGraphSnapshot, RequirementKind,
    RequirementRelation, StateContractSnapshot,
};
use super::store::LaunchProfile;

/// Stable graph id — constant, **not** revision-derived. Graph identity reflects
/// the requirements, not the revision id; artifact content enters `graph_hash`
/// via the artifact-output node's `output_content_hash` attribute.
const REQUIREMENT_GRAPH_ID: &str = "ato.install.requirement_graph.v0";
/// Stable sentinel hashed when no profile facts are available (so `profile_hash`
/// is deterministic and explicit rather than empty).
const ABSENT_PROFILE_SENTINEL: &str = "ato.install.profile.absent.v0";

pub const NODE_PROFILE_DEFAULTS: &str = "req:profile-defaults";
pub const NODE_ARTIFACT_OUTPUT: &str = "req:artifact-output";
pub const NODE_RUNTIME: &str = "req:runtime";
pub const NODE_ENTRYPOINT: &str = "req:entrypoint";

// ── Profile normalization ─────────────────────────────────────────────────────

/// Normalized, deterministic view of a [`LaunchProfile`]'s stable declared
/// config, used to compute a stable `profile_hash`.
///
/// Contains only declared references / keys / policies. It never contains a
/// secret *value*: `env_refs` values are references (e.g. `"${secret:key}"`),
/// `secret_refs` are ref *names*, and all of these are already persisted in
/// `profile.json`. No session id, port, route, pid, timestamp, or host path is
/// included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NormalizedProfile {
    pub profile_id: String,
    /// Env name -> declared reference (a ref, never a secret value). BTreeMap so
    /// the order is canonical.
    pub env_refs: BTreeMap<String, String>,
    /// Declared secret-ref names, sorted + de-duplicated.
    pub secret_refs: Vec<String>,
    /// Declared extra launch args, in declared order (order is significant).
    pub args: Vec<String>,
    pub port_policy: String,
    pub concurrency_policy: String,
    pub isolation: String,
}

impl NormalizedProfile {
    /// Normalize a [`LaunchProfile`] into a deterministic, hashable view.
    pub fn from_launch_profile(profile: &LaunchProfile) -> Self {
        let mut secret_refs = profile.secret_refs.clone();
        secret_refs.sort();
        secret_refs.dedup();
        Self {
            profile_id: profile.profile_id.as_str().to_owned(),
            env_refs: profile
                .env_refs
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            secret_refs,
            args: profile.args.clone(),
            port_policy: profile.port_policy.clone(),
            concurrency_policy: profile.concurrency_policy.clone(),
            isolation: profile.isolation.clone(),
        }
    }

    /// `blake3:<hex>` over the canonical form of the normalized profile.
    pub fn profile_hash(&self) -> Result<String> {
        canonical_hash(self)
    }
}

// ── Manifest-derived requirement facts ────────────────────────────────────────

/// Typed application-requirement facts a caller may supply once it has parsed
/// the capsule manifest. Every field is optional/empty: an absent fact yields no
/// node and a typed completeness reason — it is never fabricated.
///
/// The standard install path does not parse the manifest yet, so it passes
/// `None`; these structs let later waves feed real facts without reshaping the
/// compiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ManifestRequirementFacts {
    pub runtime: Option<RuntimeRequirementFact>,
    pub entrypoint: Option<String>,
    pub network: Vec<NetworkRequirementFact>,
    pub secrets: Vec<SecretRequirementFact>,
    pub storage: Vec<StorageRequirementFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRequirementFact {
    /// e.g. `"server_process"`, `"browser"`, `"wasm"`.
    pub kind: String,
    #[serde(default)]
    pub runtimes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRequirementFact {
    pub name: String,
    pub mode: String,
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRequirementFact {
    /// The secret *name* / ref key — never a secret value.
    pub name: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageRequirementFact {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub access: Vec<String>,
}

// ── Compile input / output ────────────────────────────────────────────────────

/// Facts available to compile a requirement graph. Anything genuinely unknown is
/// `None` / empty and yields a typed completeness reason, never a fabricated node.
#[derive(Debug, Clone, Default)]
pub struct RequirementGraphCompileInput {
    /// Used only for the snapshot's `source_revision_ref` (not a graph-hash input).
    pub install_revision_id: Option<InstallRevisionId>,
    /// Normalized launch profile. `None` => `ProfileFactsUnavailable`.
    pub profile: Option<NormalizedProfile>,
    pub capsule_ref: Option<String>,
    pub artifact_output_ref: Option<String>,
    pub output_content_hash: Option<String>,
    pub source_provenance_ref: Option<String>,
    /// Known state contracts. Empty => `StateContractsNotAnalyzed`.
    pub state_contracts: Vec<StateContractSnapshot>,
    /// Parsed manifest facts. `None` => `ManifestFactsUnavailable` (+ per-class
    /// reasons).
    pub manifest_facts: Option<ManifestRequirementFacts>,
}

/// Result of compiling a requirement graph.
#[derive(Debug, Clone)]
pub struct RequirementGraphCompileOutput {
    pub snapshot: RequirementGraphSnapshot,
    pub profile_hash: String,
    pub completeness: RequirementGraphCompleteness,
}

/// Compile a deterministic [`RequirementGraphSnapshot`] from available install
/// facts. See the module docs for the hard rules (no fabrication, deterministic
/// hash, explicit completeness).
pub fn compile_requirement_graph(
    input: RequirementGraphCompileInput,
) -> Result<RequirementGraphCompileOutput> {
    let mut nodes: Vec<RequirementGraphNode> = Vec::new();
    let mut edges: Vec<RequirementGraphEdge> = Vec::new();
    let mut reasons: Vec<RequirementGraphCompletenessReason> = Vec::new();

    // ── profile-defaults node ──
    let profile_hash = match &input.profile {
        Some(profile) => {
            let hash = profile.profile_hash()?;
            let mut attributes = BTreeMap::new();
            attributes.insert("profile_id".to_owned(), profile.profile_id.clone());
            attributes.insert("port_policy".to_owned(), profile.port_policy.clone());
            attributes.insert(
                "concurrency_policy".to_owned(),
                profile.concurrency_policy.clone(),
            );
            attributes.insert("isolation".to_owned(), profile.isolation.clone());
            // The profile hash covers every normalized field (env refs, secret
            // ref names, args, policies), so any profile change flips this node
            // — and thus the graph hash.
            attributes.insert("profile_hash".to_owned(), hash.clone());
            nodes.push(RequirementGraphNode {
                id: NODE_PROFILE_DEFAULTS.to_owned(),
                kind: RequirementKind::Policy,
                name: "profile-defaults".to_owned(),
                attributes,
                required: true,
            });
            hash
        }
        None => {
            reasons.push(RequirementGraphCompletenessReason::ProfileFactsUnavailable);
            canonical_hash(&ABSENT_PROFILE_SENTINEL)?
        }
    };

    // ── artifact-output (provenance) node ──
    {
        let mut attributes = BTreeMap::new();
        if let Some(v) = &input.capsule_ref {
            attributes.insert("capsule_ref".to_owned(), v.clone());
        }
        if let Some(v) = &input.output_content_hash {
            attributes.insert("output_content_hash".to_owned(), v.clone());
        }
        if let Some(v) = &input.artifact_output_ref {
            attributes.insert("output_ref".to_owned(), v.clone());
        }
        if let Some(v) = &input.source_provenance_ref {
            attributes.insert("source_provenance_ref".to_owned(), v.clone());
        }
        if !attributes.is_empty() {
            nodes.push(RequirementGraphNode {
                id: NODE_ARTIFACT_OUTPUT.to_owned(),
                kind: RequirementKind::Output,
                name: "artifact-output".to_owned(),
                attributes,
                required: true,
            });
        }
    }

    // ── manifest-derived requirement nodes ──
    match &input.manifest_facts {
        None => {
            // No parsed manifest: every manifest-derived class is uncompiled.
            reasons.push(RequirementGraphCompletenessReason::ManifestFactsUnavailable);
            reasons.push(RequirementGraphCompletenessReason::RuntimeRequirementNotCompiled);
            reasons.push(RequirementGraphCompletenessReason::EntrypointRequirementNotCompiled);
            reasons.push(RequirementGraphCompletenessReason::NetworkPolicyNotAnalyzed);
            reasons.push(RequirementGraphCompletenessReason::SecretRequirementsNotAnalyzed);
            reasons.push(RequirementGraphCompletenessReason::StorageRequirementsNotAnalyzed);
        }
        Some(facts) => {
            let has_runtime = match &facts.runtime {
                Some(rt) => {
                    let mut attributes = BTreeMap::new();
                    attributes.insert("kind".to_owned(), rt.kind.clone());
                    if !rt.runtimes.is_empty() {
                        let mut runtimes = rt.runtimes.clone();
                        runtimes.sort();
                        attributes.insert("runtimes".to_owned(), runtimes.join(","));
                    }
                    nodes.push(RequirementGraphNode {
                        id: NODE_RUNTIME.to_owned(),
                        kind: RequirementKind::Runtime,
                        name: rt.kind.clone(),
                        attributes,
                        required: true,
                    });
                    true
                }
                None => {
                    reasons.push(RequirementGraphCompletenessReason::RuntimeRequirementNotCompiled);
                    false
                }
            };

            match &facts.entrypoint {
                Some(entrypoint) => {
                    let mut attributes = BTreeMap::new();
                    attributes.insert("entrypoint".to_owned(), entrypoint.clone());
                    nodes.push(RequirementGraphNode {
                        id: NODE_ENTRYPOINT.to_owned(),
                        kind: RequirementKind::Service,
                        name: "entrypoint".to_owned(),
                        attributes,
                        required: true,
                    });
                    // The only edge emitted in this wave: a known runtime
                    // exposes the entrypoint. Other relations await real facts.
                    if has_runtime {
                        edges.push(RequirementGraphEdge {
                            from: NODE_RUNTIME.to_owned(),
                            to: NODE_ENTRYPOINT.to_owned(),
                            relation: RequirementRelation::Exposes,
                        });
                    }
                }
                None => {
                    reasons
                        .push(RequirementGraphCompletenessReason::EntrypointRequirementNotCompiled);
                }
            }

            if facts.network.is_empty() {
                reasons.push(RequirementGraphCompletenessReason::NetworkPolicyNotAnalyzed);
            } else {
                let mut network = facts.network.clone();
                network.sort_by(|a, b| a.name.cmp(&b.name));
                for n in network {
                    let mut attributes = BTreeMap::new();
                    attributes.insert("mode".to_owned(), n.mode.clone());
                    if !n.allow.is_empty() {
                        let mut allow = n.allow.clone();
                        allow.sort();
                        attributes.insert("allow".to_owned(), allow.join(","));
                    }
                    nodes.push(RequirementGraphNode {
                        id: format!("req:network:{}", n.name),
                        kind: RequirementKind::Network,
                        name: n.name.clone(),
                        attributes,
                        required: true,
                    });
                }
            }

            if facts.secrets.is_empty() {
                reasons.push(RequirementGraphCompletenessReason::SecretRequirementsNotAnalyzed);
            } else {
                let mut secrets = facts.secrets.clone();
                secrets.sort_by(|a, b| a.name.cmp(&b.name));
                for s in secrets {
                    let mut attributes = BTreeMap::new();
                    attributes.insert("required".to_owned(), s.required.to_string());
                    nodes.push(RequirementGraphNode {
                        id: format!("req:secret:{}", s.name),
                        kind: RequirementKind::Secret,
                        name: s.name.clone(),
                        attributes,
                        required: s.required,
                    });
                }
            }

            if facts.storage.is_empty() {
                reasons.push(RequirementGraphCompletenessReason::StorageRequirementsNotAnalyzed);
            } else {
                let mut storage = facts.storage.clone();
                storage.sort_by(|a, b| a.name.cmp(&b.name));
                for s in storage {
                    let mut attributes = BTreeMap::new();
                    attributes.insert("kind".to_owned(), s.kind.clone());
                    if !s.access.is_empty() {
                        let mut access = s.access.clone();
                        access.sort();
                        attributes.insert("access".to_owned(), access.join(","));
                    }
                    nodes.push(RequirementGraphNode {
                        id: format!("req:storage:{}", s.name),
                        kind: RequirementKind::Storage,
                        name: s.name.clone(),
                        attributes,
                        required: true,
                    });
                }
            }
        }
    }

    // ── state-contract nodes ──
    if input.state_contracts.is_empty() {
        reasons.push(RequirementGraphCompletenessReason::StateContractsNotAnalyzed);
    } else {
        let mut contracts = input.state_contracts.clone();
        contracts.sort_by(|a, b| a.contract_name.cmp(&b.contract_name));
        for contract in contracts {
            let mut attributes = BTreeMap::new();
            // No dedicated `State` RequirementKind exists yet; model a state
            // contract as a Storage-backed requirement, disambiguated by the
            // `requirement` attribute and the `req:state:` id prefix.
            attributes.insert("requirement".to_owned(), "state_contract".to_owned());
            attributes.insert(
                "state_contract_hash".to_owned(),
                contract.state_contract_hash.clone(),
            );
            nodes.push(RequirementGraphNode {
                id: format!("req:state:{}", contract.contract_name),
                kind: RequirementKind::Storage,
                name: contract.contract_name.clone(),
                attributes,
                required: true,
            });
        }
    }

    let graph = RequirementGraph {
        graph_id: REQUIREMENT_GRAPH_ID.to_owned(),
        nodes,
        edges,
    };

    let completeness = if reasons.is_empty() {
        RequirementGraphCompleteness::Complete
    } else {
        RequirementGraphCompleteness::Partial { reasons }
    };

    let snapshot_id = match &input.install_revision_id {
        Some(rev) => format!("reqgraph:{}", rev.as_str()),
        None => "reqgraph:unscoped".to_owned(),
    };
    let source_revision_ref = input
        .install_revision_id
        .as_ref()
        .map(|r| r.as_str().to_owned());

    let snapshot = RequirementGraphSnapshot::new(
        snapshot_id,
        graph,
        source_revision_ref,
        profile_hash.clone(),
    )?
    .with_completeness(completeness.clone());

    Ok(RequirementGraphCompileOutput {
        snapshot,
        profile_hash,
        completeness,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::install_lifecycle::records::StateContractSnapshot;

    fn rev() -> InstallRevisionId {
        InstallRevisionId::new(format!("rev_{}", "a".repeat(32)))
    }

    fn sample_profile() -> NormalizedProfile {
        NormalizedProfile {
            profile_id: "default".into(),
            env_refs: BTreeMap::from([("API_BASE".into(), "https://api.example.com".into())]),
            secret_refs: vec!["database_url".into()],
            args: vec!["--verbose".into()],
            port_policy: "auto".into(),
            concurrency_policy: "single".into(),
            isolation: "default".into(),
        }
    }

    fn standard_input() -> RequirementGraphCompileInput {
        RequirementGraphCompileInput {
            install_revision_id: Some(rev()),
            profile: Some(sample_profile()),
            capsule_ref: Some("acme/pgweb@1.2.3".into()),
            artifact_output_ref: Some("/artifacts/blake3/cafef00d".into()),
            output_content_hash: Some("blake3:cafef00d".into()),
            source_provenance_ref: Some("blake3:cafef00d".into()),
            state_contracts: vec![],
            manifest_facts: None,
        }
    }

    // Test 1: deterministic.
    #[test]
    fn compiling_same_input_twice_is_identical() {
        let a = compile_requirement_graph(standard_input()).unwrap();
        let b = compile_requirement_graph(standard_input()).unwrap();
        assert_eq!(a.snapshot.graph_hash, b.snapshot.graph_hash);
        assert_eq!(a.profile_hash, b.profile_hash);
        assert_eq!(a.snapshot.graph, b.snapshot.graph);
        assert_eq!(a.completeness, b.completeness);
    }

    // Test 2: changing stable profile defaults changes profile_hash AND graph_hash.
    #[test]
    fn changing_profile_changes_profile_and_graph_hash() {
        let base = compile_requirement_graph(standard_input()).unwrap();

        let mut changed = standard_input();
        changed.profile.as_mut().unwrap().port_policy = "fixed:8080".into();
        let after = compile_requirement_graph(changed).unwrap();

        assert_ne!(
            base.profile_hash, after.profile_hash,
            "profile hash must change when a stable profile field changes"
        );
        assert_ne!(
            base.snapshot.graph_hash, after.snapshot.graph_hash,
            "graph hash must change because the profile-defaults node carries the profile hash"
        );
    }

    // Test 3: revision id (a non-requirement fact) does not affect graph_hash.
    #[test]
    fn revision_id_does_not_affect_graph_hash() {
        let base = compile_requirement_graph(standard_input()).unwrap();
        let mut other_rev = standard_input();
        other_rev.install_revision_id =
            Some(InstallRevisionId::new(format!("rev_{}", "b".repeat(32))));
        let after = compile_requirement_graph(other_rev).unwrap();
        assert_eq!(
            base.snapshot.graph_hash, after.snapshot.graph_hash,
            "graph hash must not depend on the revision id (it is snapshot metadata, not a requirement)"
        );
        // But the snapshot id / source ref do reflect the revision.
        assert_ne!(base.snapshot.snapshot_id, after.snapshot.snapshot_id);
    }

    // Test 3b: artifact content DOES affect graph_hash (via artifact-output node).
    #[test]
    fn changing_artifact_content_changes_graph_hash() {
        let base = compile_requirement_graph(standard_input()).unwrap();
        let mut changed = standard_input();
        changed.output_content_hash = Some("blake3:99999999".into());
        let after = compile_requirement_graph(changed).unwrap();
        assert_ne!(base.snapshot.graph_hash, after.snapshot.graph_hash);
    }

    // Test 4: unknown runtime does not fabricate a runtime requirement.
    #[test]
    fn unknown_runtime_is_not_fabricated() {
        let out = compile_requirement_graph(standard_input()).unwrap();
        assert!(
            !out.snapshot
                .graph
                .nodes
                .iter()
                .any(|n| n.id == NODE_RUNTIME),
            "no runtime node may be invented when no runtime fact is known"
        );
        match &out.completeness {
            RequirementGraphCompleteness::Partial { reasons } => assert!(
                reasons
                    .contains(&RequirementGraphCompletenessReason::RuntimeRequirementNotCompiled)
            ),
            RequirementGraphCompleteness::Complete => panic!("must be partial"),
        }
    }

    // Test 5: unknown network/secret/storage do not fabricate requirements.
    #[test]
    fn unknown_network_secret_storage_are_not_fabricated() {
        let out = compile_requirement_graph(standard_input()).unwrap();
        for prefix in ["req:network:", "req:secret:", "req:storage:"] {
            assert!(
                !out.snapshot
                    .graph
                    .nodes
                    .iter()
                    .any(|n| n.id.starts_with(prefix)),
                "no {prefix} node may be invented without a fact"
            );
        }
        let RequirementGraphCompleteness::Partial { reasons } = &out.completeness else {
            panic!("must be partial");
        };
        for reason in [
            RequirementGraphCompletenessReason::NetworkPolicyNotAnalyzed,
            RequirementGraphCompletenessReason::SecretRequirementsNotAnalyzed,
            RequirementGraphCompletenessReason::StorageRequirementsNotAnalyzed,
            RequirementGraphCompletenessReason::StateContractsNotAnalyzed,
            RequirementGraphCompletenessReason::ManifestFactsUnavailable,
        ] {
            assert!(reasons.contains(&reason), "missing reason {reason:?}");
        }
    }

    // Test 6: partial graph carries typed reasons; standard path is never Complete.
    #[test]
    fn standard_path_is_partial_with_typed_reasons() {
        let out = compile_requirement_graph(standard_input()).unwrap();
        assert!(!out.completeness.is_complete());
        // The two genuinely-known nodes are present.
        assert!(
            out.snapshot
                .graph
                .nodes
                .iter()
                .any(|n| n.id == NODE_PROFILE_DEFAULTS)
        );
        assert!(
            out.snapshot
                .graph
                .nodes
                .iter()
                .any(|n| n.id == NODE_ARTIFACT_OUTPUT)
        );
        // No edges are fabricated on the standard path.
        assert!(out.snapshot.graph.edges.is_empty());
    }

    // Known facts DO emit nodes + the runtime->entrypoint edge, and reduce reasons.
    #[test]
    fn known_manifest_facts_emit_nodes_and_edges() {
        let mut input = standard_input();
        input.manifest_facts = Some(ManifestRequirementFacts {
            runtime: Some(RuntimeRequirementFact {
                kind: "server_process".into(),
                runtimes: vec!["oci".into()],
            }),
            entrypoint: Some("/usr/bin/pgweb".into()),
            network: vec![NetworkRequirementFact {
                name: "egress".into(),
                mode: "proxy_only".into(),
                allow: vec!["https://api.example.com".into()],
            }],
            secrets: vec![SecretRequirementFact {
                name: "database_url".into(),
                required: true,
            }],
            storage: vec![StorageRequirementFact {
                name: "user_data".into(),
                kind: "object_store".into(),
                access: vec!["read".into(), "write".into()],
            }],
        });
        input.state_contracts =
            vec![StateContractSnapshot::new("user_data", "blake3:shape").unwrap()];

        let out = compile_requirement_graph(input).unwrap();
        let ids: Vec<&str> = out
            .snapshot
            .graph
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert!(ids.contains(&NODE_RUNTIME));
        assert!(ids.contains(&NODE_ENTRYPOINT));
        assert!(ids.contains(&"req:network:egress"));
        assert!(ids.contains(&"req:secret:database_url"));
        assert!(ids.contains(&"req:storage:user_data"));
        assert!(ids.contains(&"req:state:user_data"));
        // runtime -> entrypoint edge present.
        assert!(
            out.snapshot
                .graph
                .edges
                .iter()
                .any(|e| e.from == NODE_RUNTIME
                    && e.to == NODE_ENTRYPOINT
                    && e.relation == RequirementRelation::Exposes)
        );
        // With all classes known + state analyzed, the graph is Complete.
        assert!(
            out.completeness.is_complete(),
            "all facts known => Complete, got {:?}",
            out.completeness
        );
    }

    // Manifest facts present but a class missing => only that class's reason.
    #[test]
    fn partial_manifest_facts_yield_only_missing_reasons() {
        let mut input = standard_input();
        input.manifest_facts = Some(ManifestRequirementFacts {
            runtime: Some(RuntimeRequirementFact {
                kind: "browser".into(),
                runtimes: vec![],
            }),
            entrypoint: None,
            network: vec![],
            secrets: vec![],
            storage: vec![],
        });
        let out = compile_requirement_graph(input).unwrap();
        let RequirementGraphCompleteness::Partial { reasons } = &out.completeness else {
            panic!("must be partial");
        };
        assert!(
            !reasons.contains(&RequirementGraphCompletenessReason::RuntimeRequirementNotCompiled)
        );
        assert!(!reasons.contains(&RequirementGraphCompletenessReason::ManifestFactsUnavailable));
        assert!(
            reasons.contains(&RequirementGraphCompletenessReason::EntrypointRequirementNotCompiled)
        );
    }

    // Node collection order does not affect the hash (sorted before emission).
    #[test]
    fn network_order_does_not_affect_graph_hash() {
        let mut a = standard_input();
        a.manifest_facts = Some(ManifestRequirementFacts {
            network: vec![
                NetworkRequirementFact {
                    name: "a".into(),
                    mode: "proxy".into(),
                    allow: vec![],
                },
                NetworkRequirementFact {
                    name: "b".into(),
                    mode: "proxy".into(),
                    allow: vec![],
                },
            ],
            ..Default::default()
        });
        let mut b = standard_input();
        b.manifest_facts = Some(ManifestRequirementFacts {
            network: vec![
                NetworkRequirementFact {
                    name: "b".into(),
                    mode: "proxy".into(),
                    allow: vec![],
                },
                NetworkRequirementFact {
                    name: "a".into(),
                    mode: "proxy".into(),
                    allow: vec![],
                },
            ],
            ..Default::default()
        });
        assert_eq!(
            compile_requirement_graph(a).unwrap().snapshot.graph_hash,
            compile_requirement_graph(b).unwrap().snapshot.graph_hash
        );
    }

    // Test 10: no secret value can appear in the graph/hash (only refs/keys).
    #[test]
    fn no_secret_value_in_graph_or_hash() {
        // The compiler has no secret-value input. secret_refs carry NAMES only.
        let mut input = standard_input();
        input.profile.as_mut().unwrap().secret_refs = vec!["database_url".into()];
        let out = compile_requirement_graph(input).unwrap();
        let json = serde_json::to_string(&out.snapshot).unwrap();
        // The ref NAME may appear (it is declared config), but a secret VALUE
        // never can — there is no field to carry one.
        assert!(!json.contains("hunter2") && !json.contains("swordfish"));
        // profile_hash is over refs/keys/policies only.
        assert!(out.profile_hash.starts_with("blake3:"));
    }

    #[test]
    fn no_profile_facts_marks_reason_and_no_profile_node() {
        let mut input = standard_input();
        input.profile = None;
        let out = compile_requirement_graph(input).unwrap();
        assert!(
            !out.snapshot
                .graph
                .nodes
                .iter()
                .any(|n| n.id == NODE_PROFILE_DEFAULTS)
        );
        let RequirementGraphCompleteness::Partial { reasons } = &out.completeness else {
            panic!("must be partial");
        };
        assert!(reasons.contains(&RequirementGraphCompletenessReason::ProfileFactsUnavailable));
        // profile_hash is still deterministic (hash of the absent-profile sentinel).
        assert_eq!(
            out.profile_hash,
            canonical_hash(&ABSENT_PROFILE_SENTINEL).unwrap()
        );
    }

    #[test]
    fn snapshot_completeness_survives_serde_roundtrip() {
        let out = compile_requirement_graph(standard_input()).unwrap();
        let json = serde_json::to_string(&out.snapshot).unwrap();
        let back: RequirementGraphSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.completeness, out.snapshot.completeness);
        assert_eq!(back.graph_hash, out.snapshot.graph_hash);
    }
}

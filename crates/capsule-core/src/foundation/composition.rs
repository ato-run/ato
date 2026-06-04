//! Capsule Composition Semantics (#506).
//!
//! This module consumes the Capsule Interface Model from #505
//! ([`crate::foundation::interface`]) and composes multiple
//! [`CapsuleInterface`] values into a single, deterministic
//! [`CompositionReport`].
//!
//! ## What composition produces
//!
//! Given a set of `(capsule_id, CapsuleInterface)` inputs, composition decides,
//! for every `requires` entry, which of three buckets it lands in:
//!
//! ```text
//! peer-satisfiable + exactly one matching peer provide → InterfaceBinding
//! peer-satisfiable + no matching peer provide          → unresolved (Blocked)  [service/tool/runtime]
//!                                                       → external StateBinding [state, not Blocked]
//! not peer-satisfiable                                 → ExternalRequirement
//! ```
//!
//! The result is **not** a Docker-Compose-style service graph. It is an
//! [`AggregateExecutionContract`] that deliberately *separates* peer wiring
//! (the bindings) from the external launch conditions (secrets, hardware,
//! network, ports, provider capabilities, state bindings) that the host /
//! platform / user must satisfy. Those external conditions are what later
//! stages — install-flow admission (#508/#509), placement (#498), realization
//! — consume; this PR only computes them.
//!
//! ## The boundary is owned by #505
//!
//! Composition never re-derives the provides/requires boundary. It asks
//! [`RequiredInterface::possible_satisfaction_sources`]:
//!
//! - contains [`SatisfactionSource::ProvidedInterface`] → may match peer provides;
//! - any non-peer source → if no peer provide matches, falls back to an external
//!   requirement (this is how `State` degrades to a managed
//!   [`ExternalRequirement::StateBinding`]);
//! - does **not** contain `ProvidedInterface` → never matched against peer
//!   provides (Secret / Network / Capability / Hardware / Port /
//!   ProviderCapability), always aggregated as an [`ExternalRequirement`].
//!
//! Compatibility itself is decided by #505's
//! [`provided_interface_may_satisfy`]; #506 adds no new matching rules and no
//! version/semver solver.
//!
//! ## Determinism
//!
//! Output never depends on input order: inputs are validated and stable-sorted
//! by `capsule_id`, each interface is normalized via #505's
//! [`CapsuleInterface::validate`], and every output collection is sorted by a
//! stable `(capsule_id, category, name, …)` key.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::foundation::interface::{
    CapabilityRequirement, CapsuleInterface, HardwareRequirement, InterfaceError,
    NetworkRequirement, PortRequirement, ProvidedInterface, ProviderCapabilityRequirement,
    RequiredInterface, SatisfactionSource, SecretRequirement, StateRequirement,
    provided_interface_may_satisfy,
};

// ── Input ──────────────────────────────────────────────────────────────────

/// One Capsule's interface, tagged with the id used to refer to it in bindings
/// and external requirements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapsuleInterfaceInput {
    pub capsule_id: String,
    pub interface: CapsuleInterface,
}

// ── Report ─────────────────────────────────────────────────────────────────

/// The full, deterministic result of composing a set of interfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositionReport {
    pub status: CompositionStatus,
    pub bindings: Vec<InterfaceBinding>,
    pub aggregate: AggregateExecutionContract,
    pub issues: Vec<CompositionIssue>,
}

/// Whether the composition can proceed as wired.
///
/// `Blocked` when at least one peer requirement is unresolved or ambiguous.
/// External requirements (secret/hardware/network/port/provider-capability and
/// state bindings) do **not** block — they are deferred to later stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionStatus {
    Ready,
    Blocked,
}

/// A resolved peer wiring: `consumer` requires something that `provider`
/// provides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceBinding {
    pub consumer_capsule_id: String,
    pub provider_capsule_id: String,
    pub required: RequiredInterface,
    pub provided: ProvidedInterface,
}

/// The composed unit's contract toward the outside world: what it still needs
/// (external + unresolved) and what it exposes (provides).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateExecutionContract {
    /// Requirements that are never satisfied by peer wiring — they must be
    /// satisfied by the host / platform / user (secret grant, managed resource,
    /// provider capability, port allocation, state binding).
    pub external_requirements: Vec<ExternalRequirement>,
    /// Peer-satisfiable requirements (service/tool/runtime) that found no peer
    /// provider in this composition. These are what make a composition
    /// [`CompositionStatus::Blocked`].
    pub unresolved_peer_requirements: Vec<UnresolvedPeerRequirement>,
    /// Everything the composed Capsules provide, preserved for re-export.
    pub exported_provides: Vec<ExportedProvide>,
}

/// A peer-satisfiable requirement that could not be wired in this composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedPeerRequirement {
    pub capsule_id: String,
    pub required: RequiredInterface,
}

/// A provide exported by the composed unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedProvide {
    pub capsule_id: String,
    pub provided: ProvidedInterface,
}

/// A requirement that the host / platform / user must satisfy — never a peer
/// Capsule. Secret values are **never** carried here (the #505 discipline:
/// only requirement / scope / projection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "spec")]
pub enum ExternalRequirement {
    Secret {
        capsule_id: String,
        requirement: SecretRequirement,
    },
    Network {
        capsule_id: String,
        requirement: NetworkRequirement,
    },
    Capability {
        capsule_id: String,
        requirement: CapabilityRequirement,
    },
    Hardware {
        capsule_id: String,
        requirement: HardwareRequirement,
    },
    Port {
        capsule_id: String,
        requirement: PortRequirement,
    },
    ProviderCapability {
        capsule_id: String,
        requirement: ProviderCapabilityRequirement,
    },
    /// `State` that found no peer [`ProvidedInterface::State`] and so degrades
    /// to a managed state binding.
    StateBinding {
        capsule_id: String,
        requirement: StateRequirement,
    },
}

impl ExternalRequirement {
    fn capsule_id(&self) -> &str {
        match self {
            ExternalRequirement::Secret { capsule_id, .. }
            | ExternalRequirement::Network { capsule_id, .. }
            | ExternalRequirement::Capability { capsule_id, .. }
            | ExternalRequirement::Hardware { capsule_id, .. }
            | ExternalRequirement::Port { capsule_id, .. }
            | ExternalRequirement::ProviderCapability { capsule_id, .. }
            | ExternalRequirement::StateBinding { capsule_id, .. } => capsule_id,
        }
    }

    /// `category::name` sort key (within a capsule).
    fn name_key(&self) -> String {
        match self {
            ExternalRequirement::Secret { requirement, .. } => {
                format!("secret::{}", requirement.name)
            }
            ExternalRequirement::Network { requirement, .. } => {
                format!("network::{}", requirement.logical_name)
            }
            ExternalRequirement::Capability { requirement, .. } => {
                format!("capability::{}", requirement.name)
            }
            ExternalRequirement::Hardware { requirement, .. } => {
                format!("hardware::{:?}", requirement.kind)
            }
            ExternalRequirement::Port { requirement, .. } => {
                format!("port::{}", requirement.logical_name)
            }
            ExternalRequirement::ProviderCapability { requirement, .. } => {
                format!("provider_capability::{}", requirement.name)
            }
            ExternalRequirement::StateBinding { requirement, .. } => {
                format!("state_binding::{}", requirement.name)
            }
        }
    }
}

/// A problem that prevents composition from being [`CompositionStatus::Ready`]
/// but that #506 refuses to resolve on the author's behalf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CompositionIssue {
    /// More than one peer Capsule can satisfy a single requirement. Composition
    /// does not pick one — the author (or a later policy) must disambiguate.
    AmbiguousProvider {
        consumer_capsule_id: String,
        required: RequiredInterface,
        /// Sorted, de-duplicated provider ids that all match.
        candidate_provider_ids: Vec<String>,
    },
}

impl CompositionIssue {
    fn sort_key(&self) -> (String, String) {
        match self {
            CompositionIssue::AmbiguousProvider {
                consumer_capsule_id,
                required,
                ..
            } => (consumer_capsule_id.clone(), required_name_key(required)),
        }
    }
}

// ── Errors ─────────────────────────────────────────────────────────────────

/// Hard validation errors that make composition impossible (as opposed to a
/// [`CompositionIssue`], which yields a `Blocked` report rather than an error).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompositionError {
    #[error("capsule_id must not be empty")]
    EmptyCapsuleId,

    /// A `capsule_id` containing whitespace is rejected rather than trimmed.
    /// The id is used downstream as a DB / placement-index / receipt key, so an
    /// implicit `" app " -> "app"` normalization would silently collide or
    /// mis-key; fail closed instead.
    #[error("capsule_id '{capsule_id}' must not contain whitespace")]
    InvalidCapsuleId { capsule_id: String },

    #[error("duplicate capsule_id '{capsule_id}' in composition input")]
    DuplicateCapsuleId { capsule_id: String },

    #[error("capsule '{capsule_id}' has an invalid interface: {source}")]
    InvalidInterface {
        capsule_id: String,
        #[source]
        source: InterfaceError,
    },
}

// ── Composition ────────────────────────────────────────────────────────────

/// Compose a set of Capsule interfaces into a deterministic
/// [`CompositionReport`].
///
/// Behavior is independent of `inputs` order. Returns [`CompositionError`] only
/// for hard input problems (duplicate ids, an interface that fails #505
/// validation); ambiguity and unsatisfied peer requirements are reported in the
/// `CompositionReport` (with `status = Blocked`), not as errors.
pub fn compose(inputs: &[CapsuleInterfaceInput]) -> Result<CompositionReport, CompositionError> {
    // 1. Validate + normalize each interface; reject duplicate ids.
    let mut normalized: Vec<CapsuleInterfaceInput> = Vec::with_capacity(inputs.len());
    let mut seen_ids = std::collections::BTreeSet::new();
    for input in inputs {
        // Fail closed on bad ids: a missing or whitespace-bearing id would
        // produce a materialized contract whose bindings / external
        // requirements cannot be attributed to a Capsule. No implicit trim.
        if input.capsule_id.trim().is_empty() {
            return Err(CompositionError::EmptyCapsuleId);
        }
        if input.capsule_id.chars().any(char::is_whitespace) {
            return Err(CompositionError::InvalidCapsuleId {
                capsule_id: input.capsule_id.clone(),
            });
        }
        if !seen_ids.insert(input.capsule_id.clone()) {
            return Err(CompositionError::DuplicateCapsuleId {
                capsule_id: input.capsule_id.clone(),
            });
        }
        let mut interface = input.interface.clone();
        interface
            .validate()
            .map_err(|source| CompositionError::InvalidInterface {
                capsule_id: input.capsule_id.clone(),
                source,
            })?;
        normalized.push(CapsuleInterfaceInput {
            capsule_id: input.capsule_id.clone(),
            interface,
        });
    }
    // Stable sort by id so iteration order is canonical.
    normalized.sort_by(|a, b| a.capsule_id.cmp(&b.capsule_id));

    // 2. Flat index of every provide tagged with its owning capsule.
    let all_provides: Vec<(String, ProvidedInterface)> = normalized
        .iter()
        .flat_map(|inp| {
            inp.interface
                .provides
                .iter()
                .map(move |p| (inp.capsule_id.clone(), p.clone()))
        })
        .collect();

    let mut bindings: Vec<InterfaceBinding> = Vec::new();
    let mut external: Vec<ExternalRequirement> = Vec::new();
    let mut unresolved: Vec<UnresolvedPeerRequirement> = Vec::new();
    let mut issues: Vec<CompositionIssue> = Vec::new();

    // 3. Classify every requirement.
    for input in &normalized {
        for required in &input.interface.requires {
            let peer_allowed = required
                .possible_satisfaction_sources()
                .contains(&SatisfactionSource::ProvidedInterface);

            if !peer_allowed {
                // Secret / Network / Capability / Hardware / Port /
                // ProviderCapability — never matched against peer provides.
                if let Some(ext) = to_external(&input.capsule_id, required) {
                    external.push(ext);
                }
                continue;
            }

            // Peer-satisfiable: collect matching provides from *other* capsules.
            let mut candidates: Vec<(String, ProvidedInterface)> = Vec::new();
            for (provider_id, provided) in &all_provides {
                if *provider_id == input.capsule_id {
                    // Self-binding is disallowed by default (#506 scope).
                    continue;
                }
                if provided_interface_may_satisfy(required, provided).compatible {
                    candidates.push((provider_id.clone(), provided.clone()));
                }
            }

            match candidates.len() {
                1 => {
                    let (provider_capsule_id, provided) = candidates.into_iter().next().unwrap();
                    bindings.push(InterfaceBinding {
                        consumer_capsule_id: input.capsule_id.clone(),
                        provider_capsule_id,
                        required: required.clone(),
                        provided,
                    });
                }
                0 => {
                    // No peer provider. If the requirement has a non-peer
                    // fallback source (only `State`, via StateBinding), aggregate
                    // it externally; otherwise it is genuinely unresolved.
                    if let Some(ext) = to_external(&input.capsule_id, required) {
                        external.push(ext);
                    } else {
                        unresolved.push(UnresolvedPeerRequirement {
                            capsule_id: input.capsule_id.clone(),
                            required: required.clone(),
                        });
                    }
                }
                _ => {
                    let mut ids: Vec<String> = candidates.into_iter().map(|(id, _)| id).collect();
                    ids.sort();
                    ids.dedup();
                    issues.push(CompositionIssue::AmbiguousProvider {
                        consumer_capsule_id: input.capsule_id.clone(),
                        required: required.clone(),
                        candidate_provider_ids: ids,
                    });
                }
            }
        }
    }

    // 4. Exported provides (preserved as-is).
    let mut exported_provides: Vec<ExportedProvide> = all_provides
        .iter()
        .map(|(capsule_id, provided)| ExportedProvide {
            capsule_id: capsule_id.clone(),
            provided: provided.clone(),
        })
        .collect();

    // 5. Deterministic ordering of every output collection.
    bindings.sort_by_key(|b| {
        (
            b.consumer_capsule_id.clone(),
            required_name_key(&b.required),
            b.provider_capsule_id.clone(),
        )
    });
    unresolved.sort_by_key(|u| (u.capsule_id.clone(), required_name_key(&u.required)));
    external.sort_by_key(|e| (e.capsule_id().to_string(), e.name_key()));
    exported_provides.sort_by_key(|e| (e.capsule_id.clone(), provided_name_key(&e.provided)));
    issues.sort_by_key(|i| i.sort_key());

    let status = if issues.is_empty() && unresolved.is_empty() {
        CompositionStatus::Ready
    } else {
        CompositionStatus::Blocked
    };

    Ok(CompositionReport {
        status,
        bindings,
        aggregate: AggregateExecutionContract {
            external_requirements: external,
            unresolved_peer_requirements: unresolved,
            exported_provides,
        },
        issues,
    })
}

/// Map a requirement to its [`ExternalRequirement`] form, if it can be
/// satisfied externally. Returns `None` for the purely peer-satisfiable
/// categories (service / tool / runtime), which have no external form.
fn to_external(capsule_id: &str, required: &RequiredInterface) -> Option<ExternalRequirement> {
    let capsule_id = capsule_id.to_string();
    match required {
        RequiredInterface::Service(_)
        | RequiredInterface::Tool(_)
        | RequiredInterface::Runtime(_) => None,
        RequiredInterface::State(r) => Some(ExternalRequirement::StateBinding {
            capsule_id,
            requirement: r.clone(),
        }),
        RequiredInterface::Secret(r) => Some(ExternalRequirement::Secret {
            capsule_id,
            requirement: r.clone(),
        }),
        RequiredInterface::Network(r) => Some(ExternalRequirement::Network {
            capsule_id,
            requirement: r.clone(),
        }),
        RequiredInterface::Capability(r) => Some(ExternalRequirement::Capability {
            capsule_id,
            requirement: r.clone(),
        }),
        RequiredInterface::Hardware(r) => Some(ExternalRequirement::Hardware {
            capsule_id,
            requirement: r.clone(),
        }),
        RequiredInterface::Port(r) => Some(ExternalRequirement::Port {
            capsule_id,
            requirement: r.clone(),
        }),
        RequiredInterface::ProviderCapability(r) => Some(ExternalRequirement::ProviderCapability {
            capsule_id,
            requirement: r.clone(),
        }),
    }
}

/// `category::name` sort key for a requirement (hardware keys on its kind,
/// matching #505's own dedup convention).
fn required_name_key(r: &RequiredInterface) -> String {
    match r {
        RequiredInterface::Service(x) => format!("service::{}", x.name),
        RequiredInterface::Tool(x) => format!("tool::{}", x.name),
        RequiredInterface::Runtime(x) => format!("runtime::{}", x.name),
        RequiredInterface::State(x) => format!("state::{}", x.name),
        RequiredInterface::Secret(x) => format!("secret::{}", x.name),
        RequiredInterface::Network(x) => format!("network::{}", x.logical_name),
        RequiredInterface::Capability(x) => format!("capability::{}", x.name),
        RequiredInterface::Hardware(x) => format!("hardware::{:?}", x.kind),
        RequiredInterface::Port(x) => format!("port::{}", x.logical_name),
        RequiredInterface::ProviderCapability(x) => {
            format!("provider_capability::{}", x.name)
        }
    }
}

/// `category::name` sort key for a provide.
fn provided_name_key(p: &ProvidedInterface) -> String {
    let category = match p {
        ProvidedInterface::Service(_) => "service",
        ProvidedInterface::Tool(_) => "tool",
        ProvidedInterface::Runtime(_) => "runtime",
        ProvidedInterface::State(_) => "state",
        ProvidedInterface::Ui(_) => "ui",
        ProvidedInterface::Api(_) => "api",
    };
    format!("{category}::{}", p.logical_name())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::interface::{
        HardwareKind, NetworkRequirement, PortRequirement, ProviderCapabilityRequirement,
        SecretProjection, SecretScope, ServiceProvide, ServiceRequirement, StateProvide,
        StateRequirement,
    };

    fn input(id: &str, interface: CapsuleInterface) -> CapsuleInterfaceInput {
        CapsuleInterfaceInput {
            capsule_id: id.to_string(),
            interface,
        }
    }

    fn iface(
        provides: Vec<ProvidedInterface>,
        requires: Vec<RequiredInterface>,
    ) -> CapsuleInterface {
        CapsuleInterface { provides, requires }
    }

    fn svc_req(name: &str, proto: Option<&str>) -> RequiredInterface {
        RequiredInterface::Service(ServiceRequirement {
            name: name.to_string(),
            protocol: proto.map(String::from),
            version: None,
        })
    }

    fn svc_provide(name: &str, proto: Option<&str>) -> ProvidedInterface {
        ProvidedInterface::Service(ServiceProvide {
            name: name.to_string(),
            protocol: proto.map(String::from),
            version: None,
        })
    }

    fn secret_req(name: &str) -> RequiredInterface {
        RequiredInterface::Secret(SecretRequirement {
            name: name.to_string(),
            scope: SecretScope::CapsuleInstance,
            projection: SecretProjection::Env {
                name: name.to_string(),
            },
            optional: false,
        })
    }

    fn port_req(name: &str) -> RequiredInterface {
        RequiredInterface::Port(PortRequirement {
            logical_name: name.to_string(),
            preferred_port: Some(8080),
            can_remap: true,
        })
    }

    #[test]
    fn composition_binds_service_requirement_to_matching_provider() {
        let report = compose(&[
            input("app", iface(vec![], vec![svc_req("postgres", Some("tcp"))])),
            input(
                "db",
                iface(vec![svc_provide("postgres", Some("tcp"))], vec![]),
            ),
        ])
        .unwrap();

        assert_eq!(report.status, CompositionStatus::Ready);
        assert_eq!(report.bindings.len(), 1);
        let b = &report.bindings[0];
        assert_eq!(b.consumer_capsule_id, "app");
        assert_eq!(b.provider_capsule_id, "db");
        assert!(report.aggregate.unresolved_peer_requirements.is_empty());
        assert!(report.issues.is_empty());
    }

    #[test]
    fn composition_blocks_ambiguous_service_provider() {
        let report = compose(&[
            input("app", iface(vec![], vec![svc_req("postgres", None)])),
            input("db-a", iface(vec![svc_provide("postgres", None)], vec![])),
            input("db-b", iface(vec![svc_provide("postgres", None)], vec![])),
        ])
        .unwrap();

        assert_eq!(report.status, CompositionStatus::Blocked);
        // No auto-pick: nothing bound.
        assert!(report.bindings.is_empty());
        assert_eq!(report.issues.len(), 1);
        match &report.issues[0] {
            CompositionIssue::AmbiguousProvider {
                consumer_capsule_id,
                candidate_provider_ids,
                ..
            } => {
                assert_eq!(consumer_capsule_id, "app");
                assert_eq!(candidate_provider_ids, &["db-a", "db-b"]);
            }
        }
    }

    #[test]
    fn composition_blocks_unsatisfied_peer_service_requirement() {
        let report =
            compose(&[input("app", iface(vec![], vec![svc_req("postgres", None)]))]).unwrap();

        assert_eq!(report.status, CompositionStatus::Blocked);
        assert_eq!(report.aggregate.unresolved_peer_requirements.len(), 1);
        assert_eq!(
            report.aggregate.unresolved_peer_requirements[0].capsule_id,
            "app"
        );
        assert!(report.aggregate.external_requirements.is_empty());
    }

    #[test]
    fn composition_does_not_peer_satisfy_secret_requirement() {
        // Another capsule "provides" a service named like the secret — must NOT
        // be used to satisfy the secret.
        let report = compose(&[
            input("app", iface(vec![], vec![secret_req("OPENAI_API_KEY")])),
            input(
                "x",
                iface(vec![svc_provide("OPENAI_API_KEY", None)], vec![]),
            ),
        ])
        .unwrap();

        assert!(report.bindings.is_empty());
        assert_eq!(report.aggregate.external_requirements.len(), 1);
        assert!(matches!(
            report.aggregate.external_requirements[0],
            ExternalRequirement::Secret { .. }
        ));
        // Secrets do not block.
        assert_eq!(report.status, CompositionStatus::Ready);
    }

    #[test]
    fn composition_aggregates_secret_requirement() {
        let report = compose(&[input("app", iface(vec![], vec![secret_req("TOKEN")]))]).unwrap();
        assert_eq!(report.aggregate.external_requirements.len(), 1);
        match &report.aggregate.external_requirements[0] {
            ExternalRequirement::Secret {
                capsule_id,
                requirement,
            } => {
                assert_eq!(capsule_id, "app");
                assert_eq!(requirement.name, "TOKEN");
            }
            other => panic!("expected Secret, got {other:?}"),
        }
        // The serialized aggregate must never carry a secret value field.
        let json = serde_json::to_string(&report.aggregate).unwrap();
        for forbidden in ["\"value\"", "\"plaintext\"", "\"material\""] {
            assert!(!json.contains(forbidden), "leaked secret field: {json}");
        }
    }

    #[test]
    fn composition_does_not_peer_satisfy_port_requirement() {
        let report = compose(&[
            input("app", iface(vec![], vec![port_req("http")])),
            input("x", iface(vec![svc_provide("http", Some("tcp"))], vec![])),
        ])
        .unwrap();
        assert!(report.bindings.is_empty());
        assert_eq!(report.aggregate.external_requirements.len(), 1);
        assert!(matches!(
            report.aggregate.external_requirements[0],
            ExternalRequirement::Port { .. }
        ));
        assert_eq!(report.status, CompositionStatus::Ready);
    }

    #[test]
    fn composition_aggregates_port_requirement() {
        let report = compose(&[input("app", iface(vec![], vec![port_req("http")]))]).unwrap();
        match &report.aggregate.external_requirements[0] {
            ExternalRequirement::Port {
                capsule_id,
                requirement,
            } => {
                assert_eq!(capsule_id, "app");
                assert_eq!(requirement.logical_name, "http");
            }
            other => panic!("expected Port, got {other:?}"),
        }
    }

    #[test]
    fn composition_aggregates_network_hardware_and_provider_capability() {
        let report = compose(&[input(
            "app",
            iface(
                vec![],
                vec![
                    RequiredInterface::Network(NetworkRequirement {
                        logical_name: "egress".into(),
                        egress: true,
                        hosts: vec!["api.openai.com".into()],
                    }),
                    RequiredInterface::Hardware(HardwareRequirement {
                        kind: HardwareKind::Gpu,
                        constraint: Some("vram>=8GB".into()),
                    }),
                    RequiredInterface::ProviderCapability(ProviderCapabilityRequirement {
                        name: "oci-launch".into(),
                    }),
                ],
            ),
        )])
        .unwrap();

        assert_eq!(report.status, CompositionStatus::Ready);
        assert_eq!(report.aggregate.external_requirements.len(), 3);
        let kinds: Vec<_> = report
            .aggregate
            .external_requirements
            .iter()
            .map(|e| match e {
                ExternalRequirement::Network { .. } => "network",
                ExternalRequirement::Hardware { .. } => "hardware",
                ExternalRequirement::ProviderCapability { .. } => "provider_capability",
                _ => "other",
            })
            .collect();
        assert!(kinds.contains(&"network"));
        assert!(kinds.contains(&"hardware"));
        assert!(kinds.contains(&"provider_capability"));
    }

    #[test]
    fn composition_binds_state_to_peer_state_provider_when_available() {
        let state_req = RequiredInterface::State(StateRequirement {
            name: "session".into(),
            version: None,
        });
        let state_prov = ProvidedInterface::State(StateProvide {
            name: "session".into(),
            version: None,
        });
        let report = compose(&[
            input("app", iface(vec![], vec![state_req])),
            input("store", iface(vec![state_prov], vec![])),
        ])
        .unwrap();

        assert_eq!(report.status, CompositionStatus::Ready);
        assert_eq!(report.bindings.len(), 1);
        assert_eq!(report.bindings[0].provider_capsule_id, "store");
        // Bound to a peer → not also an external state binding.
        assert!(report.aggregate.external_requirements.is_empty());
    }

    #[test]
    fn composition_falls_back_state_to_external_binding_when_no_peer_provider() {
        let report = compose(&[input(
            "app",
            iface(
                vec![],
                vec![RequiredInterface::State(StateRequirement {
                    name: "session".into(),
                    version: None,
                })],
            ),
        )])
        .unwrap();

        // No peer StateProvide → external StateBinding, and NOT blocked.
        assert_eq!(report.status, CompositionStatus::Ready);
        assert!(report.bindings.is_empty());
        assert_eq!(report.aggregate.external_requirements.len(), 1);
        assert!(matches!(
            report.aggregate.external_requirements[0],
            ExternalRequirement::StateBinding { .. }
        ));
        assert!(report.aggregate.unresolved_peer_requirements.is_empty());
    }

    #[test]
    fn composition_is_deterministic_across_input_order() {
        let app = input(
            "app",
            iface(
                vec![],
                vec![
                    svc_req("postgres", None),
                    secret_req("TOKEN"),
                    port_req("http"),
                ],
            ),
        );
        let db = input("db", iface(vec![svc_provide("postgres", None)], vec![]));
        let cache = input("cache", iface(vec![svc_provide("redis", None)], vec![]));

        let a = compose(&[app.clone(), db.clone(), cache.clone()]).unwrap();
        let b = compose(&[cache, db, app]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn composition_rejects_duplicate_capsule_ids() {
        let err = compose(&[
            input("dup", iface(vec![], vec![])),
            input("dup", iface(vec![], vec![])),
        ])
        .unwrap_err();
        assert!(matches!(err, CompositionError::DuplicateCapsuleId { .. }));
    }

    #[test]
    fn composition_rejects_empty_capsule_id() {
        let err = compose(&[input("", iface(vec![], vec![]))]).unwrap_err();
        assert!(matches!(err, CompositionError::EmptyCapsuleId));
        // Whitespace-only counts as empty.
        let err = compose(&[input("   ", iface(vec![], vec![]))]).unwrap_err();
        assert!(matches!(err, CompositionError::EmptyCapsuleId));
    }

    #[test]
    fn composition_rejects_whitespace_capsule_id() {
        // A non-empty id containing whitespace is rejected, NOT trimmed — so
        // `" app "` never silently becomes `"app"`.
        for id in [" app ", "a b", "app\t"] {
            let err = compose(&[input(id, iface(vec![], vec![]))]).unwrap_err();
            assert!(
                matches!(err, CompositionError::InvalidCapsuleId { .. }),
                "expected InvalidCapsuleId for {id:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn composition_does_not_allow_self_binding() {
        // One capsule both requires and provides the same service. It must not
        // satisfy itself → unresolved + Blocked.
        let report = compose(&[input(
            "app",
            iface(
                vec![svc_provide("postgres", None)],
                vec![svc_req("postgres", None)],
            ),
        )])
        .unwrap();

        assert_eq!(report.status, CompositionStatus::Blocked);
        assert!(report.bindings.is_empty());
        assert_eq!(report.aggregate.unresolved_peer_requirements.len(), 1);
        // The provide is still exported.
        assert_eq!(report.aggregate.exported_provides.len(), 1);
    }

    #[test]
    fn composition_preserves_exported_provides() {
        let report = compose(&[
            input("app", iface(vec![svc_provide("api", Some("http"))], vec![])),
            input("db", iface(vec![svc_provide("postgres", None)], vec![])),
        ])
        .unwrap();

        assert_eq!(report.aggregate.exported_provides.len(), 2);
        let ids: Vec<_> = report
            .aggregate
            .exported_provides
            .iter()
            .map(|e| e.capsule_id.as_str())
            .collect();
        assert_eq!(ids, ["app", "db"]); // deterministic by capsule_id
    }

    #[test]
    fn composition_report_serialization_roundtrip() {
        let report = compose(&[
            input(
                "app",
                iface(
                    vec![svc_provide("api", Some("http"))],
                    vec![svc_req("postgres", None), secret_req("TOKEN")],
                ),
            ),
            input("db", iface(vec![svc_provide("postgres", None)], vec![])),
        ])
        .unwrap();

        let json = serde_json::to_string_pretty(&report).unwrap();
        let back: CompositionReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }
}

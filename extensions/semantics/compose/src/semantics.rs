use ato_computation::{ComputationObject, ResolvedComputation, SemanticsId};
use ato_kernel::{
    Action, ChoiceId, SemanticError, SemanticHost, SemanticStep, Semantics, TransitionOffer,
};

use crate::{
    CompositeReduction, Endpoint, NodeId, NodeStep, ProtocolRolePolicy, ValidatedComposite,
    compose_semantics_id, encode_composite_residual, lift_exported_step, lift_internal_step,
    synchronize_connection, validate_composite,
};

pub struct ComposeSemantics {
    id: SemanticsId,
}

impl ComposeSemantics {
    pub fn new() -> Self {
        Self {
            id: compose_semantics_id(),
        }
    }

    fn current(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost,
    ) -> Result<ValidatedComposite, SemanticError> {
        let resolver = HostResolver { host };
        let roles = HostRolePolicy { host };
        validate_composite(current, &resolver, &roles).map_err(semantic_error)
    }
}

impl Default for ComposeSemantics {
    fn default() -> Self {
        Self::new()
    }
}

impl Semantics for ComposeSemantics {
    fn id(&self) -> &SemanticsId {
        &self.id
    }

    fn validate(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost,
    ) -> Result<(), SemanticError> {
        self.current(current, host).map(|_| ())
    }

    fn enabled(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost,
    ) -> Result<Vec<TransitionOffer>, SemanticError> {
        Ok(candidates(&self.current(current, host)?, host)?
            .into_iter()
            .map(|candidate| candidate.offer)
            .collect())
    }

    fn step(
        &self,
        current: &ResolvedComputation,
        offer: &TransitionOffer,
        host: &dyn SemanticHost,
    ) -> Result<SemanticStep, SemanticError> {
        let validated = self.current(current, host)?;
        let reduction = if matches!(offer.action, Action::Input { .. }) && offer.choice.is_none() {
            reduce_external_input(&validated, host, offer)?
        } else {
            let candidate = candidates(&validated, host)?
                .into_iter()
                .find(|candidate| candidate.offer == *offer)
                .ok_or_else(|| SemanticError::new("transition offer is not enabled"))?;
            reduce_candidate(&validated, host, candidate)?
        };
        let residual_bytes =
            encode_composite_residual(&reduction.successor).map_err(semantic_error)?;
        let residual = host.put_object(&residual_bytes).map_err(semantic_error)?;
        Ok(SemanticStep {
            offer: TransitionOffer {
                choice: offer.choice.clone(),
                action: reduction.label,
            },
            successor: ComputationObject {
                semantics: current.object().semantics.clone(),
                boundary: current.object().boundary.clone(),
                residual,
            },
        })
    }
}

#[derive(Clone)]
struct Candidate {
    offer: TransitionOffer,
    kind: CandidateKind,
}

#[derive(Clone)]
enum CandidateKind {
    ChildTau {
        node: NodeId,
        child: TransitionOffer,
    },
    Synchronize {
        output: Endpoint,
        input: Endpoint,
        child: TransitionOffer,
    },
    Export {
        node: NodeId,
        child: TransitionOffer,
    },
}

fn candidates(
    current: &ValidatedComposite,
    host: &dyn SemanticHost,
) -> Result<Vec<Candidate>, SemanticError> {
    let residual = current.residual();
    let mut candidates = Vec::new();
    for (node, reference) in &residual.nodes {
        for child in host.enabled(reference).map_err(semantic_error)? {
            match &child.action {
                Action::Tau => candidates.push(Candidate {
                    offer: TransitionOffer::selected(
                        compose_choice("tau", node, child.choice.as_ref()),
                        Action::Tau,
                    ),
                    kind: CandidateKind::ChildTau {
                        node: node.clone(),
                        child,
                    },
                }),
                Action::Input { port, payload } => {
                    if let Some(parent) = exported_parent(residual.exports.iter(), node, port) {
                        candidates.push(Candidate {
                            offer: TransitionOffer {
                                choice: child
                                    .choice
                                    .clone()
                                    .map(|choice| compose_choice("input", node, Some(&choice))),
                                action: Action::Input {
                                    port: parent,
                                    payload: payload.clone(),
                                },
                            },
                            kind: CandidateKind::Export {
                                node: node.clone(),
                                child,
                            },
                        });
                    }
                }
                Action::Output { port, payload } => {
                    for connection in &residual.connections {
                        let endpoint = Endpoint {
                            node: node.clone(),
                            port: port.clone(),
                        };
                        let input = if connection.first() == &endpoint {
                            Some(connection.second())
                        } else if connection.second() == &endpoint {
                            Some(connection.first())
                        } else {
                            None
                        };
                        if let Some(input) = input {
                            candidates.push(Candidate {
                                offer: TransitionOffer::selected(
                                    ChoiceId::new(format!(
                                        "sync:{node}:{}:{}:{}",
                                        child_choice(&child),
                                        input.node,
                                        input.port
                                    )),
                                    Action::Tau,
                                ),
                                kind: CandidateKind::Synchronize {
                                    output: endpoint,
                                    input: input.clone(),
                                    child: child.clone(),
                                },
                            });
                        }
                    }
                    if let Some(parent) = exported_parent(residual.exports.iter(), node, port) {
                        candidates.push(Candidate {
                            offer: TransitionOffer::selected(
                                compose_choice("output", node, child.choice.as_ref()),
                                Action::Output {
                                    port: parent,
                                    payload: payload.clone(),
                                },
                            ),
                            kind: CandidateKind::Export {
                                node: node.clone(),
                                child,
                            },
                        });
                    }
                }
            }
        }
    }
    Ok(candidates)
}

fn reduce_candidate(
    current: &ValidatedComposite,
    host: &dyn SemanticHost,
    candidate: Candidate,
) -> Result<CompositeReduction, SemanticError> {
    match candidate.kind {
        CandidateKind::ChildTau { node, child } => {
            let reference = &current.residual().nodes[&node];
            let transition = host
                .derive_transition(reference, &child)
                .map_err(semantic_error)?;
            let step = node_step(host, node, transition)?;
            lift_internal_step(current, &step).map_err(semantic_error)
        }
        CandidateKind::Synchronize {
            output,
            input,
            child,
        } => {
            let output_ref = &current.residual().nodes[&output.node];
            let input_ref = &current.residual().nodes[&input.node];
            let Action::Output { payload, .. } = &child.action else {
                return Err(SemanticError::new(
                    "synchronization candidate is not output",
                ));
            };
            let input_offer = TransitionOffer::external_input(input.port.clone(), payload.clone());
            let output_transition = host
                .derive_transition(output_ref, &child)
                .map_err(semantic_error)?;
            let input_transition = host
                .derive_transition(input_ref, &input_offer)
                .map_err(semantic_error)?;
            let output_step = node_step(host, output.node, output_transition)?;
            let input_step = node_step(host, input.node, input_transition)?;
            synchronize_connection(current, &output_step, &input_step).map_err(semantic_error)
        }
        CandidateKind::Export { node, child } => {
            let reference = &current.residual().nodes[&node];
            let transition = host
                .derive_transition(reference, &child)
                .map_err(semantic_error)?;
            let step = node_step(host, node, transition)?;
            lift_exported_step(current, &step).map_err(semantic_error)
        }
    }
}

fn reduce_external_input(
    current: &ValidatedComposite,
    host: &dyn SemanticHost,
    offer: &TransitionOffer,
) -> Result<CompositeReduction, SemanticError> {
    let Action::Input { port, payload } = &offer.action else {
        return Err(SemanticError::new("external input expected"));
    };
    let endpoint = current
        .residual()
        .exports
        .get(port)
        .ok_or_else(|| SemanticError::new(format!("unexported parent port {port}")))?;
    let child = TransitionOffer::external_input(endpoint.port.clone(), payload.clone());
    let reference = &current.residual().nodes[&endpoint.node];
    let transition = host
        .derive_transition(reference, &child)
        .map_err(semantic_error)?;
    let step = node_step(host, endpoint.node.clone(), transition)?;
    lift_exported_step(current, &step).map_err(semantic_error)
}

fn node_step(
    host: &dyn SemanticHost,
    node: NodeId,
    transition: ato_kernel::Transition,
) -> Result<NodeStep, SemanticError> {
    Ok(NodeStep {
        node,
        from: host.resolve(&transition.from).map_err(semantic_error)?,
        label: transition.offer.action,
        to: host.resolve(&transition.to).map_err(semantic_error)?,
    })
}

fn compose_choice(kind: &str, node: &NodeId, child: Option<&ChoiceId>) -> ChoiceId {
    ChoiceId::new(format!(
        "{kind}:{node}:{}",
        child.map_or("external", ChoiceId::as_str)
    ))
}

fn child_choice(offer: &TransitionOffer) -> &str {
    offer.choice.as_ref().map_or("external", ChoiceId::as_str)
}

fn exported_parent<'a>(
    mut exports: impl Iterator<Item = (&'a ato_computation::PortId, &'a Endpoint)>,
    node: &NodeId,
    port: &ato_computation::PortId,
) -> Option<ato_computation::PortId> {
    exports.find_map(|(parent, endpoint)| {
        (&endpoint.node == node && &endpoint.port == port).then(|| parent.clone())
    })
}

fn semantic_error(error: impl std::fmt::Display) -> SemanticError {
    SemanticError::new(error.to_string())
}

struct HostResolver<'a> {
    host: &'a dyn SemanticHost,
}

struct HostRolePolicy<'a> {
    host: &'a dyn SemanticHost,
}

impl ProtocolRolePolicy for HostRolePolicy<'_> {
    fn connection_roles_compatible(
        &self,
        protocol: &ato_computation::ProtocolId,
        first: &ato_computation::RoleId,
        second: &ato_computation::RoleId,
    ) -> bool {
        self.host
            .roles_compatible(protocol, first, second)
            .unwrap_or(false)
    }

    fn export_role_compatible(
        &self,
        _protocol: &ato_computation::ProtocolId,
        parent: &ato_computation::RoleId,
        child: &ato_computation::RoleId,
    ) -> bool {
        parent == child
    }
}

impl ato_objects::ObjectResolver for HostResolver<'_> {
    fn metadata(
        &self,
        reference: &ato_computation::ContentRef,
    ) -> Result<ato_objects::ObjectMetadata, ato_objects::ObjectError> {
        let bytes = self
            .host
            .get_object(
                reference,
                crate::MAX_COMPOSITE_RESIDUAL_BYTES
                    .max(ato_computation::MAX_COMPUTATION_OBJECT_BYTES),
            )
            .map_err(|error| ato_objects::ObjectError::Storage(error.to_string()))?;
        Ok(ato_objects::ObjectMetadata {
            size: bytes.len() as u64,
        })
    }

    fn open(
        &self,
        reference: &ato_computation::ContentRef,
    ) -> Result<Box<dyn std::io::Read + Send + '_>, ato_objects::ObjectError> {
        let bytes = self
            .host
            .get_object(
                reference,
                crate::MAX_COMPOSITE_RESIDUAL_BYTES
                    .max(ato_computation::MAX_COMPUTATION_OBJECT_BYTES),
            )
            .map_err(|error| ato_objects::ObjectError::Storage(error.to_string()))?;
        Ok(Box::new(std::io::Cursor::new(bytes)))
    }
}

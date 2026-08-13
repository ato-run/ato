use std::sync::Arc;

use ato_computation::{ComputationObject, ResolvedComputation, SemanticsId};
use ato_kernel::{Action, SemanticError, SemanticHost, SemanticStep, Semantics};

use crate::{
    CompositeReduction, Endpoint, NodeId, NodeStep, ProtocolRolePolicy, ValidatedComposite,
    compose_semantics_id, encode_composite_residual, lift_exported_step, lift_internal_step,
    synchronize_connection, validate_composite,
};

pub struct ComposeSemantics {
    id: SemanticsId,
    roles: Arc<dyn ProtocolRolePolicy>,
}

impl ComposeSemantics {
    pub fn new(roles: Arc<dyn ProtocolRolePolicy>) -> Self {
        Self {
            id: compose_semantics_id(),
            roles,
        }
    }

    fn current<V>(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost<V>,
    ) -> Result<ValidatedComposite, SemanticError> {
        let resolver = HostResolver { host };
        validate_composite(current, &resolver, self.roles.as_ref()).map_err(semantic_error)
    }
}

impl<V> Semantics<V> for ComposeSemantics
where
    V: Clone + Eq + Send + Sync + 'static,
{
    fn id(&self) -> &SemanticsId {
        &self.id
    }

    fn validate(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost<V>,
    ) -> Result<(), SemanticError> {
        self.current(current, host).map(|_| ())
    }

    fn enabled(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost<V>,
    ) -> Result<Vec<Action<V>>, SemanticError> {
        let current = self.current(current, host)?;
        let residual = current.residual();
        let mut actions = Vec::new();
        let mut tau_enabled = false;

        for (node, reference) in &residual.nodes {
            for action in host.enabled(reference).map_err(semantic_error)? {
                match action {
                    Action::Tau => tau_enabled = true,
                    Action::Input { port, value } => {
                        if let Some(parent) = exported_parent(residual.exports.iter(), node, &port)
                        {
                            actions.push(Action::Input {
                                port: parent,
                                value,
                            });
                        }
                    }
                    Action::Output { port, value } => {
                        if is_connected(residual, node, &port) {
                            tau_enabled = true;
                        }
                        if let Some(parent) = exported_parent(residual.exports.iter(), node, &port)
                        {
                            actions.push(Action::Output {
                                port: parent,
                                value,
                            });
                        }
                    }
                }
            }
        }
        if tau_enabled {
            actions.push(Action::Tau);
        }
        Ok(actions)
    }

    fn step(
        &self,
        current: &ResolvedComputation,
        action: &Action<V>,
        host: &dyn SemanticHost<V>,
    ) -> Result<SemanticStep<V>, SemanticError> {
        let validated = self.current(current, host)?;
        let reduction = match action {
            Action::Tau => reduce_tau(&validated, host)?,
            Action::Input { port, value } => reduce_export(&validated, host, port, value, true)?,
            Action::Output { port, value } => reduce_export(&validated, host, port, value, false)?,
        };
        let residual_bytes =
            encode_composite_residual(&reduction.successor).map_err(semantic_error)?;
        let residual = host.put_object(&residual_bytes).map_err(semantic_error)?;
        Ok(SemanticStep {
            action: reduction.label,
            successor: ComputationObject {
                semantics: current.object().semantics.clone(),
                boundary: current.object().boundary.clone(),
                residual,
            },
        })
    }
}

fn reduce_export<V>(
    current: &ValidatedComposite,
    host: &dyn SemanticHost<V>,
    parent_port: &ato_computation::PortId,
    value: &V,
    input: bool,
) -> Result<CompositeReduction<V>, SemanticError>
where
    V: Clone + Eq + Send + Sync + 'static,
{
    let endpoint = current
        .residual()
        .exports
        .get(parent_port)
        .ok_or_else(|| SemanticError::new(format!("unexported parent port {parent_port}")))?;
    let from_ref = current
        .residual()
        .nodes
        .get(&endpoint.node)
        .ok_or_else(|| {
            SemanticError::new(format!("export names missing node {}", endpoint.node))
        })?;
    let child_action = if input {
        Action::Input {
            port: endpoint.port.clone(),
            value: value.clone(),
        }
    } else {
        Action::Output {
            port: endpoint.port.clone(),
            value: value.clone(),
        }
    };
    let transition = host
        .transition(from_ref, &child_action)
        .map_err(semantic_error)?;
    let step = node_step(host, endpoint.node.clone(), transition)?;
    lift_exported_step(current, &step).map_err(semantic_error)
}

fn reduce_tau<V>(
    current: &ValidatedComposite,
    host: &dyn SemanticHost<V>,
) -> Result<CompositeReduction<V>, SemanticError>
where
    V: Clone + Eq + Send + Sync + 'static,
{
    for (node, reference) in &current.residual().nodes {
        if host
            .enabled(reference)
            .map_err(semantic_error)?
            .iter()
            .any(|action| matches!(action, Action::Tau))
        {
            let transition = host
                .transition(reference, &Action::Tau)
                .map_err(semantic_error)?;
            let step = node_step(host, node.clone(), transition)?;
            return lift_internal_step(current, &step).map_err(semantic_error);
        }
    }

    for connection in &current.residual().connections {
        if let Some(reduction) =
            synchronize_from_output(current, host, connection.first(), connection.second())?
        {
            return Ok(reduction);
        }
        if let Some(reduction) =
            synchronize_from_output(current, host, connection.second(), connection.first())?
        {
            return Ok(reduction);
        }
    }
    Err(SemanticError::new("no internal transition is enabled"))
}

fn synchronize_from_output<V>(
    current: &ValidatedComposite,
    host: &dyn SemanticHost<V>,
    output: &Endpoint,
    input: &Endpoint,
) -> Result<Option<CompositeReduction<V>>, SemanticError>
where
    V: Clone + Eq + Send + Sync + 'static,
{
    let output_ref = &current.residual().nodes[&output.node];
    let input_ref = &current.residual().nodes[&input.node];
    let Some(output_action) = host
        .enabled(output_ref)
        .map_err(semantic_error)?
        .into_iter()
        .find(|action| matches!(action, Action::Output { port, .. } if port == &output.port))
    else {
        return Ok(None);
    };
    let Action::Output { value, .. } = &output_action else {
        unreachable!("filtered output action")
    };
    let input_action = Action::Input {
        port: input.port.clone(),
        value: value.clone(),
    };
    let output_transition = host
        .transition(output_ref, &output_action)
        .map_err(semantic_error)?;
    let input_transition = host
        .transition(input_ref, &input_action)
        .map_err(semantic_error)?;
    let output_step = node_step(host, output.node.clone(), output_transition)?;
    let input_step = node_step(host, input.node.clone(), input_transition)?;
    synchronize_connection(current, &output_step, &input_step)
        .map(Some)
        .map_err(semantic_error)
}

fn node_step<V>(
    host: &dyn SemanticHost<V>,
    node: NodeId,
    transition: ato_kernel::Transition<V>,
) -> Result<NodeStep<V>, SemanticError> {
    Ok(NodeStep {
        node,
        from: host.resolve(&transition.from).map_err(semantic_error)?,
        label: transition.action,
        to: host.resolve(&transition.to).map_err(semantic_error)?,
    })
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

fn is_connected(
    residual: &crate::CompositeResidual,
    node: &NodeId,
    port: &ato_computation::PortId,
) -> bool {
    residual.connections.iter().any(|connection| {
        [connection.first(), connection.second()]
            .iter()
            .any(|endpoint| &endpoint.node == node && &endpoint.port == port)
    })
}

fn semantic_error(error: impl std::fmt::Display) -> SemanticError {
    SemanticError::new(error.to_string())
}

struct HostResolver<'a, V> {
    host: &'a dyn SemanticHost<V>,
}

impl<V> ato_objects::ObjectResolver for HostResolver<'_, V> {
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

//! Small-step reduction rules for `capsule.compose@1`.
//!
//! These functions consume verified child transition evidence but deliberately
//! do not evaluate child semantics or persist a successor computation.

use ato_computation::{PortId, ResolvedComputation};
use ato_kernel::Action;
use thiserror::Error;

use crate::{CompositeResidual, Endpoint, NodeId, ValidatedComposite};

/// The label on one semantic transition; it is neither a record nor a wire format.
pub type StepLabel = Action;

/// Transition evidence supplied by a child semantics.
///
/// The `from` and `to` computations are identity-verified; transition validity
/// is owned by the child semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStep {
    pub node: NodeId,
    pub from: ResolvedComputation,
    pub label: StepLabel,
    pub to: ResolvedComputation,
}

/// A candidate composite successor; callers seal and validate it separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeReduction {
    pub label: StepLabel,
    pub successor: CompositeResidual,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompositeStepError {
    #[error("step names missing node {node}")]
    MissingNode { node: NodeId },
    #[error("step from reference is stale for node {node}: expected {expected}, got {actual}")]
    StaleFrom {
        node: NodeId,
        expected: String,
        actual: String,
    },
    #[error("internal lifting requires a tau child action")]
    InternalActionMustBeTau,
    #[error("synchronization requires two distinct nodes")]
    SameNodeSynchronizationUnsupported,
    #[error("steps do not name endpoints of one declared connection")]
    ConnectionEndpointMismatch,
    #[error("synchronization requires one output and one input action")]
    ComplementaryActionsRequired,
    #[error("synchronization values differ")]
    ValueMismatch,
    #[error("child action port {port} is not exported by the composite")]
    UnexportedPort { port: PortId },
    #[error("export lifting requires an input or output child action")]
    ExportedActionRequired,
    #[error("child action port {port} is connected internally and cannot be exported")]
    ConnectedPortCannotBeExported { port: PortId },
}

/// Lift a child's internal transition without changing the parent boundary.
pub fn lift_internal_step(
    current: &ValidatedComposite,
    step: &NodeStep,
) -> Result<CompositeReduction, CompositeStepError> {
    if !matches!(step.label, StepLabel::Tau) {
        return Err(CompositeStepError::InternalActionMustBeTau);
    }
    let successor = replace_node(current.residual(), step)?;
    Ok(CompositeReduction {
        label: StepLabel::Tau,
        successor,
    })
}

/// Synchronize complementary child actions connected inside the composite.
pub fn synchronize_connection(
    current: &ValidatedComposite,
    first: &NodeStep,
    second: &NodeStep,
) -> Result<CompositeReduction, CompositeStepError> {
    if first.node == second.node {
        return Err(CompositeStepError::SameNodeSynchronizationUnsupported);
    }
    let current = current.residual();
    ensure_current_node(current, first)?;
    ensure_current_node(current, second)?;

    let (output, input) = match (&first.label, &second.label) {
        (
            StepLabel::Output { port, payload },
            StepLabel::Input {
                port: input_port,
                payload: input_payload,
            },
        ) => (
            (&first.node, port, payload),
            (&second.node, input_port, input_payload),
        ),
        (
            StepLabel::Input { port, payload },
            StepLabel::Output {
                port: output_port,
                payload: output_payload,
            },
        ) => (
            (&second.node, output_port, output_payload),
            (&first.node, port, payload),
        ),
        _ => return Err(CompositeStepError::ComplementaryActionsRequired),
    };
    if output.2 != input.2 {
        return Err(CompositeStepError::ValueMismatch);
    }
    let output_endpoint = Endpoint {
        node: output.0.clone(),
        port: output.1.clone(),
    };
    let input_endpoint = Endpoint {
        node: input.0.clone(),
        port: input.1.clone(),
    };
    if !current.connections.iter().any(|connection| {
        (connection.first() == &output_endpoint && connection.second() == &input_endpoint)
            || (connection.first() == &input_endpoint && connection.second() == &output_endpoint)
    }) {
        return Err(CompositeStepError::ConnectionEndpointMismatch);
    }

    let mut successor = replace_node(current, first)?;
    successor
        .nodes
        .insert(second.node.clone(), second.to.reference().clone());
    Ok(CompositeReduction {
        label: StepLabel::Tau,
        successor,
    })
}

/// Lift one child action through an exported parent Port.
pub fn lift_exported_step(
    current: &ValidatedComposite,
    step: &NodeStep,
) -> Result<CompositeReduction, CompositeStepError> {
    let current = current.residual();
    ensure_current_node(current, step)?;
    let (child_port, payload, is_input) = match &step.label {
        StepLabel::Input { port, payload } => (port, payload, true),
        StepLabel::Output { port, payload } => (port, payload, false),
        StepLabel::Tau => return Err(CompositeStepError::ExportedActionRequired),
    };
    let endpoint = Endpoint {
        node: step.node.clone(),
        port: child_port.clone(),
    };
    if current
        .connections
        .iter()
        .any(|connection| connection.first() == &endpoint || connection.second() == &endpoint)
    {
        return Err(CompositeStepError::ConnectedPortCannotBeExported {
            port: child_port.clone(),
        });
    }
    let parent_port = current
        .exports
        .iter()
        .find_map(|(parent, child)| (child == &endpoint).then(|| parent.clone()))
        .ok_or_else(|| CompositeStepError::UnexportedPort {
            port: child_port.clone(),
        })?;
    let successor = replace_node(current, step)?;
    let label = if is_input {
        StepLabel::Input {
            port: parent_port,
            payload: payload.clone(),
        }
    } else {
        StepLabel::Output {
            port: parent_port,
            payload: payload.clone(),
        }
    };
    Ok(CompositeReduction { label, successor })
}

fn replace_node(
    current: &CompositeResidual,
    step: &NodeStep,
) -> Result<CompositeResidual, CompositeStepError> {
    ensure_current_node(current, step)?;
    let mut successor = current.clone();
    successor
        .nodes
        .insert(step.node.clone(), step.to.reference().clone());
    Ok(successor)
}

fn ensure_current_node(
    current: &CompositeResidual,
    step: &NodeStep,
) -> Result<(), CompositeStepError> {
    let expected =
        current
            .nodes
            .get(&step.node)
            .ok_or_else(|| CompositeStepError::MissingNode {
                node: step.node.clone(),
            })?;
    if expected != step.from.reference() {
        return Err(CompositeStepError::StaleFrom {
            node: step.node.clone(),
            expected: expected.to_string(),
            actual: step.from.reference().to_string(),
        });
    }
    Ok(())
}

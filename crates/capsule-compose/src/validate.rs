use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use capsule_core::{
    ComputationObject, ComputationRef, ContentRef, PortDef, PortId, ProtocolId, RoleId,
};
use capsule_core_codec::{ObjectResolver, ResolveError, ResolvedComputation, resolve_computation};
use thiserror::Error;

use crate::{
    CompositeResidual, CompositeResidualCodecError, Connection, Endpoint,
    MAX_COMPOSITE_RESIDUAL_BYTES, NodeId, ProtocolRolePolicy, compose_semantics_id,
    decode_composite_residual,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositeLabel {
    Tau,
    External(PortId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedComposite {
    residual: CompositeResidual,
}

impl ValidatedComposite {
    pub fn residual(&self) -> &CompositeResidual {
        &self.residual
    }

    /// Classifies the visibility of an action at a child endpoint.
    ///
    /// Connected endpoints synchronize as `tau`; exported endpoints are mapped
    /// to their parent Port. `None` means the endpoint is neither connected nor
    /// externally visible.
    pub fn classify_endpoint(&self, endpoint: &Endpoint) -> Option<CompositeLabel> {
        if self
            .residual
            .connections
            .iter()
            .any(|connection| connection.first() == endpoint || connection.second() == endpoint)
        {
            return Some(CompositeLabel::Tau);
        }
        self.residual.exports.iter().find_map(|(parent, child)| {
            (child == endpoint).then(|| CompositeLabel::External(parent.clone()))
        })
    }
}

#[derive(Debug, Error)]
pub enum CompositeValidationError {
    #[error("expected capsule.compose@1, got {actual}")]
    WrongSemantics { actual: String },
    #[error("compose residual must use a blake3 ContentRef, got {0}")]
    UnsupportedResidualReference(ContentRef),
    #[error("compose residual resolution failed: {0}")]
    ResidualResolution(#[source] ResolveError),
    #[error("compose residual is {actual} bytes; maximum is {maximum}")]
    ResidualTooLarge { actual: u64, maximum: u64 },
    #[error("compose residual metadata reported {expected} bytes but resolver returned {actual}")]
    ResidualSizeMismatch { expected: u64, actual: u64 },
    #[error("compose residual identity mismatch: expected {expected}, got {actual}")]
    ResidualIdentityMismatch {
        expected: ContentRef,
        actual: ContentRef,
    },
    #[error(transparent)]
    ResidualCodec(#[from] CompositeResidualCodecError),
    #[error("child node {node} failed to resolve: {source}")]
    ChildResolution {
        node: NodeId,
        #[source]
        source: ResolveError,
    },
    #[error("recursive computation reference cycle includes {0}")]
    ReferenceCycle(ComputationRef),
    #[error("parent boundary ports and residual exports differ")]
    BoundaryExportMismatch,
    #[error("endpoint {endpoint:?} names missing node {node}")]
    MissingNode { endpoint: Endpoint, node: NodeId },
    #[error("endpoint {endpoint:?} names missing child port {port}")]
    MissingPort { endpoint: Endpoint, port: PortId },
    #[error("endpoint {endpoint:?} is used more than once")]
    DuplicateEndpoint { endpoint: Endpoint },
    #[error("connection is duplicated: {0:?}")]
    DuplicateConnection(Connection),
    #[error("connection protocols differ: {first} and {second}")]
    ConnectionProtocolMismatch {
        first: ProtocolId,
        second: ProtocolId,
    },
    #[error("protocol {protocol} rejects connection roles {first} and {second}")]
    IncompatibleConnectionRoles {
        protocol: ProtocolId,
        first: RoleId,
        second: RoleId,
    },
    #[error("export {port} protocol differs: parent {parent}, child {child}")]
    ExportProtocolMismatch {
        port: PortId,
        parent: ProtocolId,
        child: ProtocolId,
    },
    #[error("protocol {protocol} rejects export {port} roles parent={parent}, child={child}")]
    IncompatibleExportRoles {
        port: PortId,
        protocol: ProtocolId,
        parent: RoleId,
        child: RoleId,
    },
}

pub fn validate_composite(
    parent: &ResolvedComputation,
    resolver: &dyn ObjectResolver,
    roles: &dyn ProtocolRolePolicy,
) -> Result<ValidatedComposite, CompositeValidationError> {
    let mut state = ValidationState {
        resolver,
        roles,
        active: BTreeSet::new(),
        validated: BTreeSet::new(),
    };
    state.active.insert(parent.reference().clone());
    let residual = state.validate_object(parent.object())?;
    state.active.remove(parent.reference());
    Ok(ValidatedComposite { residual })
}

struct ValidationState<'a> {
    resolver: &'a dyn ObjectResolver,
    roles: &'a dyn ProtocolRolePolicy,
    active: BTreeSet<ComputationRef>,
    validated: BTreeSet<ComputationRef>,
}

impl ValidationState<'_> {
    fn validate_object(
        &mut self,
        object: &ComputationObject,
    ) -> Result<CompositeResidual, CompositeValidationError> {
        if object.semantics != compose_semantics_id() {
            return Err(CompositeValidationError::WrongSemantics {
                actual: object.semantics.to_string(),
            });
        }
        let bytes = resolve_residual(self.resolver, &object.residual)?;
        let residual = decode_composite_residual(&bytes)?;
        let children = self.resolve_children(&residual)?;

        validate_local(object, &residual, &children, self.roles)?;

        for child in children.values() {
            if child.object().semantics == compose_semantics_id() {
                let reference = child.reference().clone();
                if self.active.contains(&reference) {
                    return Err(CompositeValidationError::ReferenceCycle(reference));
                }
                if self.validated.insert(reference.clone()) {
                    self.active.insert(reference.clone());
                    let result = self.validate_object(child.object());
                    self.active.remove(&reference);
                    result?;
                }
            }
        }
        Ok(residual)
    }

    fn resolve_children(
        &self,
        residual: &CompositeResidual,
    ) -> Result<BTreeMap<NodeId, ResolvedComputation>, CompositeValidationError> {
        residual
            .nodes
            .iter()
            .map(|(node, reference)| {
                let child = resolve_computation(self.resolver, reference).map_err(|source| {
                    CompositeValidationError::ChildResolution {
                        node: node.clone(),
                        source,
                    }
                })?;
                Ok((node.clone(), child))
            })
            .collect()
    }
}

fn validate_local(
    parent: &ComputationObject,
    residual: &CompositeResidual,
    children: &BTreeMap<NodeId, ResolvedComputation>,
    roles: &dyn ProtocolRolePolicy,
) -> Result<(), CompositeValidationError> {
    if !parent.boundary.keys().eq(residual.exports.keys()) {
        return Err(CompositeValidationError::BoundaryExportMismatch);
    }

    let mut connections = BTreeSet::new();
    let mut used_endpoints = BTreeSet::new();
    for connection in &residual.connections {
        if !connections.insert(connection.clone()) {
            return Err(CompositeValidationError::DuplicateConnection(
                connection.clone(),
            ));
        }
        claim_endpoint(connection.first(), &mut used_endpoints)?;
        claim_endpoint(connection.second(), &mut used_endpoints)?;
        let first = endpoint_definition(connection.first(), children)?;
        let second = endpoint_definition(connection.second(), children)?;
        if first.protocol != second.protocol {
            return Err(CompositeValidationError::ConnectionProtocolMismatch {
                first: first.protocol.clone(),
                second: second.protocol.clone(),
            });
        }
        if !roles.connection_roles_compatible(&first.protocol, &first.role, &second.role) {
            return Err(CompositeValidationError::IncompatibleConnectionRoles {
                protocol: first.protocol.clone(),
                first: first.role.clone(),
                second: second.role.clone(),
            });
        }
    }

    for (parent_port, endpoint) in &residual.exports {
        claim_endpoint(endpoint, &mut used_endpoints)?;
        let child = endpoint_definition(endpoint, children)?;
        let parent_definition = &parent.boundary[parent_port];
        validate_export(parent_port, parent_definition, child, roles)?;
    }
    Ok(())
}

fn endpoint_definition<'a>(
    endpoint: &Endpoint,
    children: &'a BTreeMap<NodeId, ResolvedComputation>,
) -> Result<&'a PortDef, CompositeValidationError> {
    let child =
        children
            .get(&endpoint.node)
            .ok_or_else(|| CompositeValidationError::MissingNode {
                endpoint: endpoint.clone(),
                node: endpoint.node.clone(),
            })?;
    child.object().boundary.get(&endpoint.port).ok_or_else(|| {
        CompositeValidationError::MissingPort {
            endpoint: endpoint.clone(),
            port: endpoint.port.clone(),
        }
    })
}

fn claim_endpoint(
    endpoint: &Endpoint,
    used: &mut BTreeSet<Endpoint>,
) -> Result<(), CompositeValidationError> {
    if !used.insert(endpoint.clone()) {
        return Err(CompositeValidationError::DuplicateEndpoint {
            endpoint: endpoint.clone(),
        });
    }
    Ok(())
}

fn validate_export(
    port: &PortId,
    parent: &PortDef,
    child: &PortDef,
    roles: &dyn ProtocolRolePolicy,
) -> Result<(), CompositeValidationError> {
    if parent.protocol != child.protocol {
        return Err(CompositeValidationError::ExportProtocolMismatch {
            port: port.clone(),
            parent: parent.protocol.clone(),
            child: child.protocol.clone(),
        });
    }
    if !roles.export_role_compatible(&parent.protocol, &parent.role, &child.role) {
        return Err(CompositeValidationError::IncompatibleExportRoles {
            port: port.clone(),
            protocol: parent.protocol.clone(),
            parent: parent.role.clone(),
            child: child.role.clone(),
        });
    }
    Ok(())
}

fn resolve_residual(
    resolver: &dyn ObjectResolver,
    reference: &ContentRef,
) -> Result<Vec<u8>, CompositeValidationError> {
    if reference.algorithm() != "blake3" {
        return Err(CompositeValidationError::UnsupportedResidualReference(
            reference.clone(),
        ));
    }
    let metadata = resolver
        .metadata(reference)
        .map_err(CompositeValidationError::ResidualResolution)?;
    if metadata.size > MAX_COMPOSITE_RESIDUAL_BYTES {
        return Err(CompositeValidationError::ResidualTooLarge {
            actual: metadata.size,
            maximum: MAX_COMPOSITE_RESIDUAL_BYTES,
        });
    }
    let mut bytes = Vec::new();
    resolver
        .open(reference)
        .map_err(CompositeValidationError::ResidualResolution)?
        .take(MAX_COMPOSITE_RESIDUAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| CompositeValidationError::ResidualResolution(error.into()))?;
    let actual_size = bytes.len() as u64;
    if actual_size != metadata.size {
        return Err(CompositeValidationError::ResidualSizeMismatch {
            expected: metadata.size,
            actual: actual_size,
        });
    }
    let actual = ContentRef::parse(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
        .expect("BLAKE3 creates a valid ContentRef");
    if &actual != reference {
        return Err(CompositeValidationError::ResidualIdentityMismatch {
            expected: reference.clone(),
            actual,
        });
    }
    Ok(bytes)
}

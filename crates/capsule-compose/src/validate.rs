use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::Arc;

use capsule_core::{
    ComputationObject, ComputationRef, ContentRef, PortDef, PortId, ProtocolId, RoleId,
};
use capsule_core_codec::{
    CodecError, MAX_COMPUTATION_OBJECT_BYTES, ObjectResolver, ResolveError, ResolvedComputation,
    encode_computation_object,
};
use thiserror::Error;

use crate::{
    CompositeResidual, CompositeResidualCodecError, Connection, Endpoint,
    MAX_COMPOSITE_RESIDUAL_BYTES, NodeId, ProtocolRolePolicy, compose_semantics_id,
    decode_composite_residual,
};

pub const DEFAULT_MAX_VALIDATION_DEPTH: usize = 64;
pub const DEFAULT_MAX_UNIQUE_COMPUTATIONS: usize = 4_096;
pub const DEFAULT_MAX_RESOLVED_BYTES: u64 = 64 * 1024 * 1024;

/// Resource limits for transitive compose validation.
///
/// Depth counts the root as depth zero. Unique computations include the root
/// and every leaf or compose child. Resolved bytes include canonical
/// Computation Object bytes and each distinct compose residual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationBudget {
    pub max_depth: usize,
    pub max_unique_computations: usize,
    pub max_resolved_bytes: u64,
}

impl Default for ValidationBudget {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_VALIDATION_DEPTH,
            max_unique_computations: DEFAULT_MAX_UNIQUE_COMPUTATIONS,
            max_resolved_bytes: DEFAULT_MAX_RESOLVED_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationResource {
    Depth,
    UniqueComputations,
    ResolvedBytes,
}

impl std::fmt::Display for ValidationResource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Depth => "compose depth",
            Self::UniqueComputations => "unique computations",
            Self::ResolvedBytes => "resolved bytes",
        })
    }
}

/// A valid or invalid answer could not be produced within the caller's budget.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("validation resource limit exceeded for {resource}: attempted {attempted}, limit {limit}")]
pub struct ValidationResourceLimitExceeded {
    pub resource: ValidationResource,
    pub attempted: u64,
    pub limit: u64,
}

/// Structural visibility at the parent boundary.
///
/// `Internal` does not claim that a small-step synchronization occurred. A
/// future evaluator must observe complementary child actions before producing
/// a semantic `tau` transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryVisibility {
    Internal,
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

    pub fn connection_visibility(&self, connection: &Connection) -> Option<BoundaryVisibility> {
        self.residual
            .connections
            .contains(connection)
            .then_some(BoundaryVisibility::Internal)
    }

    pub fn export_visibility(&self, endpoint: &Endpoint) -> Option<BoundaryVisibility> {
        self.residual.exports.iter().find_map(|(parent, child)| {
            (child == endpoint).then(|| BoundaryVisibility::External(parent.clone()))
        })
    }
}

#[derive(Debug, Error)]
pub enum CompositeValidationError {
    #[error(transparent)]
    ResourceLimitExceeded(#[from] ValidationResourceLimitExceeded),
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
    #[error("root computation encoding failed: {0}")]
    RootEncoding(#[source] CodecError),
    #[error("iterative validation completed without the root residual")]
    RootResidualUnavailable,
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
    #[error("linear endpoint {endpoint:?} is bound more than once")]
    EndpointBoundMoreThanOnce { endpoint: Endpoint },
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
    validate_composite_with_budget(parent, resolver, roles, ValidationBudget::default())
}

pub fn validate_composite_with_budget(
    parent: &ResolvedComputation,
    resolver: &dyn ObjectResolver,
    roles: &dyn ProtocolRolePolicy,
    budget: ValidationBudget,
) -> Result<ValidatedComposite, CompositeValidationError> {
    let parent_bytes = encode_computation_object(parent.object())
        .map_err(CompositeValidationError::RootEncoding)?
        .len() as u64;
    let mut state = ValidationState::new(resolver, roles, budget);
    state.reserve_unique_computation()?;
    state.reserve_bytes(parent_bytes)?;
    state
        .computations
        .insert(parent.reference().clone(), parent.clone());
    let residual = state.validate_iteratively(parent.reference())?;
    Ok(ValidatedComposite { residual })
}

enum Visit {
    Enter {
        reference: ComputationRef,
        depth: usize,
    },
    Exit(ComputationRef),
}

struct ValidationState<'a> {
    resolver: &'a dyn ObjectResolver,
    roles: &'a dyn ProtocolRolePolicy,
    budget: ValidationBudget,
    resolved_bytes: u64,
    computations: BTreeMap<ComputationRef, ResolvedComputation>,
    residuals: BTreeMap<ContentRef, Arc<CompositeResidual>>,
    active: BTreeSet<ComputationRef>,
    validated: BTreeSet<ComputationRef>,
}

impl<'a> ValidationState<'a> {
    fn new(
        resolver: &'a dyn ObjectResolver,
        roles: &'a dyn ProtocolRolePolicy,
        budget: ValidationBudget,
    ) -> Self {
        Self {
            resolver,
            roles,
            budget,
            resolved_bytes: 0,
            computations: BTreeMap::new(),
            residuals: BTreeMap::new(),
            active: BTreeSet::new(),
            validated: BTreeSet::new(),
        }
    }

    fn validate_iteratively(
        &mut self,
        root: &ComputationRef,
    ) -> Result<CompositeResidual, CompositeValidationError> {
        let mut root_residual = None;
        let mut visits = vec![Visit::Enter {
            reference: root.clone(),
            depth: 0,
        }];

        while let Some(visit) = visits.pop() {
            match visit {
                Visit::Exit(reference) => {
                    self.active.remove(&reference);
                    self.validated.insert(reference);
                }
                Visit::Enter { reference, depth } => {
                    self.check_depth(depth)?;
                    if self.validated.contains(&reference) {
                        continue;
                    }
                    if !self.active.insert(reference.clone()) {
                        return Err(CompositeValidationError::ReferenceCycle(reference));
                    }

                    let object = self.computations[&reference].object().clone();
                    if object.semantics != compose_semantics_id() {
                        if reference != *root {
                            self.active.remove(&reference);
                            self.validated.insert(reference);
                            continue;
                        }
                        return Err(CompositeValidationError::WrongSemantics {
                            actual: object.semantics.to_string(),
                        });
                    }
                    let residual = self.resolve_residual_cached(&object.residual)?;
                    self.resolve_children(&residual)?;
                    validate_local(&object, &residual, &self.computations, self.roles)?;

                    if reference == *root {
                        root_residual = Some((*residual).clone());
                    }
                    visits.push(Visit::Exit(reference));
                    for child in residual.nodes.values().rev() {
                        visits.push(Visit::Enter {
                            reference: child.clone(),
                            depth: depth.saturating_add(1),
                        });
                    }
                }
            }
        }

        root_residual.ok_or(CompositeValidationError::RootResidualUnavailable)
    }

    fn resolve_children(
        &mut self,
        residual: &CompositeResidual,
    ) -> Result<(), CompositeValidationError> {
        for (node, reference) in &residual.nodes {
            if self.computations.contains_key(reference) {
                continue;
            }
            self.reserve_unique_computation()?;
            let metadata = self
                .resolver
                .metadata(reference.content_ref())
                .map_err(|source| CompositeValidationError::ChildResolution {
                    node: node.clone(),
                    source,
                })?;
            if metadata.size > MAX_COMPUTATION_OBJECT_BYTES {
                return Err(CompositeValidationError::ChildResolution {
                    node: node.clone(),
                    source: ResolveError::ObjectTooLarge {
                        actual: metadata.size,
                        maximum: MAX_COMPUTATION_OBJECT_BYTES,
                    },
                });
            }
            self.reserve_bytes(metadata.size)?;
            let child = resolve_verified_computation(self.resolver, reference, metadata.size)
                .map_err(|source| CompositeValidationError::ChildResolution {
                    node: node.clone(),
                    source,
                })?;
            self.computations.insert(reference.clone(), child);
        }
        Ok(())
    }

    fn resolve_residual_cached(
        &mut self,
        reference: &ContentRef,
    ) -> Result<Arc<CompositeResidual>, CompositeValidationError> {
        if let Some(residual) = self.residuals.get(reference) {
            return Ok(Arc::clone(residual));
        }
        let size = residual_size(self.resolver, reference)?;
        self.reserve_bytes(size)?;
        let bytes = resolve_residual_bytes(self.resolver, reference, size)?;
        let residual = Arc::new(decode_composite_residual(&bytes)?);
        self.residuals
            .insert(reference.clone(), Arc::clone(&residual));
        Ok(residual)
    }

    fn check_depth(&self, depth: usize) -> Result<(), ValidationResourceLimitExceeded> {
        if depth > self.budget.max_depth {
            return Err(ValidationResourceLimitExceeded {
                resource: ValidationResource::Depth,
                attempted: depth as u64,
                limit: self.budget.max_depth as u64,
            });
        }
        Ok(())
    }

    fn reserve_unique_computation(&self) -> Result<(), ValidationResourceLimitExceeded> {
        let attempted = self.computations.len().saturating_add(1);
        if attempted > self.budget.max_unique_computations {
            return Err(ValidationResourceLimitExceeded {
                resource: ValidationResource::UniqueComputations,
                attempted: attempted as u64,
                limit: self.budget.max_unique_computations as u64,
            });
        }
        Ok(())
    }

    fn reserve_bytes(&mut self, additional: u64) -> Result<(), ValidationResourceLimitExceeded> {
        let attempted = self.resolved_bytes.saturating_add(additional);
        if attempted > self.budget.max_resolved_bytes {
            return Err(ValidationResourceLimitExceeded {
                resource: ValidationResource::ResolvedBytes,
                attempted,
                limit: self.budget.max_resolved_bytes,
            });
        }
        self.resolved_bytes = attempted;
        Ok(())
    }
}

fn validate_local(
    parent: &ComputationObject,
    residual: &CompositeResidual,
    computations: &BTreeMap<ComputationRef, ResolvedComputation>,
    roles: &dyn ProtocolRolePolicy,
) -> Result<(), CompositeValidationError> {
    if !parent.boundary.keys().eq(residual.exports.keys()) {
        return Err(CompositeValidationError::BoundaryExportMismatch);
    }

    let mut connections = BTreeSet::new();
    let mut bound_endpoints = BTreeSet::new();
    for connection in &residual.connections {
        if !connections.insert(connection.clone()) {
            return Err(CompositeValidationError::DuplicateConnection(
                connection.clone(),
            ));
        }
        bind_linear_endpoint(connection.first(), &mut bound_endpoints)?;
        bind_linear_endpoint(connection.second(), &mut bound_endpoints)?;
        let first = endpoint_definition(connection.first(), residual, computations)?;
        let second = endpoint_definition(connection.second(), residual, computations)?;
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
        bind_linear_endpoint(endpoint, &mut bound_endpoints)?;
        let child = endpoint_definition(endpoint, residual, computations)?;
        let parent_definition = &parent.boundary[parent_port];
        validate_export(parent_port, parent_definition, child, roles)?;
    }
    Ok(())
}

fn endpoint_definition<'a>(
    endpoint: &Endpoint,
    residual: &CompositeResidual,
    computations: &'a BTreeMap<ComputationRef, ResolvedComputation>,
) -> Result<&'a PortDef, CompositeValidationError> {
    let reference = residual.nodes.get(&endpoint.node).ok_or_else(|| {
        CompositeValidationError::MissingNode {
            endpoint: endpoint.clone(),
            node: endpoint.node.clone(),
        }
    })?;
    computations[reference]
        .object()
        .boundary
        .get(&endpoint.port)
        .ok_or_else(|| CompositeValidationError::MissingPort {
            endpoint: endpoint.clone(),
            port: endpoint.port.clone(),
        })
}

fn bind_linear_endpoint(
    endpoint: &Endpoint,
    bound: &mut BTreeSet<Endpoint>,
) -> Result<(), CompositeValidationError> {
    if !bound.insert(endpoint.clone()) {
        return Err(CompositeValidationError::EndpointBoundMoreThanOnce {
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

fn residual_size(
    resolver: &dyn ObjectResolver,
    reference: &ContentRef,
) -> Result<u64, CompositeValidationError> {
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
    Ok(metadata.size)
}

fn resolve_residual_bytes(
    resolver: &dyn ObjectResolver,
    reference: &ContentRef,
    expected_size: u64,
) -> Result<Vec<u8>, CompositeValidationError> {
    let mut bytes = Vec::new();
    resolver
        .open(reference)
        .map_err(CompositeValidationError::ResidualResolution)?
        .take(expected_size.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| CompositeValidationError::ResidualResolution(error.into()))?;
    let actual_size = bytes.len() as u64;
    if actual_size != expected_size {
        return Err(CompositeValidationError::ResidualSizeMismatch {
            expected: expected_size,
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

fn resolve_verified_computation(
    resolver: &dyn ObjectResolver,
    reference: &ComputationRef,
    expected_size: u64,
) -> Result<ResolvedComputation, ResolveError> {
    if expected_size > MAX_COMPUTATION_OBJECT_BYTES {
        return Err(ResolveError::ObjectTooLarge {
            actual: expected_size,
            maximum: MAX_COMPUTATION_OBJECT_BYTES,
        });
    }
    let mut bytes = Vec::new();
    resolver
        .open(reference.content_ref())?
        .take(expected_size.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let actual = bytes.len() as u64;
    if actual != expected_size {
        return Err(ResolveError::SizeMismatch {
            expected: expected_size,
            actual,
        });
    }
    Ok(ResolvedComputation::verify(reference.clone(), &bytes)?)
}

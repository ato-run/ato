//! Reproduce the Composition depth-budget bypass with the *default* budget
//! (DEFAULT_MAX_VALIDATION_DEPTH = 64) and through the Kernel, i.e. the
//! production path (Kernel -> ComposeSemantics -> validate_composite).
//!
//! The same DAG is accepted or rejected depending only on the NodeId names,
//! because the iterative validator checks depth before it skips an already
//! validated node and therefore never re-checks the subtree below it.
use std::collections::BTreeMap;
use std::sync::Arc;

use ato_compose::{
    CompositeResidual, ComposeSemantics, NodeId, ProtocolRolePolicy, ValidationBudget,
    decode_composite_residual, encode_composite_residual, validate_composite,
    validate_composite_with_budget,
};
use ato_computation::{ComputationObject, ComputationRef, ContentRef, SemanticsId};
use ato_computation::{ProtocolId, RoleId};
use ato_kernel::{
    Kernel, SemanticError, SemanticHost, SemanticStep, Semantics, TransitionOffer,
};
use ato_objects::{
    MemoryObjectStore, ObjectResolver, ObjectStore, read_exact_object, resolve_computation,
};

const LEAF_SEMANTICS: &str = "example.leaf@1";

struct LeafSemantics {
    id: SemanticsId,
}

impl LeafSemantics {
    fn new() -> Self {
        Self {
            id: SemanticsId::parse(LEAF_SEMANTICS).unwrap(),
        }
    }
}

impl Semantics for LeafSemantics {
    fn id(&self) -> &SemanticsId {
        &self.id
    }
    fn enabled(
        &self,
        _: &ato_computation::ResolvedComputation,
        _: &dyn SemanticHost,
    ) -> Result<Vec<TransitionOffer>, SemanticError> {
        Ok(Vec::new())
    }
    fn step(
        &self,
        _: &ato_computation::ResolvedComputation,
        _: &TransitionOffer,
        _: &dyn SemanticHost,
    ) -> Result<SemanticStep, SemanticError> {
        Err(SemanticError::new("not used"))
    }
}

struct Roles;

impl ProtocolRolePolicy for Roles {
    fn connection_roles_compatible(&self, _: &ProtocolId, _: &RoleId, _: &RoleId) -> bool {
        true
    }
    fn export_role_compatible(&self, _: &ProtocolId, _: &RoleId, _: &RoleId) -> bool {
        true
    }
}

fn composite(store: &MemoryObjectStore, children: &[(&str, &ComputationRef)]) -> ComputationRef {
    let nodes = children
        .iter()
        .map(|(id, child)| (NodeId::parse(*id).unwrap(), (*child).clone()))
        .collect();
    let residual = CompositeResidual {
        nodes,
        connections: vec![],
        exports: BTreeMap::new(),
    };
    let content = store
        .put(&encode_composite_residual(&residual).unwrap())
        .unwrap();
    let object = ComputationObject {
        semantics: SemanticsId::parse("capsule.compose@1").unwrap(),
        boundary: BTreeMap::new(),
        residual: content,
    };
    let bytes = ato_computation::encode_computation_object(&object).unwrap();
    let reference = ato_computation::computation_ref(&object).unwrap();
    store.insert(reference.content_ref(), &bytes).unwrap();
    reference
}

/// Wrap base in that many nested single-child composites.
fn chain(store: &MemoryObjectStore, base: &ComputationRef, levels: usize) -> ComputationRef {
    let mut current = base.clone();
    for _ in 0..levels {
        current = composite(store, &[("child", &current)]);
    }
    current
}

fn residual_of(store: &MemoryObjectStore, reference: &ComputationRef) -> CompositeResidual {
    let resolved = resolve_computation(store, reference).unwrap();
    let metadata = store.metadata(&resolved.object().residual).unwrap();
    let bytes = read_exact_object(
        store,
        &resolved.object().residual,
        metadata.size,
        ato_compose::MAX_COMPOSITE_RESIDUAL_BYTES,
    )
    .unwrap();
    decode_composite_residual(&bytes).unwrap()
}

/// Largest 0-based depth over all root-to-leaf paths (the validator's depth).
fn longest_path(store: &MemoryObjectStore, reference: &ComputationRef) -> usize {
    let resolved = resolve_computation(store, reference).unwrap();
    if resolved.object().semantics != SemanticsId::parse("capsule.compose@1").unwrap() {
        return 0;
    }
    let residual = residual_of(store, reference);
    if residual.nodes.is_empty() {
        return 0;
    }
    1 + residual
        .nodes
        .values()
        .map(|child| longest_path(store, child))
        .max()
        .unwrap()
}

fn main() {
    let store = Arc::new(MemoryObjectStore::default());

    // leaf: one non-composite computation
    let leaf_residual: ContentRef = store.put(b"leaf").unwrap();
    let leaf_object = ComputationObject {
        semantics: SemanticsId::parse(LEAF_SEMANTICS).unwrap(),
        boundary: BTreeMap::new(),
        residual: leaf_residual,
    };
    let leaf_bytes = ato_computation::encode_computation_object(&leaf_object).unwrap();
    let leaf = ato_computation::computation_ref(&leaf_object).unwrap();
    store.insert(leaf.content_ref(), &leaf_bytes).unwrap();

    // shared: 63 nested composites over the leaf. Entered at depth 1 it puts the
    // leaf exactly at depth 64 = the default budget.
    let shared = chain(store.as_ref(), &leaf, 63);
    // longer: one more composite so that longer -> shared -> ... -> leaf is 65.
    let longer = composite(store.as_ref(), &[("shared", &shared)]);

    // The same DAG, only the NodeId names a/b differ.
    let shallow_first = composite(store.as_ref(), &[("a", &shared), ("b", &longer)]);
    let deep_first = composite(store.as_ref(), &[("a", &longer), ("b", &shared)]);

    let shallow = resolve_computation(store.as_ref(), &shallow_first).unwrap();
    let deep = resolve_computation(store.as_ref(), &deep_first).unwrap();

    assert_eq!(ValidationBudget::default().max_depth, 64);
    assert_eq!(longest_path(store.as_ref(), &shallow_first), 65);
    assert_eq!(longest_path(store.as_ref(), &deep_first), 65);

    // Direct validator, default budget (= what ComposeSemantics uses).
    assert!(validate_composite(&shallow, store.as_ref(), &Roles).is_ok());
    let deep_error = validate_composite(&deep, store.as_ref(), &Roles).unwrap_err();
    println!("default budget: shallow-first accepted, deep-first rejected: {deep_error}");

    // Explicit budgets make the asymmetry visible in both directions.
    let tiny = ValidationBudget {
        max_depth: 2,
        ..ValidationBudget::default()
    };
    assert!(validate_composite_with_budget(&shallow, store.as_ref(), &Roles, tiny).is_err());
    assert!(validate_composite_with_budget(&deep, store.as_ref(), &Roles, tiny).is_err());

    // Production path: Kernel with the registered ComposeSemantics.
    let mut kernel = Kernel::new(store.clone());
    kernel.register(Arc::new(ComposeSemantics::new())).unwrap();
    kernel.register(Arc::new(LeafSemantics::new())).unwrap();
    assert!(kernel.enabled(&shallow_first).is_ok());
    let kernel_error = kernel.enabled(&deep_first).unwrap_err();
    println!("Kernel::enabled: shallow-first accepted, deep-first rejected: {kernel_error}");

    println!("a path of 65 nodes passes the default depth budget because of NodeId ordering");
}

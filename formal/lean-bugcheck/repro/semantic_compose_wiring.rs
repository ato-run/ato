//! Reproduce the wiring-gate gap between the authoring/sealing path and
//! `capsule.compose@1` validation, and the duplicate-connection identity split.
//!
//! Build: rustc --edition 2024 <file> \
//!   --extern ato_computation=target/debug/libato_computation.rlib \
//!   --extern ato_compose=target/debug/libato_compose.rlib \
//!   --extern ato_objects=target/debug/libato_objects.rlib \
//!   -L dependency=target/debug/deps -o <out>
use ato_compose::{
    CompositeResidual, CompositeValidationError, Connection, Endpoint, NodeId,
    ProtocolRolePolicy, composite_residual_ref, encode_composite_residual, validate_composite,
};
use ato_computation::{
    ComputationObject, ComputationRef, PortDef, PortId, ProtocolId, RoleId, SemanticsId,
    computation_ref, encode_computation_object,
};
use ato_objects::{MemoryObjectStore, ObjectStore, resolve_computation};
use std::collections::BTreeMap;
use std::sync::Arc;

struct Roles;
impl ProtocolRolePolicy for Roles {
    fn connection_roles_compatible(&self, _: &ProtocolId, _: &RoleId, _: &RoleId) -> bool {
        true
    }
    fn export_role_compatible(&self, _: &ProtocolId, _: &RoleId, _: &RoleId) -> bool {
        true
    }
}

fn port(id: &str) -> PortId {
    PortId::parse(id).unwrap()
}

fn def(role: &str) -> PortDef {
    PortDef {
        protocol: ProtocolId::parse("fixture.text@1").unwrap(),
        role: RoleId::parse(role).unwrap(),
    }
}

/// A leaf computation that declares exactly one Port.
fn leaf(store: &MemoryObjectStore, port_id: &str, role: &str) -> ComputationRef {
    let residual = store.put(b"leaf residual").unwrap();
    let mut boundary = BTreeMap::new();
    boundary.insert(port(port_id), def(role));
    let object = ComputationObject {
        semantics: SemanticsId::parse("fixture.leaf@1").unwrap(),
        boundary,
        residual,
    };
    let reference = computation_ref(&object).unwrap();
    store
        .insert(reference.content_ref(), &encode_computation_object(&object).unwrap())
        .unwrap();
    reference
}

fn endpoint(node: &str, port_id: &str) -> Endpoint {
    Endpoint {
        node: NodeId::parse(node).unwrap(),
        port: port(port_id),
    }
}

/// Seals a `capsule.compose@1` object exactly like `authoring::seal_composite`
/// (encode the residual, address it, insert it) with no validation step.
fn seal(store: &MemoryObjectStore, residual: CompositeResidual) -> ComputationRef {
    let content = store.put(&encode_composite_residual(&residual).unwrap()).unwrap();
    let object = ComputationObject {
        semantics: SemanticsId::parse("capsule.compose@1").unwrap(),
        boundary: BTreeMap::new(),
        residual: content,
    };
    let reference = computation_ref(&object).unwrap();
    store
        .insert(reference.content_ref(), &encode_computation_object(&object).unwrap())
        .unwrap();
    reference
}

fn nodes(entries: &[(&str, &ComputationRef)]) -> BTreeMap<NodeId, ComputationRef> {
    entries
        .iter()
        .map(|(id, reference)| (NodeId::parse(*id).unwrap(), (*reference).clone()))
        .collect()
}

fn main() {
    let store = Arc::new(MemoryObjectStore::default());
    let client = leaf(&store, "a.out", "client");
    let server_b = leaf(&store, "b.in", "server");
    let server_c = leaf(&store, "c.in", "server");
    let same_node_left = leaf(&store, "x.left", "client");
    let _same_node_right = leaf(&store, "x.right", "server");

    let out = endpoint("a", "a.out");
    let into_b = endpoint("b", "b.in");
    let into_c = endpoint("c", "c.in");

    // `Connection::new` itself behaves: endpoint order is normalised and an
    // endpoint cannot be connected to itself.
    let forward = Connection::new(out.clone(), into_b.clone()).unwrap();
    let backward = Connection::new(into_b.clone(), out.clone()).unwrap();
    assert_eq!(forward, backward);
    assert!(Connection::new(out.clone(), out.clone()).is_err());
    // But two different Ports of one Node are accepted here and rejected only
    // by `validate_local` (SameNodeConnectionUnsupported).
    let same_node = Connection::new(
        endpoint("x", "x.left"),
        endpoint("x", "x.right"),
    )
    .expect("connection constructor accepts two ports of one node");

    let valid_wiring = CompositeResidual {
        nodes: nodes(&[
            ("a", &client),
            ("b", &server_b),
            ("c", &server_c),
            ("x", &same_node_left),
        ]),
        connections: vec![forward.clone()],
        exports: BTreeMap::new(),
    };
    let valid = seal(&store, valid_wiring.clone());
    validate_composite(&resolve_computation(store.as_ref(), &valid).unwrap(), store.as_ref(), &Roles)
        .expect("the linear single connection is valid");
    println!("valid single connection: sealed and validated");

    // W1: the same connection declared twice. The canonical encoder sorts but
    // never deduplicates, and the seam accepts the object; only validation
    // rejects it.
    let duplicated = CompositeResidual {
        connections: vec![forward.clone(), forward.clone()],
        ..valid_wiring.clone()
    };
    let bytes = encode_composite_residual(&duplicated).expect("encoder accepts duplicates");
    let sealed_dup = seal(&store, duplicated.clone());
    assert_ne!(
        composite_residual_ref(&valid_wiring).unwrap(),
        composite_residual_ref(&duplicated).unwrap(),
        "one edge and the same edge twice are two different identities"
    );
    assert_ne!(valid, sealed_dup);
    match validate_composite(
        &resolve_computation(store.as_ref(), &sealed_dup).unwrap(),
        store.as_ref(),
        &Roles,
    ) {
        Err(CompositeValidationError::DuplicateConnection(_)) => {}
        other => panic!("expected DuplicateConnection, got {other:?}"),
    }
    println!(
        "duplicate connection: encoded ({} bytes) and sealed, validation reports DuplicateConnection",
        bytes.len()
    );

    // W2: two different Ports of one Node.
    let same_node_wiring = CompositeResidual {
        nodes: nodes(&[("x", &same_node_left)]),
        connections: vec![same_node],
        exports: BTreeMap::new(),
    };
    let sealed_same_node = seal(&store, same_node_wiring);
    match validate_composite(
        &resolve_computation(store.as_ref(), &sealed_same_node).unwrap(),
        store.as_ref(),
        &Roles,
    ) {
        Err(CompositeValidationError::SameNodeConnectionUnsupported { .. }) => {}
        other => panic!("expected SameNodeConnectionUnsupported, got {other:?}"),
    }
    println!("same-node connection: sealed, validation reports SameNodeConnectionUnsupported");

    // W3: one client Port bound to two peers (linear endpoint use).
    let multi_bound = CompositeResidual {
        connections: vec![
            forward.clone(),
            Connection::new(out.clone(), into_c.clone()).unwrap(),
        ],
        ..valid_wiring.clone()
    };
    let sealed_multi = seal(&store, multi_bound);
    match validate_composite(
        &resolve_computation(store.as_ref(), &sealed_multi).unwrap(),
        store.as_ref(),
        &Roles,
    ) {
        Err(CompositeValidationError::EndpointBoundMoreThanOnce { endpoint }) => {
            println!("multi-bound endpoint: sealed, validation rejects {endpoint:?}");
        }
        other => panic!("expected EndpointBoundMoreThanOnce, got {other:?}"),
    }

    // Every one of the three composites is a usable, addressable sealed
    // computation: the seal gate is weaker than the semantics that owns the
    // object.
    for reference in [&valid, &sealed_dup, &sealed_same_node, &sealed_multi] {
        assert!(resolve_computation(store.as_ref(), reference).is_ok());
    }
    println!("seal accepts wiring that capsule.compose@1 validation rejects");
}

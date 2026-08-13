use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use capsule_compose::{
    COMPOSE_SEMANTICS_ID, CompositeLabel, CompositeResidual, CompositeValidationError, Connection,
    Endpoint, NodeId, ProtocolRolePolicy, composite_residual_ref, encode_composite_residual,
    validate_composite,
};
use capsule_core::{
    Boundary, ComputationObject, ComputationRef, ContentRef, PortDef, PortId, ProtocolId, RoleId,
    SemanticsId,
};
use capsule_core_codec::{
    ObjectMetadata, ObjectResolver, ResolveError, ResolvedComputation, computation_ref,
    encode_computation_object, resolve_computation,
};

#[derive(Default)]
struct MemoryObjects {
    bytes: BTreeMap<ContentRef, Vec<u8>>,
}

impl MemoryObjects {
    fn insert_content(&mut self, bytes: Vec<u8>) -> ContentRef {
        let reference = ContentRef::parse(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
            .expect("test content reference");
        self.bytes.insert(reference.clone(), bytes);
        reference
    }

    fn insert_computation(&mut self, object: &ComputationObject) -> ComputationRef {
        let bytes = encode_computation_object(object).expect("encode test computation");
        let reference = computation_ref(object).expect("reference test computation");
        self.bytes.insert(reference.content_ref().clone(), bytes);
        reference
    }

    fn resolve(&self, reference: &ComputationRef) -> ResolvedComputation {
        resolve_computation(self, reference).expect("resolve test computation")
    }
}

impl ObjectResolver for MemoryObjects {
    fn metadata(&self, reference: &ContentRef) -> Result<ObjectMetadata, ResolveError> {
        let bytes = self
            .bytes
            .get(reference)
            .ok_or_else(|| ResolveError::Storage(format!("missing {reference}")))?;
        Ok(ObjectMetadata {
            size: bytes.len() as u64,
        })
    }

    fn open(&self, reference: &ContentRef) -> Result<Box<dyn Read + Send + '_>, ResolveError> {
        let bytes = self
            .bytes
            .get(reference)
            .ok_or_else(|| ResolveError::Storage(format!("missing {reference}")))?;
        Ok(Box::new(Cursor::new(bytes.as_slice())))
    }
}

struct TextProtocol;

impl ProtocolRolePolicy for TextProtocol {
    fn connection_roles_compatible(
        &self,
        protocol: &ProtocolId,
        first: &RoleId,
        second: &RoleId,
    ) -> bool {
        protocol.as_str() == "example.text@1"
            && matches!(
                (first.as_str(), second.as_str()),
                ("sender", "receiver") | ("receiver", "sender")
            )
    }

    fn export_role_compatible(
        &self,
        protocol: &ProtocolId,
        parent: &RoleId,
        child: &RoleId,
    ) -> bool {
        protocol.as_str() == "example.text@1" && parent == child
    }
}

fn text_port(role: &str) -> PortDef {
    PortDef {
        protocol: ProtocolId::parse("example.text@1").unwrap(),
        role: RoleId::parse(role).unwrap(),
    }
}

fn endpoint(node: &str, port: &str) -> Endpoint {
    Endpoint {
        node: NodeId::parse(node).unwrap(),
        port: PortId::parse(port).unwrap(),
    }
}

fn leaf(semantics: &str, boundary: Boundary, marker: u8) -> ComputationObject {
    ComputationObject {
        semantics: SemanticsId::parse(semantics).unwrap(),
        boundary,
        residual: ContentRef::parse(format!("blake3:{}", format!("{marker:02x}").repeat(32)))
            .unwrap(),
    }
}

fn seal_composite(
    objects: &mut MemoryObjects,
    boundary: Boundary,
    residual: CompositeResidual,
) -> ComputationRef {
    let residual_bytes = encode_composite_residual(&residual).unwrap();
    let residual_ref = objects.insert_content(residual_bytes);
    assert_eq!(residual_ref, composite_residual_ref(&residual).unwrap());
    objects.insert_computation(&ComputationObject {
        semantics: SemanticsId::parse(COMPOSE_SEMANTICS_ID).unwrap(),
        boundary,
        residual: residual_ref,
    })
}

fn hello_world_fixture() -> (MemoryObjects, ComputationRef, Endpoint, Endpoint) {
    let mut objects = MemoryObjects::default();
    let greeter = leaf(
        "example.greeter@1",
        BTreeMap::from([
            (PortId::parse("name").unwrap(), text_port("receiver")),
            (PortId::parse("greeting").unwrap(), text_port("sender")),
        ]),
        0x11,
    );
    let provider = leaf(
        "example.name-provider@1",
        BTreeMap::from([(PortId::parse("name").unwrap(), text_port("sender"))]),
        0x22,
    );
    let greeter_ref = objects.insert_computation(&greeter);
    let provider_ref = objects.insert_computation(&provider);
    let internal_name = endpoint("greeter", "name");
    let greeting = endpoint("greeter", "greeting");
    let residual = CompositeResidual {
        nodes: BTreeMap::from([
            (NodeId::parse("greeter").unwrap(), greeter_ref),
            (NodeId::parse("name-provider").unwrap(), provider_ref),
        ]),
        connections: vec![
            Connection::new(internal_name.clone(), endpoint("name-provider", "name")).unwrap(),
        ],
        exports: BTreeMap::from([(PortId::parse("greeting").unwrap(), greeting.clone())]),
    };
    let parent_ref = seal_composite(
        &mut objects,
        BTreeMap::from([(PortId::parse("greeting").unwrap(), text_port("sender"))]),
        residual,
    );
    (objects, parent_ref, internal_name, greeting)
}

#[test]
fn hello_world_is_one_core_object_with_internal_tau_and_only_external_greeting() {
    let (objects, parent_ref, internal_name, greeting) = hello_world_fixture();
    let parent = objects.resolve(&parent_ref);

    let validated = validate_composite(&parent, &objects, &TextProtocol).unwrap();

    assert_eq!(parent.object().semantics.as_str(), COMPOSE_SEMANTICS_ID);
    assert_eq!(
        parent.object().boundary.keys().collect::<Vec<_>>(),
        vec![&PortId::parse("greeting").unwrap()]
    );
    let internal_connection = &validated.residual().connections[0];
    assert!(
        internal_connection.first() == &internal_name
            || internal_connection.second() == &internal_name
    );
    assert_eq!(
        validated.connection_label(internal_connection),
        Some(CompositeLabel::Tau)
    );
    assert_eq!(
        validated.export_label(&greeting),
        Some(CompositeLabel::External(PortId::parse("greeting").unwrap()))
    );
    assert_eq!(validated.residual().nodes.len(), 2);
}

#[test]
fn validator_rejects_parent_boundary_that_does_not_equal_exports() {
    let (mut objects, _, _, _) = hello_world_fixture();
    let residual = CompositeResidual {
        nodes: BTreeMap::new(),
        connections: vec![],
        exports: BTreeMap::new(),
    };
    let parent_ref = seal_composite(
        &mut objects,
        BTreeMap::from([(PortId::parse("unexpected").unwrap(), text_port("sender"))]),
        residual,
    );

    assert!(matches!(
        validate_composite(&objects.resolve(&parent_ref), &objects, &TextProtocol),
        Err(CompositeValidationError::BoundaryExportMismatch)
    ));
}

#[test]
fn validator_rejects_missing_endpoints_and_duplicate_use() {
    let mut objects = MemoryObjects::default();
    let child = leaf(
        "example.worker@1",
        BTreeMap::from([
            (PortId::parse("one").unwrap(), text_port("sender")),
            (PortId::parse("two").unwrap(), text_port("receiver")),
        ]),
        0x33,
    );
    let child_ref = objects.insert_computation(&child);
    let shared = endpoint("worker", "one");
    let residual = CompositeResidual {
        nodes: BTreeMap::from([(NodeId::parse("worker").unwrap(), child_ref)]),
        connections: vec![Connection::new(shared.clone(), endpoint("worker", "two")).unwrap()],
        exports: BTreeMap::from([(PortId::parse("out").unwrap(), shared.clone())]),
    };
    let parent_ref = seal_composite(
        &mut objects,
        BTreeMap::from([(PortId::parse("out").unwrap(), text_port("sender"))]),
        residual,
    );

    assert!(matches!(
        validate_composite(&objects.resolve(&parent_ref), &objects, &TextProtocol),
        Err(CompositeValidationError::DuplicateEndpoint { endpoint }) if endpoint == shared
    ));

    let missing = CompositeResidual {
        nodes: BTreeMap::new(),
        connections: vec![],
        exports: BTreeMap::from([(PortId::parse("out").unwrap(), endpoint("missing", "out"))]),
    };
    let missing_ref = seal_composite(
        &mut objects,
        BTreeMap::from([(PortId::parse("out").unwrap(), text_port("sender"))]),
        missing,
    );
    assert!(matches!(
        validate_composite(&objects.resolve(&missing_ref), &objects, &TextProtocol),
        Err(CompositeValidationError::MissingNode { .. })
    ));
}

#[test]
fn validator_rejects_protocol_and_role_incompatibility() {
    let mut objects = MemoryObjects::default();
    let sender_a = leaf(
        "example.sender-a@1",
        BTreeMap::from([(PortId::parse("value").unwrap(), text_port("sender"))]),
        0x44,
    );
    let sender_b = leaf(
        "example.sender-b@1",
        BTreeMap::from([(PortId::parse("value").unwrap(), text_port("sender"))]),
        0x55,
    );
    let a_ref = objects.insert_computation(&sender_a);
    let b_ref = objects.insert_computation(&sender_b);
    let residual = CompositeResidual {
        nodes: BTreeMap::from([
            (NodeId::parse("a").unwrap(), a_ref),
            (NodeId::parse("b").unwrap(), b_ref),
        ]),
        connections: vec![Connection::new(endpoint("a", "value"), endpoint("b", "value")).unwrap()],
        exports: BTreeMap::new(),
    };
    let parent_ref = seal_composite(&mut objects, BTreeMap::new(), residual);

    assert!(matches!(
        validate_composite(&objects.resolve(&parent_ref), &objects, &TextProtocol),
        Err(CompositeValidationError::IncompatibleConnectionRoles { .. })
    ));

    let other_protocol = PortDef {
        protocol: ProtocolId::parse("example.other@1").unwrap(),
        role: RoleId::parse("sender").unwrap(),
    };
    let child = leaf(
        "example.other-child@1",
        BTreeMap::from([(PortId::parse("value").unwrap(), other_protocol)]),
        0x66,
    );
    let child_ref = objects.insert_computation(&child);
    let residual = CompositeResidual {
        nodes: BTreeMap::from([(NodeId::parse("child").unwrap(), child_ref)]),
        connections: vec![],
        exports: BTreeMap::from([(PortId::parse("value").unwrap(), endpoint("child", "value"))]),
    };
    let parent_ref = seal_composite(
        &mut objects,
        BTreeMap::from([(PortId::parse("value").unwrap(), text_port("sender"))]),
        residual,
    );
    assert!(matches!(
        validate_composite(&objects.resolve(&parent_ref), &objects, &TextProtocol),
        Err(CompositeValidationError::ExportProtocolMismatch { .. })
    ));
}

#[test]
fn validator_allows_connection_graph_cycles() {
    let mut objects = MemoryObjects::default();
    let mut nodes = BTreeMap::new();
    for (index, node) in ["a", "b", "c"].into_iter().enumerate() {
        let child = leaf(
            &format!("example.node-{node}@1"),
            BTreeMap::from([
                (PortId::parse("left").unwrap(), text_port("sender")),
                (PortId::parse("right").unwrap(), text_port("receiver")),
            ]),
            0x70 + index as u8,
        );
        nodes.insert(
            NodeId::parse(node).unwrap(),
            objects.insert_computation(&child),
        );
    }
    let residual = CompositeResidual {
        nodes,
        connections: vec![
            Connection::new(endpoint("a", "left"), endpoint("b", "right")).unwrap(),
            Connection::new(endpoint("b", "left"), endpoint("c", "right")).unwrap(),
            Connection::new(endpoint("c", "left"), endpoint("a", "right")).unwrap(),
        ],
        exports: BTreeMap::new(),
    };
    let parent_ref = seal_composite(&mut objects, BTreeMap::new(), residual);

    validate_composite(&objects.resolve(&parent_ref), &objects, &TextProtocol).unwrap();
}

#[test]
fn validator_checks_nested_compose_objects_transitively() {
    let mut objects = MemoryObjects::default();
    let invalid_nested = seal_composite(
        &mut objects,
        BTreeMap::from([(
            PortId::parse("missing-export").unwrap(),
            text_port("sender"),
        )]),
        CompositeResidual {
            nodes: BTreeMap::new(),
            connections: vec![],
            exports: BTreeMap::new(),
        },
    );
    let root_ref = seal_composite(
        &mut objects,
        BTreeMap::new(),
        CompositeResidual {
            nodes: BTreeMap::from([(NodeId::parse("nested").unwrap(), invalid_nested)]),
            connections: vec![],
            exports: BTreeMap::new(),
        },
    );

    assert!(matches!(
        validate_composite(&objects.resolve(&root_ref), &objects, &TextProtocol),
        Err(CompositeValidationError::BoundaryExportMismatch)
    ));
}

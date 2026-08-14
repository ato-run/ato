use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::sync::Mutex;

use ato_computation::{
    Boundary, ComputationObject, ComputationRef, ContentRef, PortDef, PortId, ProtocolId,
    ResolvedComputation, RoleId, SemanticsId, computation_ref, encode_computation_object,
};
use ato_kernel::ProtocolPayload;
use ato_objects::{ObjectError, ObjectMetadata, ObjectResolver, resolve_computation};
use ato_semantics_compose::{
    BoundaryVisibility, COMPOSE_SEMANTICS_ID, CompositeResidual, CompositeStepError,
    CompositeValidationError, Connection, Endpoint, NodeId, NodeStep, ProtocolRolePolicy,
    StepLabel, ValidatedComposite, ValidationBudget, ValidationResource, composite_residual_ref,
    encode_composite_residual, lift_exported_step, lift_internal_step, synchronize_connection,
    validate_composite, validate_composite_with_budget,
};

#[derive(Default)]
struct MemoryObjects {
    bytes: BTreeMap<ContentRef, Vec<u8>>,
    metadata_reads: Mutex<BTreeMap<ContentRef, usize>>,
    opens: Mutex<BTreeMap<ContentRef, usize>>,
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

    fn open_count(&self, reference: &ContentRef) -> usize {
        *self.opens.lock().unwrap().get(reference).unwrap_or(&0)
    }

    fn metadata_count(&self, reference: &ContentRef) -> usize {
        *self
            .metadata_reads
            .lock()
            .unwrap()
            .get(reference)
            .unwrap_or(&0)
    }

    fn stored_size(&self, reference: &ContentRef) -> u64 {
        self.bytes[reference].len() as u64
    }
}

impl ObjectResolver for MemoryObjects {
    fn metadata(&self, reference: &ContentRef) -> Result<ObjectMetadata, ObjectError> {
        let bytes = self
            .bytes
            .get(reference)
            .ok_or_else(|| ObjectError::Storage(format!("missing {reference}")))?;
        *self
            .metadata_reads
            .lock()
            .unwrap()
            .entry(reference.clone())
            .or_default() += 1;
        Ok(ObjectMetadata {
            size: bytes.len() as u64,
        })
    }

    fn open(&self, reference: &ContentRef) -> Result<Box<dyn Read + Send + '_>, ObjectError> {
        let bytes = self
            .bytes
            .get(reference)
            .ok_or_else(|| ObjectError::Storage(format!("missing {reference}")))?;
        *self
            .opens
            .lock()
            .unwrap()
            .entry(reference.clone())
            .or_default() += 1;
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

fn seal_leaf(
    objects: &mut MemoryObjects,
    semantics: &str,
    boundary: Boundary,
    residual_bytes: &[u8],
) -> ComputationRef {
    let residual = objects.insert_content(residual_bytes.to_vec());
    objects.insert_computation(&ComputationObject {
        semantics: SemanticsId::parse(semantics).unwrap(),
        boundary,
        residual,
    })
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
fn hello_world_structurally_hides_name_connection_and_exports_only_greeting() {
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
        validated.connection_visibility(internal_connection),
        Some(BoundaryVisibility::Internal)
    );
    assert_eq!(
        validated.export_visibility(&greeting),
        Some(BoundaryVisibility::External(
            PortId::parse("greeting").unwrap()
        ))
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
fn validator_enforces_linear_single_binding_and_rejects_missing_endpoints() {
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
    let peer_ref = objects.insert_computation(&leaf(
        "example.peer@1",
        BTreeMap::from([(PortId::parse("two").unwrap(), text_port("receiver"))]),
        0x34,
    ));
    let shared = endpoint("worker", "one");
    let residual = CompositeResidual {
        nodes: BTreeMap::from([
            (NodeId::parse("worker").unwrap(), child_ref),
            (NodeId::parse("peer").unwrap(), peer_ref),
        ]),
        connections: vec![Connection::new(shared.clone(), endpoint("peer", "two")).unwrap()],
        exports: BTreeMap::from([(PortId::parse("out").unwrap(), shared.clone())]),
    };
    let parent_ref = seal_composite(
        &mut objects,
        BTreeMap::from([(PortId::parse("out").unwrap(), text_port("sender"))]),
        residual,
    );

    assert!(matches!(
        validate_composite(&objects.resolve(&parent_ref), &objects, &TextProtocol),
        Err(CompositeValidationError::EndpointBoundMoreThanOnce { endpoint }) if endpoint == shared
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

fn nested_compose_chain(objects: &mut MemoryObjects, depth: usize) -> ComputationRef {
    let mut child = seal_composite(
        objects,
        BTreeMap::new(),
        CompositeResidual {
            nodes: BTreeMap::new(),
            connections: vec![],
            exports: BTreeMap::new(),
        },
    );
    for _ in 0..depth {
        child = seal_composite(
            objects,
            BTreeMap::new(),
            CompositeResidual {
                nodes: BTreeMap::from([(NodeId::parse("child").unwrap(), child)]),
                connections: vec![],
                exports: BTreeMap::new(),
            },
        );
    }
    child
}

#[test]
fn iterative_validator_handles_deep_closure_within_explicit_budget() {
    let mut objects = MemoryObjects::default();
    let root_ref = nested_compose_chain(&mut objects, 512);
    let root = objects.resolve(&root_ref);
    let budget = ValidationBudget {
        max_depth: 512,
        max_unique_computations: 513,
        max_resolved_bytes: 64 * 1024 * 1024,
    };

    validate_composite_with_budget(&root, &objects, &TextProtocol, budget).unwrap();
}

#[test]
fn depth_limit_is_reported_as_resource_exhaustion() {
    let mut objects = MemoryObjects::default();
    let root_ref = nested_compose_chain(&mut objects, 3);
    let root = objects.resolve(&root_ref);
    let budget = ValidationBudget {
        max_depth: 2,
        ..ValidationBudget::default()
    };

    assert!(matches!(
        validate_composite_with_budget(&root, &objects, &TextProtocol, budget),
        Err(CompositeValidationError::ResourceLimitExceeded(limit))
            if limit.resource == ValidationResource::Depth
    ));
}

#[test]
fn unique_computation_limit_is_reported_as_resource_exhaustion() {
    let (objects, parent_ref, _, _) = hello_world_fixture();
    let parent = objects.resolve(&parent_ref);
    let budget = ValidationBudget {
        max_unique_computations: 2,
        ..ValidationBudget::default()
    };

    assert!(matches!(
        validate_composite_with_budget(&parent, &objects, &TextProtocol, budget),
        Err(CompositeValidationError::ResourceLimitExceeded(limit))
            if limit.resource == ValidationResource::UniqueComputations
    ));
}

#[test]
fn resolved_byte_limit_is_reported_before_closure_reads() {
    let mut objects = MemoryObjects::default();
    let child_ref = objects.insert_computation(&leaf("example.child@1", BTreeMap::new(), 0x92));
    let parent_ref = seal_composite(
        &mut objects,
        BTreeMap::new(),
        CompositeResidual {
            nodes: BTreeMap::from([(NodeId::parse("child").unwrap(), child_ref.clone())]),
            connections: vec![],
            exports: BTreeMap::new(),
        },
    );
    let parent = objects.resolve(&parent_ref);
    let bytes_before_child = objects.stored_size(parent_ref.content_ref())
        + objects.stored_size(&parent.object().residual);
    let budget = ValidationBudget {
        max_resolved_bytes: bytes_before_child,
        ..ValidationBudget::default()
    };

    assert!(matches!(
        validate_composite_with_budget(&parent, &objects, &TextProtocol, budget),
        Err(CompositeValidationError::ResourceLimitExceeded(limit))
            if limit.resource == ValidationResource::ResolvedBytes
    ));
    assert_eq!(objects.open_count(&parent.object().residual), 1);
    assert_eq!(objects.open_count(child_ref.content_ref()), 0);
}

#[test]
fn repeated_node_references_resolve_one_computation_once() {
    let mut objects = MemoryObjects::default();
    let child = leaf("example.shared@1", BTreeMap::new(), 0x91);
    let child_ref = objects.insert_computation(&child);
    let nodes = (0..100)
        .map(|index| {
            (
                NodeId::parse(format!("node-{index}")).unwrap(),
                child_ref.clone(),
            )
        })
        .collect();
    let root_ref = seal_composite(
        &mut objects,
        BTreeMap::new(),
        CompositeResidual {
            nodes,
            connections: vec![],
            exports: BTreeMap::new(),
        },
    );
    let root = objects.resolve(&root_ref);

    validate_composite(&root, &objects, &TextProtocol).unwrap();

    assert_eq!(objects.metadata_count(child_ref.content_ref()), 1);
    assert_eq!(objects.open_count(child_ref.content_ref()), 1);
}

struct HelloBranch {
    after_input: ComputationRef,
    after_sync: ComputationRef,
    after_greeting: ComputationRef,
    ready_name: ComputationRef,
    ready_greeting: ComputationRef,
}

fn evolve_hello_branch(
    objects: &mut MemoryObjects,
    initial: &ValidatedComposite,
    provider_boundary: &Boundary,
    greeter_boundary: &Boundary,
    parent_boundary: &Boundary,
    name: &str,
) -> HelloBranch {
    let provider_node = NodeId::parse("name-provider").unwrap();
    let greeter_node = NodeId::parse("greeter").unwrap();
    let initial_provider = initial.residual().nodes[&provider_node].clone();
    let initial_greeter = initial.residual().nodes[&greeter_node].clone();
    let greeting = format!("Hello, {name}!");

    let ready_name_bytes = format!("ReadyName({name})");
    let ready_name = seal_leaf(
        objects,
        "example.name-provider@1",
        provider_boundary.clone(),
        ready_name_bytes.as_bytes(),
    );
    let input_reduction = lift_exported_step(
        initial,
        &NodeStep {
            node: provider_node.clone(),
            from: objects.resolve(&initial_provider),
            label: StepLabel::Input {
                port: PortId::parse("input").unwrap(),
                payload: ProtocolPayload::from(name),
            },
            to: objects.resolve(&ready_name),
        },
    )
    .unwrap();
    assert_eq!(
        input_reduction.label,
        StepLabel::Input {
            port: PortId::parse("name").unwrap(),
            payload: ProtocolPayload::from(name),
        }
    );
    let after_input = seal_composite(objects, parent_boundary.clone(), input_reduction.successor);
    let after_input_validated =
        validate_composite(&objects.resolve(&after_input), objects, &TextProtocol).unwrap();

    let provider_done_bytes = format!("Done({name})");
    let provider_done = seal_leaf(
        objects,
        "example.name-provider@1",
        provider_boundary.clone(),
        provider_done_bytes.as_bytes(),
    );
    let ready_greeting_bytes = format!("ReadyGreeting({greeting})");
    let ready_greeting = seal_leaf(
        objects,
        "example.greeter@1",
        greeter_boundary.clone(),
        ready_greeting_bytes.as_bytes(),
    );
    let sync_reduction = synchronize_connection(
        &after_input_validated,
        &NodeStep {
            node: provider_node,
            from: objects.resolve(&ready_name),
            label: StepLabel::Output {
                port: PortId::parse("name").unwrap(),
                payload: ProtocolPayload::from(name),
            },
            to: objects.resolve(&provider_done),
        },
        &NodeStep {
            node: greeter_node.clone(),
            from: objects.resolve(&initial_greeter),
            label: StepLabel::Input {
                port: PortId::parse("name").unwrap(),
                payload: ProtocolPayload::from(name),
            },
            to: objects.resolve(&ready_greeting),
        },
    )
    .unwrap();
    assert_eq!(sync_reduction.label, StepLabel::Tau);
    let after_sync = seal_composite(objects, parent_boundary.clone(), sync_reduction.successor);
    let after_sync_validated =
        validate_composite(&objects.resolve(&after_sync), objects, &TextProtocol).unwrap();

    let greeter_done = seal_leaf(
        objects,
        "example.greeter@1",
        greeter_boundary.clone(),
        b"Done",
    );
    let greeting_reduction = lift_exported_step(
        &after_sync_validated,
        &NodeStep {
            node: greeter_node,
            from: objects.resolve(&ready_greeting),
            label: StepLabel::Output {
                port: PortId::parse("greeting").unwrap(),
                payload: ProtocolPayload::from(greeting.as_str()),
            },
            to: objects.resolve(&greeter_done),
        },
    )
    .unwrap();
    assert_eq!(
        greeting_reduction.label,
        StepLabel::Output {
            port: PortId::parse("greeting").unwrap(),
            payload: ProtocolPayload::from(greeting.as_str()),
        }
    );
    let after_greeting = seal_composite(
        objects,
        parent_boundary.clone(),
        greeting_reduction.successor,
    );
    validate_composite(&objects.resolve(&after_greeting), objects, &TextProtocol).unwrap();

    HelloBranch {
        after_input,
        after_sync,
        after_greeting,
        ready_name,
        ready_greeting,
    }
}

#[test]
fn reducer_preserves_behavioral_branches_from_same_computation() {
    let mut objects = MemoryObjects::default();
    let provider_boundary = BTreeMap::from([
        (PortId::parse("input").unwrap(), text_port("receiver")),
        (PortId::parse("name").unwrap(), text_port("sender")),
    ]);
    let greeter_boundary = BTreeMap::from([
        (PortId::parse("name").unwrap(), text_port("receiver")),
        (PortId::parse("greeting").unwrap(), text_port("sender")),
    ]);
    let initial_provider = seal_leaf(
        &mut objects,
        "example.name-provider@1",
        provider_boundary.clone(),
        b"WaitingInput",
    );
    let initial_greeter = seal_leaf(
        &mut objects,
        "example.greeter@1",
        greeter_boundary.clone(),
        b"WaitingName",
    );
    let initial_residual = CompositeResidual {
        nodes: BTreeMap::from([
            (NodeId::parse("name-provider").unwrap(), initial_provider),
            (NodeId::parse("greeter").unwrap(), initial_greeter),
        ]),
        connections: vec![
            Connection::new(
                endpoint("name-provider", "name"),
                endpoint("greeter", "name"),
            )
            .unwrap(),
        ],
        exports: BTreeMap::from([
            (
                PortId::parse("name").unwrap(),
                endpoint("name-provider", "input"),
            ),
            (
                PortId::parse("greeting").unwrap(),
                endpoint("greeter", "greeting"),
            ),
        ]),
    };
    let parent_boundary = BTreeMap::from([
        (PortId::parse("name").unwrap(), text_port("receiver")),
        (PortId::parse("greeting").unwrap(), text_port("sender")),
    ]);
    let initial = seal_composite(&mut objects, parent_boundary.clone(), initial_residual);
    let initial_validated =
        validate_composite(&objects.resolve(&initial), &objects, &TextProtocol).unwrap();

    let alice = evolve_hello_branch(
        &mut objects,
        &initial_validated,
        &provider_boundary,
        &greeter_boundary,
        &parent_boundary,
        "Alice",
    );
    let bob = evolve_hello_branch(
        &mut objects,
        &initial_validated,
        &provider_boundary,
        &greeter_boundary,
        &parent_boundary,
        "Bob",
    );

    assert_ne!(alice.after_input, bob.after_input);
    assert_ne!(alice.after_sync, bob.after_sync);
    assert_ne!(alice.after_greeting, bob.after_greeting);
    let identities = BTreeSet::from([
        initial.clone(),
        alice.after_input.clone(),
        alice.after_sync.clone(),
        alice.after_greeting.clone(),
        bob.after_input.clone(),
        bob.after_sync.clone(),
        bob.after_greeting.clone(),
    ]);
    assert_eq!(identities.len(), 7);

    for computation in identities {
        assert_eq!(
            objects.resolve(&computation).object().boundary,
            parent_boundary
        );
    }

    let alice_after_input = validate_composite(
        &objects.resolve(&alice.after_input),
        &objects,
        &TextProtocol,
    )
    .unwrap();
    let bob_after_input =
        validate_composite(&objects.resolve(&bob.after_input), &objects, &TextProtocol).unwrap();
    assert_eq!(
        alice_after_input.residual().nodes[&NodeId::parse("name-provider").unwrap()],
        alice.ready_name
    );
    assert_eq!(
        bob_after_input.residual().nodes[&NodeId::parse("name-provider").unwrap()],
        bob.ready_name
    );
    assert_eq!(
        objects.bytes[&objects.resolve(&alice.ready_name).object().residual].as_slice(),
        b"ReadyName(Alice)"
    );
    assert_eq!(
        objects.bytes[&objects.resolve(&bob.ready_name).object().residual].as_slice(),
        b"ReadyName(Bob)"
    );

    let alice_after_sync =
        validate_composite(&objects.resolve(&alice.after_sync), &objects, &TextProtocol).unwrap();
    let bob_after_sync =
        validate_composite(&objects.resolve(&bob.after_sync), &objects, &TextProtocol).unwrap();
    assert_eq!(
        alice_after_sync.residual().nodes[&NodeId::parse("greeter").unwrap()],
        alice.ready_greeting
    );
    assert_eq!(
        bob_after_sync.residual().nodes[&NodeId::parse("greeter").unwrap()],
        bob.ready_greeting
    );
    assert_eq!(
        objects.bytes[&objects.resolve(&alice.ready_greeting).object().residual].as_slice(),
        b"ReadyGreeting(Hello, Alice!)"
    );
    assert_eq!(
        objects.bytes[&objects.resolve(&bob.ready_greeting).object().residual].as_slice(),
        b"ReadyGreeting(Hello, Bob!)"
    );
}

#[test]
fn reducer_rejects_invalid_transition_evidence() {
    let (mut objects, parent_ref, _, _) = hello_world_fixture();
    let current =
        validate_composite(&objects.resolve(&parent_ref), &objects, &TextProtocol).unwrap();
    let provider = current.residual().nodes[&NodeId::parse("name-provider").unwrap()].clone();
    let greeter = current.residual().nodes[&NodeId::parse("greeter").unwrap()].clone();
    let alternate = seal_leaf(
        &mut objects,
        "example.other@1",
        BTreeMap::new(),
        b"alternate",
    );
    let provider_step = |from: &ComputationRef, label| NodeStep {
        node: NodeId::parse("name-provider").unwrap(),
        from: objects.resolve(from),
        label,
        to: objects.resolve(&alternate),
    };
    let greeter_step = |label| NodeStep {
        node: NodeId::parse("greeter").unwrap(),
        from: objects.resolve(&greeter),
        label,
        to: objects.resolve(&alternate),
    };

    assert!(matches!(
        lift_internal_step(&current, &provider_step(&alternate, StepLabel::Tau)),
        Err(CompositeStepError::StaleFrom { .. })
    ));
    assert!(matches!(
        synchronize_connection(
            &current,
            &provider_step(
                &provider,
                StepLabel::Output {
                    port: PortId::parse("name").unwrap(),
                    payload: ProtocolPayload::from("Alice")
                }
            ),
            &greeter_step(StepLabel::Output {
                port: PortId::parse("name").unwrap(),
                payload: ProtocolPayload::from("Alice")
            })
        ),
        Err(CompositeStepError::ComplementaryActionsRequired)
    ));
    assert!(matches!(
        synchronize_connection(
            &current,
            &provider_step(
                &provider,
                StepLabel::Input {
                    port: PortId::parse("name").unwrap(),
                    payload: ProtocolPayload::from("Alice")
                }
            ),
            &greeter_step(StepLabel::Input {
                port: PortId::parse("name").unwrap(),
                payload: ProtocolPayload::from("Alice")
            })
        ),
        Err(CompositeStepError::ComplementaryActionsRequired)
    ));
    assert!(matches!(
        synchronize_connection(
            &current,
            &provider_step(
                &provider,
                StepLabel::Output {
                    port: PortId::parse("name").unwrap(),
                    payload: ProtocolPayload::from("Alice")
                }
            ),
            &greeter_step(StepLabel::Input {
                port: PortId::parse("name").unwrap(),
                payload: ProtocolPayload::from("Bob")
            })
        ),
        Err(CompositeStepError::ValueMismatch)
    ));
    assert!(matches!(
        synchronize_connection(
            &current,
            &provider_step(
                &provider,
                StepLabel::Output {
                    port: PortId::parse("wrong").unwrap(),
                    payload: ProtocolPayload::from("Alice")
                }
            ),
            &greeter_step(StepLabel::Input {
                port: PortId::parse("name").unwrap(),
                payload: ProtocolPayload::from("Alice")
            })
        ),
        Err(CompositeStepError::ConnectionEndpointMismatch)
    ));
    assert!(matches!(
        lift_exported_step(
            &current,
            &provider_step(
                &provider,
                StepLabel::Output {
                    port: PortId::parse("name").unwrap(),
                    payload: ProtocolPayload::from("Alice")
                }
            )
        ),
        Err(CompositeStepError::ConnectedPortCannotBeExported { .. })
    ));
    assert!(matches!(
        lift_exported_step(
            &current,
            &greeter_step(StepLabel::Output {
                port: PortId::parse("name").unwrap(),
                payload: ProtocolPayload::from("Alice")
            })
        ),
        Err(CompositeStepError::ConnectedPortCannotBeExported { .. })
    ));
    assert!(matches!(
        lift_exported_step(
            &current,
            &provider_step(
                &provider,
                StepLabel::Output {
                    port: PortId::parse("other").unwrap(),
                    payload: ProtocolPayload::from("Alice")
                }
            )
        ),
        Err(CompositeStepError::UnexportedPort { .. })
    ));
    let lifted_input = lift_exported_step(
        &current,
        &greeter_step(StepLabel::Input {
            port: PortId::parse("greeting").unwrap(),
            payload: ProtocolPayload::from("Hello, Alice"),
        }),
    )
    .unwrap();
    assert_eq!(
        lifted_input.label,
        StepLabel::Input {
            port: PortId::parse("greeting").unwrap(),
            payload: ProtocolPayload::from("Hello, Alice"),
        }
    );
}

#[test]
fn validator_rejects_same_node_connection_and_successor_without_exported_port() {
    let mut objects = MemoryObjects::default();
    let child = objects.insert_computation(&leaf(
        "example.worker@1",
        BTreeMap::from([
            (PortId::parse("left").unwrap(), text_port("sender")),
            (PortId::parse("right").unwrap(), text_port("receiver")),
        ]),
        0x77,
    ));
    let invalid_ref = seal_composite(
        &mut objects,
        BTreeMap::new(),
        CompositeResidual {
            nodes: BTreeMap::from([(NodeId::parse("worker").unwrap(), child)]),
            connections: vec![
                Connection::new(endpoint("worker", "left"), endpoint("worker", "right")).unwrap(),
            ],
            exports: BTreeMap::new(),
        },
    );
    assert!(matches!(
        validate_composite(&objects.resolve(&invalid_ref), &objects, &TextProtocol),
        Err(CompositeValidationError::SameNodeConnectionUnsupported { .. })
    ));

    let (mut objects, parent_ref, _, _) = hello_world_fixture();
    let current =
        validate_composite(&objects.resolve(&parent_ref), &objects, &TextProtocol).unwrap();
    let greeter = current.residual().nodes[&NodeId::parse("greeter").unwrap()].clone();
    let broken = objects.insert_computation(&leaf("example.greeter@1", BTreeMap::new(), 0x88));
    let reduction = lift_internal_step(
        &current,
        &NodeStep {
            node: NodeId::parse("greeter").unwrap(),
            from: objects.resolve(&greeter),
            label: StepLabel::Tau,
            to: objects.resolve(&broken),
        },
    )
    .unwrap();
    let successor = seal_composite(
        &mut objects,
        BTreeMap::from([(PortId::parse("greeting").unwrap(), text_port("sender"))]),
        reduction.successor,
    );
    assert!(matches!(
        validate_composite(&objects.resolve(&successor), &objects, &TextProtocol),
        Err(CompositeValidationError::MissingPort { .. })
    ));
}

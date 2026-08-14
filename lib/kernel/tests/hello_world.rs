use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use ato_computation::{
    Boundary, ComputationObject, ComputationRef, PortDef, PortId, ProtocolId, ResolvedComputation,
    RoleId, SemanticsId,
};
use ato_kernel::{
    Action, ChoiceId, Kernel, ProtocolError, ProtocolPayload, ProtocolSemantics, Run,
    SemanticError, SemanticHost, SemanticStep, Semantics, Transition, TransitionOffer,
    TransitionSink,
};
use ato_objects::{MemoryObjectStore, ObjectStore};
use ato_semantics_compose::{
    ComposeSemantics, CompositeResidual, Connection, Endpoint, NodeId, encode_composite_residual,
};

const TEXT_PROTOCOL: &str = "example.text@1";

#[derive(Default)]
struct RecordingSink(Mutex<Vec<Transition>>);

impl TransitionSink for RecordingSink {
    fn observe(&self, transition: &Transition) {
        self.0.lock().unwrap().push(transition.clone());
    }
}

struct TextProtocol {
    id: ProtocolId,
}

impl TextProtocol {
    fn new() -> Self {
        Self {
            id: ProtocolId::parse(TEXT_PROTOCOL).unwrap(),
        }
    }
}

impl ProtocolSemantics for TextProtocol {
    fn id(&self) -> &ProtocolId {
        &self.id
    }

    fn roles_compatible(&self, left: &RoleId, right: &RoleId) -> Result<bool, ProtocolError> {
        Ok(BTreeSet::from([left.as_str(), right.as_str()])
            == BTreeSet::from(["receiver", "sender"]))
    }

    fn validate_input(
        &self,
        role: &RoleId,
        payload: &ProtocolPayload,
    ) -> Result<(), ProtocolError> {
        validate_text("receiver", role, payload)
    }

    fn validate_output(
        &self,
        role: &RoleId,
        payload: &ProtocolPayload,
    ) -> Result<(), ProtocolError> {
        validate_text("sender", role, payload)
    }
}

fn validate_text(
    expected_role: &str,
    role: &RoleId,
    payload: &ProtocolPayload,
) -> Result<(), ProtocolError> {
    if role.as_str() != expected_role {
        return Err(ProtocolError::new("text action has the wrong role"));
    }
    std::str::from_utf8(payload.as_bytes())
        .map(|_| ())
        .map_err(|_| ProtocolError::new("text payload is not UTF-8"))
}

struct NameProvider {
    id: SemanticsId,
}

impl NameProvider {
    fn new() -> Self {
        Self {
            id: SemanticsId::parse("example.name-provider@1").unwrap(),
        }
    }
}

impl Semantics for NameProvider {
    fn id(&self) -> &SemanticsId {
        &self.id
    }

    fn enabled(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost,
    ) -> Result<Vec<TransitionOffer>, SemanticError> {
        let state = residual_text(current, host)?;
        Ok(match state.strip_prefix("ReadyName:") {
            Some(name) => vec![TransitionOffer::selected(
                ChoiceId::new("emit-name"),
                Action::Output {
                    port: port("name"),
                    payload: ProtocolPayload::from(name),
                },
            )],
            None => Vec::new(),
        })
    }

    fn step(
        &self,
        current: &ResolvedComputation,
        offer: &TransitionOffer,
        host: &dyn SemanticHost,
    ) -> Result<SemanticStep, SemanticError> {
        let state = residual_text(current, host)?;
        let next = match (state.as_str(), &offer.action) {
            ("WaitingInput", Action::Input { port, payload }) if port.as_str() == "input" => {
                format!("ReadyName:{}", payload_text(payload)?)
            }
            (state, Action::Output { port, payload })
                if port.as_str() == "name"
                    && state.strip_prefix("ReadyName:") == Some(payload_text(payload)?) =>
            {
                format!("Done:{}", payload_text(payload)?)
            }
            _ => return Err(SemanticError::new("name-provider action is not enabled")),
        };
        successor(current, offer, next, host)
    }
}

struct Greeter {
    id: SemanticsId,
}

impl Greeter {
    fn new() -> Self {
        Self {
            id: SemanticsId::parse("example.greeter@1").unwrap(),
        }
    }
}

impl Semantics for Greeter {
    fn id(&self) -> &SemanticsId {
        &self.id
    }

    fn enabled(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost,
    ) -> Result<Vec<TransitionOffer>, SemanticError> {
        let state = residual_text(current, host)?;
        Ok(match state.strip_prefix("ReadyGreeting:") {
            Some(greeting) => vec![TransitionOffer::selected(
                ChoiceId::new("emit-greeting"),
                Action::Output {
                    port: port("greeting"),
                    payload: ProtocolPayload::from(greeting),
                },
            )],
            None => Vec::new(),
        })
    }

    fn step(
        &self,
        current: &ResolvedComputation,
        offer: &TransitionOffer,
        host: &dyn SemanticHost,
    ) -> Result<SemanticStep, SemanticError> {
        let state = residual_text(current, host)?;
        let next = match (state.as_str(), &offer.action) {
            ("WaitingName", Action::Input { port, payload }) if port.as_str() == "name" => {
                format!("ReadyGreeting:Hello, {}!", payload_text(payload)?)
            }
            (state, Action::Output { port, payload })
                if port.as_str() == "greeting"
                    && state.strip_prefix("ReadyGreeting:") == Some(payload_text(payload)?) =>
            {
                "Done".to_owned()
            }
            _ => return Err(SemanticError::new("greeter action is not enabled")),
        };
        successor(current, offer, next, host)
    }
}

fn successor(
    current: &ResolvedComputation,
    offer: &TransitionOffer,
    residual: String,
    host: &dyn SemanticHost,
) -> Result<SemanticStep, SemanticError> {
    let residual = host
        .put_object(residual.as_bytes())
        .map_err(|error| SemanticError::new(error.to_string()))?;
    Ok(SemanticStep {
        offer: offer.clone(),
        successor: ComputationObject {
            semantics: current.object().semantics.clone(),
            boundary: current.object().boundary.clone(),
            residual,
        },
    })
}

fn residual_text(
    current: &ResolvedComputation,
    host: &dyn SemanticHost,
) -> Result<String, SemanticError> {
    let bytes = host
        .get_object(&current.object().residual, 1024)
        .map_err(|error| SemanticError::new(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| SemanticError::new(error.to_string()))
}

fn payload_text(payload: &ProtocolPayload) -> Result<&str, SemanticError> {
    std::str::from_utf8(payload.as_bytes()).map_err(|error| SemanticError::new(error.to_string()))
}

#[test]
fn behavioral_hello_world_branches_from_same_computation() {
    let objects = Arc::new(MemoryObjectStore::default());
    let sink = Arc::new(RecordingSink::default());
    let mut kernel = Kernel::new(objects.clone()).with_transition_sink(sink.clone());
    kernel.register(Arc::new(NameProvider::new())).unwrap();
    kernel.register(Arc::new(Greeter::new())).unwrap();
    kernel.register(Arc::new(ComposeSemantics::new())).unwrap();
    kernel
        .register_protocol(Arc::new(TextProtocol::new()))
        .unwrap();

    let provider = seal_leaf(
        &kernel,
        objects.as_ref(),
        "example.name-provider@1",
        BTreeMap::from([
            (port("input"), text_port("receiver")),
            (port("name"), text_port("sender")),
        ]),
        "WaitingInput",
    );
    let greeter = seal_leaf(
        &kernel,
        objects.as_ref(),
        "example.greeter@1",
        BTreeMap::from([
            (port("name"), text_port("receiver")),
            (port("greeting"), text_port("sender")),
        ]),
        "WaitingName",
    );
    let boundary = BTreeMap::from([
        (port("name"), text_port("receiver")),
        (port("greeting"), text_port("sender")),
    ]);
    let residual = CompositeResidual {
        nodes: BTreeMap::from([
            (node("greeter"), greeter),
            (node("name-provider"), provider),
        ]),
        connections: vec![
            Connection::new(
                endpoint("name-provider", "name"),
                endpoint("greeter", "name"),
            )
            .unwrap(),
        ],
        exports: BTreeMap::from([
            (port("name"), endpoint("name-provider", "input")),
            (port("greeting"), endpoint("greeter", "greeting")),
        ]),
    };
    let residual = objects
        .put(&encode_composite_residual(&residual).unwrap())
        .unwrap();
    let computation = kernel
        .seal(&ComputationObject {
            semantics: SemanticsId::parse("capsule.compose@1").unwrap(),
            boundary: boundary.clone(),
            residual,
        })
        .unwrap();

    let alice = branch(&kernel, &computation, "Alice");
    let bob = branch(&kernel, &computation, "Bob");
    let all = BTreeSet::from([
        computation.clone(),
        alice[0].clone(),
        alice[1].clone(),
        alice[2].clone(),
        bob[0].clone(),
        bob[1].clone(),
        bob[2].clone(),
    ]);

    assert_eq!(all.len(), 7);
    for reference in all {
        assert_eq!(
            kernel.resolve(&reference).unwrap().object().boundary,
            boundary
        );
    }
    assert_branch_residuals(&kernel, &alice, "Alice");
    assert_branch_residuals(&kernel, &bob, "Bob");
    let visible = sink.0.lock().unwrap();
    assert_eq!(visible.len(), 6);
    assert_eq!(
        visible
            .iter()
            .filter(|transition| matches!(transition.offer.action, Action::Tau))
            .count(),
        2
    );
}

fn branch(kernel: &Kernel, computation: &ComputationRef, name: &str) -> [ComputationRef; 3] {
    let mut run = Run {
        head: computation.clone(),
    };
    let after_input = kernel
        .step(
            &mut run,
            &TransitionOffer::external_input(port("name"), ProtocolPayload::from(name)),
        )
        .unwrap();
    let tau = only_offer(kernel, &run, |action| matches!(action, Action::Tau));
    let after_sync = kernel.step(&mut run, &tau).unwrap();
    assert_eq!(after_sync.offer.action, Action::Tau);
    let greeting = format!("Hello, {name}!");
    let output = only_offer(kernel, &run, |action| {
        matches!(action, Action::Output { port, payload }
            if port.as_str() == "greeting" && payload.as_bytes() == greeting.as_bytes())
    });
    let after_output = kernel.step(&mut run, &output).unwrap();
    assert_eq!(run.head, after_output.to);
    [after_input.to, after_sync.to, after_output.to]
}

fn only_offer(kernel: &Kernel, run: &Run, predicate: impl Fn(&Action) -> bool) -> TransitionOffer {
    let matching: Vec<_> = kernel
        .enabled(&run.head)
        .unwrap()
        .into_iter()
        .filter(|offer| predicate(&offer.action))
        .collect();
    assert_eq!(matching.len(), 1);
    matching.into_iter().next().unwrap()
}

fn assert_branch_residuals(kernel: &Kernel, branch: &[ComputationRef; 3], name: &str) {
    let after_input = composite(kernel, &branch[0]);
    let provider = &after_input.nodes[&node("name-provider")];
    assert_eq!(leaf_state(kernel, provider), format!("ReadyName:{name}"));

    let after_sync = composite(kernel, &branch[1]);
    let greeter = &after_sync.nodes[&node("greeter")];
    assert_eq!(
        leaf_state(kernel, greeter),
        format!("ReadyGreeting:Hello, {name}!")
    );
}

fn composite(kernel: &Kernel, reference: &ComputationRef) -> CompositeResidual {
    let computation = kernel.resolve(reference).unwrap();
    let bytes =
        SemanticHost::get_object(kernel, &computation.object().residual, 1024 * 1024).unwrap();
    ato_semantics_compose::decode_composite_residual(&bytes).unwrap()
}

fn leaf_state(kernel: &Kernel, reference: &ComputationRef) -> String {
    let computation = kernel.resolve(reference).unwrap();
    String::from_utf8(
        SemanticHost::get_object(kernel, &computation.object().residual, 1024).unwrap(),
    )
    .unwrap()
}

fn seal_leaf(
    kernel: &Kernel,
    objects: &MemoryObjectStore,
    semantics: &str,
    boundary: Boundary,
    state: &str,
) -> ComputationRef {
    let residual = objects.put(state.as_bytes()).unwrap();
    kernel
        .seal(&ComputationObject {
            semantics: SemanticsId::parse(semantics).unwrap(),
            boundary,
            residual,
        })
        .unwrap()
}

fn text_port(role: &str) -> PortDef {
    PortDef {
        protocol: ProtocolId::parse(TEXT_PROTOCOL).unwrap(),
        role: RoleId::parse(role).unwrap(),
    }
}

fn port(value: &str) -> PortId {
    PortId::parse(value).unwrap()
}

fn node(value: &str) -> NodeId {
    NodeId::parse(value).unwrap()
}

fn endpoint(node_id: &str, port_id: &str) -> Endpoint {
    Endpoint {
        node: node(node_id),
        port: port(port_id),
    }
}

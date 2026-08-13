use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ato_computation::{
    Boundary, ComputationObject, ComputationRef, PortDef, PortId, ProtocolId, ResolvedComputation,
    RoleId, SemanticsId,
};
use ato_kernel::{Action, Kernel, Run, SemanticError, SemanticHost, SemanticStep, Semantics};
use ato_objects::{MemoryObjectStore, ObjectStore};
use ato_semantics_compose::{
    ComposeSemantics, CompositeResidual, Connection, Endpoint, NodeId, ProtocolRolePolicy,
    encode_composite_residual,
};

const TEXT_PROTOCOL: &str = "example.text@1";

struct TextProtocol;

impl ProtocolRolePolicy for TextProtocol {
    fn connection_roles_compatible(
        &self,
        protocol: &ProtocolId,
        first: &RoleId,
        second: &RoleId,
    ) -> bool {
        protocol.as_str() == TEXT_PROTOCOL
            && BTreeSet::from([first.as_str(), second.as_str()])
                == BTreeSet::from(["receiver", "sender"])
    }

    fn export_role_compatible(
        &self,
        protocol: &ProtocolId,
        parent: &RoleId,
        child: &RoleId,
    ) -> bool {
        protocol.as_str() == TEXT_PROTOCOL && parent == child
    }
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

impl Semantics<String> for NameProvider {
    fn id(&self) -> &SemanticsId {
        &self.id
    }

    fn enabled(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost<String>,
    ) -> Result<Vec<Action<String>>, SemanticError> {
        let state = residual_text(current, host)?;
        Ok(match state.strip_prefix("ReadyName:") {
            Some(name) => vec![Action::Output {
                port: port("name"),
                value: name.to_owned(),
            }],
            None => Vec::new(),
        })
    }

    fn step(
        &self,
        current: &ResolvedComputation,
        action: &Action<String>,
        host: &dyn SemanticHost<String>,
    ) -> Result<SemanticStep<String>, SemanticError> {
        let state = residual_text(current, host)?;
        let next = match (state.as_str(), action) {
            ("WaitingInput", Action::Input { port, value }) if port.as_str() == "input" => {
                format!("ReadyName:{value}")
            }
            (state, Action::Output { port, value })
                if port.as_str() == "name"
                    && state.strip_prefix("ReadyName:") == Some(value.as_str()) =>
            {
                format!("Done:{value}")
            }
            _ => return Err(SemanticError::new("name-provider action is not enabled")),
        };
        successor(current, action, next, host)
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

impl Semantics<String> for Greeter {
    fn id(&self) -> &SemanticsId {
        &self.id
    }

    fn enabled(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost<String>,
    ) -> Result<Vec<Action<String>>, SemanticError> {
        let state = residual_text(current, host)?;
        Ok(match state.strip_prefix("ReadyGreeting:") {
            Some(greeting) => vec![Action::Output {
                port: port("greeting"),
                value: greeting.to_owned(),
            }],
            None => Vec::new(),
        })
    }

    fn step(
        &self,
        current: &ResolvedComputation,
        action: &Action<String>,
        host: &dyn SemanticHost<String>,
    ) -> Result<SemanticStep<String>, SemanticError> {
        let state = residual_text(current, host)?;
        let next = match (state.as_str(), action) {
            ("WaitingName", Action::Input { port, value }) if port.as_str() == "name" => {
                format!("ReadyGreeting:Hello, {value}!")
            }
            (state, Action::Output { port, value })
                if port.as_str() == "greeting"
                    && state.strip_prefix("ReadyGreeting:") == Some(value.as_str()) =>
            {
                "Done".to_owned()
            }
            _ => return Err(SemanticError::new("greeter action is not enabled")),
        };
        successor(current, action, next, host)
    }
}

fn successor(
    current: &ResolvedComputation,
    action: &Action<String>,
    residual: String,
    host: &dyn SemanticHost<String>,
) -> Result<SemanticStep<String>, SemanticError> {
    let residual = host
        .put_object(residual.as_bytes())
        .map_err(|error| SemanticError::new(error.to_string()))?;
    Ok(SemanticStep {
        action: action.clone(),
        successor: ComputationObject {
            semantics: current.object().semantics.clone(),
            boundary: current.object().boundary.clone(),
            residual,
        },
    })
}

fn residual_text(
    current: &ResolvedComputation,
    host: &dyn SemanticHost<String>,
) -> Result<String, SemanticError> {
    let bytes = host
        .get_object(&current.object().residual, 1024)
        .map_err(|error| SemanticError::new(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| SemanticError::new(error.to_string()))
}

#[test]
fn behavioral_hello_world_branches_from_same_computation() {
    let objects = Arc::new(MemoryObjectStore::default());
    let mut kernel = Kernel::<String>::new(objects.clone());
    kernel.register(Arc::new(NameProvider::new())).unwrap();
    kernel.register(Arc::new(Greeter::new())).unwrap();
    kernel
        .register(Arc::new(ComposeSemantics::new(Arc::new(TextProtocol))))
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
}

fn branch(
    kernel: &Kernel<String>,
    computation: &ComputationRef,
    name: &str,
) -> [ComputationRef; 3] {
    let mut run = Run {
        head: computation.clone(),
    };
    let after_input = kernel
        .step(
            &mut run,
            &Action::Input {
                port: port("name"),
                value: name.to_owned(),
            },
        )
        .unwrap();
    let after_sync = kernel.step(&mut run, &Action::Tau).unwrap();
    assert_eq!(after_sync.action, Action::Tau);
    let greeting = format!("Hello, {name}!");
    let after_output = kernel
        .step(
            &mut run,
            &Action::Output {
                port: port("greeting"),
                value: greeting,
            },
        )
        .unwrap();
    assert_eq!(run.head, after_output.to);
    [after_input.to, after_sync.to, after_output.to]
}

fn assert_branch_residuals(kernel: &Kernel<String>, branch: &[ComputationRef; 3], name: &str) {
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

fn composite(kernel: &Kernel<String>, reference: &ComputationRef) -> CompositeResidual {
    let computation = kernel.resolve(reference).unwrap();
    let bytes =
        SemanticHost::get_object(kernel, &computation.object().residual, 1024 * 1024).unwrap();
    ato_semantics_compose::decode_composite_residual(&bytes).unwrap()
}

fn leaf_state(kernel: &Kernel<String>, reference: &ComputationRef) -> String {
    let computation = kernel.resolve(reference).unwrap();
    String::from_utf8(
        SemanticHost::get_object(kernel, &computation.object().residual, 1024).unwrap(),
    )
    .unwrap()
}

fn seal_leaf(
    kernel: &Kernel<String>,
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

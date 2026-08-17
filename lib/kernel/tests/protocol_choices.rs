use std::collections::BTreeMap;
use std::sync::Arc;

use ato_compose::{
    ComposeSemantics, CompositeResidual, Endpoint, NodeId, encode_composite_residual,
};
use ato_computation::{
    ComputationObject, ComputationRef, PortDef, PortId, ProtocolId, ResolvedComputation, RoleId,
    SemanticsId,
};
use ato_kernel::{
    Action, ChoiceId, Kernel, ProtocolError, ProtocolPayload, ProtocolSemantics, SemanticError,
    SemanticHost, SemanticStep, Semantics, TransitionOffer,
};
use ato_objects::{MemoryObjectStore, ObjectStore};

struct TestProtocol {
    id: ProtocolId,
    kind: PayloadKind,
}

enum PayloadKind {
    Text,
    Binary,
}

impl ProtocolSemantics for TestProtocol {
    fn id(&self) -> &ProtocolId {
        &self.id
    }

    fn roles_compatible(&self, left: &RoleId, right: &RoleId) -> Result<bool, ProtocolError> {
        Ok(left.as_str() == "sender" && right.as_str() == "receiver"
            || left.as_str() == "receiver" && right.as_str() == "sender")
    }

    fn validate_input(
        &self,
        _role: &RoleId,
        payload: &ProtocolPayload,
    ) -> Result<(), ProtocolError> {
        self.validate(payload)
    }

    fn validate_output(
        &self,
        _role: &RoleId,
        payload: &ProtocolPayload,
    ) -> Result<(), ProtocolError> {
        self.validate(payload)
    }
}

impl TestProtocol {
    fn validate(&self, payload: &ProtocolPayload) -> Result<(), ProtocolError> {
        match self.kind {
            PayloadKind::Text => std::str::from_utf8(payload.as_bytes())
                .map(|_| ())
                .map_err(|_| ProtocolError::new("invalid text")),
            PayloadKind::Binary if payload.as_bytes() == [0, 255] => Ok(()),
            PayloadKind::Binary => Err(ProtocolError::new("invalid binary frame")),
        }
    }
}

struct EmitOnce {
    id: SemanticsId,
    port: PortId,
    payload: ProtocolPayload,
}

impl Semantics for EmitOnce {
    fn id(&self) -> &SemanticsId {
        &self.id
    }

    fn enabled(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost,
    ) -> Result<Vec<TransitionOffer>, SemanticError> {
        Ok((residual(current, host)? == b"ready")
            .then(|| {
                TransitionOffer::selected(
                    ChoiceId::new("emit"),
                    Action::Output {
                        port: self.port.clone(),
                        payload: self.payload.clone(),
                    },
                )
            })
            .into_iter()
            .collect())
    }

    fn step(
        &self,
        current: &ResolvedComputation,
        offer: &TransitionOffer,
        host: &dyn SemanticHost,
    ) -> Result<SemanticStep, SemanticError> {
        if self.enabled(current, host)?.first() != Some(offer) {
            return Err(SemanticError::new("offer not enabled"));
        }
        let residual = host
            .put_object(b"done")
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
}

struct TwoTau {
    id: SemanticsId,
}

impl Semantics for TwoTau {
    fn id(&self) -> &SemanticsId {
        &self.id
    }

    fn enabled(
        &self,
        _current: &ResolvedComputation,
        _host: &dyn SemanticHost,
    ) -> Result<Vec<TransitionOffer>, SemanticError> {
        Ok(["A", "B"]
            .into_iter()
            .map(|choice| TransitionOffer::selected(ChoiceId::new(choice), Action::Tau))
            .collect())
    }

    fn step(
        &self,
        current: &ResolvedComputation,
        offer: &TransitionOffer,
        host: &dyn SemanticHost,
    ) -> Result<SemanticStep, SemanticError> {
        let choice = offer
            .choice
            .as_ref()
            .ok_or_else(|| SemanticError::new("choice required"))?;
        let residual = host
            .put_object(choice.as_str().as_bytes())
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
}

#[test]
fn one_composite_carries_heterogeneous_protocol_payloads() {
    let objects = Arc::new(MemoryObjectStore::default());
    let mut kernel = Kernel::new(objects.clone());
    let text_protocol = protocol("example.text@1");
    let binary_protocol = protocol("example.binary@1");
    kernel
        .register_protocol(Arc::new(TestProtocol {
            id: text_protocol.clone(),
            kind: PayloadKind::Text,
        }))
        .unwrap();
    kernel
        .register_protocol(Arc::new(TestProtocol {
            id: binary_protocol.clone(),
            kind: PayloadKind::Binary,
        }))
        .unwrap();
    let text_semantics = semantics("example.text-source@1");
    let binary_semantics = semantics("example.binary-source@1");
    kernel
        .register(Arc::new(EmitOnce {
            id: text_semantics.clone(),
            port: port("out"),
            payload: ProtocolPayload::from("hello"),
        }))
        .unwrap();
    kernel
        .register(Arc::new(EmitOnce {
            id: binary_semantics.clone(),
            port: port("out"),
            payload: ProtocolPayload::from(vec![0, 255]),
        }))
        .unwrap();
    kernel.register(Arc::new(ComposeSemantics::new())).unwrap();

    let text = leaf(
        &kernel,
        objects.as_ref(),
        text_semantics,
        text_protocol.clone(),
    );
    let binary = leaf(
        &kernel,
        objects.as_ref(),
        binary_semantics,
        binary_protocol.clone(),
    );
    let residual = CompositeResidual {
        nodes: BTreeMap::from([(node("binary"), binary), (node("text"), text)]),
        connections: Vec::new(),
        exports: BTreeMap::from([
            (
                port("binary"),
                Endpoint {
                    node: node("binary"),
                    port: port("out"),
                },
            ),
            (
                port("text"),
                Endpoint {
                    node: node("text"),
                    port: port("out"),
                },
            ),
        ]),
    };
    let residual = objects
        .put(&encode_composite_residual(&residual).unwrap())
        .unwrap();
    let root = kernel
        .seal(&ComputationObject {
            semantics: semantics("capsule.compose@1"),
            boundary: BTreeMap::from([
                (port("binary"), port_def(binary_protocol, "sender")),
                (port("text"), port_def(text_protocol, "sender")),
            ]),
            residual,
        })
        .unwrap();

    let offers = kernel.enabled(&root).unwrap();
    assert_eq!(offers.len(), 2);
    assert!(offers.iter().any(|offer| matches!(
        &offer.action,
        Action::Output { port, payload }
            if port.as_str() == "text" && payload.as_bytes() == b"hello"
    )));
    assert!(offers.iter().any(|offer| matches!(
        &offer.action,
        Action::Output { port, payload }
            if port.as_str() == "binary" && payload.as_bytes() == [0, 255]
    )));
}

#[test]
fn same_visible_tau_action_has_two_selectable_semantic_choices() {
    let objects = Arc::new(MemoryObjectStore::default());
    let mut kernel = Kernel::new(objects.clone());
    let id = semantics("example.two-tau@1");
    kernel
        .register(Arc::new(TwoTau { id: id.clone() }))
        .unwrap();
    let residual = objects.put(b"ready").unwrap();
    let root = kernel
        .seal(&ComputationObject {
            semantics: id,
            boundary: BTreeMap::new(),
            residual,
        })
        .unwrap();

    let offers = kernel.enabled(&root).unwrap();
    assert_eq!(offers.len(), 2);
    assert!(offers.iter().all(|offer| offer.action == Action::Tau));
    let first = kernel.derive_transition(&root, &offers[0]).unwrap();
    let second = kernel.derive_transition(&root, &offers[1]).unwrap();
    assert_ne!(first.offer.choice, second.offer.choice);
    assert_ne!(first.to, second.to);
}

fn residual(
    current: &ResolvedComputation,
    host: &dyn SemanticHost,
) -> Result<Vec<u8>, SemanticError> {
    host.get_object(&current.object().residual, 1024)
        .map_err(|error| SemanticError::new(error.to_string()))
}

fn leaf(
    kernel: &Kernel,
    objects: &MemoryObjectStore,
    semantics: SemanticsId,
    protocol: ProtocolId,
) -> ComputationRef {
    let residual = objects.put(b"ready").unwrap();
    kernel
        .seal(&ComputationObject {
            semantics,
            boundary: BTreeMap::from([(port("out"), port_def(protocol, "sender"))]),
            residual,
        })
        .unwrap()
}

fn port_def(protocol: ProtocolId, role: &str) -> PortDef {
    PortDef {
        protocol,
        role: RoleId::parse(role).unwrap(),
    }
}

fn protocol(value: &str) -> ProtocolId {
    ProtocolId::parse(value).unwrap()
}

fn semantics(value: &str) -> SemanticsId {
    SemanticsId::parse(value).unwrap()
}

fn port(value: &str) -> PortId {
    PortId::parse(value).unwrap()
}

fn node(value: &str) -> NodeId {
    NodeId::parse(value).unwrap()
}

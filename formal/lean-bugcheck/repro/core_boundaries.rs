//! Reproduce independent Kernel registration and Composition depth defects.
use ato_compose::{
    CompositeResidual, CompositeValidationError, NodeId, ProtocolRolePolicy, ValidationBudget,
    ValidationResource, encode_composite_residual, validate_composite_with_budget,
};
use ato_computation::{
    ComputationObject, ComputationRef, ProtocolId, ResolvedComputation, RoleId, SemanticsId,
};
use ato_kernel::{
    Action, ChoiceId, Kernel, KernelError, ProtocolError, ProtocolPayload, ProtocolSemantics,
    SemanticError, SemanticHost, SemanticStep, Semantics, TransitionOffer,
};
use ato_objects::{MemoryObjectStore, ObjectStore, resolve_computation};
use std::collections::BTreeMap;
use std::sync::Arc;

struct TestSemantics {
    id: SemanticsId,
    marker: &'static str,
}
impl Semantics for TestSemantics {
    fn id(&self) -> &SemanticsId {
        &self.id
    }
    fn enabled(
        &self,
        _: &ResolvedComputation,
        _: &dyn SemanticHost,
    ) -> Result<Vec<TransitionOffer>, SemanticError> {
        Ok(vec![TransitionOffer::selected(
            ChoiceId::new(self.marker),
            Action::Tau,
        )])
    }
    fn step(
        &self,
        _: &ResolvedComputation,
        _: &TransitionOffer,
        _: &dyn SemanticHost,
    ) -> Result<SemanticStep, SemanticError> {
        Err(SemanticError::new("not used"))
    }
}
struct TestProtocol {
    id: ProtocolId,
    allows: bool,
}
impl ProtocolSemantics for TestProtocol {
    fn id(&self) -> &ProtocolId {
        &self.id
    }
    fn roles_compatible(&self, _: &RoleId, _: &RoleId) -> Result<bool, ProtocolError> {
        Ok(self.allows)
    }
    fn validate_input(&self, _: &RoleId, _: &ProtocolPayload) -> Result<(), ProtocolError> {
        Ok(())
    }
    fn validate_output(&self, _: &RoleId, _: &ProtocolPayload) -> Result<(), ProtocolError> {
        Ok(())
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
fn main() {
    let store = Arc::new(MemoryObjectStore::default());
    let mut kernel = Kernel::new(store.clone());
    let sid = SemanticsId::parse("example.test@1").unwrap();
    kernel
        .register(Arc::new(TestSemantics {
            id: sid.clone(),
            marker: "original",
        }))
        .unwrap();
    assert!(matches!(
        kernel.register(Arc::new(TestSemantics {
            id: sid.clone(),
            marker: "replacement"
        })),
        Err(KernelError::DuplicateSemantics(_))
    ));
    let residual = store.put(b"test").unwrap();
    let object = ComputationObject {
        semantics: sid,
        boundary: BTreeMap::new(),
        residual,
    };
    let head = kernel.seal(&object).unwrap();
    assert_eq!(
        kernel.enabled(&head).unwrap()[0]
            .choice
            .as_ref()
            .unwrap()
            .as_str(),
        "replacement"
    );
    let pid = ProtocolId::parse("example.test@1").unwrap();
    kernel
        .register_protocol(Arc::new(TestProtocol {
            id: pid.clone(),
            allows: false,
        }))
        .unwrap();
    assert!(matches!(
        kernel.register_protocol(Arc::new(TestProtocol {
            id: pid.clone(),
            allows: true
        })),
        Err(KernelError::DuplicateProtocol(_))
    ));
    let role = RoleId::parse("test").unwrap();
    assert!(SemanticHost::roles_compatible(&kernel, &pid, &role, &role).unwrap());
    println!("duplicate registration reports an error but replaces semantics and protocol");

    let leaf = composite(&store, &[]);
    let shared = composite(&store, &[("leaf", &leaf)]);
    let longer = composite(&store, &[("shared", &shared)]);
    let shallow_first = composite(&store, &[("a", &shared), ("b", &longer)]);
    let deep_first = composite(&store, &[("a", &longer), ("b", &shared)]);
    let budget = ValidationBudget {
        max_depth: 2,
        ..ValidationBudget::default()
    };
    let a = resolve_computation(store.as_ref(), &shallow_first).unwrap();
    let b = resolve_computation(store.as_ref(), &deep_first).unwrap();
    assert!(validate_composite_with_budget(&a, store.as_ref(), &Roles, budget).is_ok());
    assert!(validate_composite_with_budget(&b, store.as_ref(), &Roles, budget).is_err());
    println!("a depth-3 path is accepted or rejected depending on node ordering");

    // A plain linear chain deeper than the budget is rejected, as documented.
    let mut linear = composite(&store, &[]);
    for _ in 0..70 {
        linear = composite(&store, &[("child", &linear)]);
    }
    let linear = resolve_computation(store.as_ref(), &linear).unwrap();
    assert!(matches!(
        validate_composite_with_budget(&linear, store.as_ref(), &Roles, ValidationBudget::default()),
        Err(CompositeValidationError::ResourceLimitExceeded(limit))
            if limit.resource == ValidationResource::Depth
    ));

    // The default budget (max_depth = 64) is bypassed when every chain node is
    // also a direct child of the root: each node is first entered at depth 1 and
    // later skipped by the validated set, so the real DAG height is never seen.
    // The accepted path root -> c070 -> ... -> c000 has 71 edges.
    let mut chain = vec![composite(&store, &[])];
    for _ in 0..70 {
        let parent = composite(&store, &[("child", chain.last().unwrap())]);
        chain.push(parent);
    }
    let owned: Vec<(String, ComputationRef)> = (1..=70)
        .map(|index| (format!("n{index:03}"), chain[index].clone()))
        .collect();
    let listed: Vec<(&str, &ComputationRef)> = owned
        .iter()
        .map(|(id, reference)| (id.as_str(), reference))
        .collect();
    let wide = composite(&store, &listed);
    let wide = resolve_computation(store.as_ref(), &wide).unwrap();
    validate_composite_with_budget(&wide, store.as_ref(), &Roles, ValidationBudget::default())
        .expect("default depth budget must not accept a 71-edge path");
    println!("default max_depth=64 accepted a 71-edge path with every node listed at the root");
}

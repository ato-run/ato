//! bundle.rs::closure keys Content and Computation visits in one result map.
//! When a computation's content reference has already been inserted as another
//! computation's residual, the Computation arm takes its
//! "if result.contains_key(&content) { continue; }" branch: that computation is
//! never resolved and its own outgoing references are never traversed.
//! import_bundle still accepts the result, because it compares the declared
//! object set with the traversed set, so a closure that was truncated by the
//! short-circuit equals itself in both directions.
//!
//! Repro for BundleClosureV2.v2_closure_drops_a_reachable_object_BUG and
//! BundleClosureV2.accepted_bundle_is_not_closed_BUG.
//!
//! Run: see formal/lean-bugcheck/run.sh

use std::collections::BTreeMap;
use std::sync::Arc;

use ato_computation::{
    ComputationObject, ComputationRef, ResolvedComputation, SemanticsId, computation_ref,
    encode_computation_object,
};
use ato_objects::{
    BundleError, BundleObjectKind, ComputationReferences, MemoryObjectStore, ObjectLink,
    ObjectResolver, ObjectStore, ReferenceRegistry, decode_bundle, encode_bundle, export_bundle,
    import_bundle, resolve_computation,
};

struct Links {
    semantics: SemanticsId,
    root: ComputationRef,
    child: ComputationRef,
}

impl ComputationReferences for Links {
    fn semantics(&self) -> &SemanticsId {
        &self.semantics
    }

    fn outgoing(
        &self,
        computation: &ResolvedComputation,
        _objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError> {
        // The root names its child as a computation; the child names nothing of
        // its own beyond the residual it carries.
        if computation.reference() == &self.root {
            Ok(vec![ObjectLink::Computation(self.child.clone())])
        } else {
            Ok(Vec::new())
        }
    }
}

fn seal(store: &MemoryObjectStore, object: &ComputationObject) -> ComputationRef {
    let reference = computation_ref(object).expect("sealable computation object");
    store
        .insert(
            reference.content_ref(),
            &encode_computation_object(object).expect("canonical computation bytes"),
        )
        .expect("insert computation object");
    reference
}

fn main() {
    let semantics = SemanticsId::parse("example.closure@1").expect("semantics id");
    let source = MemoryObjectStore::default();

    // The grandchild object the child computation actually references.
    let grandchild = source.put(b"grandchild bytes").expect("put grandchild");

    let child = ComputationObject {
        semantics: semantics.clone(),
        boundary: BTreeMap::new(),
        residual: grandchild.clone(),
    };
    let child_ref = seal(&source, &child);

    // The root's residual is the child's computation object: a content reference
    // that is simultaneously the address of a computation.
    let root = ComputationObject {
        semantics: semantics.clone(),
        boundary: BTreeMap::new(),
        residual: child_ref.content_ref().clone(),
    };
    let root_ref = seal(&source, &root);

    let mut registry = ReferenceRegistry::default();
    registry
        .register(Arc::new(Links {
            semantics,
            root: root_ref.clone(),
            child: child_ref.clone(),
        }))
        .expect("register extractor");

    // 1. Export from the real store. The true reference closure of the root is
    //    {root, child, grandchild}; the exported bundle has two objects.
    let bundle = export_bundle(&root_ref, &source, &registry).expect("export");
    println!("exported objects = {}", bundle.index.objects.len());
    let child_descriptor = bundle
        .index
        .objects
        .iter()
        .find(|descriptor| descriptor.reference == child_ref.content_ref().as_str())
        .expect("the child computation object is in the bundle");
    println!(
        "child declared kind = {:?} (it is a computation object)",
        child_descriptor.kind
    );
    assert_eq!(bundle.index.objects.len(), 2, "root + child");
    assert_eq!(
        bundle
            .index
            .objects
            .iter()
            .filter(|descriptor| descriptor.reference == grandchild.as_str())
            .count(),
        0,
        "the child's residual is not in the bundle"
    );
    assert_eq!(child_descriptor.kind, BundleObjectKind::Content);

    // 2. Round-trip and import. import_bundle accepts: declared == traversed.
    let bytes = encode_bundle(&bundle).expect("encode");
    let decoded = decode_bundle(&bytes).expect("decode");
    let destination = MemoryObjectStore::default();
    let imported = import_bundle(&decoded, &destination, &registry).expect("import accepted");
    assert_eq!(imported, root_ref);
    println!("import accepted the two-object bundle");

    // 3. The accepted bundle is not closed. The child computation resolves, it
    //    names the grandchild, and the grandchild is absent from the destination.
    let resolved_child =
        resolve_computation(&destination, &child_ref).expect("the child computation resolves");
    assert_eq!(resolved_child.object().residual, grandchild);
    assert!(
        !destination.contains(&grandchild),
        "the imported closure omits an object its own computation references"
    );
    println!("the imported closure omits {grandchild} (referenced by the child)");

    // 4. Control: traversing from the child as a root does find the grandchild,
    //    so the traversal itself is sound; only the shared result map truncates.
    let from_child = export_bundle(&child_ref, &source, &registry).expect("export child closure");
    assert_eq!(from_child.index.objects.len(), 2, "child + grandchild");
    println!("closure(child) objects = {}", from_child.index.objects.len());

    println!("BUG REPRODUCED: v2 closure short-circuits a computation whose content ref is a residual");
}
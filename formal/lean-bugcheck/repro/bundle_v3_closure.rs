//! Declared closure == reachable closure, in both directions, on the real
//! portable-bundle validator.
//!
//! validate_portable_application_closure checks declared (the index descriptors)
//! against reachable (the traversal from the four bundle roots) and
//! reachable_portable_application_objects fails when the traversal arrives at a
//! reference no descriptor declares. Both checks are exercised here with the
//! production code, using schema-matched extractors so the closure mechanism is
//! the one the real registry drives.
//!
//! Run: see formal/lean-bugcheck/run.sh

use std::sync::Arc;

use ato_computation::ContentRef;
use ato_objects::{
    PortableApplicationBundle, PortableBundleError, PortableBundleIndex,
    PortableBundleObjectDescriptor, PortableBundleObjectKind, PortableBundlePayload,
    PortableReferenceExtractor, PortableReferenceRegistry, encode_portable_application_bundle,
    validate_portable_application_closure,
};
use ato_portable_application::bundle_sha256;

const CONTRACT: &str = "ato.contract/1";
const APPLICATION: &str = "ato.application/1";
const DERIVATION: &str = "ato.derivation/1";

struct Leaf(&'static str);

impl PortableReferenceExtractor for Leaf {
    fn schema(&self) -> &str {
        self.0
    }

    fn outgoing(&self, _bytes: &[u8]) -> Result<Vec<ContentRef>, PortableBundleError> {
        Ok(Vec::new())
    }
}

struct DerivationEmits(&'static str, Option<ContentRef>);

impl PortableReferenceExtractor for DerivationEmits {
    fn schema(&self) -> &str {
        self.0
    }

    fn outgoing(&self, _bytes: &[u8]) -> Result<Vec<ContentRef>, PortableBundleError> {
        Ok(self.1.clone().into_iter().collect())
    }
}

fn structured(schema: &'static str) -> (String, Vec<u8>) {
    let bytes = format!("{{\"schema\":\"{schema}\"}}").into_bytes();
    (bundle_sha256(&bytes), bytes)
}

fn blob(bytes: &[u8]) -> (String, Vec<u8>) {
    (bundle_sha256(bytes), bytes.to_vec())
}

fn bundle(entries: Vec<(String, PortableBundleObjectKind, Option<&'static str>, Vec<u8>)>, derivations: Vec<String>) -> PortableApplicationBundle {
    let mut entries = entries;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let contract = bundle_sha256(format!("{{\"schema\":\"{CONTRACT}\"}}").as_bytes());
    let application = bundle_sha256(format!("{{\"schema\":\"{APPLICATION}\"}}").as_bytes());
    PortableApplicationBundle {
        index: PortableBundleIndex {
            version: ato_objects::PORTABLE_APPLICATION_BUNDLE_VERSION,
            profile: ato_objects::PORTABLE_APPLICATION_PROFILE.to_owned(),
            root_contract_ref: contract,
            application_ref: application,
            instance_snapshot_ref: None,
            derivations,
            objects: entries
                .iter()
                .map(|(reference, kind, schema, bytes)| PortableBundleObjectDescriptor {
                    reference: reference.clone(),
                    kind: *kind,
                    schema: schema.map(ToOwned::to_owned),
                    size: bytes.len() as u64,
                })
                .collect(),
        },
        payloads: entries
            .into_iter()
            .map(|(reference, _, _, bytes)| PortableBundlePayload {
                reference,
                bytes: base64_encode(&bytes),
            })
            .collect(),
        portability: None,
        signatures: Vec::new(),
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        output.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        output.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn registry(tree: Option<ContentRef>) -> PortableReferenceRegistry {
    let mut registry = PortableReferenceRegistry::default();
    registry.register(Arc::new(Leaf(CONTRACT))).unwrap();
    registry.register(Arc::new(Leaf(APPLICATION))).unwrap();
    registry
        .register(Arc::new(DerivationEmits(DERIVATION, tree)))
        .unwrap();
    registry
}

fn roots(derivation: String) -> (Vec<(String, PortableBundleObjectKind, Option<&'static str>, Vec<u8>)>, Vec<String>) {
    let (contract, contract_bytes) = structured(CONTRACT);
    let (application, application_bytes) = structured(APPLICATION);
    let (derivation_ref, derivation_bytes) = (
        derivation,
        format!("{{\"schema\":\"{DERIVATION}\"}}").into_bytes(),
    );
    (
        vec![
            (contract, PortableBundleObjectKind::Structured, Some(CONTRACT), contract_bytes),
            (application, PortableBundleObjectKind::Structured, Some(APPLICATION), application_bytes),
            (derivation_ref.clone(), PortableBundleObjectKind::Structured, Some(DERIVATION), derivation_bytes),
        ],
        vec![derivation_ref],
    )
}

fn main() {
    let (tree_ref, tree_bytes) = blob(b"portable tree bytes");
    let tree = ContentRef::parse(tree_ref.clone()).unwrap();
    let (base_entries, derivations) = roots(bundle_sha256(format!("{{\"schema\":\"{DERIVATION}\"}}").as_bytes()));

    // 1. Declared closure is exactly the reachable closure: the derivation names
    //    the tree, and the tree is declared.
    let mut entries = base_entries.clone();
    entries.push((tree_ref.clone(), PortableBundleObjectKind::Blob, None, tree_bytes.clone()));
    let good = bundle(entries, derivations.clone());
    encode_portable_application_bundle(&good).expect("a complete bundle encodes");
    validate_portable_application_closure(&good, &registry(Some(tree.clone())))
        .expect("a complete bundle validates");
    println!("complete closure accepted");

    // 2. An object the traversal never reaches is refused.
    let (orphan_ref, orphan_bytes) = blob(b"never referenced");
    let mut entries = base_entries.clone();
    entries.push((tree_ref.clone(), PortableBundleObjectKind::Blob, None, tree_bytes.clone()));
    entries.push((orphan_ref, PortableBundleObjectKind::Blob, None, orphan_bytes));
    let with_orphan = bundle(entries, derivations.clone());
    match validate_portable_application_closure(&with_orphan, &registry(Some(tree.clone()))) {
        Err(PortableBundleError::UnreachableObject(_)) => {
            println!("unreachable declared object refused")
        }
        other => panic!("expected UnreachableObject, got {other:?}"),
    }

    // 3. A reference the traversal reaches but no descriptor declares is refused.
    let missing = ContentRef::parse(bundle_sha256(b"absent")).unwrap();
    let without_tree = bundle(base_entries.clone(), derivations.clone());
    match validate_portable_application_closure(&without_tree, &registry(Some(missing))) {
        Err(PortableBundleError::IncompleteClosure(reference)) => {
            println!("incomplete closure refused for {reference}")
        }
        other => panic!("expected IncompleteClosure, got {other:?}"),
    }

    // 4. A structured object with no registered extractor is refused rather than
    //    skipped, so an unknown schema cannot hide its own references.
    let unknown_bytes = b"{\"schema\":\"ato.derivation/9\"}".to_vec();
    let unknown_ref = bundle_sha256(&unknown_bytes);
    let (contract, contract_bytes) = structured(CONTRACT);
    let (application, application_bytes) = structured(APPLICATION);
    let unknown = bundle(
        vec![
            (contract, PortableBundleObjectKind::Structured, Some(CONTRACT), contract_bytes),
            (application, PortableBundleObjectKind::Structured, Some(APPLICATION), application_bytes),
            (
                unknown_ref.clone(),
                PortableBundleObjectKind::Structured,
                Some("ato.derivation/9"),
                unknown_bytes,
            ),
        ],
        vec![unknown_ref],
    );
    match validate_portable_application_closure(&unknown, &registry(None)) {
        Err(PortableBundleError::MissingExtractor(schema)) => {
            println!("unknown structured schema refused: {schema}")
        }
        other => panic!("expected MissingExtractor, got {other:?}"),
    }

    println!("CLOSURE EQUALITY CONFIRMED: declared == reachable, both directions, unknown schema fails closed");
}
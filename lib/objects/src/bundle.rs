use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use ato_computation::{ComputationRef, ContentRef, ResolvedComputation, SemanticsId};
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    MemoryObjectStore, ObjectError, ObjectResolver, ObjectStore, read_exact_object,
    resolve_computation,
};

pub const BUNDLE_VERSION: u32 = 2;
const MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_BUNDLE_OBJECTS: usize = 100_000;
const MAX_BUNDLE_OBJECT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_GRAPH_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_GRAPH_OBJECTS: usize = 10_000;
const MAX_GRAPH_LOGICAL_BYTES: u64 = 16 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectLink {
    Computation(ComputationRef),
    Content(ContentRef),
}

/// Semantics-specific closure discovery. `ato-objects` owns traversal but
/// deliberately knows nothing about concrete residual encodings.
pub trait ComputationReferences: Send + Sync {
    fn semantics(&self) -> &SemanticsId;

    fn outgoing(
        &self,
        computation: &ResolvedComputation,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError>;
}

/// Materializer-specific closure discovery without teaching Objects concrete
/// replay, snapshot, or protocol schemas.
pub trait MaterializationReferences: Send + Sync {
    fn materializer_id(&self) -> &str;

    fn outgoing(
        &self,
        descriptor: &ContentRef,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError>;
}

#[derive(Default)]
pub struct ReferenceRegistry {
    extractors: BTreeMap<SemanticsId, Arc<dyn ComputationReferences>>,
    materializers: BTreeMap<String, Arc<dyn MaterializationReferences>>,
}

impl ReferenceRegistry {
    pub fn register(
        &mut self,
        extractor: Arc<dyn ComputationReferences>,
    ) -> Result<(), BundleError> {
        let semantics = extractor.semantics().clone();
        if self
            .extractors
            .insert(semantics.clone(), extractor)
            .is_some()
        {
            return Err(BundleError::DuplicateSemantics(semantics));
        }
        Ok(())
    }

    fn get(&self, semantics: &SemanticsId) -> Option<&dyn ComputationReferences> {
        self.extractors.get(semantics).map(Arc::as_ref)
    }

    pub fn register_materializer(
        &mut self,
        extractor: Arc<dyn MaterializationReferences>,
    ) -> Result<(), BundleError> {
        let id = extractor.materializer_id().to_owned();
        if self.materializers.insert(id.clone(), extractor).is_some() {
            return Err(BundleError::DuplicateMaterializer(id));
        }
        Ok(())
    }

    fn materializer(&self, id: &str) -> Option<&dyn MaterializationReferences> {
        self.materializers.get(id).map(Arc::as_ref)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleObjectKind {
    Computation,
    Content,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleObjectDescriptor {
    pub reference: String,
    pub size: u64,
    pub kind: BundleObjectKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleSignature {
    pub public_key: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleMaterialization {
    pub materializer_id: String,
    pub descriptor_ref: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphRestoreCapability {
    Supported,
    VerifyOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphMaterialization {
    pub id: String,
    pub descriptor_ref: String,
    pub restore_capability: GraphRestoreCapability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphObjectKind {
    Computation,
    Materialization,
    Payload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphObjectDescriptor {
    pub content_ref: String,
    pub size_bytes: u64,
    pub kind: GraphObjectKind,
    pub references: Vec<String>,
}

/// Reachable object closure for the large-object transport lane.
///
/// `root_computation_ref` is supplied by the caller and is never derived from
/// the descriptor list, materialization bytes, or transport index ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectGraphClosure {
    pub root_computation_ref: String,
    pub objects: Vec<GraphObjectDescriptor>,
    pub materializations: Vec<GraphMaterialization>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleIndex {
    pub version: u32,
    pub root: String,
    pub objects: Vec<BundleObjectDescriptor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materializations: Vec<BundleMaterialization>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<BundleSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundlePayload {
    reference: String,
    bytes: String,
}

/// Portable `.capsule` bytes are a transport envelope. Their identity is
/// always `index.root`, never the envelope digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleBundle {
    pub index: BundleIndex,
    payloads: Vec<BundlePayload>,
}

#[derive(Debug, Error)]
pub enum BundleError {
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error("bundle JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bundle is {actual} bytes; maximum is {maximum}")]
    BundleTooLarge { actual: u64, maximum: u64 },
    #[error("bundle contains too many objects: {0}")]
    TooManyObjects(usize),
    #[error("unsupported bundle version {0}")]
    UnsupportedVersion(u32),
    #[error("invalid bundle reference `{value}`: {reason}")]
    InvalidReference { value: String, reason: String },
    #[error("duplicate object `{0}`")]
    DuplicateObject(String),
    #[error("object `{reference}` has declared size {size}; maximum is {maximum}")]
    OversizedObject {
        reference: String,
        size: u64,
        maximum: u64,
    },
    #[error("bundle descriptor/payload mismatch for `{0}`")]
    DescriptorMismatch(String),
    #[error("bundle closure is incomplete; missing `{0}`")]
    IncompleteClosure(ContentRef),
    #[error("bundle contains unreachable object `{0}`")]
    UnreachableObject(ContentRef),
    #[error("no reference extractor registered for semantics `{0}`")]
    MissingExtractor(SemanticsId),
    #[error("reference extractor already registered for semantics `{0}`")]
    DuplicateSemantics(SemanticsId),
    #[error("no reference extractor registered for materializer `{0}`")]
    MissingMaterializer(String),
    #[error("reference extractor already registered for materializer `{0}`")]
    DuplicateMaterializer(String),
    #[error("bundle contains duplicate materializer `{0}`")]
    DuplicateMaterialization(String),
    #[error("bundle signature is malformed")]
    MalformedSignature,
    #[error("bundle signature verification failed")]
    SignatureFailure,
    #[error("bundle has a path-like object reference; paths are forbidden")]
    PathLikeReference,
}

pub fn bundle_root(bundle: &CapsuleBundle) -> Result<ComputationRef, BundleError> {
    parse_computation(&bundle.index.root)
}

pub fn export_bundle(
    root: &ComputationRef,
    objects: &dyn ObjectResolver,
    references: &ReferenceRegistry,
) -> Result<CapsuleBundle, BundleError> {
    export_bundle_with_materializations(root, &[], objects, references)
}

pub fn export_bundle_with_materializations(
    root: &ComputationRef,
    materializations: &[BundleMaterialization],
    objects: &dyn ObjectResolver,
    references: &ReferenceRegistry,
) -> Result<CapsuleBundle, BundleError> {
    let closure = closure(root, materializations, objects, references)?;
    let mut descriptors = Vec::with_capacity(closure.len());
    let mut payloads = Vec::with_capacity(closure.len());
    for (reference, kind) in closure {
        let metadata = objects.metadata(&reference)?;
        if metadata.size > MAX_BUNDLE_OBJECT_BYTES {
            return Err(BundleError::OversizedObject {
                reference: reference.to_string(),
                size: metadata.size,
                maximum: MAX_BUNDLE_OBJECT_BYTES,
            });
        }
        let bytes = read_exact_object(objects, &reference, metadata.size, MAX_BUNDLE_OBJECT_BYTES)?;
        descriptors.push(BundleObjectDescriptor {
            reference: reference.to_string(),
            size: metadata.size,
            kind,
        });
        payloads.push(BundlePayload {
            reference: reference.to_string(),
            bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    }
    Ok(CapsuleBundle {
        index: BundleIndex {
            version: BUNDLE_VERSION,
            root: root.to_string(),
            objects: descriptors,
            materializations: materializations.to_vec(),
            signatures: Vec::new(),
        },
        payloads,
    })
}

pub fn export_object_graph(
    root: &ComputationRef,
    materializations: &[GraphMaterialization],
    objects: &dyn ObjectResolver,
    references: &ReferenceRegistry,
) -> Result<ObjectGraphClosure, BundleError> {
    let mut materializer_ids = BTreeSet::new();
    let mut descriptors = BTreeMap::new();
    for materialization in materializations {
        if !materializer_ids.insert(materialization.id.clone()) {
            return Err(BundleError::DuplicateMaterialization(
                materialization.id.clone(),
            ));
        }
        reject_path(&materialization.descriptor_ref)?;
        let descriptor = parse_content(&materialization.descriptor_ref)?;
        if descriptors
            .insert(descriptor, materialization.id.clone())
            .is_some()
        {
            return Err(BundleError::DuplicateObject(
                materialization.descriptor_ref.clone(),
            ));
        }
    }

    let mut queue = VecDeque::from([ObjectLink::Computation(root.clone())]);
    let mut graph = BTreeMap::<ContentRef, (GraphObjectKind, u64, BTreeSet<ContentRef>)>::new();
    while let Some(link) = queue.pop_front() {
        let reference = match &link {
            ObjectLink::Computation(computation) => computation.content_ref().clone(),
            ObjectLink::Content(content) => content.clone(),
        };
        if graph.contains_key(&reference) {
            continue;
        }
        if graph.len() >= MAX_GRAPH_OBJECTS {
            return Err(BundleError::TooManyObjects(graph.len() + 1));
        }
        let metadata = objects.metadata(&reference)?;
        if metadata.size > MAX_GRAPH_OBJECT_BYTES {
            return Err(BundleError::OversizedObject {
                reference: reference.to_string(),
                size: metadata.size,
                maximum: MAX_GRAPH_OBJECT_BYTES,
            });
        }

        let (kind, outgoing) = match link {
            ObjectLink::Computation(computation) => {
                let resolved = resolve_computation(objects, &computation)?;
                let extractor = references
                    .get(&resolved.object().semantics)
                    .ok_or_else(|| {
                        BundleError::MissingExtractor(resolved.object().semantics.clone())
                    })?;
                let mut outgoing = extractor.outgoing(&resolved, objects)?;
                outgoing.push(ObjectLink::Content(resolved.object().residual.clone()));
                if &computation == root {
                    outgoing.extend(descriptors.keys().cloned().map(ObjectLink::Content));
                }
                (GraphObjectKind::Computation, outgoing)
            }
            ObjectLink::Content(content) => match descriptors.get(&content) {
                Some(materializer_id) => {
                    let extractor = references
                        .materializer(materializer_id)
                        .ok_or_else(|| BundleError::MissingMaterializer(materializer_id.clone()))?;
                    (
                        GraphObjectKind::Materialization,
                        extractor.outgoing(&content, objects)?,
                    )
                }
                None => (GraphObjectKind::Payload, Vec::new()),
            },
        };
        let outgoing_refs = outgoing
            .iter()
            .map(|link| match link {
                ObjectLink::Computation(computation) => computation.content_ref().clone(),
                ObjectLink::Content(content) => content.clone(),
            })
            .collect::<BTreeSet<_>>();
        queue.extend(outgoing);
        graph.insert(reference, (kind, metadata.size, outgoing_refs));
    }

    let logical_bytes = graph.values().try_fold(0_u64, |total, (_, size, _)| {
        total.checked_add(*size).ok_or(BundleError::BundleTooLarge {
            actual: u64::MAX,
            maximum: MAX_GRAPH_LOGICAL_BYTES,
        })
    })?;
    if logical_bytes > MAX_GRAPH_LOGICAL_BYTES {
        return Err(BundleError::BundleTooLarge {
            actual: logical_bytes,
            maximum: MAX_GRAPH_LOGICAL_BYTES,
        });
    }

    Ok(ObjectGraphClosure {
        root_computation_ref: root.to_string(),
        objects: graph
            .into_iter()
            .map(
                |(reference, (kind, size_bytes, references))| GraphObjectDescriptor {
                    content_ref: reference.to_string(),
                    size_bytes,
                    kind,
                    references: references
                        .into_iter()
                        .map(|item| item.to_string())
                        .collect(),
                },
            )
            .collect(),
        materializations: materializations.to_vec(),
    })
}

pub fn sign_bundle(
    bundle: &mut CapsuleBundle,
    signing_key: &SigningKey,
) -> Result<(), BundleError> {
    validate_shape(bundle)?;
    let signature = signing_key.sign(&unsigned_index_bytes(&bundle.index)?);
    bundle.index.signatures.push(BundleSignature {
        public_key: hex::encode(signing_key.verifying_key().to_bytes()),
        signature: hex::encode(signature.to_bytes()),
    });
    bundle
        .index
        .signatures
        .sort_by(|left, right| left.public_key.cmp(&right.public_key));
    Ok(())
}

pub fn encode_bundle(bundle: &CapsuleBundle) -> Result<Vec<u8>, BundleError> {
    validate_shape(bundle)?;
    let bytes = serde_jcs::to_vec(bundle)?;
    ensure_bundle_size(bytes.len() as u64)?;
    Ok(bytes)
}

pub fn decode_bundle(bytes: &[u8]) -> Result<CapsuleBundle, BundleError> {
    ensure_bundle_size(bytes.len() as u64)?;
    let bundle: CapsuleBundle = serde_json::from_slice(bytes)?;
    validate_shape(&bundle)?;
    if serde_jcs::to_vec(&bundle)? != bytes {
        return Err(BundleError::Json(serde_json::Error::io(
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bundle is not canonical JCS",
            ),
        )));
    }
    Ok(bundle)
}

pub fn import_bundle(
    bundle: &CapsuleBundle,
    destination: &dyn ObjectStore,
    references: &ReferenceRegistry,
) -> Result<ComputationRef, BundleError> {
    validate_shape(bundle)?;
    verify_signatures(&bundle.index)?;
    let root = bundle_root(bundle)?;
    let staging = MemoryObjectStore::default();
    for (descriptor, payload) in bundle.index.objects.iter().zip(&bundle.payloads) {
        let reference = parse_content(&descriptor.reference)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&payload.bytes)
            .map_err(|_| BundleError::DescriptorMismatch(descriptor.reference.clone()))?;
        if bytes.len() as u64 != descriptor.size {
            return Err(BundleError::DescriptorMismatch(
                descriptor.reference.clone(),
            ));
        }
        staging.insert(&reference, &bytes)?;
    }

    let reachable =
        closure(&root, &bundle.index.materializations, &staging, references).map_err(|error| {
            match error {
                BundleError::Object(ObjectError::NotFound(reference)) => {
                    BundleError::IncompleteClosure(reference)
                }
                other => other,
            }
        })?;
    let declared: BTreeSet<_> = bundle
        .index
        .objects
        .iter()
        .map(|descriptor| parse_content(&descriptor.reference))
        .collect::<Result<_, _>>()?;
    let actual: BTreeSet<_> = reachable.keys().cloned().collect();
    if let Some(missing) = actual.difference(&declared).next() {
        return Err(BundleError::IncompleteClosure((*missing).clone()));
    }
    if let Some(extra) = declared.difference(&actual).next() {
        return Err(BundleError::UnreachableObject((*extra).clone()));
    }

    for descriptor in &bundle.index.objects {
        let reference = parse_content(&descriptor.reference)?;
        let metadata = staging.metadata(&reference)?;
        let bytes =
            read_exact_object(&staging, &reference, metadata.size, MAX_BUNDLE_OBJECT_BYTES)?;
        destination.insert(&reference, &bytes)?;
    }
    Ok(root)
}

pub(crate) fn closure(
    root: &ComputationRef,
    materializations: &[BundleMaterialization],
    objects: &dyn ObjectResolver,
    references: &ReferenceRegistry,
) -> Result<BTreeMap<ContentRef, BundleObjectKind>, BundleError> {
    let mut queue = VecDeque::from([ObjectLink::Computation(root.clone())]);
    for materialization in materializations {
        let descriptor = parse_content(&materialization.descriptor_ref)?;
        queue.push_back(ObjectLink::Content(descriptor.clone()));
        let extractor = references
            .materializer(&materialization.materializer_id)
            .ok_or_else(|| {
                BundleError::MissingMaterializer(materialization.materializer_id.clone())
            })?;
        queue.extend(extractor.outgoing(&descriptor, objects)?);
    }
    let mut result = BTreeMap::new();
    while let Some(link) = queue.pop_front() {
        if result.len() >= MAX_BUNDLE_OBJECTS {
            return Err(BundleError::TooManyObjects(result.len() + 1));
        }
        match link {
            ObjectLink::Content(reference) => {
                if result
                    .insert(reference.clone(), BundleObjectKind::Content)
                    .is_none()
                {
                    objects.metadata(&reference)?;
                }
            }
            ObjectLink::Computation(reference) => {
                let content = reference.content_ref().clone();
                if result.contains_key(&content) {
                    continue;
                }
                let resolved = resolve_computation(objects, &reference)?;
                result.insert(content, BundleObjectKind::Computation);
                let residual = resolved.object().residual.clone();
                if result
                    .insert(residual.clone(), BundleObjectKind::Content)
                    .is_none()
                {
                    objects.metadata(&residual)?;
                }
                let extractor = references
                    .get(&resolved.object().semantics)
                    .ok_or_else(|| {
                        BundleError::MissingExtractor(resolved.object().semantics.clone())
                    })?;
                queue.extend(extractor.outgoing(&resolved, objects)?);
            }
        }
    }
    Ok(result)
}

fn validate_shape(bundle: &CapsuleBundle) -> Result<(), BundleError> {
    if bundle.index.version != BUNDLE_VERSION {
        return Err(BundleError::UnsupportedVersion(bundle.index.version));
    }
    parse_computation(&bundle.index.root)?;
    let mut materializers = BTreeSet::new();
    for materialization in &bundle.index.materializations {
        reject_path(&materialization.descriptor_ref)?;
        parse_content(&materialization.descriptor_ref)?;
        if !materializers.insert(materialization.materializer_id.clone()) {
            return Err(BundleError::DuplicateMaterialization(
                materialization.materializer_id.clone(),
            ));
        }
    }
    if bundle.index.objects.len() > MAX_BUNDLE_OBJECTS {
        return Err(BundleError::TooManyObjects(bundle.index.objects.len()));
    }
    if bundle.index.objects.len() != bundle.payloads.len() {
        return Err(BundleError::DescriptorMismatch("object count".to_owned()));
    }
    let mut seen = BTreeSet::new();
    for (descriptor, payload) in bundle.index.objects.iter().zip(&bundle.payloads) {
        reject_path(&descriptor.reference)?;
        let reference = parse_content(&descriptor.reference)?;
        if !seen.insert(reference) {
            return Err(BundleError::DuplicateObject(descriptor.reference.clone()));
        }
        if payload.reference != descriptor.reference {
            return Err(BundleError::DescriptorMismatch(
                descriptor.reference.clone(),
            ));
        }
        if descriptor.size > MAX_BUNDLE_OBJECT_BYTES {
            return Err(BundleError::OversizedObject {
                reference: descriptor.reference.clone(),
                size: descriptor.size,
                maximum: MAX_BUNDLE_OBJECT_BYTES,
            });
        }
    }
    Ok(())
}

fn verify_signatures(index: &BundleIndex) -> Result<(), BundleError> {
    let bytes = unsigned_index_bytes(index)?;
    for signed in &index.signatures {
        let public: [u8; 32] = hex::decode(&signed.public_key)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(BundleError::MalformedSignature)?;
        let signature: [u8; 64] = hex::decode(&signed.signature)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(BundleError::MalformedSignature)?;
        VerifyingKey::from_bytes(&public)
            .map_err(|_| BundleError::MalformedSignature)?
            .verify(&bytes, &Signature::from_bytes(&signature))
            .map_err(|_| BundleError::SignatureFailure)?;
    }
    Ok(())
}

fn unsigned_index_bytes(index: &BundleIndex) -> Result<Vec<u8>, BundleError> {
    let mut unsigned = index.clone();
    unsigned.signatures.clear();
    Ok(serde_jcs::to_vec(&unsigned)?)
}

fn reject_path(value: &str) -> Result<(), BundleError> {
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(BundleError::PathLikeReference);
    }
    Ok(())
}

fn parse_content(value: &str) -> Result<ContentRef, BundleError> {
    ContentRef::parse(value).map_err(|error| BundleError::InvalidReference {
        value: value.to_owned(),
        reason: error.to_string(),
    })
}

fn parse_computation(value: &str) -> Result<ComputationRef, BundleError> {
    ComputationRef::parse(value).map_err(|error| BundleError::InvalidReference {
        value: value.to_owned(),
        reason: error.to_string(),
    })
}

fn ensure_bundle_size(actual: u64) -> Result<(), BundleError> {
    if actual > MAX_BUNDLE_BYTES {
        return Err(BundleError::BundleTooLarge {
            actual,
            maximum: MAX_BUNDLE_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use ato_computation::{
        ComputationObject, SemanticsId, computation_ref, encode_computation_object,
    };

    use super::*;

    struct LeafReferences(SemanticsId);

    impl ComputationReferences for LeafReferences {
        fn semantics(&self) -> &SemanticsId {
            &self.0
        }

        fn outgoing(
            &self,
            _computation: &ResolvedComputation,
            _objects: &dyn ObjectResolver,
        ) -> Result<Vec<ObjectLink>, BundleError> {
            Ok(Vec::new())
        }
    }

    struct TestMaterializationReferences;

    impl MaterializationReferences for TestMaterializationReferences {
        fn materializer_id(&self) -> &str {
            "ato.materialize.vm.snapshot@1"
        }

        fn outgoing(
            &self,
            descriptor: &ContentRef,
            objects: &dyn ObjectResolver,
        ) -> Result<Vec<ObjectLink>, BundleError> {
            let metadata = objects.metadata(descriptor)?;
            let bytes = read_exact_object(objects, descriptor, metadata.size, 1024)?;
            let references: Vec<String> = serde_json::from_slice(&bytes)?;
            references
                .into_iter()
                .map(|reference| parse_content(&reference).map(ObjectLink::Content))
                .collect()
        }
    }

    fn fixture() -> (MemoryObjectStore, ReferenceRegistry, ComputationRef) {
        let objects = MemoryObjectStore::default();
        let semantics = SemanticsId::parse("example.bundle@1").unwrap();
        let residual = objects.put(b"future").unwrap();
        let computation = ComputationObject {
            semantics: semantics.clone(),
            boundary: BTreeMap::new(),
            residual,
        };
        let root = computation_ref(&computation).unwrap();
        objects
            .insert(
                root.content_ref(),
                &encode_computation_object(&computation).unwrap(),
            )
            .unwrap();
        let mut registry = ReferenceRegistry::default();
        registry
            .register(Arc::new(LeafReferences(semantics)))
            .unwrap();
        (objects, registry, root)
    }

    #[test]
    fn capsule_bundle_round_trips_with_root_identity_unchanged() {
        let (source, references, root) = fixture();
        let mut bundle = export_bundle(&root, &source, &references).unwrap();
        sign_bundle(&mut bundle, &SigningKey::from_bytes(&[7; 32])).unwrap();
        let bytes = encode_bundle(&bundle).unwrap();
        let decoded = decode_bundle(&bytes).unwrap();
        let destination = MemoryObjectStore::default();

        let imported = import_bundle(&decoded, &destination, &references).unwrap();

        assert_eq!(imported, root);
        assert!(destination.contains(root.content_ref()));
    }

    #[test]
    fn object_graph_keeps_computation_identity_separate_from_vm_bytes() {
        let (objects, mut references, root) = fixture();
        references
            .register_materializer(Arc::new(TestMaterializationReferences))
            .unwrap();
        let first_vm = objects.put(b"first VM bytes").unwrap();
        let second_vm = objects.put(b"different VM bytes").unwrap();
        let first_descriptor = objects
            .put(&serde_jcs::to_vec(&vec![first_vm.to_string()]).unwrap())
            .unwrap();
        let second_descriptor = objects
            .put(&serde_jcs::to_vec(&vec![second_vm.to_string()]).unwrap())
            .unwrap();
        let materialization = |descriptor: &ContentRef| GraphMaterialization {
            id: "ato.materialize.vm.snapshot@1".to_owned(),
            descriptor_ref: descriptor.to_string(),
            restore_capability: GraphRestoreCapability::Supported,
        };

        let first = export_object_graph(
            &root,
            &[materialization(&first_descriptor)],
            &objects,
            &references,
        )
        .unwrap();
        let second = export_object_graph(
            &root,
            &[materialization(&second_descriptor)],
            &objects,
            &references,
        )
        .unwrap();

        assert_eq!(first.root_computation_ref, root.to_string());
        assert_eq!(second.root_computation_ref, root.to_string());
        assert_ne!(first.objects, second.objects);
        let root_object = first
            .objects
            .iter()
            .find(|object| object.content_ref == root.to_string())
            .unwrap();
        assert!(
            root_object
                .references
                .contains(&first_descriptor.to_string())
        );
        assert!(first.objects.iter().any(|object| {
            object.content_ref == first_descriptor.to_string()
                && object.kind == GraphObjectKind::Materialization
                && object.references == vec![first_vm.to_string()]
        }));
    }

    #[test]
    fn duplicate_object_is_rejected() {
        let (source, references, root) = fixture();
        let mut bundle = export_bundle(&root, &source, &references).unwrap();
        bundle.index.objects.push(bundle.index.objects[0].clone());
        bundle.payloads.push(bundle.payloads[0].clone());
        assert!(matches!(
            validate_shape(&bundle),
            Err(BundleError::DuplicateObject(_))
        ));
    }

    #[test]
    fn path_like_reference_is_rejected_before_loading() {
        let (source, references, root) = fixture();
        let mut bundle = export_bundle(&root, &source, &references).unwrap();
        bundle.index.objects[0].reference = "../object".to_owned();
        bundle.payloads[0].reference = "../object".to_owned();
        assert!(matches!(
            validate_shape(&bundle),
            Err(BundleError::PathLikeReference)
        ));
    }

    #[test]
    fn oversized_object_is_rejected_from_the_index() {
        let (source, references, root) = fixture();
        let mut bundle = export_bundle(&root, &source, &references).unwrap();
        bundle.index.objects[0].size = MAX_BUNDLE_OBJECT_BYTES + 1;
        assert!(matches!(
            validate_shape(&bundle),
            Err(BundleError::OversizedObject { .. })
        ));
    }

    #[test]
    fn hash_mismatch_is_rejected_without_partial_import() {
        let (source, references, root) = fixture();
        let mut bundle = export_bundle(&root, &source, &references).unwrap();
        bundle.payloads[0].bytes = base64::engine::general_purpose::STANDARD.encode(b"tampered");
        bundle.index.objects[0].size = 8;
        let destination = MemoryObjectStore::default();
        assert!(matches!(
            import_bundle(&bundle, &destination, &references),
            Err(BundleError::Object(ObjectError::IdentityMismatch { .. }))
        ));
        assert!(!destination.contains(root.content_ref()));
    }

    #[test]
    fn incomplete_closure_is_rejected() {
        let (source, references, root) = fixture();
        let mut bundle = export_bundle(&root, &source, &references).unwrap();
        let residual_index = bundle
            .index
            .objects
            .iter()
            .position(|object| object.kind == BundleObjectKind::Content)
            .unwrap();
        bundle.index.objects.remove(residual_index);
        bundle.payloads.remove(residual_index);
        assert!(matches!(
            import_bundle(&bundle, &MemoryObjectStore::default(), &references),
            Err(BundleError::IncompleteClosure(_))
        ));
    }

    #[test]
    fn signature_failure_is_rejected() {
        let (source, references, root) = fixture();
        let mut bundle = export_bundle(&root, &source, &references).unwrap();
        sign_bundle(&mut bundle, &SigningKey::from_bytes(&[9; 32])).unwrap();
        bundle.index.objects[0].size += 1;
        assert!(matches!(
            import_bundle(&bundle, &MemoryObjectStore::default(), &references),
            Err(BundleError::SignatureFailure)
        ));
    }

    #[test]
    fn malformed_or_noncanonical_index_is_rejected() {
        let bytes =
            br#"{"index":{"objects":[],"root":"../x","version":1},"payloads":[],"unknown":true}"#;
        assert!(matches!(decode_bundle(bytes), Err(BundleError::Json(_))));
    }
}

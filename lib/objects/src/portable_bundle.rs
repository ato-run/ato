use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use ato_computation::ContentRef;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{BundleError, CapsuleBundle, decode_bundle};

pub const PORTABLE_APPLICATION_BUNDLE_VERSION: u32 = 3;
pub const PORTABLE_APPLICATION_PROFILE: &str = "ato.portable-application/1";
pub const PORTABLE_APPLICATION_BUNDLE_VERSION_V4: u32 = 4;
pub const PORTABLE_APPLICATION_PROFILE_V2: &str = "ato.portable-application/2";
const MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_BUNDLE_OBJECTS: usize = 10_000;
const MAX_BUNDLE_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DECODED_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortableBundleObjectKind {
    Structured,
    Blob,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableBundleObjectDescriptor {
    pub reference: String,
    pub kind: PortableBundleObjectKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableBundleIndex {
    pub version: u32,
    pub profile: String,
    pub root_contract_ref: String,
    pub application_ref: String,
    pub derivations: Vec<String>,
    pub objects: Vec<PortableBundleObjectDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableBundlePayload {
    pub reference: String,
    pub bytes: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortableDependencyProfile {
    Thin,
    Cached,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableExternalObject {
    pub reference: String,
    pub sources: Vec<String>,
}

/// Transport/cache information only. This object is not referenced by K or D.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableDependencyTransport {
    pub profile: PortableDependencyProfile,
    pub external_objects: Vec<PortableExternalObject>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableApplicationBundle {
    pub index: PortableBundleIndex,
    pub payloads: Vec<PortableBundlePayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portability: Option<PortableDependencyTransport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<serde_json::Value>,
}

#[derive(Debug)]
pub enum CapsuleBundleDocument {
    ComputationV2(CapsuleBundle),
    PortableApplicationV3(PortableApplicationBundle),
    PortableApplicationV4(PortableApplicationBundle),
}

#[derive(Debug, Error)]
pub enum CapsuleBundleDocumentError {
    #[error(transparent)]
    BundleV2(#[from] BundleError),
    #[error(transparent)]
    PortableV3(#[from] PortableBundleError),
    #[error("bundle JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported bundle version {0}")]
    UnsupportedVersion(u32),
}

#[derive(Debug, Error)]
pub enum PortableBundleError {
    #[error("bundle JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bundle is {actual} bytes; maximum is {maximum}")]
    BundleTooLarge { actual: u64, maximum: u64 },
    #[error("bundle contains too many objects: {0}")]
    TooManyObjects(usize),
    #[error("unsupported portable bundle version {0}")]
    UnsupportedVersion(u32),
    #[error("unsupported portable application profile `{0}`")]
    UnsupportedProfile(String),
    #[error("bundle is not canonical JCS")]
    NonCanonical,
    #[error("invalid bundle reference `{value}`: {reason}")]
    InvalidReference { value: String, reason: String },
    #[error("portable application references must use SHA-256: `{0}`")]
    NonSha256Reference(String),
    #[error("bundle object descriptors and payloads must be sorted by reference")]
    NonCanonicalOrder,
    #[error("duplicate object `{0}`")]
    DuplicateObject(String),
    #[error("bundle descriptor/payload mismatch for `{0}`")]
    DescriptorMismatch(String),
    #[error("external object `{0}` is invalid or not a declared blob")]
    InvalidExternalObject(String),
    #[error("offline bundle declares external object `{0}`")]
    OfflineExternalObject(String),
    #[error("object `{reference}` has declared size {size}; maximum is {maximum}")]
    OversizedObject {
        reference: String,
        size: u64,
        maximum: u64,
    },
    #[error("decoded bundle content is {actual} bytes; maximum is {maximum}")]
    DecodedContentTooLarge { actual: u64, maximum: u64 },
    #[error("object digest mismatch for `{0}`")]
    ObjectDigestMismatch(String),
    #[error("structured object `{0}` is not canonical JCS")]
    StructuredObjectNonCanonical(String),
    #[error("structured object `{reference}` declares schema `{declared}` but contains `{actual}`")]
    StructuredSchemaMismatch {
        reference: String,
        declared: String,
        actual: String,
    },
    #[error("object `{0}` has an invalid kind/schema combination")]
    InvalidObjectKind(String),
    #[error("portable application bundles must contain at least one derivation")]
    MissingDerivation,
    #[error("bundle root list contains duplicate reference `{0}`")]
    DuplicateRoot(String),
    #[error("bundle closure is incomplete; missing `{0}`")]
    IncompleteClosure(ContentRef),
    #[error("bundle contains unreachable object `{0}`")]
    UnreachableObject(ContentRef),
    #[error("no reference extractor registered for structured schema `{0}`")]
    MissingExtractor(String),
    #[error("reference extractor already registered for structured schema `{0}`")]
    DuplicateSchema(String),
    #[error("portable application bundle signatures are not supported")]
    UnsupportedSignature,
    #[error("reference extraction for schema `{schema}` failed: {reason}")]
    ReferenceExtraction { schema: String, reason: String },
}

pub trait PortableReferenceExtractor: Send + Sync {
    fn schema(&self) -> &str;
    fn outgoing(&self, bytes: &[u8]) -> Result<Vec<ContentRef>, PortableBundleError>;
}

#[derive(Default)]
pub struct PortableReferenceRegistry {
    extractors: BTreeMap<String, Arc<dyn PortableReferenceExtractor>>,
}

impl PortableReferenceRegistry {
    pub fn register(
        &mut self,
        extractor: Arc<dyn PortableReferenceExtractor>,
    ) -> Result<(), PortableBundleError> {
        let schema = extractor.schema().to_owned();
        if self.extractors.insert(schema.clone(), extractor).is_some() {
            return Err(PortableBundleError::DuplicateSchema(schema));
        }
        Ok(())
    }

    fn get(&self, schema: &str) -> Option<&dyn PortableReferenceExtractor> {
        self.extractors.get(schema).map(Arc::as_ref)
    }
}

pub fn encode_portable_application_bundle(
    bundle: &PortableApplicationBundle,
) -> Result<Vec<u8>, PortableBundleError> {
    validate_shape_and_payloads(bundle)?;
    let bytes = serde_jcs::to_vec(bundle)?;
    ensure_bundle_size(bytes.len() as u64)?;
    Ok(bytes)
}

pub fn decode_portable_application_bundle(
    bytes: &[u8],
) -> Result<PortableApplicationBundle, PortableBundleError> {
    ensure_bundle_size(bytes.len() as u64)?;
    let bundle: PortableApplicationBundle = serde_json::from_slice(bytes)?;
    validate_shape_and_payloads(&bundle)?;
    if serde_jcs::to_vec(&bundle)? != bytes {
        return Err(PortableBundleError::NonCanonical);
    }
    Ok(bundle)
}

pub fn decode_capsule_bundle_document(
    bytes: &[u8],
) -> Result<CapsuleBundleDocument, CapsuleBundleDocumentError> {
    #[derive(Deserialize)]
    struct Envelope {
        index: Version,
    }

    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }

    let version = serde_json::from_slice::<Envelope>(bytes)?.index.version;
    match version {
        2 => Ok(CapsuleBundleDocument::ComputationV2(decode_bundle(bytes)?)),
        PORTABLE_APPLICATION_BUNDLE_VERSION => Ok(CapsuleBundleDocument::PortableApplicationV3(
            decode_portable_application_bundle(bytes)?,
        )),
        PORTABLE_APPLICATION_BUNDLE_VERSION_V4 => Ok(CapsuleBundleDocument::PortableApplicationV4(
            decode_portable_application_bundle(bytes)?,
        )),
        version => Err(CapsuleBundleDocumentError::UnsupportedVersion(version)),
    }
}

pub fn validate_portable_application_closure(
    bundle: &PortableApplicationBundle,
    registry: &PortableReferenceRegistry,
) -> Result<(), PortableBundleError> {
    validate_shape_and_payloads(bundle)?;
    let descriptors = bundle
        .index
        .objects
        .iter()
        .map(|descriptor| Ok((parse_sha256(&descriptor.reference)?, descriptor)))
        .collect::<Result<BTreeMap<_, _>, PortableBundleError>>()?;
    let payloads = bundle
        .payloads
        .iter()
        .map(|payload| Ok((parse_sha256(&payload.reference)?, payload)))
        .collect::<Result<BTreeMap<_, _>, PortableBundleError>>()?;

    let mut queue = VecDeque::new();
    let mut roots = BTreeSet::new();
    for value in std::iter::once(&bundle.index.root_contract_ref)
        .chain(std::iter::once(&bundle.index.application_ref))
        .chain(bundle.index.derivations.iter())
    {
        let reference = parse_sha256(value)?;
        if !roots.insert(reference.clone()) {
            return Err(PortableBundleError::DuplicateRoot(value.clone()));
        }
        queue.push_back(reference);
    }

    let mut reachable = BTreeSet::new();
    while let Some(reference) = queue.pop_front() {
        if !reachable.insert(reference.clone()) {
            continue;
        }
        let descriptor = descriptors
            .get(&reference)
            .ok_or_else(|| PortableBundleError::IncompleteClosure(reference.clone()))?;
        if descriptor.kind == PortableBundleObjectKind::Blob {
            continue;
        }
        let payload = payloads
            .get(&reference)
            .expect("shape validation embeds every structured object");
        let schema = descriptor
            .schema
            .as_deref()
            .expect("shape validation requires a schema for structured objects");
        let extractor = registry
            .get(schema)
            .ok_or_else(|| PortableBundleError::MissingExtractor(schema.to_owned()))?;
        let bytes = decode_payload(payload)?;
        for outgoing in extractor.outgoing(&bytes)? {
            if outgoing.algorithm() != "sha256" {
                return Err(PortableBundleError::NonSha256Reference(
                    outgoing.to_string(),
                ));
            }
            queue.push_back(outgoing);
        }
    }

    if let Some(reference) = descriptors
        .keys()
        .find(|reference| !reachable.contains(*reference))
    {
        return Err(PortableBundleError::UnreachableObject(reference.clone()));
    }
    Ok(())
}

impl PortableApplicationBundle {
    pub fn descriptor(&self, reference: &ContentRef) -> Option<&PortableBundleObjectDescriptor> {
        self.index
            .objects
            .binary_search_by(|descriptor| descriptor.reference.as_str().cmp(reference.as_str()))
            .ok()
            .map(|index| &self.index.objects[index])
    }

    pub fn payload_bytes(&self, reference: &ContentRef) -> Result<Vec<u8>, PortableBundleError> {
        let index = self
            .payloads
            .binary_search_by(|payload| payload.reference.as_str().cmp(reference.as_str()))
            .map_err(|_| PortableBundleError::IncompleteClosure(reference.clone()))?;
        decode_payload(&self.payloads[index])
    }
}

fn validate_shape_and_payloads(
    bundle: &PortableApplicationBundle,
) -> Result<(), PortableBundleError> {
    let external = match bundle.index.version {
        PORTABLE_APPLICATION_BUNDLE_VERSION => {
            if bundle.index.profile != PORTABLE_APPLICATION_PROFILE || bundle.portability.is_some()
            {
                return Err(PortableBundleError::UnsupportedProfile(
                    bundle.index.profile.clone(),
                ));
            }
            &[][..]
        }
        PORTABLE_APPLICATION_BUNDLE_VERSION_V4 => {
            if bundle.index.profile != PORTABLE_APPLICATION_PROFILE_V2 {
                return Err(PortableBundleError::UnsupportedProfile(
                    bundle.index.profile.clone(),
                ));
            }
            let portability = bundle.portability.as_ref().ok_or_else(|| {
                PortableBundleError::DescriptorMismatch("missing portability manifest".to_owned())
            })?;
            if portability.profile == PortableDependencyProfile::Offline {
                if let Some(first) = portability.external_objects.first() {
                    return Err(PortableBundleError::OfflineExternalObject(
                        first.reference.clone(),
                    ));
                }
            }
            portability.external_objects.as_slice()
        }
        version => return Err(PortableBundleError::UnsupportedVersion(version)),
    };
    if bundle.index.objects.len() > MAX_BUNDLE_OBJECTS {
        return Err(PortableBundleError::TooManyObjects(
            bundle.index.objects.len(),
        ));
    }
    if bundle.index.objects.len() != bundle.payloads.len() + external.len() {
        return Err(PortableBundleError::DescriptorMismatch(
            "object count".to_owned(),
        ));
    }
    if bundle.index.derivations.is_empty() {
        return Err(PortableBundleError::MissingDerivation);
    }
    if !bundle.signatures.is_empty() {
        return Err(PortableBundleError::UnsupportedSignature);
    }
    ensure_strictly_sorted(&bundle.index.derivations)?;
    parse_sha256(&bundle.index.root_contract_ref)?;
    parse_sha256(&bundle.index.application_ref)?;

    let descriptor_refs = bundle
        .index
        .objects
        .iter()
        .map(|descriptor| descriptor.reference.as_str())
        .collect::<Vec<_>>();
    let payload_refs = bundle
        .payloads
        .iter()
        .map(|payload| payload.reference.as_str())
        .collect::<Vec<_>>();
    let external_refs = external
        .iter()
        .map(|object| object.reference.as_str())
        .collect::<Vec<_>>();
    if !is_strictly_sorted(&descriptor_refs)
        || !is_strictly_sorted(&payload_refs)
        || !is_strictly_sorted(&external_refs)
    {
        return Err(PortableBundleError::NonCanonicalOrder);
    }
    for value in std::iter::once(&bundle.index.root_contract_ref)
        .chain(std::iter::once(&bundle.index.application_ref))
        .chain(bundle.index.derivations.iter())
    {
        let reference = parse_sha256(value)?;
        let index = descriptor_refs
            .binary_search(&value.as_str())
            .map_err(|_| PortableBundleError::IncompleteClosure(reference))?;
        if bundle.index.objects[index].kind != PortableBundleObjectKind::Structured {
            return Err(PortableBundleError::InvalidObjectKind(value.clone()));
        }
    }

    let mut decoded_total = 0_u64;
    for descriptor in &bundle.index.objects {
        let reference = parse_sha256(&descriptor.reference)?;
        if descriptor.size > MAX_BUNDLE_OBJECT_BYTES {
            return Err(PortableBundleError::OversizedObject {
                reference: descriptor.reference.clone(),
                size: descriptor.size,
                maximum: MAX_BUNDLE_OBJECT_BYTES,
            });
        }
        decoded_total = decoded_total.saturating_add(descriptor.size);
        if decoded_total > MAX_DECODED_BYTES {
            return Err(PortableBundleError::DecodedContentTooLarge {
                actual: decoded_total,
                maximum: MAX_DECODED_BYTES,
            });
        }
        match (&descriptor.kind, &descriptor.schema) {
            (PortableBundleObjectKind::Structured, Some(schema)) if !schema.is_empty() => {}
            (PortableBundleObjectKind::Blob, None) => {}
            _ => {
                return Err(PortableBundleError::InvalidObjectKind(
                    descriptor.reference.clone(),
                ));
            }
        }
        let payload = payload_refs
            .binary_search(&descriptor.reference.as_str())
            .ok()
            .map(|index| &bundle.payloads[index]);
        let source = external_refs
            .binary_search(&descriptor.reference.as_str())
            .ok()
            .map(|index| &external[index]);
        let Some(payload) = payload else {
            let source = source.ok_or_else(|| {
                PortableBundleError::DescriptorMismatch(descriptor.reference.clone())
            })?;
            if descriptor.kind != PortableBundleObjectKind::Blob
                || source.sources.is_empty()
                || source
                    .sources
                    .iter()
                    .any(|url| !url.starts_with("https://") || url.contains('#'))
            {
                return Err(PortableBundleError::InvalidExternalObject(
                    descriptor.reference.clone(),
                ));
            }
            continue;
        };
        if source.is_some() {
            return Err(PortableBundleError::DescriptorMismatch(
                descriptor.reference.clone(),
            ));
        }
        let bytes = decode_payload(payload)?;
        if bytes.len() as u64 != descriptor.size {
            return Err(PortableBundleError::DescriptorMismatch(
                descriptor.reference.clone(),
            ));
        }
        let actual = format!("sha256:{:x}", Sha256::digest(&bytes));
        if actual != reference.as_str() {
            return Err(PortableBundleError::ObjectDigestMismatch(
                descriptor.reference.clone(),
            ));
        }
        if descriptor.kind == PortableBundleObjectKind::Structured {
            validate_structured_object(descriptor, &bytes)?;
        }
    }
    Ok(())
}

fn validate_structured_object(
    descriptor: &PortableBundleObjectDescriptor,
    bytes: &[u8],
) -> Result<(), PortableBundleError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    if serde_jcs::to_vec(&value)? != bytes {
        return Err(PortableBundleError::StructuredObjectNonCanonical(
            descriptor.reference.clone(),
        ));
    }
    let actual = value
        .as_object()
        .and_then(|object| object.get("schema"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let declared = descriptor.schema.as_deref().unwrap_or("");
    if actual != declared {
        return Err(PortableBundleError::StructuredSchemaMismatch {
            reference: descriptor.reference.clone(),
            declared: declared.to_owned(),
            actual: actual.to_owned(),
        });
    }
    Ok(())
}

fn decode_payload(payload: &PortableBundlePayload) -> Result<Vec<u8>, PortableBundleError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&payload.bytes)
        .map_err(|_| PortableBundleError::DescriptorMismatch(payload.reference.clone()))?;
    if base64::engine::general_purpose::STANDARD.encode(&bytes) != payload.bytes {
        return Err(PortableBundleError::DescriptorMismatch(
            payload.reference.clone(),
        ));
    }
    Ok(bytes)
}

fn ensure_strictly_sorted(values: &[String]) -> Result<(), PortableBundleError> {
    if is_strictly_sorted(&values.iter().map(String::as_str).collect::<Vec<_>>()) {
        Ok(())
    } else if let Some(duplicate) = values.windows(2).find(|pair| pair[0] == pair[1]) {
        Err(PortableBundleError::DuplicateRoot(duplicate[0].clone()))
    } else {
        Err(PortableBundleError::NonCanonicalOrder)
    }
}

fn is_strictly_sorted(values: &[&str]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn parse_sha256(value: &str) -> Result<ContentRef, PortableBundleError> {
    let reference = ContentRef::parse(value.to_owned()).map_err(|error| {
        PortableBundleError::InvalidReference {
            value: value.to_owned(),
            reason: error.to_string(),
        }
    })?;
    if reference.algorithm() != "sha256" {
        return Err(PortableBundleError::NonSha256Reference(value.to_owned()));
    }
    Ok(reference)
}

fn ensure_bundle_size(actual: u64) -> Result<(), PortableBundleError> {
    if actual > MAX_BUNDLE_BYTES {
        return Err(PortableBundleError::BundleTooLarge {
            actual,
            maximum: MAX_BUNDLE_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::Engine;
    use sha2::{Digest, Sha256};

    use super::*;

    struct LeafExtractor(&'static str);

    impl PortableReferenceExtractor for LeafExtractor {
        fn schema(&self) -> &str {
            self.0
        }

        fn outgoing(&self, _bytes: &[u8]) -> Result<Vec<ContentRef>, PortableBundleError> {
            Ok(Vec::new())
        }
    }

    fn structured(
        schema: &str,
        name: &str,
    ) -> (PortableBundleObjectDescriptor, PortableBundlePayload) {
        let bytes =
            serde_jcs::to_vec(&serde_json::json!({"name": name, "schema": schema})).unwrap();
        let reference = format!("sha256:{:x}", Sha256::digest(&bytes));
        (
            PortableBundleObjectDescriptor {
                reference: reference.clone(),
                kind: PortableBundleObjectKind::Structured,
                schema: Some(schema.to_owned()),
                size: bytes.len() as u64,
            },
            PortableBundlePayload {
                reference,
                bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
            },
        )
    }

    fn fixture() -> PortableApplicationBundle {
        let mut pairs = vec![
            structured("ato.application/1", "app"),
            structured("ato.contract/1", "contract"),
            structured("ato.derivation/1", "derivation"),
        ];
        pairs.sort_by(|left, right| left.0.reference.cmp(&right.0.reference));
        let contract = pairs
            .iter()
            .find(|pair| pair.0.schema.as_deref() == Some("ato.contract/1"))
            .unwrap()
            .0
            .reference
            .clone();
        let application = pairs
            .iter()
            .find(|pair| pair.0.schema.as_deref() == Some("ato.application/1"))
            .unwrap()
            .0
            .reference
            .clone();
        let derivation = pairs
            .iter()
            .find(|pair| pair.0.schema.as_deref() == Some("ato.derivation/1"))
            .unwrap()
            .0
            .reference
            .clone();
        PortableApplicationBundle {
            index: PortableBundleIndex {
                version: PORTABLE_APPLICATION_BUNDLE_VERSION,
                profile: PORTABLE_APPLICATION_PROFILE.to_owned(),
                root_contract_ref: contract,
                application_ref: application,
                derivations: vec![derivation],
                objects: pairs.iter().map(|pair| pair.0.clone()).collect(),
            },
            payloads: pairs.into_iter().map(|pair| pair.1).collect(),
            portability: None,
            signatures: Vec::new(),
        }
    }

    fn registry() -> PortableReferenceRegistry {
        let mut registry = PortableReferenceRegistry::default();
        for schema in ["ato.application/1", "ato.contract/1", "ato.derivation/1"] {
            registry.register(Arc::new(LeafExtractor(schema))).unwrap();
        }
        registry
    }

    #[test]
    fn v3_round_trip_is_canonical_and_closure_complete() {
        let bundle = fixture();
        let bytes = encode_portable_application_bundle(&bundle).unwrap();
        let decoded = decode_portable_application_bundle(&bytes).unwrap();
        assert_eq!(decoded, bundle);
        validate_portable_application_closure(&decoded, &registry()).unwrap();
    }

    #[test]
    fn v3_rejects_a_one_byte_payload_tamper() {
        let mut bundle = fixture();
        let encoded = &mut bundle.payloads[0].bytes;
        encoded.replace_range(0..1, if encoded.starts_with('e') { "f" } else { "e" });
        assert!(encode_portable_application_bundle(&bundle).is_err());
    }

    #[test]
    fn v3_rejects_a_tampered_root_contract_reference() {
        let mut bundle = fixture();
        bundle.index.root_contract_ref = format!("sha256:{}", "0".repeat(64));
        assert!(encode_portable_application_bundle(&bundle).is_err());
    }

    #[test]
    fn v3_rejects_an_unsupported_profile_without_v2_fallback() {
        let mut bundle = fixture();
        bundle.index.profile = "ato.portable-application/999".to_owned();
        let bytes = serde_jcs::to_vec(&bundle).unwrap();
        assert!(matches!(
            decode_capsule_bundle_document(&bytes),
            Err(CapsuleBundleDocumentError::PortableV3(_))
        ));
    }

    #[test]
    fn dispatcher_preserves_the_v2_reader() {
        let bytes = format!(
            "{{\"index\":{{\"objects\":[],\"root\":\"blake3:{}\",\"version\":2}},\"payloads\":[]}}",
            "0".repeat(64)
        );
        assert!(matches!(
            decode_capsule_bundle_document(bytes.as_bytes()).unwrap(),
            CapsuleBundleDocument::ComputationV2(_)
        ));
    }
}

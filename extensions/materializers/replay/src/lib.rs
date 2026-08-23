//! Versioned replay materializers. v1 preserves legacy Adapter/head chaining;
//! v2 dispatches portable Protocol operations without deriving Computation identity.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use ato_adapter_api::OperationRequirement;
use ato_computation::{ComputationRef, ContentRef, OperationId, PortId, ProtocolId};
use ato_materializer_api::{
    Compatibility, Materializer, MaterializerContext, MaterializerError, Realization,
    RestoreCapability,
};
use ato_objects::{
    BundleError, Direction, MaterializationReferences, ObjectLink, ObjectResolver, RecordBodyV2,
    RecordEnvelope, RecordEnvelopeV2, RecordId, RecordIdV2, read_exact_object,
};
use serde::{Deserialize, Serialize};

pub const REPLAY_MATERIALIZER_ID: &str = "ato.replay@1";
pub const REPLAY_MATERIALIZER_V2_ID: &str = "ato.replay@2";
const REPLAY_VERSION: u32 = 1;
const REPLAY_V2_VERSION: u32 = 2;
const MAX_REPLAY_DESCRIPTOR_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayDescriptor {
    version: u32,
    target: String,
    anchor: String,
    records: Vec<RecordWire>,
    required_adapters: Vec<String>,
    required_protocols: Vec<String>,
    required_bindings: Vec<String>,
    contracts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordWire {
    id: RecordId,
    adapter_id: String,
    protocol_id: String,
    port_id: String,
    direction: Direction,
    payload_ref: String,
    head_before: String,
    head_after: String,
    caused_by: Vec<RecordId>,
    observed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayDescriptorV2 {
    version: u32,
    target: String,
    anchor: String,
    records: Vec<RecordWireV2>,
    required_operations: Vec<RequiredOperationWire>,
    required_bindings: Vec<String>,
    contracts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordWireV2 {
    id: String,
    protocol_id: String,
    operation_id: String,
    port_id: String,
    payload_ref: String,
    payload_version: u32,
    required_features: BTreeSet<String>,
    recorded_by: Option<String>,
    stream: String,
    local_seq: u64,
    writer_order: u64,
    caused_by: Vec<String>,
    observed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredOperationWire {
    protocol_id: String,
    operation_id: String,
    payload_version: u32,
    required_features: BTreeSet<String>,
}

#[derive(Default)]
pub struct ReplayMaterializer;

#[derive(Default)]
pub struct ReplayMaterializerV2;

impl Materializer for ReplayMaterializerV2 {
    fn id(&self) -> &str {
        REPLAY_MATERIALIZER_V2_ID
    }

    fn restore_capability(&self) -> RestoreCapability {
        RestoreCapability::Supported
    }

    fn encode(
        &self,
        target: &ComputationRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ContentRef, MaterializerError> {
        let anchor = if context.records_v2.is_empty() {
            target
        } else {
            context.replay_anchor.ok_or_else(|| {
                MaterializerError::Operation(
                    "ato.replay@2 requires an explicit anchor for a non-empty Record closure"
                        .to_owned(),
                )
            })?
        };
        let descriptor = ReplayDescriptorV2 {
            version: REPLAY_V2_VERSION,
            target: target.to_string(),
            anchor: anchor.to_string(),
            records: context.records_v2.iter().map(RecordWireV2::from).collect(),
            required_operations: derive_required_operations(context.records_v2),
            required_bindings: Vec::new(),
            contracts: Vec::new(),
        };
        Ok(context.objects.put(&serde_jcs::to_vec(&descriptor)?)?)
    }

    fn verify(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ComputationRef, MaterializerError> {
        let (descriptor, records) = load_descriptor_v2(descriptor, context.objects)
            .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        if let Some(driver) = context.realization {
            driver.preflight_operations(&records)?;
        }
        parse_computation(&descriptor.target)
    }

    fn compatibility(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Compatibility {
        match self.verify(descriptor, context) {
            Ok(_) => Compatibility::Compatible,
            Err(MaterializerError::OperationReplayUnsupported) => Compatibility::Incompatible,
            Err(_) => Compatibility::Unknown,
        }
    }

    fn restore(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<Box<dyn Realization>, MaterializerError> {
        let (descriptor, records) = load_descriptor_v2(descriptor, context.objects)
            .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        let anchor = parse_computation(&descriptor.anchor)?;
        let target = parse_computation(&descriptor.target)?;
        let driver = context
            .realization
            .ok_or_else(|| MaterializerError::RealizationUnavailable(self.id().to_owned()))?;
        driver.preflight_operations(&records)?;
        let mut runtime = driver.begin_operations(&anchor)?;
        for record in &records {
            runtime.apply(record)?;
        }
        runtime.finish(&target)
    }
}

impl Materializer for ReplayMaterializer {
    fn id(&self) -> &str {
        REPLAY_MATERIALIZER_ID
    }

    fn restore_capability(&self) -> RestoreCapability {
        RestoreCapability::Supported
    }

    fn encode(
        &self,
        target: &ComputationRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ContentRef, MaterializerError> {
        let mut required_adapters = Vec::new();
        let mut required_protocols = Vec::new();
        for record in context.records {
            let adapter = context.adapters.get(&record.adapter_id).map_err(|_| {
                MaterializerError::MissingApply {
                    materializer: self.id().to_owned(),
                    adapter: record.adapter_id.clone(),
                }
            })?;
            if !adapter.capabilities().apply {
                return Err(MaterializerError::MissingApply {
                    materializer: self.id().to_owned(),
                    adapter: record.adapter_id.clone(),
                });
            }
            required_adapters.push(record.adapter_id.clone());
            required_protocols.push(record.protocol_id.to_string());
        }
        required_adapters.sort();
        required_adapters.dedup();
        required_protocols.sort();
        required_protocols.dedup();
        let anchor = context
            .records
            .first()
            .map_or_else(|| target.clone(), |record| record.head_before.clone());
        let descriptor = ReplayDescriptor {
            version: REPLAY_VERSION,
            target: target.to_string(),
            anchor: anchor.to_string(),
            records: context.records.iter().map(RecordWire::from).collect(),
            required_adapters,
            required_protocols,
            required_bindings: Vec::new(),
            contracts: Vec::new(),
        };
        Ok(context.objects.put(&serde_jcs::to_vec(&descriptor)?)?)
    }

    fn verify(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ComputationRef, MaterializerError> {
        let descriptor = load_descriptor(descriptor, context.objects)
            .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        for id in &descriptor.required_adapters {
            let adapter =
                context
                    .adapters
                    .get(id)
                    .map_err(|_| MaterializerError::MissingApply {
                        materializer: self.id().to_owned(),
                        adapter: id.clone(),
                    })?;
            if !adapter.capabilities().apply {
                return Err(MaterializerError::MissingApply {
                    materializer: self.id().to_owned(),
                    adapter: id.clone(),
                });
            }
        }
        parse_computation(&descriptor.target)
    }

    fn compatibility(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Compatibility {
        match self.verify(descriptor, context) {
            Ok(_) => Compatibility::Compatible,
            Err(MaterializerError::MissingApply { .. }) => Compatibility::Incompatible,
            Err(_) => Compatibility::Unknown,
        }
    }

    fn restore(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<Box<dyn Realization>, MaterializerError> {
        let descriptor = load_descriptor(descriptor, context.objects)
            .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        let anchor = parse_computation(&descriptor.anchor)?;
        let target = parse_computation(&descriptor.target)?;
        let driver = context
            .realization
            .ok_or_else(|| MaterializerError::RealizationUnavailable(self.id().to_owned()))?;
        let mut runtime = driver.begin(&anchor)?;
        let mut current = anchor;
        for wire in descriptor.records {
            let record = RecordEnvelope::try_from(wire)?;
            if record.head_before != current {
                return Err(MaterializerError::Operation(format!(
                    "replay causal head mismatch at {:?}: expected {}, got {}",
                    record.id, current, record.head_before
                )));
            }
            runtime.apply(&record)?;
            current = record.head_after;
        }
        if current != target {
            return Err(MaterializerError::Operation(format!(
                "replay derived {current}, descriptor target is {target}"
            )));
        }
        runtime.finish(&target)
    }
}

impl From<&RecordEnvelope> for RecordWire {
    fn from(value: &RecordEnvelope) -> Self {
        Self {
            id: value.id.clone(),
            adapter_id: value.adapter_id.clone(),
            protocol_id: value.protocol_id.to_string(),
            port_id: value.port_id.to_string(),
            direction: value.direction,
            payload_ref: value.payload_ref.to_string(),
            head_before: value.head_before.to_string(),
            head_after: value.head_after.to_string(),
            caused_by: value.caused_by.clone(),
            observed_at: value.observed_at.clone(),
        }
    }
}

impl TryFrom<RecordWire> for RecordEnvelope {
    type Error = MaterializerError;

    fn try_from(value: RecordWire) -> Result<Self, Self::Error> {
        let invalid =
            |error: Box<dyn std::fmt::Display>| MaterializerError::Operation(error.to_string());
        Ok(Self {
            id: value.id,
            adapter_id: value.adapter_id,
            protocol_id: ProtocolId::parse(value.protocol_id)
                .map_err(|error| invalid(Box::new(error)))?,
            port_id: PortId::parse(value.port_id).map_err(|error| invalid(Box::new(error)))?,
            direction: value.direction,
            payload_ref: ContentRef::parse(value.payload_ref)
                .map_err(|error| invalid(Box::new(error)))?,
            head_before: parse_computation(&value.head_before)?,
            head_after: parse_computation(&value.head_after)?,
            caused_by: value.caused_by,
            observed_at: value.observed_at,
        })
    }
}

impl From<&RecordEnvelopeV2> for RecordWireV2 {
    fn from(value: &RecordEnvelopeV2) -> Self {
        Self {
            id: value.id.to_string(),
            protocol_id: value.protocol_id.to_string(),
            operation_id: value.operation_id.to_string(),
            port_id: value.port_id.to_string(),
            payload_ref: value.payload_ref.to_string(),
            payload_version: value.payload_version,
            required_features: value.required_features.clone(),
            recorded_by: value.recorded_by.clone(),
            stream: value.stream.clone(),
            local_seq: value.local_seq,
            writer_order: value.writer_order,
            caused_by: value.caused_by.iter().map(ToString::to_string).collect(),
            observed_at: value.observed_at.clone(),
        }
    }
}

impl TryFrom<RecordWireV2> for RecordEnvelopeV2 {
    type Error = MaterializerError;

    fn try_from(value: RecordWireV2) -> Result<Self, Self::Error> {
        let expected_id = value.id;
        let record = RecordEnvelopeV2::seal(RecordBodyV2 {
            protocol_id: ProtocolId::parse(value.protocol_id)
                .map_err(|error| MaterializerError::Operation(error.to_string()))?,
            operation_id: OperationId::parse(value.operation_id)
                .map_err(|error| MaterializerError::Operation(error.to_string()))?,
            port_id: PortId::parse(value.port_id)
                .map_err(|error| MaterializerError::Operation(error.to_string()))?,
            payload_ref: ContentRef::parse(value.payload_ref)
                .map_err(|error| MaterializerError::Operation(error.to_string()))?,
            payload_version: value.payload_version,
            required_features: value.required_features,
            recorded_by: value.recorded_by,
            stream: value.stream,
            local_seq: value.local_seq,
            writer_order: value.writer_order,
            caused_by: value
                .caused_by
                .into_iter()
                .map(RecordIdV2::parse)
                .collect::<Result<_, _>>()
                .map_err(|error| MaterializerError::Operation(error.to_string()))?,
            observed_at: value.observed_at,
        })
        .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        if record.id.to_string() != expected_id {
            return Err(MaterializerError::Operation(format!(
                "Record identity mismatch: expected {}, descriptor has {expected_id}",
                record.id
            )));
        }
        Ok(record)
    }
}

fn derive_required_operations(records: &[RecordEnvelopeV2]) -> Vec<RequiredOperationWire> {
    records
        .iter()
        .map(OperationRequirement::from)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|requirement| RequiredOperationWire {
            protocol_id: requirement.protocol_id.to_string(),
            operation_id: requirement.operation_id.to_string(),
            payload_version: requirement.payload_version,
            required_features: requirement.required_features,
        })
        .collect()
}

fn load_descriptor_v2(
    reference: &ContentRef,
    objects: &dyn ObjectResolver,
) -> Result<(ReplayDescriptorV2, Vec<RecordEnvelopeV2>), BundleError> {
    let metadata = objects.metadata(reference)?;
    let bytes = read_exact_object(
        objects,
        reference,
        metadata.size,
        MAX_REPLAY_DESCRIPTOR_BYTES,
    )?;
    let descriptor: ReplayDescriptorV2 =
        serde_json::from_slice(&bytes).map_err(BundleError::Json)?;
    if descriptor.version != REPLAY_V2_VERSION
        || serde_jcs::to_vec(&descriptor).map_err(BundleError::Json)? != bytes
    {
        return Err(invalid_descriptor(
            "ato.replay@2 descriptor is non-canonical or unsupported",
        ));
    }
    let records = descriptor
        .records
        .iter()
        .cloned()
        .map(RecordEnvelopeV2::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid_descriptor(&error.to_string()))?;
    if records
        .windows(2)
        .any(|pair| pair[0].writer_order >= pair[1].writer_order)
    {
        return Err(invalid_descriptor(
            "ato.replay@2 Record writer order must be strictly increasing",
        ));
    }
    if descriptor.required_operations != derive_required_operations(&records) {
        return Err(invalid_descriptor(
            "ato.replay@2 required_operations does not match the Record closure",
        ));
    }
    Ok((descriptor, records))
}

fn invalid_descriptor(message: &str) -> BundleError {
    BundleError::Json(serde_json::Error::io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.to_owned(),
    )))
}

fn load_descriptor(
    reference: &ContentRef,
    objects: &dyn ObjectResolver,
) -> Result<ReplayDescriptor, BundleError> {
    let metadata = objects.metadata(reference)?;
    let bytes = read_exact_object(
        objects,
        reference,
        metadata.size,
        MAX_REPLAY_DESCRIPTOR_BYTES,
    )?;
    let descriptor: ReplayDescriptor = serde_json::from_slice(&bytes).map_err(BundleError::Json)?;
    if serde_jcs::to_vec(&descriptor).map_err(BundleError::Json)? != bytes {
        return Err(BundleError::Json(serde_json::Error::io(
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "replay descriptor is not canonical JCS",
            ),
        )));
    }
    if descriptor.version != REPLAY_VERSION {
        return Err(BundleError::Json(serde_json::Error::io(
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsupported replay descriptor version",
            ),
        )));
    }
    Ok(descriptor)
}

fn parse_computation(value: &str) -> Result<ComputationRef, MaterializerError> {
    ComputationRef::parse(value).map_err(|error| MaterializerError::Operation(error.to_string()))
}

#[derive(Default)]
pub struct ReplayReferences;

#[derive(Default)]
pub struct ReplayV2References;

impl MaterializationReferences for ReplayV2References {
    fn materializer_id(&self) -> &str {
        REPLAY_MATERIALIZER_V2_ID
    }

    fn outgoing(
        &self,
        descriptor: &ContentRef,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError> {
        let (descriptor, records) = load_descriptor_v2(descriptor, objects)?;
        let mut links = vec![
            ObjectLink::Computation(ComputationRef::parse(descriptor.target).map_err(|error| {
                BundleError::InvalidReference {
                    value: "target".to_owned(),
                    reason: error.to_string(),
                }
            })?),
            ObjectLink::Computation(ComputationRef::parse(descriptor.anchor).map_err(|error| {
                BundleError::InvalidReference {
                    value: "anchor".to_owned(),
                    reason: error.to_string(),
                }
            })?),
        ];
        links.extend(
            records
                .into_iter()
                .map(|record| ObjectLink::Content(record.payload_ref)),
        );
        Ok(links)
    }
}

impl MaterializationReferences for ReplayReferences {
    fn materializer_id(&self) -> &str {
        REPLAY_MATERIALIZER_ID
    }

    fn outgoing(
        &self,
        descriptor: &ContentRef,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError> {
        let descriptor = load_descriptor(descriptor, objects)?;
        let mut links = vec![
            ObjectLink::Computation(ComputationRef::parse(descriptor.target).map_err(|error| {
                BundleError::InvalidReference {
                    value: "target".to_owned(),
                    reason: error.to_string(),
                }
            })?),
            ObjectLink::Computation(ComputationRef::parse(descriptor.anchor).map_err(|error| {
                BundleError::InvalidReference {
                    value: "anchor".to_owned(),
                    reason: error.to_string(),
                }
            })?),
        ];
        for record in descriptor.records {
            links.push(ObjectLink::Computation(
                ComputationRef::parse(record.head_before).map_err(|error| {
                    BundleError::InvalidReference {
                        value: "record head_before".to_owned(),
                        reason: error.to_string(),
                    }
                })?,
            ));
            links.push(ObjectLink::Computation(
                ComputationRef::parse(record.head_after).map_err(|error| {
                    BundleError::InvalidReference {
                        value: "record head_after".to_owned(),
                        reason: error.to_string(),
                    }
                })?,
            ));
            links.push(ObjectLink::Content(
                ContentRef::parse(record.payload_ref).map_err(|error| {
                    BundleError::InvalidReference {
                        value: "payload_ref".to_owned(),
                        reason: error.to_string(),
                    }
                })?,
            ));
        }
        Ok(links)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use ato_adapter_api::{AdapterRegistry, WorkspaceCapturePolicy};
    use ato_objects::{MemoryObjectStore, ObjectStore};

    use super::*;

    fn computation(byte: &str) -> ComputationRef {
        ComputationRef::parse(format!("blake3:{}", byte.repeat(64))).unwrap()
    }

    fn record(objects: &MemoryObjectStore, writer_order: u64) -> RecordEnvelopeV2 {
        let payload_ref = objects.put(br#"{"button":"ArrowLeft"}"#).unwrap();
        RecordEnvelopeV2::seal(RecordBodyV2 {
            protocol_id: ProtocolId::parse("ato.browser@1").unwrap(),
            operation_id: OperationId::parse("key").unwrap(),
            port_id: PortId::parse("ui.main").unwrap(),
            payload_ref,
            payload_version: 1,
            required_features: BTreeSet::from(["keyboard".to_owned()]),
            recorded_by: Some("browser.chrome@1".to_owned()),
            stream: "browser.main".to_owned(),
            local_seq: 7,
            writer_order,
            caused_by: Vec::new(),
            observed_at: "2030-01-01T00:00:00Z".to_owned(),
        })
        .unwrap()
    }

    #[test]
    fn v1_and_v2_keep_distinct_public_ids() {
        assert_eq!(ReplayMaterializer.id(), "ato.replay@1");
        assert_eq!(ReplayMaterializerV2.id(), "ato.replay@2");
    }

    #[test]
    fn v2_roundtrips_without_computation_head_fields() {
        let objects = MemoryObjectStore::default();
        let records = [record(&objects, 1)];
        let target = computation("a");
        let anchor = computation("b");
        let adapters = AdapterRegistry::default();
        let policy = WorkspaceCapturePolicy::secure_default();
        let context = MaterializerContext {
            objects: &objects,
            adapters: &adapters,
            records: &[],
            records_v2: &records,
            replay_anchor: Some(&anchor),
            workspace: Path::new("."),
            workspace_policy: &policy,
            realization: None,
        };

        let descriptor_ref = ReplayMaterializerV2.encode(&target, &context).unwrap();
        let (descriptor, decoded) = load_descriptor_v2(&descriptor_ref, &objects).unwrap();
        let bytes = read_exact_object(
            &objects,
            &descriptor_ref,
            objects.metadata(&descriptor_ref).unwrap().size,
            MAX_REPLAY_DESCRIPTOR_BYTES,
        )
        .unwrap();
        let json = String::from_utf8(bytes).unwrap();

        assert_eq!(descriptor.target, target.to_string());
        assert_eq!(descriptor.anchor, anchor.to_string());
        assert_eq!(decoded, records);
        assert!(!json.contains("head_before"));
        assert!(!json.contains("head_after"));
        assert!(!json.contains("semantic_frontier"));
    }

    #[test]
    fn v2_rejects_descriptor_requirements_that_omit_a_record_operation() {
        let objects = MemoryObjectStore::default();
        let records = [record(&objects, 1)];
        let descriptor = ReplayDescriptorV2 {
            version: REPLAY_V2_VERSION,
            target: computation("a").to_string(),
            anchor: computation("b").to_string(),
            records: records.iter().map(RecordWireV2::from).collect(),
            required_operations: Vec::new(),
            required_bindings: Vec::new(),
            contracts: Vec::new(),
        };
        let reference = objects
            .put(&serde_jcs::to_vec(&descriptor).unwrap())
            .unwrap();

        let error = load_descriptor_v2(&reference, &objects).unwrap_err();

        assert!(error.to_string().contains("required_operations"));
    }

    #[test]
    fn v2_rejects_duplicate_or_decreasing_writer_order() {
        let objects = MemoryObjectStore::default();
        let records = [record(&objects, 2), record(&objects, 1)];
        let descriptor = ReplayDescriptorV2 {
            version: REPLAY_V2_VERSION,
            target: computation("a").to_string(),
            anchor: computation("b").to_string(),
            records: records.iter().map(RecordWireV2::from).collect(),
            required_operations: derive_required_operations(&records),
            required_bindings: Vec::new(),
            contracts: Vec::new(),
        };
        let reference = objects
            .put(&serde_jcs::to_vec(&descriptor).unwrap())
            .unwrap();

        let error = load_descriptor_v2(&reference, &objects).unwrap_err();

        assert!(error.to_string().contains("strictly increasing"));
    }
}

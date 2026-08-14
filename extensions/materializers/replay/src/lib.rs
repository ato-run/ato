//! Replay delegates every record to its declared Adapter; it has no protocol switch.

#![forbid(unsafe_code)]

use ato_computation::{ComputationRef, ContentRef, PortId, ProtocolId};
use ato_materializer_api::{
    Compatibility, Materializer, MaterializerContext, MaterializerError, RestoreCapability,
};
use ato_objects::{
    BundleError, Direction, MaterializationReferences, ObjectLink, ObjectResolver, RecordEnvelope,
    read_exact_object,
};
use serde::{Deserialize, Serialize};

pub const REPLAY_MATERIALIZER_ID: &str = "ato.replay@1";
const REPLAY_VERSION: u32 = 1;
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
    seq: u64,
    stream: String,
    adapter_id: String,
    protocol_id: String,
    port_id: String,
    direction: Direction,
    payload_ref: String,
    head_before: String,
    head_after: String,
    caused_by: Vec<u64>,
    observed_at: String,
}

#[derive(Default)]
pub struct ReplayMaterializer;

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
    ) -> Result<ComputationRef, MaterializerError> {
        let descriptor = load_descriptor(descriptor, context.objects)
            .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        for wire in descriptor.records {
            let record = RecordEnvelope::try_from(wire)?;
            let adapter = context.adapters.get(&record.adapter_id).map_err(|_| {
                MaterializerError::MissingApply {
                    materializer: self.id().to_owned(),
                    adapter: record.adapter_id.clone(),
                }
            })?;
            adapter
                .apply(
                    &record,
                    &ato_adapter_api::AdapterContext {
                        workspace: context.workspace,
                        objects: context.objects,
                    },
                )
                .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        }
        parse_computation(&descriptor.target)
    }
}

impl From<&RecordEnvelope> for RecordWire {
    fn from(value: &RecordEnvelope) -> Self {
        Self {
            seq: value.seq,
            stream: value.stream.clone(),
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
            seq: value.seq,
            stream: value.stream,
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

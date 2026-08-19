//! Binding evidence contains logical/safe provider identities, never values.

#![forbid(unsafe_code)]

use ato_adapter_api::{
    AdapterAttachContext, AdapterCapabilities, AdapterContext, AdapterError, AdapterFactory,
    AdapterInstance, AttachedAdapter,
};
use ato_objects::{RecordEnvelope, read_exact_object};
use serde::{Deserialize, Serialize};

pub const BINDING_ADAPTER_ID: &str = "ato.binding@1";
pub const BINDING_PROTOCOL_ID: &str = "ato.binding@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingAdapterConfig {
    pub binding_id: String,
    pub protocol: String,
    pub provider_ref: String,
    pub port_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BindingEvent {
    Attach {
        binding_id: String,
        protocol: String,
        provider_ref: String,
    },
    Replace {
        binding_id: String,
        protocol: String,
        provider_ref: String,
    },
    Detach {
        binding_id: String,
    },
}

pub fn encode_event(event: &BindingEvent) -> Result<Vec<u8>, serde_json::Error> {
    serde_jcs::to_vec(event)
}

pub fn decode_event(bytes: &[u8]) -> Result<BindingEvent, serde_json::Error> {
    let event = serde_json::from_slice(bytes)?;
    if serde_jcs::to_vec(&event)? != bytes {
        return Err(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "binding event is not canonical JCS",
        )));
    }
    Ok(event)
}

#[derive(Default)]
pub struct BindingAdapter;

impl AdapterFactory for BindingAdapter {
    fn id(&self) -> &str {
        BINDING_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            observe: true,
            apply: true,
            verify: true,
            quiesce: true,
            capture_consistency: ato_adapter_api::CaptureConsistency::AdapterMediated,
        }
    }

    fn attach(
        &self,
        instance: &AdapterInstance,
        context: &AdapterAttachContext<'_>,
    ) -> Result<Box<dyn AttachedAdapter>, AdapterError> {
        let config: BindingAdapterConfig = serde_json::from_value(instance.config.clone())?;
        let event = BindingEvent::Attach {
            binding_id: config.binding_id,
            protocol: config.protocol,
            provider_ref: config.provider_ref,
        };
        reject_secret_like_fields(&event)?;
        context
            .observations
            .emit(ato_adapter_api::AdapterObservation {
                adapter_id: BINDING_ADAPTER_ID.to_owned(),
                protocol_id: ato_computation::ProtocolId::parse(BINDING_PROTOCOL_ID)
                    .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?,
                port_id: ato_computation::PortId::parse(config.port_id)
                    .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?,
                direction: ato_objects::Direction::Inbound,
                payload: encode_event(&event)?,
                caused_by: Vec::new(),
                effect: ato_adapter_api::ObservationEffect::Evolution,
            })?;
        Ok(Box::new(BindingSession {
            instance_id: instance.instance_id.clone(),
        }))
    }
}

struct BindingSession {
    instance_id: String,
}

impl AttachedAdapter for BindingSession {
    fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn adapter_id(&self) -> &str {
        BINDING_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterFactory::capabilities(&BindingAdapter)
    }

    fn apply(
        &mut self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        let metadata = context.objects.metadata(&record.payload_ref)?;
        let bytes =
            read_exact_object(context.objects, &record.payload_ref, metadata.size, 1 << 20)?;
        let event =
            decode_event(&bytes).map_err(|error| AdapterError::Operation(error.to_string()))?;
        reject_secret_like_fields(&event)
    }

    fn verify(
        &mut self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        AttachedAdapter::apply(self, record, context)
    }

    fn pause_for_capture(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Ok(())
    }

    fn resume_after_capture(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Ok(())
    }
}

fn reject_secret_like_fields(event: &BindingEvent) -> Result<(), AdapterError> {
    let values: Vec<&str> = match event {
        BindingEvent::Attach {
            binding_id,
            protocol,
            provider_ref,
        }
        | BindingEvent::Replace {
            binding_id,
            protocol,
            provider_ref,
        } => vec![binding_id, protocol, provider_ref],
        BindingEvent::Detach { binding_id } => vec![binding_id],
    };
    if values.iter().any(|value| {
        let lower = value.to_ascii_lowercase();
        lower.contains("secret=")
            || lower.contains("token=")
            || lower.contains("password=")
            || lower.contains("bearer ")
    }) {
        return Err(AdapterError::Operation(
            "binding evidence contains a likely secret value".to_owned(),
        ));
    }
    Ok(())
}

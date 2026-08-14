//! Binding evidence contains logical/safe provider identities, never values.

#![forbid(unsafe_code)]

use ato_adapter_api::{Adapter, AdapterCapabilities, AdapterContext, AdapterError};
use ato_objects::{RecordEnvelope, read_exact_object};
use serde::{Deserialize, Serialize};

pub const BINDING_ADAPTER_ID: &str = "ato.binding@1";
pub const BINDING_PROTOCOL_ID: &str = "ato.binding@1";

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

impl Adapter for BindingAdapter {
    fn id(&self) -> &str {
        BINDING_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            observe: true,
            apply: true,
            verify: true,
            quiesce: true,
        }
    }

    fn apply(
        &self,
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
        &self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        self.apply(record, context)
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

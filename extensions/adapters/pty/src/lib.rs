//! Byte-level PTY evidence. It deliberately does not infer shell commands.

#![forbid(unsafe_code)]

use ato_adapter_api::{
    AdapterAttachContext, AdapterCapabilities, AdapterContext, AdapterError, AdapterFactory,
    AdapterInstance, AttachedAdapter,
};
use ato_objects::{RecordEnvelope, read_exact_object};
use serde::{Deserialize, Serialize};

pub const PTY_ADAPTER_ID: &str = "ato.pty@1";
pub const PTY_PROTOCOL_ID: &str = "ato.pty@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PtyEvent {
    Input { bytes: Vec<u8> },
    Output { bytes: Vec<u8> },
    Resize { columns: u16, rows: u16 },
    Signal { name: String },
    Attach,
    Detach,
}

pub fn encode_event(event: &PtyEvent) -> Result<Vec<u8>, serde_json::Error> {
    serde_jcs::to_vec(event)
}

pub fn decode_event(bytes: &[u8]) -> Result<PtyEvent, serde_json::Error> {
    let event = serde_json::from_slice(bytes)?;
    if serde_jcs::to_vec(&event)? != bytes {
        return Err(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "PTY event is not canonical JCS",
        )));
    }
    Ok(event)
}

#[derive(Default)]
pub struct PtyAdapter;

impl AdapterFactory for PtyAdapter {
    fn id(&self) -> &str {
        PTY_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            observe: true,
            apply: true,
            verify: true,
            quiesce: true,
        }
    }

    fn attach(
        &self,
        instance: &AdapterInstance,
        _context: &AdapterAttachContext<'_>,
    ) -> Result<Box<dyn AttachedAdapter>, AdapterError> {
        Ok(Box::new(PtySession {
            instance_id: instance.instance_id.clone(),
        }))
    }
}

struct PtySession {
    instance_id: String,
}

impl AttachedAdapter for PtySession {
    fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn adapter_id(&self) -> &str {
        PTY_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterFactory::capabilities(&PtyAdapter)
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
        match event {
            PtyEvent::Input { .. } | PtyEvent::Resize { .. } | PtyEvent::Signal { .. } => Ok(()),
            PtyEvent::Output { .. } | PtyEvent::Attach | PtyEvent::Detach => Ok(()),
        }
    }

    fn verify(
        &mut self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        AttachedAdapter::apply(self, record, context)
    }
}

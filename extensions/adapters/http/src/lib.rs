//! HTTP request and response are deliberately separate adapter records.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use ato_adapter_api::{Adapter, AdapterCapabilities, AdapterContext, AdapterError};
use ato_objects::{RecordEnvelope, read_exact_object};
use serde::{Deserialize, Serialize};

pub const HTTP_ADAPTER_ID: &str = "ato.http@1";
pub const HTTP_PROTOCOL_ID: &str = "ato.http@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpEvent {
    Request {
        method: String,
        path: String,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    },
    Response {
        status: u16,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    },
}

pub fn encode_event(event: &HttpEvent) -> Result<Vec<u8>, serde_json::Error> {
    serde_jcs::to_vec(event)
}

pub fn decode_event(bytes: &[u8]) -> Result<HttpEvent, serde_json::Error> {
    let event = serde_json::from_slice(bytes)?;
    if serde_jcs::to_vec(&event)? != bytes {
        return Err(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "HTTP event is not canonical JCS",
        )));
    }
    Ok(event)
}

#[derive(Default)]
pub struct HttpAdapter;

impl Adapter for HttpAdapter {
    fn id(&self) -> &str {
        HTTP_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            observe: true,
            apply: false,
            verify: true,
            quiesce: true,
        }
    }

    fn verify(
        &self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        let metadata = context.objects.metadata(&record.payload_ref)?;
        let bytes = read_exact_object(
            context.objects,
            &record.payload_ref,
            metadata.size,
            16 << 20,
        )?;
        decode_event(&bytes)
            .map(|_| ())
            .map_err(|error| AdapterError::Operation(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_response_are_distinct_protocol_events() {
        let request = encode_event(&HttpEvent::Request {
            method: "POST".to_owned(),
            path: "/increment".to_owned(),
            headers: BTreeMap::new(),
            body: Vec::new(),
        })
        .unwrap();
        let response = encode_event(&HttpEvent::Response {
            status: 204,
            headers: BTreeMap::new(),
            body: Vec::new(),
        })
        .unwrap();
        assert_ne!(request, response);
        assert!(matches!(
            decode_event(&request).unwrap(),
            HttpEvent::Request { .. }
        ));
        assert!(matches!(
            decode_event(&response).unwrap(),
            HttpEvent::Response { .. }
        ));
    }
}

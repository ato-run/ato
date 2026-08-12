use std::collections::BTreeSet;
use std::io::{Read, Write};

use serde_json::Value;
use thiserror::Error;

pub const MAX_CONTROL_FRAME_BYTES: usize = 8 * 1024 * 1024;

pub fn write_json_frame(writer: &mut impl Write, value: &Value) -> Result<(), DriverBindingError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.is_empty() || bytes.len() > MAX_CONTROL_FRAME_BYTES {
        return Err(DriverBindingError::InvalidFrameLength(bytes.len()));
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| DriverBindingError::InvalidFrameLength(bytes.len()))?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&bytes)?;
    Ok(())
}

pub fn read_json_frame(reader: &mut impl Read) -> Result<Option<Value>, DriverBindingError> {
    let mut length_bytes = [0_u8; 4];
    match reader.read(&mut length_bytes[..1])? {
        0 => return Ok(None),
        1 => {}
        _ => unreachable!("one-byte read returned more than one byte"),
    }
    reader
        .read_exact(&mut length_bytes[1..])
        .map_err(|error| mid_frame(error, "length"))?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length == 0 || length > MAX_CONTROL_FRAME_BYTES {
        return Err(DriverBindingError::InvalidFrameLength(length));
    }
    let mut bytes = vec![0_u8; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| mid_frame(error, "body"))?;
    let value = serde_json::from_slice(&bytes)?;
    Ok(Some(value))
}

fn mid_frame(error: std::io::Error, part: &'static str) -> DriverBindingError {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        DriverBindingError::MidFrameEof { part }
    } else {
        DriverBindingError::Io(error)
    }
}

#[derive(Debug, Default)]
pub struct RequestIdTracker {
    active: BTreeSet<String>,
}

impl RequestIdTracker {
    pub fn begin(&mut self, id: &Value) -> Result<(), DriverBindingError> {
        if !id.is_string() && !id.is_number() {
            return Err(DriverBindingError::InvalidRequestId);
        }
        let canonical = serde_json::to_string(id)?;
        if !self.active.insert(canonical) {
            return Err(DriverBindingError::DuplicateRequestId);
        }
        Ok(())
    }

    pub fn complete(&mut self, id: &Value) -> Result<(), DriverBindingError> {
        let canonical = serde_json::to_string(id)?;
        if !self.active.remove(&canonical) {
            return Err(DriverBindingError::UnknownRequestId);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectHandleScope {
    pub session_id: String,
    pub driver_instance: String,
    pub supervisor_generation: u64,
    pub incarnation_nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectHandle {
    pub id: String,
    pub scope: ObjectHandleScope,
}

impl ObjectHandle {
    pub fn authorize(&self, current: &ObjectHandleScope) -> Result<(), DriverBindingError> {
        if &self.scope != current {
            return Err(DriverBindingError::StaleObjectHandle);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum DriverBindingError {
    #[error("driver binding I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("driver binding JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid control frame length {0}")]
    InvalidFrameLength(usize),
    #[error("EOF in control frame {part}")]
    MidFrameEof { part: &'static str },
    #[error("request id must be a string or number")]
    InvalidRequestId,
    #[error("duplicate active request id")]
    DuplicateRequestId,
    #[error("response references an unknown request id")]
    UnknownRequestId,
    #[error("object handle belongs to another Driver or Supervisor incarnation")]
    StaleObjectHandle,
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::json;

    use super::*;

    #[test]
    fn framed_json_round_trips() {
        let expected = json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
        let mut bytes = Vec::new();
        write_json_frame(&mut bytes, &expected).expect("write frame");
        let actual = read_json_frame(&mut Cursor::new(bytes))
            .expect("read frame")
            .expect("frame");
        assert_eq!(actual, expected);
    }

    #[test]
    fn rejects_eof_in_frame_body() {
        let bytes = [0, 0, 0, 4, b'{', b'}'];
        assert!(matches!(
            read_json_frame(&mut Cursor::new(bytes)),
            Err(DriverBindingError::MidFrameEof { part: "body" })
        ));
    }

    #[test]
    fn rejects_duplicate_active_request_id() {
        let mut tracker = RequestIdTracker::default();
        tracker.begin(&json!(7)).expect("first request");
        assert!(matches!(
            tracker.begin(&json!(7)),
            Err(DriverBindingError::DuplicateRequestId)
        ));
    }

    #[test]
    fn object_handle_is_bound_to_supervisor_incarnation() {
        let scope = ObjectHandleScope {
            session_id: "session-1".to_owned(),
            driver_instance: "driver-1".to_owned(),
            supervisor_generation: 2,
            incarnation_nonce: "nonce-a".to_owned(),
        };
        let handle = ObjectHandle {
            id: "object-1".to_owned(),
            scope: scope.clone(),
        };
        let mut stale = scope;
        stale.supervisor_generation += 1;
        assert!(matches!(
            handle.authorize(&stale),
            Err(DriverBindingError::StaleObjectHandle)
        ));
    }
}

use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};

pub const GUEST_PROTOCOL_VERSION: &str = "guest.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GuestMode {
    Widget,
    Headless,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GuestContextRole {
    Consumer,
    Owner,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GuestPermission {
    #[serde(default)]
    pub can_read_payload: bool,
    #[serde(default)]
    pub can_read_context: bool,
    #[serde(default)]
    pub can_write_payload: bool,
    #[serde(default)]
    pub can_write_context: bool,
    #[serde(default)]
    pub can_execute_wasm: bool,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub allowed_env: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestContext {
    pub mode: GuestMode,
    pub role: GuestContextRole,
    pub permissions: GuestPermission,
    pub sync_path: String,
    pub host_app: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestRequest {
    pub version: String,
    pub request_id: String,
    pub action: GuestAction,
    pub context: GuestContext,
    #[serde(default)]
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GuestAction {
    ReadPayload,
    ReadContext,
    WritePayload,
    WriteContext,
    ExecuteWasm,
    UpdatePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestResponse {
    pub version: String,
    pub request_id: String,
    pub ok: bool,
    pub result: Option<serde_json::Value>,
    pub error: Option<GuestError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GuestErrorCode {
    PermissionDenied,
    InvalidRequest,
    ExecutionFailed,
    HostUnavailable,
    ProtocolError,
    IoError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestError {
    pub code: GuestErrorCode,
    pub message: String,
}

impl GuestError {
    pub fn new(code: GuestErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub fn encode_payload_base64(payload: &[u8]) -> String {
    general_purpose::STANDARD.encode(payload)
}

pub fn decode_payload_base64(value: &str) -> Result<Vec<u8>, GuestError> {
    general_purpose::STANDARD
        .decode(value)
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))
}

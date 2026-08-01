//! Secret-bridge wire types — the JSON protocol between a host (the desktop
//! shell today) and `ato secrets bridge --json`.
//!
//! Single-sourced here so the producer (`cli::cli::dispatch::secrets`) and the
//! consumer (`desktop::secret_bridge`) share one definition instead of the two
//! mirror copies they used to keep. The request is `#[serde(tag = "op")]`, the
//! response `#[serde(tag = "status")]`. Optional request fields carry BOTH
//! `default` (lenient decode, what the CLI producer relied on) and
//! `skip_serializing_if` (compact encode, what the desktop consumer relied on)
//! so the merged type reproduces the original wire byte-for-byte in both
//! directions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A request sent to `ato secrets bridge --json` (one JSON line).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum BridgeRequest {
    #[serde(rename = "status")]
    Status,
    #[serde(rename = "list")]
    List,
    #[serde(rename = "set")]
    Set {
        key: String,
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allow: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deny: Option<Vec<String>>,
    },
    #[serde(rename = "delete")]
    Delete {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    #[serde(rename = "update_acl")]
    UpdateAcl {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allow: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deny: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    #[serde(rename = "resolve_for_capsule")]
    ResolveForCapsule { capsule_handle: String },
}

/// A response from the bridge (one JSON line).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum BridgeResponse {
    #[serde(rename = "ok")]
    Ok {
        #[serde(default)]
        data: Value,
    },
    #[serde(rename = "error")]
    Error { code: String, message: String },
}

impl BridgeResponse {
    /// Build an `ok` response, serialising `data` (falls back to `null` if the
    /// value cannot be serialised).
    pub fn ok_data(data: impl Serialize) -> Self {
        Self::Ok {
            data: serde_json::to_value(data).unwrap_or(Value::Null),
        }
    }

    /// Build an `error` response with a machine-readable `code`.
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Metadata view of a stored secret (no value), returned by the `list` op.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretEntryView {
    pub key: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub allow: Option<Vec<String>>,
    #[serde(default)]
    pub deny: Option<Vec<String>>,
}

/// A resolved secret (key + value) returned by the `resolve_for_capsule` op.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedSecret {
    pub key: String,
    pub value: String,
}

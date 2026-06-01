use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use crate::proc_util::CommandNoWindowExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Typed error from the CLI bridge, preserving the machine-readable code
/// so the UI can branch on `identity_not_loaded` etc.
#[derive(Debug)]
pub(crate) struct BridgeError {
    pub code: String,
    pub message: String,
}

impl BridgeError {}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "secret bridge {}: {}", self.code, self.message)
    }
}

impl std::error::Error for BridgeError {}

/// JSON bridge request sent to `ato secrets bridge --json`.
#[derive(Debug, Serialize)]
#[serde(tag = "op")]
enum BridgeRequest {
    #[serde(rename = "status")]
    Status,
    #[serde(rename = "list")]
    List,
    #[serde(rename = "set")]
    Set {
        key: String,
        value: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        allow: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        deny: Option<Vec<String>>,
    },
    #[serde(rename = "delete")]
    Delete {
        key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    #[serde(rename = "update_acl")]
    UpdateAcl {
        key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        allow: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        deny: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    #[serde(rename = "resolve_for_capsule")]
    ResolveForCapsule { capsule_handle: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status")]
enum BridgeResponse {
    #[serde(rename = "ok")]
    Ok { data: Value },
    #[serde(rename = "error")]
    Error { code: String, message: String },
}

/// Thin client that calls the CLI's `ato secrets bridge --json`.
pub(crate) struct CliSecretBridge;

/// Internal result alias for bridge communication.
type BridgeResult<T> = std::result::Result<T, BridgeError>;

impl CliSecretBridge {
    fn call(request: &BridgeRequest) -> BridgeResult<BridgeResponse> {
        let ato = crate::orchestrator::resolve_ato_binary().map_err(|e| BridgeError {
            code: "binary_not_found".into(),
            message: format!("failed to resolve ato binary: {e}"),
        })?;
        let request_json = serde_json::to_string(request).map_err(|e| BridgeError {
            code: "serialization_failed".into(),
            message: format!("{e}"),
        })?;

        let mut child = Command::new(&ato)
            .no_console_window()
            .args(["secrets", "bridge", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| BridgeError {
                code: "spawn_failed".into(),
                message: format!("failed to spawn ato bridge ({}): {e}", ato.display()),
            })?;

        {
            let stdin = child.stdin.as_mut().ok_or_else(|| BridgeError {
                code: "spawn_failed".into(),
                message: "failed to open bridge stdin".into(),
            })?;
            stdin
                .write_all(request_json.as_bytes())
                .map_err(|e| BridgeError {
                    code: "spawn_failed".into(),
                    message: format!("failed to write bridge request: {e}"),
                })?;
            stdin.write_all(b"\n").map_err(|e| BridgeError {
                code: "spawn_failed".into(),
                message: format!("failed to write newline: {e}"),
            })?;
            stdin.flush().map_err(|e| BridgeError {
                code: "spawn_failed".into(),
                message: format!("failed to flush bridge stdin: {e}"),
            })?;
        }

        let output = child.wait_with_output().map_err(|e| BridgeError {
            code: "spawn_failed".into(),
            message: format!("failed to wait for bridge process: {e}"),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(BridgeError {
                code: "spawn_failed".into(),
                message: format!("bridge exited with {}: {}", output.status, stderr.trim()),
            });
        }

        let mut stdout_reader = BufReader::new(&output.stdout[..]);
        let mut line = String::new();
        stdout_reader
            .read_line(&mut line)
            .map_err(|e| BridgeError {
                code: "spawn_failed".into(),
                message: format!("failed to read bridge response: {e}"),
            })?;

        serde_json::from_str(line.trim()).map_err(|e| BridgeError {
            code: "malformed_response".into(),
            message: format!("failed to parse bridge response: {e}"),
        })
    }

    fn map_resp(resp: BridgeResponse) -> BridgeResult<()> {
        match resp {
            BridgeResponse::Ok { .. } => Ok(()),
            BridgeResponse::Error { code, message } => Err(BridgeError { code, message }),
        }
    }

    fn map_resp_data<T: serde::de::DeserializeOwned>(
        resp: BridgeResponse,
        label: &str,
    ) -> BridgeResult<T> {
        match resp {
            BridgeResponse::Ok { data } => serde_json::from_value(data).map_err(|e| BridgeError {
                code: "malformed_response".into(),
                message: format!("failed to parse bridge {label} response: {e}"),
            }),
            BridgeResponse::Error { code, message } => Err(BridgeError { code, message }),
        }
    }

    pub(crate) fn set(
        key: &str,
        value: &str,
        namespace: Option<&str>,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> BridgeResult<()> {
        Self::map_resp(Self::call(&BridgeRequest::Set {
            key: key.to_string(),
            value: value.to_string(),
            namespace: namespace.map(|s| s.to_string()),
            description: None,
            allow,
            deny,
        })?)
    }

    pub(crate) fn delete(key: &str, namespace: Option<&str>) -> BridgeResult<()> {
        Self::map_resp(Self::call(&BridgeRequest::Delete {
            key: key.to_string(),
            namespace: namespace.map(|s| s.to_string()),
        })?)
    }

    pub(crate) fn list() -> BridgeResult<Vec<SecretEntryView>> {
        Self::map_resp_data(Self::call(&BridgeRequest::List)?, "list")
    }

    pub(crate) fn resolve_for_capsule(handle: &str) -> BridgeResult<Vec<ResolvedSecret>> {
        Self::map_resp_data(
            Self::call(&BridgeRequest::ResolveForCapsule {
                capsule_handle: handle.to_string(),
            })?,
            "resolve_for_capsule",
        )
    }

    pub(crate) fn update_acl(
        key: &str,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> BridgeResult<()> {
        Self::map_resp(Self::call(&BridgeRequest::UpdateAcl {
            key: key.to_string(),
            allow,
            deny,
            namespace: None,
        })?)
    }
}

/// View model returned by `list` (metadata only, no values).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SecretEntryView {
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

/// Resolved secret (key + value) returned by `resolve_for_capsule`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ResolvedSecret {
    pub key: String,
    pub value: String,
}

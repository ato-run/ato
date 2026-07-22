use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use crate::proc_util::CommandNoWindowExt;
use protocol::secret_bridge::{BridgeRequest, BridgeResponse, ResolvedSecret, SecretEntryView};

/// Typed error from the CLI bridge, preserving the machine-readable code
/// so the UI can branch on `identity_not_loaded` etc.
///
/// This is a local transport artifact — its codes describe how the *desktop*
/// failed to reach the bridge (`binary_not_found`, `spawn_failed`,
/// `malformed_response`), not a wire type — so it stays desktop-side while the
/// request/response types live in `protocol::secret_bridge`.
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

/// Thin client that calls the CLI's `ato secrets bridge --json`.
///
/// The wire types (`BridgeRequest` / `BridgeResponse` / `SecretEntryView` /
/// `ResolvedSecret`) are single-sourced in `protocol::secret_bridge`, shared
/// with the CLI producer (`cli::cli::dispatch::secrets`).
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

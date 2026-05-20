use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    ResolveForCapsule {
        capsule_handle: String,
    },
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

impl CliSecretBridge {
    fn call(request: &BridgeRequest) -> Result<BridgeResponse> {
        let ato = crate::orchestrator::resolve_ato_binary()
            .context("failed to resolve ato binary for secret bridge")?;
        let request_json =
            serde_json::to_string(request).context("failed to serialize bridge request")?;

        let mut child = Command::new(&ato)
            .args(["secrets", "bridge", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn ato bridge ({})", ato.display()))?;

        {
            let stdin = child
                .stdin
                .as_mut()
                .context("failed to open bridge stdin")?;
            stdin
                .write_all(request_json.as_bytes())
                .context("failed to write bridge request")?;
            stdin
                .write_all(b"\n")
                .context("failed to write newline to bridge")?;
            stdin.flush().context("failed to flush bridge stdin")?;
        }

        let output = child
            .wait_with_output()
            .context("failed to wait for bridge process")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!(
                "bridge exited with {}: {}",
                output.status,
                stderr.trim()
            ));
        }

        let mut stdout_reader = BufReader::new(&output.stdout[..]);
        let mut line = String::new();
        stdout_reader
            .read_line(&mut line)
            .context("failed to read bridge response")?;

        serde_json::from_str(line.trim()).context("failed to parse bridge response")
    }

    pub(crate) fn status() -> Result<bool> {
        let resp = Self::call(&BridgeRequest::Status)?;
        match resp {
            BridgeResponse::Ok { data } => Ok(data["identity_loaded"].as_bool().unwrap_or(false)),
            BridgeResponse::Error { .. } => Ok(false),
        }
    }

    pub(crate) fn set(
        key: &str,
        value: &str,
        namespace: Option<&str>,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        let resp = Self::call(&BridgeRequest::Set {
            key: key.to_string(),
            value: value.to_string(),
            namespace: namespace.map(|s| s.to_string()),
            description: None,
            allow,
            deny,
        })?;
        match resp {
            BridgeResponse::Ok { .. } => Ok(()),
            BridgeResponse::Error { code, message } => {
                Err(anyhow::anyhow!("secret bridge {code}: {message}"))
            }
        }
    }

    pub(crate) fn delete(key: &str, namespace: Option<&str>) -> Result<()> {
        let resp = Self::call(&BridgeRequest::Delete {
            key: key.to_string(),
            namespace: namespace.map(|s| s.to_string()),
        })?;
        match resp {
            BridgeResponse::Ok { .. } => Ok(()),
            BridgeResponse::Error { code, message } => {
                Err(anyhow::anyhow!("secret bridge {code}: {message}"))
            }
        }
    }

    pub(crate) fn list() -> Result<Vec<SecretEntryView>> {
        let resp = Self::call(&BridgeRequest::List)?;
        match resp {
            BridgeResponse::Ok { data } => {
                let entries: Vec<SecretEntryView> = serde_json::from_value(data)
                    .context("failed to parse bridge list response")?;
                Ok(entries)
            }
            BridgeResponse::Error { code, message } => {
                Err(anyhow::anyhow!("secret bridge {code}: {message}"))
            }
        }
    }

    pub(crate) fn resolve_for_capsule(handle: &str) -> Result<Vec<ResolvedSecret>> {
        let resp = Self::call(&BridgeRequest::ResolveForCapsule {
            capsule_handle: handle.to_string(),
        })?;
        match resp {
            BridgeResponse::Ok { data } => {
                let entries: Vec<ResolvedSecret> = serde_json::from_value(data)
                    .context("failed to parse bridge resolve response")?;
                Ok(entries)
            }
            BridgeResponse::Error { code, message } => {
                Err(anyhow::anyhow!("secret bridge {code}: {message}"))
            }
        }
    }

    pub(crate) fn update_acl(
        key: &str,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        let resp = Self::call(&BridgeRequest::UpdateAcl {
            key: key.to_string(),
            allow,
            deny,
            namespace: None,
        })?;
        match resp {
            BridgeResponse::Ok { .. } => Ok(()),
            BridgeResponse::Error { code, message } => {
                Err(anyhow::anyhow!("secret bridge {code}: {message}"))
            }
        }
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

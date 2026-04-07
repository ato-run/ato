use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::state::{ActivityEntry, ActivityTone};

#[derive(Clone, Debug)]
pub struct GuestSessionContext {
    pub pane_id: usize,
    pub session_id: String,
    pub adapter: String,
    pub invoke_url: String,
    pub app_root: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum GuestBridgeRequest {
    Handshake {
        session: String,
    },
    Invoke {
        request_id: u64,
        command: String,
        capability: String,
        payload: Value,
    },
    CapabilityProbe {
        request_id: u64,
        capability: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum GuestBridgeResponse {
    Ok {
        request_id: Option<u64>,
        message: String,
        payload: Value,
    },
    Denied {
        request_id: Option<u64>,
        message: String,
    },
    Error {
        request_id: Option<u64>,
        message: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum ShellEvent {
    SessionReady { pane_id: usize },
    PermissionDenied { pane_id: usize, capability: String },
    SessionClosed { pane_id: usize },
    UrlChanged { pane_id: usize, url: String },
    TitleChanged { pane_id: usize, title: String },
}

#[derive(Clone, Default)]
pub struct BridgeProxy {
    activity: Arc<Mutex<Vec<ActivityEntry>>>,
    shell_events: Arc<Mutex<Vec<ShellEvent>>>,
}

impl BridgeProxy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn preload_environment(&self, allowlist: &[String]) -> String {
        serde_json::to_string(allowlist).expect("bridge allowlist should be serializable")
    }

    pub fn handle_message(
        &self,
        raw: &str,
        allowlist: &[String],
        session: Option<&GuestSessionContext>,
    ) -> GuestBridgeResponse {
        let request: GuestBridgeRequest = match serde_json::from_str(raw) {
            Ok(request) => request,
            Err(error) => {
                self.log(
                    ActivityTone::Error,
                    "Rejected malformed guest bridge payload",
                );
                return GuestBridgeResponse::Error {
                    request_id: None,
                    message: format!("invalid guest bridge payload: {error}"),
                };
            }
        };

        match request {
            GuestBridgeRequest::Handshake {
                session: guest_session,
            } => {
                self.log(
                    ActivityTone::Info,
                    format!("Guest session {guest_session} attached"),
                );
                GuestBridgeResponse::Ok {
                    request_id: None,
                    message: "handshake accepted".to_string(),
                    payload: session
                        .map(|context| {
                            serde_json::json!({
                                "sessionId": context.session_id,
                                "adapter": context.adapter,
                            })
                        })
                        .unwrap_or(Value::Null),
                }
            }
            GuestBridgeRequest::CapabilityProbe {
                request_id,
                capability,
            } => {
                if capability_allowed(allowlist, &capability) {
                    GuestBridgeResponse::Ok {
                        request_id: Some(request_id),
                        message: "capability granted".to_string(),
                        payload: serde_json::json!({ "capability": capability }),
                    }
                } else {
                    self.log(
                        ActivityTone::Warning,
                        format!("Denied guest probe for {capability}"),
                    );
                    GuestBridgeResponse::Denied {
                        request_id: Some(request_id),
                        message: format!("capability {capability} is not granted"),
                    }
                }
            }
            GuestBridgeRequest::Invoke {
                request_id,
                command,
                capability,
                payload,
            } => {
                if !capability_allowed(allowlist, &capability) {
                    self.log(
                        ActivityTone::Warning,
                        format!("Fail-closed guest invoke denied: {command} requires {capability}"),
                    );
                    return GuestBridgeResponse::Denied {
                        request_id: Some(request_id),
                        message: format!("capability {capability} is not granted"),
                    };
                }

                self.log(
                    ActivityTone::Info,
                    format!("Guest invoke {command} accepted under {capability}"),
                );

                match self.dispatch_invoke(&command, payload, session) {
                    Ok(payload) => GuestBridgeResponse::Ok {
                        request_id: Some(request_id),
                        message: format!("command {command} accepted"),
                        payload,
                    },
                    Err(error) => GuestBridgeResponse::Error {
                        request_id: Some(request_id),
                        message: error.to_string(),
                    },
                }
            }
        }
    }

    pub fn handle_payload_bytes(
        &self,
        payload: &[u8],
        allowlist: &[String],
        session: Option<&GuestSessionContext>,
    ) -> Result<GuestBridgeResponse> {
        let raw =
            std::str::from_utf8(payload).context("guest bridge payload is not valid UTF-8")?;
        Ok(self.handle_message(raw, allowlist, session))
    }

    pub fn serialize_response(&self, response: &GuestBridgeResponse) -> Result<Vec<u8>> {
        serde_json::to_vec(response).context("failed to serialize guest bridge response")
    }

    pub fn log(&self, tone: ActivityTone, message: impl Into<String>) {
        self.activity
            .lock()
            .expect("bridge activity lock poisoned")
            .push(ActivityEntry {
                tone,
                message: message.into(),
            });
    }

    pub fn drain_activity(&self) -> Vec<ActivityEntry> {
        let mut activity = self.activity.lock().expect("bridge activity lock poisoned");
        std::mem::take(&mut *activity)
    }

    pub fn push_shell_event(&self, event: ShellEvent) {
        self.shell_events
            .lock()
            .expect("bridge shell event lock poisoned")
            .push(event);
    }

    pub fn drain_shell_events(&self) -> Vec<ShellEvent> {
        let mut shell_events = self
            .shell_events
            .lock()
            .expect("bridge shell event lock poisoned");
        std::mem::take(&mut *shell_events)
    }

    fn dispatch_invoke(
        &self,
        command: &str,
        payload: Value,
        session: Option<&GuestSessionContext>,
    ) -> Result<Value> {
        match command {
            "shell.workspaceInfo" => Ok(payload),
            "plugin:window|setTitle" => {
                let title = payload
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Ato Desktop")
                    .to_string();
                self.log(ActivityTone::Info, format!("Host title request: {title}"));
                if let Some(session) = session {
                    self.push_shell_event(ShellEvent::TitleChanged {
                        pane_id: session.pane_id,
                        title: title.clone(),
                    });
                }
                Ok(serde_json::json!({ "title": title }))
            }
            "plugin:fs|readFile" => read_session_file(session, &payload),
            "plugin:dialog|open" => Ok(serde_json::json!({ "selected": Value::Null })),
            "shell.open" => open_external(&payload),
            _ => proxy_to_guest_backend(session, command, payload),
        }
    }
}

fn capability_allowed(allowlist: &[String], capability: &str) -> bool {
    allowlist.iter().any(|grant| grant == capability)
}

fn read_session_file(session: Option<&GuestSessionContext>, payload: &Value) -> Result<Value> {
    let session =
        session.ok_or_else(|| anyhow::anyhow!("file read requires guest session context"))?;
    let requested = payload
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("plugin:fs|readFile requires string payload.path"))?;

    let root = session
        .app_root
        .canonicalize()
        .with_context(|| format!("failed to resolve app root {}", session.app_root.display()))?;
    let candidate = root.join(requested);
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", candidate.display()))?;
    if !canonical.starts_with(&root) {
        anyhow::bail!("path escapes guest workspace boundary: {requested}");
    }

    let contents = fs::read_to_string(&canonical)
        .with_context(|| format!("failed to read {}", canonical.display()))?;
    Ok(serde_json::json!({ "path": requested, "contents": contents }))
}

fn open_external(payload: &Value) -> Result<Value> {
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("shell.open requires string payload.url"))?;
    let status = Command::new("open")
        .arg(url)
        .status()
        .with_context(|| format!("failed to invoke open for {url}"))?;
    if !status.success() {
        anyhow::bail!("open returned non-zero status for {url}");
    }
    Ok(serde_json::json!({ "opened": url }))
}

fn proxy_to_guest_backend(
    session: Option<&GuestSessionContext>,
    command: &str,
    payload: Value,
) -> Result<Value> {
    let session =
        session.ok_or_else(|| anyhow::anyhow!("backend invoke requires active guest session"))?;
    let url = Url::parse(&session.invoke_url)
        .with_context(|| format!("invalid invoke URL {}", session.invoke_url))?;
    let host = url.host_str().unwrap_or("127.0.0.1");
    let port = url.port_or_known_default().unwrap_or(80);
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": format!("guest-{}", command),
        "method": "capsule/invoke",
        "params": {
            "command": command,
            "payload": payload,
        }
    });
    let body =
        serde_json::to_vec(&request).context("failed to serialize backend invoke request")?;

    let mut stream = TcpStream::connect((host, port))
        .with_context(|| format!("failed to connect guest backend {}:{}", host, port))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    write!(
        stream,
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        path,
        host,
        body.len()
    )
    .context("failed to write backend invoke headers")?;
    stream
        .write_all(&body)
        .context("failed to write backend invoke body")?;
    let _ = stream.flush();

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .context("failed to read backend invoke response")?;
    let response_text =
        String::from_utf8(response).context("backend invoke response is not UTF-8")?;
    let (_, body) = response_text
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("backend invoke returned malformed HTTP response"))?;
    let value: Value =
        serde_json::from_str(body).context("backend invoke returned invalid JSON")?;
    if let Some(error) = value.get("error") {
        anyhow::bail!("guest backend returned error: {}", error);
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_denies_unknown_capability() {
        let bridge = BridgeProxy::new();
        let request = serde_json::json!({
            "kind": "invoke",
            "request_id": 1,
            "command": "shell.openExternal",
            "capability": "open-external",
            "payload": {"url": "https://example.com"}
        });
        let response =
            bridge.handle_message(&request.to_string(), &["read-file".to_string()], None);
        assert!(matches!(response, GuestBridgeResponse::Denied { .. }));
    }

    #[test]
    fn bridge_serializes_json_response() {
        let bridge = BridgeProxy::new();
        let response = GuestBridgeResponse::Ok {
            request_id: Some(7),
            message: "ok".to_string(),
            payload: serde_json::json!({"hello": "world"}),
        };
        let bytes = bridge
            .serialize_response(&response)
            .expect("serialize response");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("parse json");
        assert_eq!(value["status"], "ok");
        assert_eq!(value["request_id"], 7);
    }
}

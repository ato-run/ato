use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use tiny_http::{Header, Method, Response, Server, StatusCode};

#[derive(Debug, Clone)]
pub struct GuestContext {
    sample_root: PathBuf,
    adapter: String,
    session_id: String,
    guest_mode: Option<String>,
    host: String,
    port: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct CommandEnvelope {
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PingResponse {
    pub ok: bool,
    pub adapter: String,
    pub session_id: String,
    pub command: String,
    pub echo: String,
}

#[derive(Debug, Serialize)]
pub struct CheckEnvResponse {
    pub ok: bool,
    pub adapter: String,
    pub session_id: String,
    pub ato_guest_mode: Option<String>,
}

impl GuestContext {
    pub fn from_env(adapter: &str, default_port: u16, sample_root: impl Into<PathBuf>) -> Self {
        let guest_mode = match std::env::var("ATO_GUEST_MODE") {
            Ok(value) if value == "1" => Some(value),
            _ => None,
        };

        Self {
            sample_root: sample_root.into(),
            adapter: std::env::var("DESKY_SESSION_ADAPTER").unwrap_or_else(|_| adapter.to_string()),
            session_id: std::env::var("DESKY_SESSION_ID")
                .unwrap_or_else(|_| "desky-session".to_string()),
            guest_mode,
            host: std::env::var("DESKY_SESSION_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            port: std::env::var("DESKY_SESSION_PORT").unwrap_or_else(|_| default_port.to_string()),
        }
    }

    pub fn adapter(&self) -> &str {
        &self.adapter
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn guest_mode(&self) -> Option<&str> {
        self.guest_mode.as_deref()
    }

    pub fn sample_root(&self) -> &PathBuf {
        &self.sample_root
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn check_env(&self) -> CheckEnvResponse {
        CheckEnvResponse {
            ok: true,
            adapter: self.adapter.clone(),
            session_id: self.session_id.clone(),
            ato_guest_mode: self.guest_mode.clone(),
        }
    }

    pub fn ping_response(&self, message: String) -> PingResponse {
        PingResponse {
            ok: true,
            adapter: self.adapter.clone(),
            session_id: self.session_id.clone(),
            command: "ping".to_string(),
            echo: message,
        }
    }

    pub fn resolve_allowed_path(&self, relative_path: &str) -> Result<PathBuf, String> {
        let root = self
            .sample_root
            .canonicalize()
            .map_err(|err| err.to_string())?;
        let candidate = root.join(relative_path);
        let resolved = candidate.canonicalize().map_err(|err| err.to_string())?;
        if resolved != root && !resolved.starts_with(&root) {
            return Err(format!(
                "BoundaryPolicyError: Guest file read is outside the allowed root: {}",
                relative_path
            ));
        }
        Ok(resolved)
    }
}

pub fn builtin_result(
    context: &GuestContext,
    command: &str,
    envelope: &CommandEnvelope,
) -> Option<Value> {
    match command {
        "check_env" => Some(json!(context.check_env())),
        "ping" => Some(json!(
            context.ping_response(message_from_payload(&envelope.payload,))
        )),
        _ => None,
    }
}

pub fn message_from_payload(payload: &Value) -> String {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn serve_guest_http<F>(context: &GuestContext, mut handler: F) -> Result<(), String>
where
    F: FnMut(&GuestContext, &str, CommandEnvelope) -> Value,
{
    let server = Server::http(context.bind_addr()).map_err(|err| err.to_string())?;

    for mut request in server.incoming_requests() {
        match (request.method(), request.url()) {
            (&Method::Get, "/health") => {
                let response = json!({
                    "ok": true,
                    "adapter": context.adapter(),
                    "session_id": context.session_id(),
                    "guest_mode": context.guest_mode(),
                });
                let _ = request.respond(json_response(StatusCode(200), &response));
            }
            (&Method::Post, "/rpc") => {
                let response = match read_request_body(&mut request)
                    .and_then(parse_request)
                    .map(|request| dispatch_request(context, request, &mut handler))
                {
                    Ok(value) => value,
                    Err(error) => json!({
                        "jsonrpc": "2.0",
                        "error": error,
                    }),
                };
                let _ = request.respond(json_response(StatusCode(200), &response));
            }
            _ => {
                let response = json!({ "ok": false, "error": "not_found" });
                let _ = request.respond(json_response(StatusCode(404), &response));
            }
        }
    }

    Ok(())
}

fn dispatch_request<F>(context: &GuestContext, request: Value, handler: &mut F) -> Value
where
    F: FnMut(&GuestContext, &str, CommandEnvelope) -> Value,
{
    let Value::Object(map) = request else {
        return json!({ "jsonrpc": "2.0", "error": "invalid_request" });
    };

    let id = map.get("id").cloned().unwrap_or(Value::Null);
    let params = map.get("params").cloned().unwrap_or_else(|| json!({}));
    let command = params
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let envelope = serde_json::from_value(params).unwrap_or_default();

    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": handler(context, &command, envelope),
    })
}

fn read_request_body(request: &mut tiny_http::Request) -> Result<String, String> {
    let mut body = String::new();
    request
        .as_reader()
        .read_to_string(&mut body)
        .map_err(|err| err.to_string())?;
    Ok(body)
}

fn parse_request(body: String) -> Result<Value, String> {
    serde_json::from_str(if body.is_empty() { "{}" } else { &body }).map_err(|err| err.to_string())
}

fn json_response(status: StatusCode, value: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec(value).expect("serialize response");
    let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("content-type header");
    Response::from_data(body)
        .with_status_code(status)
        .with_header(header)
}

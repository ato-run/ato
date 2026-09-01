//! Scoped HTTP client used by the Activity MCP product adapter.
//!
//! A Controller binding credential is read once from a protected connection
//! file. Only its derived Controller session bearer is used for operation
//! traffic, and neither credential is formatted into errors or diagnostics.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use reqwest::Method;
use reqwest::blocking::{Client, Response};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_CONNECTION_FILE_BYTES: u64 = 16 * 1024;
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const EXTERNAL_MCP_CONTROLLER_KIND: &str = "external_mcp";

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityConnectionFile {
    pub api_url: String,
    pub activity_id: String,
    pub actor_id: String,
    #[serde(skip_serializing)]
    controller_key: String,
    #[serde(default)]
    trace_id: Option<String>,
}

impl fmt::Debug for ActivityConnectionFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActivityConnectionFile")
            .field("api_url", &self.api_url)
            .field("activity_id", &self.activity_id)
            .field("actor_id", &self.actor_id)
            .field("trace_id", &self.trace_id)
            .field("controller_key", &"[REDACTED]")
            .finish()
    }
}

impl ActivityConnectionFile {
    pub fn load(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("inspect connection file {}", path.display()))?;
        ensure!(
            metadata.file_type().is_file(),
            "connection file is not a regular file"
        );
        ensure!(
            metadata.len() <= MAX_CONNECTION_FILE_BYTES,
            "connection file exceeds size limit"
        );
        validate_private_permissions(path, &metadata)?;
        let bytes =
            fs::read(path).with_context(|| format!("read connection file {}", path.display()))?;
        let value: Self = serde_json::from_slice(&bytes).context("decode connection file")?;
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<()> {
        ensure!(valid_scoped_id(&self.activity_id), "invalid Activity id");
        ensure!(valid_scoped_id(&self.actor_id), "invalid Actor id");
        ensure!(
            self.trace_id.as_deref().is_none_or(valid_coop_trace_id),
            "invalid Coop trace id"
        );
        ensure!(
            self.controller_key.starts_with("atoc_")
                && (32..=160).contains(&self.controller_key.len()),
            "invalid Controller credential"
        );
        validate_api_url(&self.api_url).map(|_| ())
    }
}

#[cfg(unix)]
fn validate_private_permissions(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let mode = metadata.permissions().mode();
    ensure!(
        mode & 0o077 == 0,
        "connection file {} must not be accessible by group or other users (use chmod 600)",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_permissions(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ControllerSessionProjection {
    pub id: String,
    pub activity_id: String,
    pub actor_id: String,
    pub actor_run_id: String,
    pub epoch: u64,
    pub controller_kind: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ControllerSessionCreated {
    controller_session_token: String,
    session: ControllerSessionProjection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityApiError {
    pub status: u16,
    pub code: String,
}

impl fmt::Display for ActivityApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Activity API {} ({})", self.status, self.code)
    }
}

impl std::error::Error for ActivityApiError {}

pub struct ActivityClient {
    connection_path: PathBuf,
    base_url: reqwest::Url,
    http: Client,
    session: ControllerSessionProjection,
    session_token: Option<String>,
    trace_id: Option<String>,
    trace_started_at: Instant,
    first_agent_operation_reported: AtomicBool,
}

impl ActivityClient {
    pub fn connect(path: &Path) -> Result<Self> {
        let trace_started_at = Instant::now();
        let connection = ActivityConnectionFile::load(path)?;
        emit_coop_trace(
            connection.trace_id.as_deref(),
            "agent_start_requested",
            trace_started_at,
        );
        let base_url = validate_api_url(&connection.api_url)?;
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(70))
            .redirect(Policy::none())
            .user_agent(concat!("ato-activity-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("build Activity API client")?;
        emit_coop_trace(
            connection.trace_id.as_deref(),
            "agent_controller_created",
            trace_started_at,
        );
        let created = create_controller_session(&http, &base_url, &connection)?;
        emit_coop_trace(
            connection.trace_id.as_deref(),
            "agent_transport_connected",
            trace_started_at,
        );
        ensure!(
            created.session.activity_id == connection.activity_id
                && created.session.actor_id == connection.actor_id
                && created.session.controller_kind == EXTERNAL_MCP_CONTROLLER_KIND
                && created.session.epoch > 0
                && valid_scoped_id(&created.session.actor_run_id)
                && created.controller_session_token.starts_with("atoc_"),
            "Controller session escaped connection scope"
        );
        Ok(Self {
            connection_path: path.to_path_buf(),
            base_url,
            http,
            session: created.session,
            session_token: Some(created.controller_session_token),
            trace_id: connection.trace_id,
            trace_started_at,
            first_agent_operation_reported: AtomicBool::new(false),
        })
    }

    pub fn session(&self) -> &ControllerSessionProjection {
        &self.session
    }

    pub fn connection_path(&self) -> &Path {
        &self.connection_path
    }

    pub fn mark_agent_operation_applied(&self) {
        if !self
            .first_agent_operation_reported
            .swap(true, Ordering::Relaxed)
        {
            emit_coop_trace(
                self.trace_id.as_deref(),
                "first_agent_operation_applied",
                self.trace_started_at,
            );
            emit_coop_trace(
                self.trace_id.as_deref(),
                "agent_ready",
                self.trace_started_at,
            );
        }
    }

    pub fn get_context(&self) -> Result<Value> {
        self.session_request(Method::GET, "/v1/controller/context", None)
    }

    pub fn observe_surfaces(&self) -> Result<Value> {
        let value = self.session_request(Method::GET, "/v1/controller/surfaces", None)?;
        emit_coop_trace(
            self.trace_id.as_deref(),
            "agent_surface_discovered",
            self.trace_started_at,
        );
        Ok(value)
    }

    pub fn list_operations(&self, surface_id: &str) -> Result<Value> {
        ensure!(valid_scoped_id(surface_id), "invalid Surface id");
        self.session_request(
            Method::GET,
            &format!(
                "/v1/controller/surfaces/{}/operations",
                encode_path_segment(surface_id)
            ),
            None,
        )
    }

    pub fn invoke_operation(
        &self,
        operation_id: &str,
        surface_epoch: u64,
        arguments: Value,
        client_sequence: u64,
    ) -> Result<Value> {
        ensure!(valid_scoped_id(operation_id), "invalid Operation id");
        ensure!(surface_epoch > 0, "invalid Surface epoch");
        ensure!(client_sequence > 0, "invalid client sequence");
        self.session_request(
            Method::POST,
            &format!(
                "/v1/controller/operations/{}/invoke",
                encode_path_segment(operation_id)
            ),
            Some(json!({
                "surface_epoch": surface_epoch,
                "arguments": arguments,
                "client_sequence": client_sequence,
            })),
        )
    }

    pub fn read_operation(&self, operation_id: &str) -> Result<Value> {
        ensure!(valid_scoped_id(operation_id), "invalid Operation id");
        let value = self.session_request(
            Method::GET,
            &format!(
                "/v1/controller/operations/{}",
                encode_path_segment(operation_id)
            ),
            None,
        )?;
        let applied = value
            .get("receipt")
            .and_then(|receipt| receipt.get("result"))
            .and_then(Value::as_str)
            == Some("applied");
        if applied {
            self.mark_agent_operation_applied();
        }
        Ok(value)
    }

    pub fn read_memo(&self) -> Result<Value> {
        self.session_request(Method::GET, "/v1/controller/memo", None)
    }

    pub fn update_memo(&self, markdown: String, expected_version: u64) -> Result<Value> {
        ensure!(markdown.len() <= 64 * 1024, "memo exceeds size limit");
        self.session_request(
            Method::PATCH,
            "/v1/controller/memo",
            Some(json!({"markdown": markdown, "expected_version": expected_version})),
        )
    }

    pub fn list_interactions(&self) -> Result<Value> {
        self.session_request(Method::GET, "/v1/controller/interactions", None)
    }

    pub fn send_interaction(
        &self,
        to_actor_id: String,
        protocol_id: String,
        payload: Value,
    ) -> Result<Value> {
        ensure!(
            valid_scoped_id(&to_actor_id),
            "invalid destination Actor id"
        );
        ensure!(
            matches!(
                protocol_id.as_str(),
                "ato.actor.message@1"
                    | "ato.actor.request@1"
                    | "ato.actor.handoff@1"
                    | "ato.actor.notify@1"
            ),
            "unsupported communicative Interaction protocol"
        );
        self.session_request(
            Method::POST,
            "/v1/controller/interactions",
            Some(json!({
                "to_actor_id": to_actor_id,
                "protocol_id": protocol_id,
                "payload": payload,
            })),
        )
    }

    pub fn release_control(&mut self) -> Result<Value> {
        let result = self.session_request(Method::POST, "/v1/controller/release", Some(json!({})));
        if result.is_ok() {
            self.session_token = None;
        }
        result
    }

    fn session_request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let token = self
            .session_token
            .as_deref()
            .context("Controller session has been released")?;
        send_json(
            &self.http,
            &self.base_url,
            method,
            path,
            token,
            body,
            self.trace_id.as_deref(),
        )
    }
}

fn create_controller_session(
    http: &Client,
    base_url: &reqwest::Url,
    connection: &ActivityConnectionFile,
) -> Result<ControllerSessionCreated> {
    let value = send_json(
        http,
        base_url,
        Method::POST,
        "/v1/controller-sessions",
        &connection.controller_key,
        Some(json!({"controller_kind": EXTERNAL_MCP_CONTROLLER_KIND})),
        connection.trace_id.as_deref(),
    )?;
    serde_json::from_value(value).context("decode Controller session response")
}

fn send_json(
    http: &Client,
    base_url: &reqwest::Url,
    method: Method,
    path: &str,
    bearer: &str,
    body: Option<Value>,
    trace_id: Option<&str>,
) -> Result<Value> {
    let url = base_url
        .join(path.trim_start_matches('/'))
        .context("resolve Activity API route")?;
    let mut request = http
        .request(method, url)
        .bearer_auth(bearer)
        .header("accept", "application/json");
    if let Some(trace_id) = trace_id {
        request = request.header("x-ato-trace-id", trace_id);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().context("Activity API request failed")?;
    decode_response(response)
}

fn valid_coop_trace_id(value: &str) -> bool {
    value.len() == 37
        && value.starts_with("coop_")
        && value[5..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn emit_coop_trace(trace_id: Option<&str>, stage: &str, started_at: Instant) {
    let Some(trace_id) = trace_id else { return };
    eprintln!(
        "{}",
        json!({
            "event":"ato.coop.trace",
            "trace_id":trace_id,
            "component":"ato-activity-mcp",
            "stage":stage,
            "elapsed_ms":(started_at.elapsed().as_secs_f64() * 10000.0).round() / 10.0,
        })
    );
}

fn decode_response(response: Response) -> Result<Value> {
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        bail!("Activity API response exceeds size limit");
    }
    let bytes = response.bytes().context("read Activity API response")?;
    ensure!(
        bytes.len() as u64 <= MAX_RESPONSE_BYTES,
        "Activity API response exceeds size limit"
    );
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).context("decode Activity API response")?
    };
    if status.is_success() {
        return Ok(value);
    }
    let code = value
        .get("code")
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 80
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
        .unwrap_or("activity_api_error")
        .to_owned();
    Err(ActivityApiError {
        status: status.as_u16(),
        code,
    }
    .into())
}

fn validate_api_url(value: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(value).context("parse Activity API URL")?;
    let loopback_http =
        url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"));
    ensure!(
        url.scheme() == "https" || loopback_http,
        "Activity API URL must use HTTPS (HTTP is allowed only for loopback tests)"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Activity API URL contains forbidden components"
    );
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

fn valid_scoped_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn encode_path_segment(value: &str) -> String {
    // valid_scoped_id deliberately excludes all URL metacharacters.
    value.to_owned()
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use tempfile::NamedTempFile;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn connection_file_requires_private_permissions_and_redacts_debug() {
        use std::os::unix::fs::PermissionsExt as _;

        let mut file = NamedTempFile::new().expect("temporary connection file");
        let key = format!("atoc_{}", "x".repeat(43));
        write!(
            file,
            "{}",
            json!({
                "api_url":"https://staging.api.ato.run",
                "activity_id":"act_test",
                "actor_id":"actor_test",
                "controller_key":key,
            })
        )
        .expect("write connection file");
        fs::set_permissions(file.path(), fs::Permissions::from_mode(0o600))
            .expect("set private mode");
        let loaded = ActivityConnectionFile::load(file.path()).expect("load private connection");
        assert!(!format!("{loaded:?}").contains(&key));

        fs::set_permissions(file.path(), fs::Permissions::from_mode(0o644))
            .expect("set public mode");
        assert!(ActivityConnectionFile::load(file.path()).is_err());
    }

    #[test]
    fn rejects_credential_urls_and_non_loopback_http() {
        assert!(validate_api_url("http://api.ato.run").is_err());
        assert!(validate_api_url("https://user:secret@api.ato.run").is_err());
        assert!(validate_api_url("http://127.0.0.1:8787").is_ok());
    }

    #[test]
    fn api_errors_preserve_only_stable_code() {
        let error = ActivityApiError {
            status: 409,
            code: "fenced_controller".to_owned(),
        };
        assert_eq!(error.to_string(), "Activity API 409 (fenced_controller)");
    }
}

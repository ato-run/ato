//! Activity product-runtime controller for the connected worker.
//!
//! This module owns Room/WebRTC/media orchestration only. Browser interaction
//! still crosses the generic `ato.browser@1` ingress and its Evolution/Record
//! ordering before an Activity receipt is emitted.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use ato_adapter_browser::{
    BrowserEvent, BrowserSurfaceProjectionV1, OperationSource, SurfaceOperationDescriptorV1,
};

use super::BrowserControlIngress;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECEIPTS: usize = 1024;
const BROWSER_PROTOCOL: &str = "ato.browser@1";
const WEBMCP_PROTOCOL: &str = "ato.webmcp@1";
const CONTROLLER_HTML: &str = include_str!("activity_controller.html");

#[derive(Debug)]
pub(crate) enum ActivityControllerEvent {
    Ready,
    Ended,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityControllerPageConfig {
    pub run_id: String,
    pub room_url: String,
    pub executor_credential: String,
    pub ice_servers: Value,
}

pub(crate) struct ActivityControllerServer {
    target_url: String,
    context: Arc<ActivityControllerContext>,
    events: Receiver<ActivityControllerEvent>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl ActivityControllerServer {
    pub(crate) fn start(
        config: ActivityControllerPageConfig,
        ingress: Arc<dyn BrowserControlIngress>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .context("bind Activity controller")?;
        listener
            .set_nonblocking(true)
            .context("configure Activity controller")?;
        let address = listener.local_addr()?;
        let origin = format!("http://{address}");
        let secret = random_secret();
        let nonce = random_secret();
        let bootstrap_path = format!("/bootstrap/{}", random_secret());
        let target_url = format!("{origin}{bootstrap_path}");
        let room_origin = websocket_origin(&config.room_url)?;
        let html = controller_html(&config, &secret, &nonce)?;
        let csp = format!(
            "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; connect-src 'self' {room_origin}; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
        );
        let stopping = Arc::new(AtomicBool::new(false));
        let (event_tx, events) = mpsc::channel();
        let context = Arc::new(ActivityControllerContext {
            html,
            csp,
            secret,
            bootstrap_path,
            run_id: config.run_id,
            events: event_tx,
            frame: Mutex::new(None),
            receipts: Mutex::new(ReceiptCache::default()),
            surface: Mutex::new(None),
            abort_requests: Mutex::new(BTreeSet::new()),
            ingress,
        });
        let thread_context = Arc::clone(&context);
        let thread_stopping = Arc::clone(&stopping);
        let thread = thread::spawn(move || serve(listener, thread_context, thread_stopping));
        Ok(Self {
            target_url,
            context,
            events,
            stopping,
            thread: Some(thread),
        })
    }

    pub(crate) fn target_url(&self) -> &str {
        &self.target_url
    }

    pub(crate) fn publish_frame(&self, frame: Vec<u8>) -> Result<()> {
        ensure!(
            !frame.is_empty() && frame.len() <= MAX_FRAME_BYTES,
            "Activity presentation frame exceeds bound"
        );
        *self
            .context
            .frame
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity frame mutex poisoned"))? = Some(frame);
        Ok(())
    }

    pub(crate) fn publish_surface(
        &self,
        projection: BrowserSurfaceProjectionV1,
        registry_generation: u64,
    ) -> Result<()> {
        ensure!(
            projection.observation.target_run_id == self.context.run_id
                && projection.observation.surface_epoch > 0
                && registry_generation > 0,
            "Activity surface escaped its Run scope"
        );
        *self
            .context
            .surface
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity surface mutex poisoned"))? =
            Some(PublishedSurface {
                projection,
                registry_generation,
            });
        Ok(())
    }

    pub(crate) fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<ActivityControllerEvent, mpsc::RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }

    pub(crate) fn stop(mut self) -> Result<()> {
        self.stopping.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("Activity controller thread panicked"))??;
        }
        Ok(())
    }
}

impl Drop for ActivityControllerServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HostedRunInput {
    run_id: String,
    operation_id: String,
    client_seq: u64,
    adapter_id: String,
    protocol_id: String,
    event: Value,
    actor_participant_id: String,
    source_connection_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HostedOperationInput {
    operation_id: String,
    descriptor_id: String,
    actor_id: String,
    actor_run_id: String,
    controller_session_id: String,
    controller_epoch: u64,
    #[serde(default)]
    target_run_id: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    surface_id: String,
    surface_epoch: u64,
    protocol_id: String,
    operation_name: String,
    #[serde(default)]
    arguments: Value,
    client_sequence: u64,
    #[serde(default)]
    actor_participant_id: Option<String>,
}

impl HostedOperationInput {
    fn target_run_id(&self) -> Result<&str> {
        match (self.target_run_id.as_deref(), self.run_id.as_deref()) {
            (Some(target), None) | (None, Some(target)) => Ok(target),
            (Some(target), Some(run)) if target == run => Ok(target),
            _ => bail!("Activity operation target Run is ambiguous"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostedAbortInput {
    operation_id: String,
    actor_id: String,
    actor_run_id: String,
    controller_session_id: String,
    controller_epoch: u64,
    #[serde(default)]
    target_run_id: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    surface_id: String,
    surface_epoch: u64,
}

#[derive(Debug, Clone, Serialize)]
struct ActivityOperationReceipt {
    run_sequence: u64,
    operation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor_participant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    controller_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    controller_epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    surface_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    surface_epoch: Option<u64>,
    client_sequence: u64,
    result: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    output: Value,
    applied_at: String,
}

#[derive(Debug, Clone, Serialize)]
struct ActivityAbortReceipt {
    operation_id: String,
    actor_id: String,
    actor_run_id: String,
    controller_session_id: String,
    controller_epoch: u64,
    target_run_id: String,
    surface_id: String,
    surface_epoch: u64,
    status: &'static str,
    best_effort_result: &'static str,
    requested_at: String,
}

#[derive(Default)]
struct ReceiptCache {
    by_id: BTreeMap<String, (Vec<u8>, ActivityOperationReceipt)>,
    insertion_order: VecDeque<String>,
}

impl ReceiptCache {
    fn get(&self, operation_id: &str, payload: &[u8]) -> Result<Option<ActivityOperationReceipt>> {
        let Some((known, receipt)) = self.by_id.get(operation_id) else {
            return Ok(None);
        };
        ensure!(
            known == payload,
            "Activity operation id was reused with different input"
        );
        let mut duplicate = receipt.clone();
        duplicate.result = "duplicate".to_owned();
        Ok(Some(duplicate))
    }

    fn insert(
        &mut self,
        operation_id: String,
        payload: Vec<u8>,
        receipt: ActivityOperationReceipt,
    ) {
        if self.by_id.contains_key(&operation_id) {
            return;
        }
        self.insertion_order.push_back(operation_id.clone());
        self.by_id.insert(operation_id, (payload, receipt));
        while self.insertion_order.len() > MAX_RECEIPTS {
            if let Some(oldest) = self.insertion_order.pop_front() {
                self.by_id.remove(&oldest);
            }
        }
    }
}

struct ActivityControllerContext {
    html: String,
    csp: String,
    secret: String,
    bootstrap_path: String,
    run_id: String,
    events: Sender<ActivityControllerEvent>,
    frame: Mutex<Option<Vec<u8>>>,
    receipts: Mutex<ReceiptCache>,
    surface: Mutex<Option<PublishedSurface>>,
    abort_requests: Mutex<BTreeSet<String>>,
    ingress: Arc<dyn BrowserControlIngress>,
}

#[derive(Debug, Clone, Serialize)]
struct PublishedSurface {
    #[serde(flatten)]
    projection: BrowserSurfaceProjectionV1,
    #[serde(skip_serializing)]
    registry_generation: u64,
}

fn serve(
    listener: TcpListener,
    context: Arc<ActivityControllerContext>,
    stopping: Arc<AtomicBool>,
) -> Result<()> {
    while !stopping.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }
                let context = Arc::clone(&context);
                thread::spawn(move || {
                    if handle_request(&mut stream, &context).is_err() {
                        let _ = respond_json(
                            &mut stream,
                            400,
                            &serde_json::json!({"error":"invalid_request"}),
                        );
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error).context("accept Activity controller request"),
        }
    }
    Ok(())
}

fn handle_request(stream: &mut TcpStream, context: &ActivityControllerContext) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let request = read_request(stream)?;
    if request.method == "GET" && request.path == context.bootstrap_path {
        return respond(
            stream,
            200,
            "text/html; charset=utf-8",
            context.html.as_bytes(),
            &[("Content-Security-Policy", &context.csp)],
        );
    }
    if request.path == "/frame" && request.method == "GET" {
        authorize_host_request(&request, context)?;
        let frame = context
            .frame
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity frame mutex poisoned"))?
            .take();
        return match frame {
            Some(frame) => respond(stream, 200, "image/jpeg", &frame, &[]),
            None => respond(stream, 204, "application/octet-stream", &[], &[]),
        };
    }
    if request.path == "/surface" && request.method == "GET" {
        authorize_host_request(&request, context)?;
        let surface = context
            .surface
            .lock()
            .map_err(|_| anyhow::anyhow!("Activity surface mutex poisoned"))?
            .clone();
        return match surface {
            Some(surface) => respond_json(stream, 200, &surface),
            None => respond(stream, 204, "application/octet-stream", &[], &[]),
        };
    }
    authorize_host_request(&request, context)?;
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/input") => {
            let input: HostedRunInput =
                serde_json::from_slice(&request.body).context("decode Activity Browser input")?;
            let receipt = apply_input(context, input)?;
            respond_json(stream, 200, &receipt)
        }
        ("POST", "/operation/invoke") => {
            let input: HostedOperationInput = serde_json::from_slice(&request.body)
                .context("decode Activity operation invocation")?;
            match apply_operation(context, input) {
                Ok(receipt) => respond_json(stream, 200, &receipt),
                Err(error) => respond_json(
                    stream,
                    409,
                    &serde_json::json!({"error":stable_operation_error(&error)}),
                ),
            }
        }
        ("POST", "/operation/abort") => {
            let input: HostedAbortInput =
                serde_json::from_slice(&request.body).context("decode Activity operation abort")?;
            match request_abort(context, input) {
                Ok(receipt) => respond_json(stream, 200, &receipt),
                Err(error) => respond_json(
                    stream,
                    409,
                    &serde_json::json!({"error":stable_operation_error(&error)}),
                ),
            }
        }
        ("POST", "/ready") => {
            let _ = context.events.send(ActivityControllerEvent::Ready);
            respond_json(stream, 200, &serde_json::json!({"ok":true}))
        }
        ("POST", "/end") => {
            let _ = context.events.send(ActivityControllerEvent::Ended);
            respond_json(stream, 200, &serde_json::json!({"ok":true}))
        }
        ("POST", "/failure") => {
            let _ = context.events.send(ActivityControllerEvent::Failed);
            respond_json(stream, 200, &serde_json::json!({"ok":true}))
        }
        _ => respond_json(stream, 404, &serde_json::json!({"error":"not_found"})),
    }
}

fn authorize_host_request(
    request: &HttpRequest,
    context: &ActivityControllerContext,
) -> Result<()> {
    ensure!(
        request
            .headers
            .get("x-ato-activity-host")
            .map(String::as_str)
            == Some(context.secret.as_str()),
        "Activity controller authorization failed"
    );
    Ok(())
}

fn apply_input(
    context: &ActivityControllerContext,
    input: HostedRunInput,
) -> Result<ActivityOperationReceipt> {
    ensure!(
        input.run_id == context.run_id
            && input.adapter_id == BROWSER_PROTOCOL
            && input.protocol_id == BROWSER_PROTOCOL
            && !input.source_connection_id.is_empty()
            && !input.actor_participant_id.is_empty(),
        "Activity input escaped its Run scope"
    );
    let payload = serde_json::to_vec(&input)?;
    if let Some(receipt) = context
        .receipts
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
        .get(&input.operation_id, &payload)?
    {
        return Ok(receipt);
    }
    let event_bytes = serde_json::to_vec(&input.event)?;
    let event =
        ato_adapter_browser::decode_event(&event_bytes).context("decode Activity Browser event")?;
    let accepted = context
        .ingress
        .accept_control_operation(input.operation_id.clone(), event)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    ensure!(
        accepted.record_error.is_none(),
        "Activity Browser Record submission failed after apply"
    );
    let receipt = ActivityOperationReceipt {
        run_sequence: accepted.run_seq,
        operation_id: input.operation_id.clone(),
        actor_participant_id: Some(input.actor_participant_id),
        actor_id: None,
        actor_run_id: None,
        controller_session_id: None,
        controller_epoch: None,
        target_run_id: Some(context.run_id.clone()),
        surface_id: None,
        surface_epoch: None,
        client_sequence: input.client_seq,
        result: "applied".to_owned(),
        output: Value::Null,
        applied_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
    };
    context
        .receipts
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
        .insert(input.operation_id, payload, receipt.clone());
    Ok(receipt)
}

fn apply_operation(
    context: &ActivityControllerContext,
    input: HostedOperationInput,
) -> Result<ActivityOperationReceipt> {
    let target_run_id = input.target_run_id()?.to_owned();
    ensure!(
        target_run_id == context.run_id
            && !input.operation_id.is_empty()
            && !input.descriptor_id.is_empty()
            && !input.actor_id.is_empty()
            && !input.actor_run_id.is_empty()
            && !input.controller_session_id.is_empty()
            && input.controller_epoch > 0
            && input.client_sequence > 0,
        "Activity operation escaped its Controller scope"
    );
    let published = context
        .surface
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity surface mutex poisoned"))?
        .clone()
        .context("stale_operation")?;
    ensure!(
        published.projection.observation.surface_id == input.surface_id
            && published.projection.observation.surface_epoch == input.surface_epoch,
        "stale_operation"
    );
    let descriptor = published
        .projection
        .operations
        .iter()
        .find(|descriptor| descriptor.id == input.descriptor_id)
        .context("stale_operation")?;
    ensure!(
        descriptor.protocol_id == input.protocol_id
            && descriptor.operation_name == input.operation_name,
        "stale_operation"
    );
    let payload = serde_jcs::to_vec(&input)?;
    if let Some(receipt) = context
        .receipts
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
        .get(&input.operation_id, &payload)?
    {
        return Ok(receipt);
    }
    let event = operation_event(descriptor, &input.arguments, published.registry_generation)?;
    let accepted = context
        .ingress
        .accept_control_operation(input.operation_id.clone(), event)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    ensure!(
        accepted.record_error.is_none(),
        "Activity Browser Record submission failed after operation apply"
    );
    let abort_requested = context
        .abort_requests
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity abort mutex poisoned"))?
        .remove(&input.operation_id);
    let receipt = ActivityOperationReceipt {
        run_sequence: accepted.run_seq,
        operation_id: input.operation_id.clone(),
        actor_participant_id: input.actor_participant_id,
        actor_id: Some(input.actor_id),
        actor_run_id: Some(input.actor_run_id),
        controller_session_id: Some(input.controller_session_id),
        controller_epoch: Some(input.controller_epoch),
        target_run_id: Some(target_run_id),
        surface_id: Some(input.surface_id),
        surface_epoch: Some(input.surface_epoch),
        client_sequence: input.client_sequence,
        result: if abort_requested {
            "applied_after_abort_requested".to_owned()
        } else {
            "applied".to_owned()
        },
        // Page-provided output never becomes an Ato instruction. Callers can
        // re-observe the surface to inspect resulting state.
        output: serde_json::json!({"adapter_ack":true}),
        applied_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
    };
    context
        .receipts
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
        .insert(input.operation_id, payload, receipt.clone());
    Ok(receipt)
}

fn operation_event(
    descriptor: &SurfaceOperationDescriptorV1,
    arguments: &Value,
    registry_generation: u64,
) -> Result<BrowserEvent> {
    if descriptor.source == OperationSource::Webmcp {
        ensure!(descriptor.protocol_id == WEBMCP_PROTOCOL, "stale_operation");
        return Ok(BrowserEvent::Operation {
            operation_name: descriptor.operation_name.clone(),
            arguments: arguments.clone(),
            surface_generation: registry_generation,
        });
    }
    ensure!(
        descriptor.source == OperationSource::Browser && descriptor.protocol_id == BROWSER_PROTOCOL,
        "unsupported_operation"
    );
    let mut event = arguments
        .as_object()
        .cloned()
        .context("invalid_operation")?;
    let event_type = match descriptor.operation_name.as_str() {
        "browser_keyboard" => "keyboard",
        "browser_pointer" => "pointer",
        "browser_click" => "click",
        "browser_scroll_to" => "scroll",
        _ => bail!("unsupported_operation"),
    };
    event.insert("type".to_owned(), Value::String(event_type.to_owned()));
    let bytes = serde_jcs::to_vec(&Value::Object(event))?;
    ato_adapter_browser::decode_event(&bytes)
        .map_err(|error| anyhow::anyhow!("invalid_operation: {error}"))
}

fn request_abort(
    context: &ActivityControllerContext,
    input: HostedAbortInput,
) -> Result<ActivityAbortReceipt> {
    let target_run_id = match (input.target_run_id.as_deref(), input.run_id.as_deref()) {
        (Some(target), None) | (None, Some(target)) => target,
        (Some(target), Some(run)) if target == run => target,
        _ => bail!("invalid_operation"),
    };
    ensure!(
        target_run_id == context.run_id
            && input.controller_epoch > 0
            && !input.actor_id.is_empty()
            && !input.actor_run_id.is_empty()
            && !input.controller_session_id.is_empty(),
        "invalid_operation"
    );
    let published = context
        .surface
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity surface mutex poisoned"))?
        .clone()
        .context("stale_operation")?;
    ensure!(
        published.projection.observation.surface_id == input.surface_id
            && published.projection.observation.surface_epoch == input.surface_epoch,
        "stale_operation"
    );
    context
        .abort_requests
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity abort mutex poisoned"))?
        .insert(input.operation_id.clone());
    Ok(ActivityAbortReceipt {
        operation_id: input.operation_id,
        actor_id: input.actor_id,
        actor_run_id: input.actor_run_id,
        controller_session_id: input.controller_session_id,
        controller_epoch: input.controller_epoch,
        target_run_id: target_run_id.to_owned(),
        surface_id: input.surface_id,
        surface_epoch: input.surface_epoch,
        status: "abort_requested",
        best_effort_result: "settle_only",
        requested_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
    })
}

fn stable_operation_error(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    for code in [
        "stale_operation",
        "unsupported_operation",
        "invalid_operation",
        "fenced_controller",
    ] {
        if message.contains(code) {
            return code;
        }
    }
    "operation_failed"
}

struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest> {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            bail!("Activity controller request closed before headers");
        }
        bytes.extend_from_slice(&chunk[..read]);
        ensure!(
            bytes.len() <= MAX_HEADER_BYTES + MAX_BODY_BYTES,
            "Activity controller request exceeds bound"
        );
        if let Some(position) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            break position + 4;
        }
        ensure!(
            bytes.len() <= MAX_HEADER_BYTES,
            "Activity controller headers exceed bound"
        );
    };
    let header = std::str::from_utf8(&bytes[..header_end - 4])?;
    let mut lines = header.split("\r\n");
    let mut request_line = lines
        .next()
        .context("Activity controller request line missing")?
        .split_whitespace();
    let method = request_line
        .next()
        .context("request method missing")?
        .to_owned();
    let path = request_line
        .next()
        .context("request path missing")?
        .to_owned();
    ensure!(
        request_line.next() == Some("HTTP/1.1") && request_line.next().is_none(),
        "Activity controller requires HTTP/1.1"
    );
    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').context("invalid request header")?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    ensure!(
        content_length <= MAX_BODY_BYTES,
        "Activity controller body exceeds bound"
    );
    while bytes.len() - header_end < content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            bail!("Activity controller request closed before body");
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

fn respond_json(stream: &mut TcpStream, status: u16, value: &impl Serialize) -> Result<()> {
    respond(
        stream,
        status,
        "application/json",
        &serde_json::to_vec(value)?,
        &[],
    )
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        body.len()
    )?;
    for (name, value) in extra_headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    stream.write_all(b"\r\n")?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn random_secret() -> String {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn websocket_origin(value: &str) -> Result<String> {
    let parsed = url::Url::parse(value).context("parse Activity Room URL")?;
    ensure!(
        matches!(parsed.scheme(), "ws" | "wss")
            && parsed.username().is_empty()
            && parsed.password().is_none(),
        "Activity Room URL is invalid"
    );
    Ok(parsed.origin().ascii_serialization())
}

fn controller_html(
    config: &ActivityControllerPageConfig,
    secret: &str,
    nonce: &str,
) -> Result<String> {
    let mut value = serde_json::to_value(config)?;
    value["hostSecret"] = Value::String(secret.to_owned());
    let encoded = serde_json::to_string(&value)?.replace('<', "\\u003c");
    Ok(CONTROLLER_HTML
        .replace("__ATO_CONFIG__", &encoded)
        .replace("__ATO_NONCE__", nonce))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_uses_canvas_media_and_application_receipts() {
        assert!(CONTROLLER_HTML.contains("mediaCanvas.captureStream(30)"));
        assert!(CONTROLLER_HTML.contains("run.operation.receipt"));
        assert!(CONTROLLER_HTML.contains("surface.observe"));
        assert!(CONTROLLER_HTML.contains("surface.operations.replace"));
        assert!(CONTROLLER_HTML.contains("run.operation.invoke"));
        assert!(CONTROLLER_HTML.contains("run.operation.abort.receipt"));
        assert!(!CONTROLLER_HTML.contains("app_view_token"));
    }

    #[test]
    fn receipt_cache_rejects_operation_id_payload_conflicts() {
        let mut cache = ReceiptCache::default();
        cache.insert(
            "op-1".to_owned(),
            b"first".to_vec(),
            ActivityOperationReceipt {
                run_sequence: 1,
                operation_id: "op-1".to_owned(),
                actor_participant_id: Some("participant-1".to_owned()),
                actor_id: None,
                actor_run_id: None,
                controller_session_id: None,
                controller_epoch: None,
                target_run_id: Some("run-1".to_owned()),
                surface_id: None,
                surface_epoch: None,
                client_sequence: 1,
                result: "applied".to_owned(),
                output: Value::Null,
                applied_at: "2026-08-25T00:00:00Z".to_owned(),
            },
        );
        assert!(cache.get("op-1", b"different").is_err());
    }

    #[test]
    fn webmcp_operation_maps_to_generic_browser_ingress_without_raw_description() {
        let descriptor = SurfaceOperationDescriptorV1 {
            id: "descriptor-1".to_owned(),
            protocol_id: WEBMCP_PROTOCOL.to_owned(),
            operation_name: "increment_counter".to_owned(),
            safe_description: "safe".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            source: OperationSource::Webmcp,
            origin: "https://fixture.example".to_owned(),
            read_only: false,
            discovered_at: "now".to_owned(),
        };
        assert_eq!(
            operation_event(&descriptor, &serde_json::json!({"amount":1}), 7)
                .expect("operation should map"),
            BrowserEvent::Operation {
                operation_name: "increment_counter".to_owned(),
                arguments: serde_json::json!({"amount":1}),
                surface_generation: 7,
            }
        );
    }

    #[test]
    fn fixed_browser_operations_map_to_legacy_events() {
        let descriptor = SurfaceOperationDescriptorV1 {
            id: "descriptor-click".to_owned(),
            protocol_id: BROWSER_PROTOCOL.to_owned(),
            operation_name: "browser_click".to_owned(),
            safe_description: "safe".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            source: OperationSource::Browser,
            origin: "https://fixture.example".to_owned(),
            read_only: false,
            discovered_at: "now".to_owned(),
        };
        assert_eq!(
            operation_event(
                &descriptor,
                &serde_json::json!({
                    "x_normalized":0.25,
                    "y_normalized":0.75,
                    "button":0
                }),
                1,
            )
            .expect("click should map"),
            BrowserEvent::Click {
                x_normalized: 0.25,
                y_normalized: 0.75,
                button: 0,
            }
        );
    }

    #[test]
    fn receipt_serialization_carries_actor_controller_and_runner_ordering() {
        let receipt = ActivityOperationReceipt {
            run_sequence: 91,
            operation_id: "operation-91".to_owned(),
            actor_participant_id: None,
            actor_id: Some("actor-1".to_owned()),
            actor_run_id: Some("actor-run-1".to_owned()),
            controller_session_id: Some("controller-session-1".to_owned()),
            controller_epoch: Some(4),
            target_run_id: Some("browser-run-1".to_owned()),
            surface_id: Some("surface-1".to_owned()),
            surface_epoch: Some(3),
            client_sequence: 2,
            result: "applied".to_owned(),
            output: serde_json::json!({"adapter_ack":true}),
            applied_at: "2026-08-26T00:00:00Z".to_owned(),
        };
        let value = serde_json::to_value(receipt).expect("receipt should serialize");
        assert_eq!(value["run_sequence"], 91);
        assert_eq!(value["actor_id"], "actor-1");
        assert_eq!(value["actor_run_id"], "actor-run-1");
        assert_eq!(value["controller_session_id"], "controller-session-1");
        assert_eq!(value["controller_epoch"], 4);
        assert_eq!(value["surface_epoch"], 3);
    }
}

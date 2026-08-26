//! Activity product-runtime controller for the connected worker.
//!
//! This module owns Room/WebRTC/media orchestration only. Browser interaction
//! still crosses the generic `ato.browser@1` ingress and its Evolution/Record
//! ordering before an Activity receipt is emitted.

use std::collections::{BTreeMap, VecDeque};
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

use super::BrowserControlIngress;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECEIPTS: usize = 1024;
const BROWSER_PROTOCOL: &str = "ato.browser@1";
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
            ingress,
        });
        let thread_context = Arc::clone(&context);
        let thread_stopping = Arc::clone(&stopping);
        let thread = thread::spawn(move || serve(listener, &thread_context, &thread_stopping));
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

#[derive(Debug, Clone, Serialize)]
struct ActivityOperationReceipt {
    run_sequence: u64,
    operation_id: String,
    actor_participant_id: String,
    client_sequence: u64,
    result: &'static str,
    applied_at: String,
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
        duplicate.result = "duplicate";
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
    ingress: Arc<dyn BrowserControlIngress>,
}

fn serve(
    listener: TcpListener,
    context: &ActivityControllerContext,
    stopping: &AtomicBool,
) -> Result<()> {
    while !stopping.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }
                if handle_request(&mut stream, context).is_err() {
                    let _ = respond_json(
                        &mut stream,
                        400,
                        &serde_json::json!({"error":"invalid_request"}),
                    );
                }
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
    authorize_host_request(&request, context)?;
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/input") => {
            let input: HostedRunInput =
                serde_json::from_slice(&request.body).context("decode Activity Browser input")?;
            let receipt = apply_input(context, input)?;
            respond_json(stream, 200, &receipt)
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
        actor_participant_id: input.actor_participant_id,
        client_sequence: input.client_seq,
        result: "applied",
        applied_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
    };
    context
        .receipts
        .lock()
        .map_err(|_| anyhow::anyhow!("Activity receipt mutex poisoned"))?
        .insert(input.operation_id, payload, receipt.clone());
    Ok(receipt)
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
                actor_participant_id: "participant-1".to_owned(),
                client_sequence: 1,
                result: "applied",
                applied_at: "2026-08-25T00:00:00Z".to_owned(),
            },
        );
        assert!(cache.get("op-1", b"different").is_err());
    }
}

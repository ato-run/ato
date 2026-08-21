use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::activity_executor_gateway::{ActivityInputRequest, exchange};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;
const BROWSER_PROTOCOL: &str = "ato.browser@1";

#[derive(Debug)]
pub(crate) enum ActivityHostEvent {
    Ready,
    Ended,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityHostPageConfig {
    pub run_id: String,
    pub experience_url: String,
    pub experience_origin: String,
    pub room_url: String,
    pub executor_credential: String,
    pub ice_servers: Value,
}

pub(crate) struct ActivityHostServer {
    target_url: String,
    expected_origin: String,
    events: Receiver<ActivityHostEvent>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl ActivityHostServer {
    pub(crate) fn start(config: ActivityHostPageConfig, repository_root: PathBuf) -> Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .context("bind Activity Browser controller")?;
        listener
            .set_nonblocking(true)
            .context("configure Activity Browser controller")?;
        let address = listener.local_addr()?;
        let expected_origin = format!("http://{address}");
        let secret = random_secret();
        let bootstrap_path = format!("/bootstrap/{secret}");
        let target_url = format!("{expected_origin}{bootstrap_path}");
        let nonce = random_secret();
        let html = activity_host_html(&config, &secret, &nonce)?;
        let room_origin = websocket_origin(&config.room_url)?;
        let csp = format!(
            "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; frame-src {}; connect-src 'self' {room_origin}; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            config.experience_origin
        );
        let stopping = Arc::new(AtomicBool::new(false));
        let thread_stopping = Arc::clone(&stopping);
        let (event_tx, events) = mpsc::channel();
        let context = ActivityHostContext {
            html,
            csp,
            secret,
            bootstrap_path,
            run_id: config.run_id,
            repository_root,
            events: event_tx,
        };
        let thread = thread::spawn(move || serve(listener, &context, &thread_stopping));
        Ok(Self {
            target_url,
            expected_origin,
            events,
            stopping,
            thread: Some(thread),
        })
    }

    pub(crate) fn target_url(&self) -> &str {
        &self.target_url
    }

    pub(crate) fn expected_origin(&self) -> &str {
        &self.expected_origin
    }

    pub(crate) fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<ActivityHostEvent, mpsc::RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }

    pub(crate) fn stop(mut self) -> Result<()> {
        self.stopping.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("Activity Browser controller thread panicked"))??;
        }
        Ok(())
    }
}

impl Drop for ActivityHostServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
    }
}

#[derive(Debug, Deserialize)]
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

struct ActivityHostContext {
    html: String,
    csp: String,
    secret: String,
    bootstrap_path: String,
    run_id: String,
    repository_root: PathBuf,
    events: Sender<ActivityHostEvent>,
}

fn serve(
    listener: TcpListener,
    context: &ActivityHostContext,
    stopping: &AtomicBool,
) -> Result<()> {
    while !stopping.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }
                if let Err(error) = handle_request(&mut stream, context) {
                    let _ = respond_json(
                        &mut stream,
                        400,
                        &serde_json::json!({"error":"invalid_request"}),
                    );
                    eprintln!("Activity Browser controller rejected request: {error:#}");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error).context("accept Activity Browser controller request"),
        }
    }
    Ok(())
}

fn handle_request(stream: &mut TcpStream, context: &ActivityHostContext) -> Result<()> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .context("set Activity controller read deadline")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .context("set Activity controller write deadline")?;
    let request = read_request(stream)?;
    if request.method == "GET" && request.path == context.bootstrap_path.as_str() {
        return respond(
            stream,
            200,
            "text/html; charset=utf-8",
            context.html.as_bytes(),
            &[("Content-Security-Policy", &context.csp)],
        );
    }
    if request
        .headers
        .get("x-ato-activity-host")
        .map(String::as_str)
        != Some(context.secret.as_str())
    {
        return respond_json(stream, 403, &serde_json::json!({"error":"forbidden"}));
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/input") => {
            let input: HostedRunInput =
                serde_json::from_slice(&request.body).context("decode hosted Browser input")?;
            if input.run_id != context.run_id.as_str()
                || input.adapter_id != BROWSER_PROTOCOL
                || input.protocol_id != BROWSER_PROTOCOL
                || input.source_connection_id.is_empty()
            {
                bail!("hosted Browser input escaped its Run scope");
            }
            let receipt = exchange(
                &context.repository_root,
                &ActivityInputRequest {
                    request_id: format!("gateway_{}", input.operation_id),
                    operation_id: input.operation_id,
                    actor_participant_id: input.actor_participant_id,
                    client_sequence: input.client_seq,
                    adapter_id: input.adapter_id,
                    protocol_id: input.protocol_id,
                    event: input.event,
                },
            )?;
            respond_json(stream, 200, &receipt)
        }
        ("POST", "/ready") => {
            let _ = context.events.send(ActivityHostEvent::Ready);
            respond_json(stream, 200, &serde_json::json!({"ok":true}))
        }
        ("POST", "/end") => {
            let _ = context.events.send(ActivityHostEvent::Ended);
            respond_json(stream, 200, &serde_json::json!({"ok":true}))
        }
        ("POST", "/failure") => {
            let _ = context.events.send(ActivityHostEvent::Failed);
            respond_json(stream, 200, &serde_json::json!({"ok":true}))
        }
        _ => respond_json(stream, 404, &serde_json::json!({"error":"not_found"})),
    }
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
        let read = stream
            .read(&mut chunk)
            .context("read Activity controller request")?;
        if read == 0 {
            bail!("Activity controller request closed before headers");
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_HEADER_BYTES + MAX_BODY_BYTES {
            bail!("Activity controller request exceeds bounds");
        }
        if let Some(position) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            break position + 4;
        }
        if bytes.len() > MAX_HEADER_BYTES {
            bail!("Activity controller request headers exceed bounds");
        }
    };
    let header = std::str::from_utf8(&bytes[..header_end - 4])
        .context("Activity controller request headers are not UTF-8")?;
    let mut lines = header.split("\r\n");
    let mut request_line = lines
        .next()
        .context("Activity controller request has no request line")?
        .split_whitespace();
    let method = request_line
        .next()
        .context("request method missing")?
        .to_owned();
    let path = request_line
        .next()
        .context("request path missing")?
        .to_owned();
    if request_line.next() != Some("HTTP/1.1") || request_line.next().is_some() {
        bail!("Activity controller requires HTTP/1.1");
    }
    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').context("invalid request header")?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("invalid Content-Length")?
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        bail!("Activity controller request body exceeds bounds");
    }
    while bytes.len() - header_end < content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream
            .read(&mut chunk)
            .context("read Activity controller body")?;
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
        400 => "Bad Request",
        403 => "Forbidden",
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
    if !matches!(parsed.scheme(), "ws" | "wss") {
        bail!("Activity Room controller URL must use WebSocket");
    }
    Ok(parsed.origin().ascii_serialization())
}

fn activity_host_html(
    config: &ActivityHostPageConfig,
    secret: &str,
    nonce: &str,
) -> Result<String> {
    let mut value = serde_json::to_value(config)?;
    value["hostSecret"] = Value::String(secret.to_owned());
    let encoded = serde_json::to_string(&value)?.replace('<', "\\u003c");
    Ok(ACTIVITY_HOST_HTML
        .replace("__ATO_CONFIG__", &encoded)
        .replace("__ATO_NONCE__", nonce))
}

const ACTIVITY_HOST_HTML: &str = r#"<!doctype html>
<html lang="en" data-ato-browser-apply-bridge="post-message">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>Ato Activity Run</title>
  <style nonce="__ATO_NONCE__">html,body,iframe{width:100%;height:100%;margin:0;border:0;background:#030503}body{overflow:hidden}</style>
</head>
<body>
  <iframe id="experience" title="Hosted Browser Experience" allow="autoplay" sandbox="allow-scripts allow-same-origin"></iframe>
  <script nonce="__ATO_NONCE__">
  (() => {
    "use strict";
    const config = __ATO_CONFIG__;
    const roomProtocol = "ato.activity.room@1";
    const roomWebSocketProtocol = "ato.activity.room.v1";
    const experienceProtocol = "ato.activity-experience@1";
    const hostApplyProtocol = "ato.browser.host-apply@1";
    const frame = document.getElementById("experience");
    const channelToken = cryptoToken();
    const instanceId = `ahex_${cryptoToken()}`;
    const experienceUrl = new URL(config.experienceUrl);
    experienceUrl.hash = new URLSearchParams({
      channel_token: channelToken,
      instance_id: instanceId,
      parent_origin: location.origin,
      run_id: config.runId,
    }).toString();
    frame.src = experienceUrl.toString();

    let room = null;
    let roomOutboundSequence = 0;
    let roomInboundSequence = 0;
    let roomIdentity = null;
    let experienceInboundSequence = 0;
    let experienceOutboundSequence = 0;
    let experienceReady = false;
    let readyReported = false;
    let ending = false;
    let activeInput = null;
    let inputChain = Promise.resolve();
    const subscribers = new Set();
    const peers = new Map();

    addEventListener("message", (event) => {
      if (event.source === frame.contentWindow && event.origin === config.experienceOrigin) {
        receiveExperience(event.data);
        return;
      }
      if (event.source === window && event.origin === location.origin) {
        receiveBrowserApply(event.data);
      }
    });

    connectRoom();

    function connectRoom() {
      room = new WebSocket(config.roomUrl, [
        roomWebSocketProtocol,
        `activity-executor.${config.executorCredential}`,
      ]);
      room.addEventListener("message", (event) => {
        let envelope;
        try { envelope = JSON.parse(String(event.data)); } catch { return fail(); }
        if (!isObject(envelope) || envelope.protocol !== roomProtocol || !Number.isSafeInteger(envelope.seq) || envelope.seq <= roomInboundSequence) return fail();
        roomInboundSequence = envelope.seq;
        receiveRoom(envelope.type, isObject(envelope.payload) ? envelope.payload : {});
      });
      room.addEventListener("close", () => { if (!ending) fail(); });
      room.addEventListener("error", () => { if (!ending) fail(); });
    }

    function receiveRoom(type, payload) {
      if (type === "room.connected") {
        if (typeof payload.connection_id !== "string" || !Number.isSafeInteger(payload.media_generation)) return fail();
        roomIdentity = { connectionId: payload.connection_id, mediaGeneration: payload.media_generation };
        for (const participant of Array.isArray(payload.participant_connections) ? payload.participant_connections : []) {
          if (isObject(participant) && typeof participant.participant_id === "string" && Array.isArray(participant.subscribable_run_ids) && participant.subscribable_run_ids.includes(config.runId)) {
            startPeer(participant.participant_id);
          }
        }
        announceReady();
        return;
      }
      if (type === "participant.connected" && typeof payload.participant_id === "string") {
        startPeer(payload.participant_id);
        return;
      }
      if (type === "participant.disconnected" && typeof payload.participant_id === "string") {
        stopParticipant(payload.participant_id, true);
        return;
      }
      if (type === "run.input") {
        inputChain = inputChain.then(() => applyInput(payload)).catch(() => fail());
        return;
      }
      if (type === "run.media.answer" || type === "run.media.ice") {
        if (matchesPublishedPeer(payload)) {
          sendExperience(type === "run.media.answer" ? "experience.media.answer" : "experience.media.ice", payload);
        }
        return;
      }
      if (type === "run.media.closed") {
        stopPeer(payload.peer_id, false);
        return;
      }
      if (type === "activity.started") {
        const delay = Math.max(0, Date.parse(payload.starts_at) - Date.parse(payload.server_now));
        if (!Number.isFinite(delay)) return fail();
        setTimeout(() => sendExperience("experience.resume", {}), delay);
        return;
      }
      if (type === "activity.ended") end();
    }

    function receiveExperience(value) {
      if (!isObject(value) || value.protocol !== experienceProtocol || value.channelToken !== channelToken || value.instanceId !== instanceId || !Number.isSafeInteger(value.sequence) || value.sequence <= experienceOutboundSequence || typeof value.type !== "string" || !isObject(value.payload)) return;
      experienceOutboundSequence = value.sequence;
      if (value.type === "experience.hello") {
        sendExperience("experience.configure", { run_id: config.runId });
      } else if (value.type === "experience.ready") {
        experienceReady = true;
        announceReady();
      } else if (value.type === "experience.state") {
        if (value.payload.state === "paused") sendRoom("run.state", { run_id: config.runId, state: "paused" });
        if (value.payload.state === "running") sendRoom("run.state", { run_id: config.runId, state: "live" });
        if (value.payload.state === "failed") fail();
      } else if (value.type === "experience.browser.applied") {
        const applied = activeInput && value.payload.operation_id === activeInput.operation_id && value.payload.applied === true;
        postMessage({ protocol: hostApplyProtocol, type: applied ? "applied" : "error", request_id: activeInput?.browserRequestId ?? "" }, location.origin);
      } else if (value.type === "experience.media.offer") {
        sendRoom("run.media.offer", value.payload);
      } else if (value.type === "experience.media.ice") {
        sendRoom("run.media.ice", value.payload);
      } else if (value.type === "experience.media.error") {
        stopPeer(value.payload.peer_id, true);
      }
    }

    function receiveBrowserApply(value) {
      if (!isObject(value) || value.protocol !== hostApplyProtocol || value.type !== "apply" || typeof value.request_id !== "string" || !activeInput) return;
      activeInput.browserRequestId = value.request_id;
      sendExperience("experience.browser.apply", {
        operation_id: activeInput.operation_id,
        client_sequence: activeInput.client_seq,
        adapter_id: "ato.browser@1",
        protocol_id: "ato.browser@1",
        event: value.event,
      });
    }

    async function applyInput(input) {
      if (!isObject(input) || input.run_id !== config.runId || typeof input.operation_id !== "string") throw new Error("invalid input");
      activeInput = { ...input, browserRequestId: null };
      try {
        const response = await hostFetch("/input", input);
        if (!response.ok) throw new Error("apply failed");
        const receipt = await response.json();
        sendRoom("run.operation.receipt", {
          run_id: config.runId,
          run_sequence: receipt.run_sequence,
          operation_id: receipt.operation_id,
          actor_participant_id: receipt.actor_participant_id,
          client_sequence: receipt.client_sequence,
          result: receipt.result,
          adapter_id: receipt.adapter_id,
          record_ref: receipt.record_ref,
          applied_at: new Date().toISOString(),
        });
      } finally {
        activeInput = null;
      }
    }

    function startPeer(participantId) {
      subscribers.add(participantId);
      if (!roomIdentity || !experienceReady || peers.has(participantId)) return;
      const signal = {
        run_id: config.runId,
        peer_id: `peer_${cryptoToken()}`,
        media_generation: roomIdentity.mediaGeneration,
        publisher_connection_id: roomIdentity.connectionId,
        subscriber_participant_id: participantId,
      };
      peers.set(participantId, signal);
      sendExperience("experience.media.start", { ...signal, ice_servers: config.iceServers });
    }

    function announceReady() {
      if (!experienceReady || !roomIdentity) return;
      for (const participantId of subscribers) startPeer(participantId);
      sendRoom("run.state", { run_id: config.runId, state: "paused" });
      if (!readyReported) {
        readyReported = true;
        void hostFetch("/ready", {});
      }
    }

    function stopParticipant(participantId, publish) {
      subscribers.delete(participantId);
      const signal = peers.get(participantId);
      if (signal) stopPeer(signal.peer_id, publish);
    }

    function stopPeer(peerId, publish) {
      const entry = [...peers.entries()].find(([, signal]) => signal.peer_id === peerId);
      if (!entry) return;
      const [participantId, signal] = entry;
      peers.delete(participantId);
      if (publish) sendRoom("run.media.stop", signal);
      sendExperience("experience.media.stop", signal);
    }

    function matchesPublishedPeer(payload) {
      return isObject(payload) && [...peers.values()].some((signal) => signal.peer_id === payload.peer_id && signal.publisher_connection_id === payload.publisher_connection_id && signal.subscriber_participant_id === payload.subscriber_participant_id);
    }

    function sendExperience(type, payload) {
      frame.contentWindow?.postMessage({ protocol: experienceProtocol, channelToken, instanceId, sequence: ++experienceInboundSequence, type, payload }, config.experienceOrigin);
    }

    function sendRoom(type, payload) {
      if (room?.readyState !== WebSocket.OPEN) return;
      room.send(JSON.stringify({ protocol: roomProtocol, seq: ++roomOutboundSequence, type, payload }));
    }

    async function end() {
      if (ending) return;
      ending = true;
      for (const signal of [...peers.values()]) {
        sendRoom("run.media.stop", signal);
        sendExperience("experience.media.stop", signal);
      }
      peers.clear();
      sendExperience("experience.dispose", {});
      room?.close(1000, "Activity ended");
      await hostFetch("/end", {});
    }

    function fail() {
      if (ending) return;
      ending = true;
      room?.close(1011, "Runner failed");
      void hostFetch("/failure", {});
    }

    function hostFetch(path, body) {
      return fetch(path, { method: "POST", headers: { "content-type": "application/json", "x-ato-activity-host": config.hostSecret }, body: JSON.stringify(body), cache: "no-store" });
    }

    function cryptoToken() {
      const bytes = crypto.getRandomValues(new Uint8Array(24));
      return btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
    }

    function isObject(value) { return value !== null && typeof value === "object" && !Array.isArray(value); }
  })();
  </script>
</body>
</html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn page_config() -> ActivityHostPageConfig {
        ActivityHostPageConfig {
            run_id: "arun_01KAAAAAAAAAAAAAAAAAAAAA0".to_owned(),
            experience_url: "https://experience.example/index.html".to_owned(),
            experience_origin: "https://experience.example".to_owned(),
            room_url: "wss://activity.example/runner-room".to_owned(),
            executor_credential: "ato_aes_secret".to_owned(),
            ice_servers: serde_json::json!([{"urls":["stun:stun.example:3478"]}]),
        }
    }

    #[test]
    fn host_page_keeps_executor_credential_out_of_urls_and_uses_one_capture_source() {
        let config = page_config();
        let html = activity_host_html(&config, "local_secret", "nonce").unwrap();
        assert!(html.contains("activity-executor.${config.executorCredential}"));
        assert!(html.contains("experience.media.start"));
        assert!(html.contains("const subscribers = new Set()"));
        assert!(!config.room_url.contains(&config.executor_credential));
        assert!(!html.contains("turn:"));
    }

    #[test]
    fn loopback_bootstrap_is_private_and_never_cacheable() {
        let repository = tempfile::tempdir().unwrap();
        let server =
            ActivityHostServer::start(page_config(), repository.path().to_owned()).unwrap();
        assert!(server.target_url().starts_with(server.expected_origin()));
        assert!(server.target_url().contains("/bootstrap/"));
        assert!(!server.target_url().contains("ato_aes_secret"));

        let client = reqwest::blocking::Client::new();
        let response = client.get(server.target_url()).send().unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::CACHE_CONTROL)
                .unwrap(),
            "no-store"
        );
        assert!(response.headers().contains_key("content-security-policy"));

        let denied = client
            .get(format!("{}/", server.expected_origin()))
            .send()
            .unwrap();
        assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);
        server.stop().unwrap();
    }
}

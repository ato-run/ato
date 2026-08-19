//! Runtime-private Browser presentation capture through the Browser Host CDP.
//! The caller owns frontier association and Replay ordering.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use ato_adapter_api::{AdapterError, PresentationAsset, PresentationKind};
use base64::Engine;
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket, client};
use url::Url;

const PROFILE_DIR_NAME: &str = "browser-host-profile";
const CDP_PORT_FILE_NAME: &str = "browser-host-cdp-port";
const INITIAL_FRAME_FILE_NAME: &str = "browser-host-initial.png";
const INITIAL_FRAME_METADATA_FILE_NAME: &str = "browser-host-initial.json";
const IO_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_SCREENSHOT_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn capture_final(
    runtime_dir: &Path,
    expected_origin: &str,
) -> Result<Vec<PresentationAsset>, AdapterError> {
    let mut assets = initial_frame(runtime_dir)?.into_iter().collect::<Vec<_>>();
    assets.extend(capture(
        runtime_dir,
        expected_origin,
        PresentationKind::FinalState,
        0,
    )?);
    Ok(assets)
}

pub(crate) fn capture_keyframe(
    runtime_dir: &Path,
    expected_origin: &str,
    sequence: u32,
) -> Result<Option<PresentationAsset>, AdapterError> {
    capture(
        runtime_dir,
        expected_origin,
        PresentationKind::ArchiveKeyframe,
        sequence,
    )
}

fn capture(
    runtime_dir: &Path,
    expected_origin: &str,
    kind: PresentationKind,
    sequence: u32,
) -> Result<Option<PresentationAsset>, AdapterError> {
    let port_path = runtime_dir.join(PROFILE_DIR_NAME).join(CDP_PORT_FILE_NAME);
    if !port_path.exists() {
        return Ok(None);
    }
    let port_bytes = std::fs::read(&port_path)?;
    if port_bytes.len() > 16 {
        return Err(operation("Browser Host CDP port file is too large"));
    }
    let port = std::str::from_utf8(&port_bytes)
        .map_err(|_| operation("Browser Host CDP port is not UTF-8"))?
        .trim()
        .parse::<u16>()
        .map_err(|_| operation("Browser Host CDP port is invalid"))?;
    if port == 0 {
        return Err(operation("Browser Host CDP port must not be zero"));
    }

    let version = debugger_json(port, "/json/version")?;
    let websocket_url = version
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| operation("Browser Host CDP version has no WebSocket URL"))?;
    let mut cdp = Cdp::connect(websocket_url, port)?;
    let targets = cdp.call("Target.getTargets", json!({}), None)?;
    let target_id = select_page_target(&targets, expected_origin)?.to_owned();
    let attached = cdp.call(
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        None,
    )?;
    let session_id = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| operation("Browser Host CDP attach has no sessionId"))?
        .to_owned();
    let metrics = cdp.call("Page.getLayoutMetrics", json!({}), Some(&session_id))?;
    let viewport = metrics
        .get("cssVisualViewport")
        .or_else(|| metrics.get("visualViewport"))
        .ok_or_else(|| operation("Browser Host CDP returned no visual viewport"))?;
    let width = bounded_dimension(viewport, "clientWidth")?;
    let height = bounded_dimension(viewport, "clientHeight")?;
    let screenshot = cdp.call(
        "Page.captureScreenshot",
        json!({
            "format": "png",
            "fromSurface": true,
            "captureBeyondViewport": false
        }),
        Some(&session_id),
    )?;
    let encoded = screenshot
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| operation("Browser Host CDP screenshot has no data"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| operation("Browser Host CDP screenshot is not valid base64"))?;
    if bytes.is_empty() || bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(operation(
            "Browser Host screenshot exceeds the bounded asset contract",
        ));
    }
    let _ = cdp.call(
        "Target.detachFromTarget",
        json!({"sessionId": session_id}),
        None,
    );
    Ok(Some(PresentationAsset {
        kind,
        content_type: "image/png".to_owned(),
        width: Some(width),
        height: Some(height),
        sequence,
        bytes,
    }))
}

fn initial_frame(runtime_dir: &Path) -> Result<Option<PresentationAsset>, AdapterError> {
    let profile = runtime_dir.join(PROFILE_DIR_NAME);
    let image_path = profile.join(INITIAL_FRAME_FILE_NAME);
    let metadata_path = profile.join(INITIAL_FRAME_METADATA_FILE_NAME);
    if !image_path.exists() && !metadata_path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(image_path)?;
    if bytes.is_empty() || bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(operation(
            "Browser Host initial screenshot exceeds the bounded asset contract",
        ));
    }
    let metadata_bytes = std::fs::read(metadata_path)?;
    if metadata_bytes.len() > 1024 {
        return Err(operation(
            "Browser Host initial frame metadata is too large",
        ));
    }
    let metadata: Value = serde_json::from_slice(&metadata_bytes)?;
    if serde_jcs::to_vec(&metadata)? != metadata_bytes {
        return Err(operation(
            "Browser Host initial frame metadata is not canonical",
        ));
    }
    let width = bounded_integer_dimension(&metadata, "width")?;
    let height = bounded_integer_dimension(&metadata, "height")?;
    Ok(Some(PresentationAsset {
        kind: PresentationKind::ArchiveKeyframe,
        content_type: "image/png".to_owned(),
        width: Some(width),
        height: Some(height),
        sequence: 0,
        bytes,
    }))
}

fn bounded_integer_dimension(value: &Value, field: &str) -> Result<u32, AdapterError> {
    let dimension = value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| operation("Browser Host initial viewport dimension is missing"))?;
    u32::try_from(dimension)
        .ok()
        .filter(|dimension| (1..=8192).contains(dimension))
        .ok_or_else(|| operation("Browser Host initial viewport dimension is outside bounds"))
}

fn select_page_target<'a>(
    targets: &'a Value,
    expected_origin: &str,
) -> Result<&'a str, AdapterError> {
    let pages = targets
        .get("targetInfos")
        .and_then(Value::as_array)
        .ok_or_else(|| operation("Browser Host CDP response has no targetInfos"))?;
    let mut matches = pages.iter().filter(|target| {
        target.get("type").and_then(Value::as_str) == Some("page")
            && target
                .get("url")
                .and_then(Value::as_str)
                .and_then(|url| Url::parse(url).ok())
                .is_some_and(|url| url.origin().ascii_serialization() == expected_origin)
    });
    let target = matches
        .next()
        .ok_or_else(|| operation("Browser Host has no page for the Adapter origin"))?;
    if matches.next().is_some() {
        return Err(operation(
            "Browser Host has multiple pages for the Adapter origin",
        ));
    }
    target
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or_else(|| operation("Browser Host page has no targetId"))
}

fn bounded_dimension(value: &Value, field: &str) -> Result<u32, AdapterError> {
    let dimension = value
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(|| operation("Browser Host viewport dimension is missing"))?;
    if !dimension.is_finite() || !(1.0..=8192.0).contains(&dimension) {
        return Err(operation(
            "Browser Host viewport dimension is outside bounds",
        ));
    }
    Ok(dimension.ceil() as u32)
}

fn debugger_json(port: u16, path: &str) -> Result<Value, AdapterError> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let response = read_http_response(&mut stream)?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| operation("Browser Host CDP HTTP response has no body"))?;
    let headers = std::str::from_utf8(&response[..split])
        .map_err(|_| operation("Browser Host CDP HTTP headers are not UTF-8"))?;
    if !headers.starts_with("HTTP/1.1 200") && !headers.starts_with("HTTP/1.0 200") {
        return Err(operation("Browser Host CDP HTTP request failed"));
    }
    serde_json::from_slice(&response[split + 4..]).map_err(AdapterError::from)
}

fn read_http_response(stream: &mut TcpStream) -> Result<Vec<u8>, AdapterError> {
    let mut response = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(size) => {
                response.extend_from_slice(&buffer[..size]);
                if response.len() > 1024 * 1024 {
                    return Err(operation("Browser Host CDP HTTP response is too large"));
                }
                if let Some(header_end) =
                    response.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let headers = std::str::from_utf8(&response[..header_end])
                        .map_err(|_| operation("Browser Host CDP HTTP headers are not UTF-8"))?;
                    let content_length = headers.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    });
                    if content_length.is_some_and(|length| {
                        response.len() >= header_end.saturating_add(4).saturating_add(length)
                    }) {
                        break;
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) && !response.is_empty() =>
            {
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(response)
}

struct Cdp {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl Cdp {
    fn connect(websocket_url: &str, expected_port: u16) -> Result<Self, AdapterError> {
        let mut url = Url::parse(websocket_url)
            .map_err(|_| operation("Browser Host CDP WebSocket URL is invalid"))?;
        if url.scheme() != "ws"
            || !matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
            || url.port().is_some_and(|port| port != expected_port)
        {
            return Err(operation(
                "Browser Host CDP WebSocket must remain loopback-only",
            ));
        }
        url.set_host(Some("127.0.0.1"))
            .map_err(|_| operation("Browser Host CDP WebSocket host is invalid"))?;
        url.set_port(Some(expected_port))
            .map_err(|_| operation("Browser Host CDP WebSocket port is invalid"))?;
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, expected_port));
        let stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| operation("Browser Host CDP WebSocket request is invalid"))?;
        request.headers_mut().insert(
            "Origin",
            "http://localhost"
                .parse()
                .map_err(|_| operation("Browser Host CDP Origin is invalid"))?,
        );
        let (socket, _) = client(request, MaybeTlsStream::Plain(stream))
            .map_err(|error| operation(&format!("Browser Host CDP handshake failed: {error}")))?;
        Ok(Self { socket, next_id: 1 })
    }

    fn call(
        &mut self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value, AdapterError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut request = json!({"id": id, "method": method, "params": params});
        if let Some(session_id) = session_id {
            request["sessionId"] = Value::String(session_id.to_owned());
        }
        self.socket
            .send(Message::Text(request.to_string().into()))
            .map_err(|error| operation(&format!("Browser Host CDP send failed: {error}")))?;
        loop {
            let message = self
                .socket
                .read()
                .map_err(|error| operation(&format!("Browser Host CDP read failed: {error}")))?;
            let Message::Text(text) = message else {
                continue;
            };
            let response: Value = serde_json::from_str(&text)?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                return Err(operation(&format!(
                    "Browser Host CDP {method} failed: {error}"
                )));
            }
            return response
                .get("result")
                .cloned()
                .ok_or_else(|| operation("Browser Host CDP response has no result"));
        }
    }
}

fn operation(message: &str) -> AdapterError {
    AdapterError::Operation(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_selection_is_origin_scoped_and_ambiguous_pages_fail_closed() {
        let one = json!({"targetInfos": [
            {"type": "page", "url": "https://app.test/path", "targetId": "one"},
            {"type": "page", "url": "https://other.test/", "targetId": "other"}
        ]});
        assert_eq!(select_page_target(&one, "https://app.test").unwrap(), "one");
        let many = json!({"targetInfos": [
            {"type": "page", "url": "https://app.test/a", "targetId": "one"},
            {"type": "page", "url": "https://app.test/b", "targetId": "two"}
        ]});
        assert!(select_page_target(&many, "https://app.test").is_err());
    }
}

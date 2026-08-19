//! Host-private Chrome delivery for `ato.browser@1`.
//!
//! This module is deliberately runtime orchestration.  It reads a live
//! Browser Adapter discovery document, injects the generic bridge through a
//! Chrome isolated world, and owns only the disposable Chrome process/profile.
//! It never reads Records or participates in Replay ordering.

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ato_adapter_browser::BrowserRuntimeBootstrap;
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket, client};
use url::Url;

const DISCOVERY_WAIT: Duration = Duration::from_secs(30);
const CDP_WAIT: Duration = Duration::from_secs(15);
const NAVIGATION_WAIT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const PROFILE_DIR_NAME: &str = "browser-host-profile";
const BRIDGE_SOURCE: &str =
    include_str!("../../../extensions/adapters/browser/bridge/browser-bridge.js");

pub(crate) fn run(
    runtime_dir: &Path,
    target_url: &str,
    chrome: &Path,
    headless: bool,
) -> Result<()> {
    validate_runtime_dir(runtime_dir)?;
    let bootstrap_path = wait_for_discovery(runtime_dir, DISCOVERY_WAIT)?;
    let bootstrap = read_bootstrap(&bootstrap_path)?;
    validate_target(target_url, &bootstrap.expected_origin)?;
    if !chrome.is_absolute() || !chrome.is_file() {
        bail!("Browser Host Chrome executable must be an absolute existing file");
    }

    let profile = runtime_dir.join(PROFILE_DIR_NAME);
    if profile.exists() {
        bail!("Browser Host profile already exists; refusing to reuse a live browser profile");
    }
    fs::create_dir(&profile).context("create Browser Host private profile")?;

    let mut chrome_process = match ChromeProcess::launch(chrome, &profile, headless) {
        Ok(process) => process,
        Err(error) => {
            let _ = fs::remove_dir_all(&profile);
            return Err(error);
        }
    };
    let result = attach_bridge(&profile, target_url, &bootstrap).and_then(|_cdp| {
        wait_for_run_end(&bootstrap_path, &mut chrome_process)?;
        Ok(())
    });
    let cleanup = chrome_process
        .cleanup()
        .and_then(|_| fs::remove_dir_all(&profile).context("remove Browser Host private profile"));
    result.and(cleanup)
}

fn validate_runtime_dir(runtime_dir: &Path) -> Result<()> {
    if !runtime_dir.is_absolute() || !runtime_dir.is_dir() {
        bail!("Browser Host runtime directory must be an absolute existing directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(runtime_dir)
            .context("inspect Browser Host runtime directory")?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            bail!("Browser Host runtime directory must not be group/world accessible");
        }
    }
    Ok(())
}

fn wait_for_discovery(runtime_dir: &Path, timeout: Duration) -> Result<PathBuf> {
    let deadline = Instant::now() + timeout;
    loop {
        let matches = browser_discovery_paths(runtime_dir)?;
        if matches.len() == 1 {
            return Ok(matches.into_iter().next().expect("one path was checked"));
        }
        if matches.len() > 1 {
            bail!("Browser Host found multiple live Browser Adapter discovery documents");
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for Browser Adapter runtime discovery");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn browser_discovery_paths(runtime_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(runtime_dir).context("read Browser Host runtime directory")? {
        let entry = entry.context("read Browser Host runtime directory entry")?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("browser-") && name.ends_with(".json") {
            let metadata = entry
                .metadata()
                .context("inspect Browser Adapter discovery document")?;
            if !metadata.is_file() {
                bail!("Browser Adapter discovery document must be a regular file");
            }
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn read_bootstrap(path: &Path) -> Result<BrowserRuntimeBootstrap> {
    let bytes = fs::read(path).context("read Browser Adapter runtime discovery")?;
    let bootstrap: BrowserRuntimeBootstrap =
        serde_json::from_slice(&bytes).context("decode Browser Adapter runtime discovery")?;
    if bootstrap.protocol != ato_adapter_browser::BROWSER_PROTOCOL_ID
        || bootstrap.channel_credential.is_empty()
        || bootstrap.browser_session.is_empty()
    {
        bail!("Browser Adapter runtime discovery is invalid");
    }
    let control =
        Url::parse(&bootstrap.control_url).context("parse Browser Adapter control URL")?;
    if control.scheme() != "ws"
        || !matches!(control.host_str(), Some("127.0.0.1") | Some("localhost"))
        || control.port().is_none()
    {
        bail!("Browser Adapter control URL must be loopback WebSocket");
    }
    validate_target(&bootstrap.expected_origin, &bootstrap.expected_origin)?;
    Ok(bootstrap)
}

fn validate_target(target_url: &str, expected_origin: &str) -> Result<()> {
    let target = Url::parse(target_url).context("parse Browser Host target URL")?;
    if !matches!(target.scheme(), "http" | "https")
        || target.origin().ascii_serialization() != expected_origin
    {
        bail!("Browser Host target URL must have Browser Adapter's exact expected origin");
    }
    Ok(())
}

struct ChromeProcess {
    child: Child,
}

impl ChromeProcess {
    fn launch(chrome: &Path, profile: &Path, headless: bool) -> Result<Self> {
        let mut command = Command::new(chrome);
        command
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--remote-debugging-address=127.0.0.1")
            .arg("--remote-debugging-port=0")
            .arg("--remote-allow-origins=*")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if headless {
            command
                .arg("--headless=new")
                .arg("--disable-gpu")
                // The disposable Browser Host profile is safe to back with
                // files when a constrained runner exposes too little /dev/shm.
                // This preserves Chrome's sandbox, unlike --no-sandbox.
                .arg("--disable-dev-shm-usage");
        }
        let child = command.spawn().context("launch Browser Host Chrome")?;
        Ok(Self { child })
    }

    fn cleanup(&mut self) -> Result<()> {
        if self
            .child
            .try_wait()
            .context("inspect Browser Host Chrome")?
            .is_none()
        {
            self.child.kill().context("stop Browser Host Chrome")?;
            self.child.wait().context("wait for Browser Host Chrome")?;
        }
        Ok(())
    }
}

fn wait_for_run_end(discovery_path: &Path, chrome: &mut ChromeProcess) -> Result<()> {
    loop {
        if !discovery_path.exists() {
            return Ok(());
        }
        if let Some(status) = chrome
            .child
            .try_wait()
            .context("inspect Browser Host Chrome")?
        {
            bail!("Browser Host Chrome exited before Browser Adapter detached: {status}");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

struct Cdp {
    websocket: WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

fn attach_bridge(
    profile: &Path,
    target_url: &str,
    bootstrap: &BrowserRuntimeBootstrap,
) -> Result<Cdp> {
    let port = wait_for_debug_port(profile, CDP_WAIT)?;
    let ws_url = debugger_websocket_url(port)?;
    let websocket = connect_cdp(&ws_url)?;
    let mut cdp = Cdp {
        websocket,
        next_id: 1,
    };
    let targets = cdp.call("Target.getTargets", json!({}), None)?;
    let target_id = initial_page_target_id(&targets)?.to_owned();
    let attached = cdp.call(
        "Target.attachToTarget",
        json!({"targetId": target_id.clone(), "flatten": true}),
        None,
    )?;
    let session_id = required_string(&attached, "sessionId")?.to_owned();
    let source = format!(
        "globalThis.__ATO_BROWSER_BOOTSTRAP__ = {};\n{BRIDGE_SOURCE}",
        serde_json::to_string(bootstrap).context("encode Browser Host bootstrap")?
    );
    let world_name = format!("ato.browser.bridge.{}", bootstrap.browser_session);
    cdp.call("Page.enable", json!({}), Some(&session_id))?;
    cdp.call(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": source, "worldName": world_name}),
        Some(&session_id),
    )?;
    let navigation = cdp.call(
        "Page.navigate",
        json!({"url": target_url}),
        Some(&session_id),
    )?;
    if let Some(error) = navigation.get("errorText").and_then(Value::as_str) {
        bail!("Browser Host page navigation failed: {error}");
    }
    wait_for_target_origin(&mut cdp, &target_id, target_url)?;
    Ok(cdp)
}

fn initial_page_target_id(targets: &Value) -> Result<&str> {
    let pages: Vec<&Value> = targets
        .get("targetInfos")
        .and_then(Value::as_array)
        .context("Browser Host CDP response has no targetInfos")?
        .iter()
        .filter(|target| target.get("type").and_then(Value::as_str) == Some("page"))
        .collect();
    let [page] = pages.as_slice() else {
        bail!("Browser Host expected exactly one initial page target");
    };
    if page.get("url").and_then(Value::as_str) != Some("about:blank") {
        bail!("Browser Host initial page target must be about:blank");
    }
    required_string(page, "targetId")
}

fn connect_cdp(ws_url: &str) -> Result<WebSocket<MaybeTlsStream<TcpStream>>> {
    let url = Url::parse(ws_url).context("parse Browser Host CDP WebSocket URL")?;
    let port = url
        .port()
        .context("Browser Host CDP WebSocket URL has no port")?;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let stream = TcpStream::connect_timeout(&address, CDP_WAIT)
        .context("connect Browser Host CDP WebSocket")?;
    stream
        .set_read_timeout(Some(CDP_WAIT))
        .context("set Browser Host CDP read deadline")?;
    stream
        .set_write_timeout(Some(CDP_WAIT))
        .context("set Browser Host CDP write deadline")?;
    let mut request = ws_url
        .into_client_request()
        .context("build Browser Host CDP WebSocket request")?;
    request
        .headers_mut()
        .insert("Origin", "http://localhost".parse()?);
    let (websocket, _) = client(request, MaybeTlsStream::Plain(stream))
        .map_err(|error| anyhow::anyhow!(error))
        .context("complete Browser Host CDP WebSocket handshake")?;
    Ok(websocket)
}

fn wait_for_target_origin(cdp: &mut Cdp, target_id: &str, target_url: &str) -> Result<()> {
    let expected_origin = Url::parse(target_url)
        .context("parse Browser Host navigation target")?
        .origin()
        .ascii_serialization();
    let deadline = Instant::now() + NAVIGATION_WAIT;
    loop {
        let targets = cdp.call("Target.getTargets", json!({}), None)?;
        let url = targets
            .get("targetInfos")
            .and_then(Value::as_array)
            .and_then(|targets| {
                targets.iter().find(|target| {
                    target.get("targetId").and_then(Value::as_str) == Some(target_id)
                })
            })
            .and_then(|target| target.get("url").and_then(Value::as_str));
        if let Some(url) = url
            && validate_target(url, &expected_origin).is_ok()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("Browser Host page did not navigate to the expected origin");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

impl Cdp {
    fn call(&mut self, method: &str, params: Value, session_id: Option<&str>) -> Result<Value> {
        let request_id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut request = json!({"id": request_id, "method": method, "params": params});
        if let Some(session_id) = session_id {
            request["sessionId"] = Value::String(session_id.to_owned());
        }
        eprintln!("Browser Host CDP request: {method}");
        self.websocket
            .send(Message::Text(request.to_string().into()))
            .context("send Browser Host CDP request")?;
        loop {
            let message = self
                .websocket
                .read()
                .context("read Browser Host CDP response")?;
            let Message::Text(text) = message else {
                continue;
            };
            let response: Value =
                serde_json::from_str(&text).context("decode Browser Host CDP response")?;
            if response.get("id").and_then(Value::as_u64) != Some(request_id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                bail!("Browser Host CDP {method} failed: {error}");
            }
            eprintln!("Browser Host CDP response: {method}");
            return response
                .get("result")
                .cloned()
                .context("Browser Host CDP response has no result");
        }
    }
}

fn wait_for_debug_port(profile: &Path, timeout: Duration) -> Result<u16> {
    let path = profile.join("DevToolsActivePort");
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(value) = fs::read_to_string(&path)
            && let Some(port) = parse_debug_port(&value)
        {
            return Ok(port);
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for Browser Host Chrome CDP");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn parse_debug_port(value: &str) -> Option<u16> {
    value.lines().next()?.trim().parse().ok()
}

fn debugger_websocket_url(port: u16) -> Result<String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).context("connect Browser Host CDP HTTP")?;
    stream
        .set_read_timeout(Some(CDP_WAIT))
        .context("set Browser Host CDP HTTP read deadline")?;
    stream
        .write_all(b"GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .context("request Browser Host CDP version")?;
    let response = read_http_response(&mut stream)?;
    let (_, body) = response
        .split_once("\r\n\r\n")
        .context("Browser Host CDP version returned no body")?;
    let value: Value = serde_json::from_str(body).context("decode Browser Host CDP version")?;
    let url = value
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .context("Browser Host CDP version returned no WebSocket URL")?;
    normalize_debugger_websocket_url(url, port)
}

fn normalize_debugger_websocket_url(url: &str, port: u16) -> Result<String> {
    let mut parsed = Url::parse(url).context("parse Browser Host CDP WebSocket URL")?;
    if parsed.scheme() != "ws" || parsed.host_str() != Some("127.0.0.1") {
        bail!("Browser Host CDP WebSocket URL must be loopback");
    }
    if parsed.port().is_none() {
        parsed
            .set_port(Some(port))
            .map_err(|()| anyhow::anyhow!("set Browser Host CDP WebSocket port"))?;
    }
    Ok(parsed.into())
}

fn read_http_response(stream: &mut TcpStream) -> Result<String> {
    const MAX_RESPONSE_BYTES: usize = 64 * 1024;

    let mut response = Vec::new();
    let mut content_length = None;
    loop {
        let mut chunk = [0_u8; 4096];
        let read = stream
            .read(&mut chunk)
            .context("read Browser Host CDP version")?;
        if read == 0 {
            bail!("Browser Host CDP version closed before a complete HTTP response");
        }
        response.extend_from_slice(&chunk[..read]);
        if response.len() > MAX_RESPONSE_BYTES {
            bail!("Browser Host CDP version HTTP response is too large");
        }
        let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        if content_length.is_none() {
            let headers = std::str::from_utf8(&response[..header_end])
                .context("Browser Host CDP version headers are not UTF-8")?;
            content_length = headers.lines().find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
            });
        }
        let length = content_length.context("Browser Host CDP version has no Content-Length")?;
        if response.len() >= header_end + 4 + length {
            return String::from_utf8(response).context("Browser Host CDP version is not UTF-8");
        }
    }
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("Browser Host CDP response has no {field}"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn target_requires_the_adapter_origin_exactly() {
        validate_target("https://example.test/app", "https://example.test").expect("same origin");
        assert!(validate_target("https://other.test/", "https://example.test").is_err());
        assert!(validate_target("http://example.test/", "https://example.test").is_err());
    }

    #[test]
    fn cdp_port_file_requires_a_valid_port() {
        assert_eq!(parse_debug_port("9222\n/devtools/browser/id\n"), Some(9222));
        assert_eq!(parse_debug_port("not-a-port\n"), None);
        assert_eq!(parse_debug_port("\n"), None);
    }

    #[test]
    fn discovery_requires_exactly_one_regular_browser_document() {
        let directory = tempfile::tempdir().expect("runtime directory");
        fs::write(directory.path().join("browser-one.json"), "{}").expect("discovery");
        assert_eq!(
            browser_discovery_paths(directory.path())
                .expect("paths")
                .len(),
            1
        );
        fs::write(directory.path().join("browser-two.json"), "{}").expect("discovery");
        assert_eq!(
            browser_discovery_paths(directory.path())
                .expect("paths")
                .len(),
            2
        );
    }

    #[test]
    fn debugger_websocket_url_uses_the_known_loopback_port_when_omitted() {
        assert_eq!(
            normalize_debugger_websocket_url("ws://127.0.0.1/devtools/browser/id", 9222)
                .expect("loopback URL"),
            "ws://127.0.0.1:9222/devtools/browser/id"
        );
        assert!(
            normalize_debugger_websocket_url("ws://example.test/devtools/browser/id", 9222)
                .is_err()
        );
    }

    #[test]
    fn initial_page_target_is_the_single_blank_page() {
        let targets = json!({
            "targetInfos": [
                {"targetId": "extension", "type": "background_page", "url": "chrome-extension://id"},
                {"targetId": "page", "type": "page", "url": "about:blank"},
            ],
        });
        assert_eq!(
            initial_page_target_id(&targets).expect("initial page"),
            "page"
        );

        let non_blank = json!({
            "targetInfos": [{"targetId": "page", "type": "page", "url": "https://example.test"}],
        });
        assert!(initial_page_target_id(&non_blank).is_err());
    }
}

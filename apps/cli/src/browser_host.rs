//! Host-private Chrome delivery for `ato.browser@1`.
//!
//! This module is deliberately runtime orchestration.  It reads a live
//! Browser Adapter discovery document, injects the generic bridge through a
//! Chrome isolated world, and owns only the disposable Chrome process/profile.
//! It never reads Records or participates in Replay ordering.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ato_adapter_browser::BrowserRuntimeBootstrap;
use base64::Engine;
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket, client};
use url::Url;

#[cfg(unix)]
use crate::network_runner::TcpProxy;

const DISCOVERY_WAIT: Duration = Duration::from_secs(30);
const CDP_WAIT: Duration = Duration::from_secs(15);
const NAVIGATION_WAIT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const CDP_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PROFILE_DIR_NAME: &str = "browser-host-profile";
const CDP_PORT_FILE_NAME: &str = "browser-host-cdp-port";
const CHROME_STDERR_FILE_NAME: &str = "chrome-stderr.log";
const INITIAL_FRAME_FILE_NAME: &str = "browser-host-initial.png";
const INITIAL_FRAME_METADATA_FILE_NAME: &str = "browser-host-initial.json";
const DEVTOOLS_ACTIVE_PORT_FILE_NAME: &str = "DevToolsActivePort";
const MAX_CHROME_DIAGNOSTIC_BYTES: usize = 16 * 1024;
const MAX_PRESENTATION_ASSET_BYTES: usize = 8 * 1024 * 1024;
const LIVE_FRAME_INTERVAL: Duration = Duration::from_millis(100);
const BRIDGE_SOURCE: &str =
    include_str!("../../../extensions/adapters/browser/bridge/browser-bridge.js");

pub(crate) fn run(
    runtime_dir: &Path,
    target_url: &str,
    chrome: &Path,
    headless: bool,
    presentation_sink: Option<BrowserPresentationSink>,
    product_controller_url: Option<&str>,
) -> Result<()> {
    validate_runtime_dir(runtime_dir)?;
    let bootstrap_path = wait_for_discovery(runtime_dir, DISCOVERY_WAIT)?;
    let bootstrap = read_bootstrap(&bootstrap_path)?;
    #[cfg(unix)]
    let (bootstrap, _control_proxy) = {
        let mut bootstrap = bootstrap;
        let proxy = if let Some(file_name) = bootstrap.control_socket.as_deref() {
            let socket = validated_control_socket(runtime_dir, file_name)?;
            let proxy = TcpProxy::start_unix(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), socket)?;
            bootstrap.control_url = format!("ws://{}", proxy.local_addr());
            Some(proxy)
        } else {
            None
        };
        (bootstrap, proxy)
    };
    #[cfg(not(unix))]
    if bootstrap.control_socket.is_some() {
        bail!("Browser Adapter Unix control relay is unsupported on this host");
    }
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
    let result = attach_bridge(
        &profile,
        target_url,
        &bootstrap,
        &mut chrome_process,
        product_controller_url,
    )
    .and_then(|mut attachment| {
        wait_for_run_end(
            &bootstrap_path,
            &mut chrome_process,
            &mut attachment,
            presentation_sink.as_ref(),
        )?;
        Ok(())
    });
    let failure_diagnostic = result
        .as_ref()
        .err()
        .map(|_| chrome_process.startup_diagnostic());
    let exit_status = chrome_process.cleanup();
    let profile_cleanup =
        fs::remove_dir_all(&profile).context("remove Browser Host private profile");
    match result {
        Ok(()) => {
            exit_status?;
            profile_cleanup
        }
        Err(error) => {
            let exit_status = exit_status
                .map(|status| status.to_string())
                .unwrap_or_else(|cleanup_error| format!("cleanup failed: {cleanup_error:#}"));
            let diagnostic = failure_diagnostic.unwrap_or_else(|| "unavailable".to_owned());
            let _ = profile_cleanup;
            Err(error.context(format!(
                "Browser Host Chrome exit status after cleanup: {exit_status}; {diagnostic}"
            )))
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct BrowserPresentationSink {
    pub endpoint: String,
    pub credential: String,
}

impl BrowserPresentationSink {
    pub(crate) fn from_environment(endpoint: String) -> Result<Self> {
        let credential = std::env::var("ATO_BROWSER_PRESENTATION_SINK_CREDENTIAL")
            .context("Browser presentation sink credential is missing")?;
        let sink = Self {
            endpoint,
            credential,
        };
        validate_presentation_sink(&sink)?;
        Ok(sink)
    }
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

#[cfg(unix)]
fn validated_control_socket(runtime_dir: &Path, file_name: &str) -> Result<PathBuf> {
    if file_name.is_empty()
        || file_name.contains('/')
        || file_name.contains('\\')
        || matches!(file_name, "." | "..")
    {
        bail!("Browser Adapter control relay has an unsafe socket name");
    }
    let path = runtime_dir.join(file_name);
    let metadata =
        fs::symlink_metadata(&path).context("inspect Browser Adapter control relay socket")?;
    if !std::os::unix::fs::FileTypeExt::is_socket(&metadata.file_type()) {
        bail!("Browser Adapter control relay is not a Unix socket");
    }
    Ok(path)
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
    stderr_path: PathBuf,
}

impl ChromeProcess {
    fn launch(chrome: &Path, profile: &Path, headless: bool) -> Result<Self> {
        let stderr_path = profile.join(CHROME_STDERR_FILE_NAME);
        let stderr = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&stderr_path)
            .context("create Browser Host Chrome stderr log")?;
        #[cfg(unix)]
        fs::set_permissions(
            &stderr_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .context("restrict Browser Host Chrome stderr log")?;
        let mut command = Command::new(chrome);
        command
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--remote-debugging-address=127.0.0.1")
            // Chrome owns the ephemeral port reservation and publishes the
            // result through DevToolsActivePort. This avoids a bind/drop race.
            .arg("--remote-debugging-port=0")
            .arg("--remote-allow-origins=*")
            .arg("--window-size=800,600")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
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
        Ok(Self { child, stderr_path })
    }

    fn cleanup(&mut self) -> Result<ExitStatus> {
        if let Some(status) = self
            .child
            .try_wait()
            .context("inspect Browser Host Chrome")?
        {
            return Ok(status);
        }
        self.child.kill().context("stop Browser Host Chrome")?;
        self.child.wait().context("wait for Browser Host Chrome")
    }

    fn startup_diagnostic(&mut self) -> String {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => status.to_string(),
            Ok(None) => "still running at readiness deadline".to_owned(),
            Err(error) => format!("status unavailable: {error}"),
        };
        let stderr = fs::read(&self.stderr_path)
            .map(|bytes| {
                let start = bytes.len().saturating_sub(MAX_CHROME_DIAGNOSTIC_BYTES);
                String::from_utf8_lossy(&bytes[start..]).into_owned()
            })
            .unwrap_or_else(|error| format!("<Chrome stderr unavailable: {error}>"));
        format!(
            "Browser Host Chrome status before cleanup: {status}; Chrome stderr: {}",
            if stderr.is_empty() {
                "<empty>"
            } else {
                &stderr
            }
        )
    }
}

fn wait_for_run_end(
    discovery_path: &Path,
    chrome: &mut ChromeProcess,
    attachment: &mut BrowserAttachment,
    presentation_sink: Option<&BrowserPresentationSink>,
) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(CDP_WAIT)
        .build()
        .context("build Browser presentation sink client")?;
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
        if let Some(sink) = presentation_sink {
            publish_live_frame(attachment, sink, &client)?;
            thread::sleep(LIVE_FRAME_INTERVAL);
        } else {
            thread::sleep(POLL_INTERVAL);
        }
    }
}

struct Cdp {
    websocket: WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

struct BrowserAttachment {
    cdp: Cdp,
    session_id: String,
}

fn attach_bridge(
    profile: &Path,
    target_url: &str,
    bootstrap: &BrowserRuntimeBootstrap,
    chrome: &mut ChromeProcess,
    product_controller_url: Option<&str>,
) -> Result<BrowserAttachment> {
    let (debug_port, browser_ws_url) = wait_for_debugger_websocket_url(profile, chrome, CDP_WAIT)?;
    let websocket = connect_cdp(&browser_ws_url)?;
    let mut cdp = Cdp {
        websocket,
        next_id: 1,
    };
    let targets = cdp.call("Target.getTargets", json!({}), None)?;
    let startup_target_id = initial_page_target_id(&targets)?.to_owned();
    let context = cdp.call("Target.createBrowserContext", json!({}), None)?;
    let browser_context_id = required_string(&context, "browserContextId")?.to_owned();
    let target = cdp.call(
        "Target.createTarget",
        json!({"url": "about:blank", "browserContextId": browser_context_id}),
        None,
    )?;
    let target_id = required_string(&target, "targetId")?.to_owned();
    cdp.call(
        "Target.closeTarget",
        json!({"targetId": startup_target_id}),
        None,
    )?;
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
    wait_for_document_ready(&mut cdp, &session_id)?;
    if let Some(controller_url) = product_controller_url {
        validate_product_controller(controller_url)?;
        cdp.call(
            "Target.createTarget",
            json!({"url": controller_url, "browserContextId": browser_context_id}),
            None,
        )?;
    }
    capture_initial_frame(&mut cdp, &session_id, profile)?;
    write_cdp_port(profile, debug_port)?;
    Ok(BrowserAttachment { cdp, session_id })
}

fn validate_product_controller(value: &str) -> Result<()> {
    let url = Url::parse(value).context("parse Product controller URL")?;
    if url.scheme() != "http"
        || !matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"))
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        bail!("Product controller URL must be a credential-free loopback HTTP URL");
    }
    Ok(())
}

fn publish_live_frame(
    attachment: &mut BrowserAttachment,
    sink: &BrowserPresentationSink,
    client: &reqwest::blocking::Client,
) -> Result<()> {
    validate_presentation_sink(sink)?;
    let screenshot = attachment.cdp.call(
        "Page.captureScreenshot",
        json!({
            "format": "jpeg",
            "quality": 65,
            "fromSurface": true,
            "captureBeyondViewport": false
        }),
        Some(&attachment.session_id),
    )?;
    let encoded = required_string(&screenshot, "data")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("decode Browser Host live frame")?;
    if bytes.is_empty() || bytes.len() > MAX_PRESENTATION_ASSET_BYTES {
        bail!("Browser Host live frame exceeds the bounded asset contract");
    }
    client
        .post(&sink.endpoint)
        .header("content-type", "image/jpeg")
        .header("x-ato-browser-presentation", &sink.credential)
        .body(bytes)
        .send()
        .context("publish Browser Host live frame")?
        .error_for_status()
        .context("Browser presentation sink rejected live frame")?;
    Ok(())
}

fn validate_presentation_sink(sink: &BrowserPresentationSink) -> Result<()> {
    let endpoint = Url::parse(&sink.endpoint).context("parse Browser presentation sink URL")?;
    if endpoint.scheme() != "http"
        || !matches!(endpoint.host_str(), Some("127.0.0.1") | Some("localhost"))
        || endpoint.port().is_none()
        || endpoint.path() != "/frame"
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || sink.credential.len() < 32
    {
        bail!("Browser presentation sink must be a credentialed loopback /frame endpoint");
    }
    Ok(())
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

fn wait_for_document_ready(cdp: &mut Cdp, session_id: &str) -> Result<()> {
    let deadline = Instant::now() + NAVIGATION_WAIT;
    loop {
        let evaluated = cdp.call(
            "Runtime.evaluate",
            json!({"expression": "document.readyState", "returnByValue": true}),
            Some(session_id),
        )?;
        let state = evaluated
            .get("result")
            .and_then(|result| result.get("value"))
            .and_then(Value::as_str);
        if state == Some("complete") {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("Browser Host page did not reach document.readyState=complete");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn capture_initial_frame(cdp: &mut Cdp, session_id: &str, profile: &Path) -> Result<()> {
    let metrics = cdp.call("Page.getLayoutMetrics", json!({}), Some(session_id))?;
    let viewport = metrics
        .get("cssVisualViewport")
        .or_else(|| metrics.get("visualViewport"))
        .context("Browser Host CDP returned no initial visual viewport")?;
    let width = presentation_dimension(viewport, "clientWidth")?;
    let height = presentation_dimension(viewport, "clientHeight")?;
    let screenshot = cdp.call(
        "Page.captureScreenshot",
        json!({
            "format": "png",
            "fromSurface": true,
            "captureBeyondViewport": false
        }),
        Some(session_id),
    )?;
    let encoded = required_string(&screenshot, "data")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("decode Browser Host initial screenshot")?;
    if bytes.is_empty() || bytes.len() > MAX_PRESENTATION_ASSET_BYTES {
        bail!("Browser Host initial screenshot exceeds the bounded asset contract");
    }
    write_private_file(&profile.join(INITIAL_FRAME_FILE_NAME), &bytes)?;
    write_private_file(
        &profile.join(INITIAL_FRAME_METADATA_FILE_NAME),
        &serde_jcs::to_vec(&json!({"height": height, "width": width}))?,
    )
}

fn presentation_dimension(viewport: &Value, field: &str) -> Result<u32> {
    let value = viewport
        .get(field)
        .and_then(Value::as_f64)
        .context("Browser Host viewport dimension is missing")?;
    if !value.is_finite() || !(1.0..=8192.0).contains(&value) {
        bail!("Browser Host viewport dimension is outside bounds");
    }
    Ok(value.ceil() as u32)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .with_context(|| format!("create private Browser Host file {}", path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .with_context(|| format!("restrict private Browser Host file {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write private Browser Host file {}", path.display()))
}

impl Cdp {
    fn send(&mut self, method: &str, params: Value, session_id: Option<&str>) -> Result<u64> {
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
        Ok(request_id)
    }

    fn call(&mut self, method: &str, params: Value, session_id: Option<&str>) -> Result<Value> {
        let request_id = self.send(method, params, session_id)?;
        let deadline = Instant::now() + CDP_WAIT;
        loop {
            let message = match self.websocket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if Instant::now() >= deadline {
                        return Err(error)
                            .context("read Browser Host CDP response before deadline");
                    }
                    continue;
                }
                Err(error) => {
                    return Err(error).context("read Browser Host CDP response");
                }
            };
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

fn wait_for_debugger_websocket_url(
    profile: &Path,
    chrome: &mut ChromeProcess,
    timeout: Duration,
) -> Result<(u16, String)> {
    let deadline = Instant::now() + timeout;
    loop {
        let probe = read_devtools_active_port(profile).and_then(|(port, active_url)| {
            let version_url = debugger_websocket_url(port)?;
            if version_url != active_url {
                bail!("Browser Host CDP discovery sources disagree");
            }
            Ok((port, version_url))
        });
        let last_error = match probe {
            Ok(ready) => return Ok(ready),
            Err(error) => format!("{error:#}"),
        };
        if let Some(status) = chrome
            .child
            .try_wait()
            .context("inspect Browser Host Chrome during CDP readiness")?
        {
            bail!("Browser Host Chrome exited before CDP readiness ({status}): {last_error}");
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for Browser Host Chrome CDP: {last_error}");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn read_devtools_active_port(profile: &Path) -> Result<(u16, String)> {
    let path = profile.join(DEVTOOLS_ACTIVE_PORT_FILE_NAME);
    let bytes = fs::read(&path).context("read Browser Host DevToolsActivePort")?;
    if bytes.len() > 4096 {
        bail!("Browser Host DevToolsActivePort is too large");
    }
    let text =
        std::str::from_utf8(&bytes).context("Browser Host DevToolsActivePort is not UTF-8")?;
    let mut lines = text.lines();
    let port = lines
        .next()
        .context("Browser Host DevToolsActivePort has no port")?
        .parse::<u16>()
        .context("Browser Host DevToolsActivePort has an invalid port")?;
    if port == 0 {
        bail!("Browser Host DevToolsActivePort published port zero");
    }
    let path = lines
        .next()
        .filter(|value| {
            value.starts_with("/devtools/browser/") && !value.chars().any(char::is_control)
        })
        .context("Browser Host DevToolsActivePort has an invalid WebSocket path")?;
    Ok((port, format!("ws://127.0.0.1:{port}{path}")))
}

fn write_cdp_port(profile: &Path, port: u16) -> Result<()> {
    let path = profile.join(CDP_PORT_FILE_NAME);
    fs::write(&path, port.to_string()).context("write Browser Host CDP port")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .context("restrict Browser Host CDP port")?;
    }
    Ok(())
}

fn debugger_websocket_url(port: u16) -> Result<String> {
    let value = debugger_json(port, "/json/version")?;
    let url = value
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .context("Browser Host CDP version returned no WebSocket URL")?;
    normalize_debugger_websocket_url(url, port)
}

fn debugger_json(port: u16, path: &str) -> Result<Value> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, CDP_PROBE_TIMEOUT)
        .context("connect Browser Host CDP HTTP")?;
    stream
        .set_nonblocking(false)
        .context("configure Browser Host CDP HTTP blocking mode")?;
    stream
        .set_read_timeout(Some(CDP_PROBE_TIMEOUT))
        .context("set Browser Host CDP HTTP read deadline")?;
    stream
        .set_write_timeout(Some(CDP_PROBE_TIMEOUT))
        .context("set Browser Host CDP HTTP write deadline")?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .context("request Browser Host CDP version")?;
    let response = read_http_response(&mut stream)?;
    let (_, body) = response
        .split_once("\r\n\r\n")
        .context("Browser Host CDP version returned no body")?;
    serde_json::from_str(body).context("decode Browser Host CDP JSON")
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
    fn devtools_active_port_requires_a_nonzero_port_and_browser_path() {
        let directory = tempfile::tempdir().expect("profile");
        fs::write(
            directory.path().join(DEVTOOLS_ACTIVE_PORT_FILE_NAME),
            "9222\n/devtools/browser/opaque-id\n",
        )
        .expect("active port");
        assert_eq!(
            read_devtools_active_port(directory.path()).expect("active port"),
            (
                9222,
                "ws://127.0.0.1:9222/devtools/browser/opaque-id".to_owned()
            )
        );
        fs::write(
            directory.path().join(DEVTOOLS_ACTIVE_PORT_FILE_NAME),
            "0\n/devtools/browser/opaque-id\n",
        )
        .expect("invalid active port");
        assert!(read_devtools_active_port(directory.path()).is_err());
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
                {"targetId": "page-1", "type": "page", "url": "about:blank"},
                {"targetId": "worker-1", "type": "service_worker", "url": "https://example.test/worker.js"}
            ]
        });
        assert_eq!(
            initial_page_target_id(&targets).expect("single blank page"),
            "page-1"
        );

        let multiple_pages = json!({
            "targetInfos": [
                {"targetId": "page-1", "type": "page", "url": "about:blank"},
                {"targetId": "page-2", "type": "page", "url": "about:blank"}
            ]
        });
        assert!(initial_page_target_id(&multiple_pages).is_err());
    }

    #[test]
    fn presentation_sink_is_loopback_credentialed_and_path_scoped() {
        let valid = BrowserPresentationSink {
            endpoint: "http://127.0.0.1:49152/frame".to_owned(),
            credential: "s".repeat(32),
        };
        validate_presentation_sink(&valid).expect("loopback sink");
        for endpoint in [
            "https://127.0.0.1:49152/frame",
            "http://example.test:49152/frame",
            "http://127.0.0.1:49152/other",
            "http://127.0.0.1:49152/frame?token=secret",
        ] {
            assert!(
                validate_presentation_sink(&BrowserPresentationSink {
                    endpoint: endpoint.to_owned(),
                    credential: "s".repeat(32),
                })
                .is_err()
            );
        }
    }

    #[test]
    fn product_controller_is_a_separate_credential_free_loopback_target() {
        validate_product_controller("http://127.0.0.1:49152/bootstrap/opaque")
            .expect("loopback controller");
        for value in [
            "https://127.0.0.1:49152/bootstrap/opaque",
            "http://example.test:49152/bootstrap/opaque",
            "http://secret@127.0.0.1:49152/bootstrap/opaque",
            "http://127.0.0.1:49152/bootstrap/opaque#secret",
        ] {
            assert!(validate_product_controller(value).is_err());
        }
    }
}

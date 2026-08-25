//! Generic Runner-owned Chrome realization for `ato.browser@1`.
//!
//! This crate owns only an ephemeral Chrome process, private profile, loopback
//! CDP connection, exact-origin navigation, and Browser bridge injection. It
//! contains neither Activity concepts nor Record/Evolution semantics.

#![forbid(unsafe_code)]

use std::fs::{self, OpenOptions};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use ato_adapter_browser::BrowserRuntimeBootstrap;
use base64::Engine as _;
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket, client};
use url::Url;

const DISCOVERY_WAIT: Duration = Duration::from_secs(30);
const CDP_WAIT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const PROFILE_NAME: &str = "browser-profile";
const DEVTOOLS_ACTIVE_PORT: &str = "DevToolsActivePort";
const BRIDGE_SOURCE: &str = include_str!("../../adapters/browser/bridge/browser-bridge.js");
const MAX_PRESENTATION_FRAME_BYTES: usize = 8 * 1024 * 1024;

const MAX_LOCAL_STORAGE_ITEMS: usize = 4096;
const MAX_LOCAL_STORAGE_KEY_BYTES: usize = 16 * 1024;
const MAX_LOCAL_STORAGE_VALUE_BYTES: usize = 1024 * 1024;
const MAX_LOCAL_STORAGE_TOTAL_BYTES: usize = 8 * 1024 * 1024;

/// Origin-scoped physical Browser state. It is never a Computation residual.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalStorageEntry {
    pub key: String,
    pub value: String,
}

/// Physical configuration only. None of these values participate in a
/// Computation identity.
#[derive(Debug, Clone)]
pub struct BrowserHostConfig {
    pub runtime_dir: PathBuf,
    pub bootstrap_path: PathBuf,
    pub target_url: String,
    pub chrome: PathBuf,
    pub headless: bool,
}

/// A ready, isolated Chrome realization. Dropping it stops Chrome and removes
/// its profile; callers may also use `stop` to surface cleanup errors.
pub struct BrowserHost {
    child: Child,
    profile: PathBuf,
    cdp: Cdp,
    session_id: String,
    browser_context_id: String,
    origin: String,
    stopped: bool,
}

impl BrowserHost {
    pub fn start(config: BrowserHostConfig) -> Result<Self> {
        validate_runtime_dir(&config.runtime_dir)?;
        ensure!(
            config.bootstrap_path.is_absolute(),
            "Browser bootstrap path must be absolute"
        );
        let bootstrap = wait_for_bootstrap(&config.bootstrap_path)?;
        validate_target(&config.target_url, &bootstrap.expected_origin)?;
        ensure!(
            config.chrome.is_absolute() && config.chrome.is_file(),
            "Chrome executable must be an absolute regular file"
        );

        let profile = config.runtime_dir.join(PROFILE_NAME);
        ensure!(!profile.exists(), "refusing to reuse Browser profile");
        fs::create_dir(&profile).context("create private Browser profile")?;
        restrict_dir(&profile)?;
        let mut child = match launch_chrome(&config.chrome, &profile, config.headless) {
            Ok(child) => child,
            Err(error) => {
                let _ = remove_private_profile(&profile);
                return Err(error);
            }
        };
        let result = attach_bridge(&profile, &config.target_url, &bootstrap, &mut child);
        match result {
            Ok((cdp, session_id, browser_context_id)) => {
                wait_for_bridge_ready(&config.bootstrap_path)?;
                Ok(Self {
                    child,
                    profile,
                    cdp,
                    session_id,
                    browser_context_id,
                    origin: Url::parse(&config.target_url)?
                        .origin()
                        .ascii_serialization(),
                    stopped: false,
                })
            }
            Err(error) => {
                let _ = stop_child(&mut child);
                let _ = remove_private_profile(&profile);
                Err(error)
            }
        }
    }

    pub fn is_running(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_none())
    }

    /// Evaluates a bounded expression in the already-attached, private target.
    /// This is a Runner-internal CDP capability; it cannot navigate, expose
    /// DevTools, or address any Browser other than this Host's target.
    pub fn evaluate(&mut self, expression: &str) -> Result<Value> {
        ensure!(
            expression.len() <= 8 * 1024,
            "Browser evaluation expression exceeds bound"
        );
        self.cdp.call(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true}),
            Some(&self.session_id),
        )
    }

    /// Opens one credential-free loopback controller in the same private
    /// Browser context. Product runtimes may use this for media/control
    /// orchestration without giving the application target those credentials.
    pub fn open_auxiliary_target(&mut self, target_url: &str) -> Result<()> {
        validate_auxiliary_target(target_url)?;
        self.cdp.call(
            "Target.createTarget",
            json!({"url": target_url, "browserContextId": self.browser_context_id}),
            None,
        )?;
        Ok(())
    }

    /// Captures the attached application target only. The returned JPEG is a
    /// bounded physical presentation frame, never Record or Computation data.
    pub fn capture_jpeg(&mut self) -> Result<Vec<u8>> {
        let screenshot = self.cdp.call(
            "Page.captureScreenshot",
            json!({
                "format": "jpeg",
                "quality": 65,
                "fromSurface": true,
                "captureBeyondViewport": false
            }),
            Some(&self.session_id),
        )?;
        let encoded = required_string(&screenshot, "data")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .context("decode Browser presentation frame")?;
        ensure!(
            !bytes.is_empty() && bytes.len() <= MAX_PRESENTATION_FRAME_BYTES,
            "Browser presentation frame exceeds bound"
        );
        Ok(bytes)
    }

    /// Captures only the active target's origin-scoped localStorage through
    /// CDP's DOMStorage domain. Cookies, sessionStorage, IndexedDB, console,
    /// DOM projections, and arbitrary page evaluation are deliberately absent.
    pub fn capture_local_storage(&mut self) -> Result<Vec<LocalStorageEntry>> {
        let value = self.cdp.call(
            "DOMStorage.getDOMStorageItems",
            json!({"storageId":{"securityOrigin":self.origin,"isLocalStorage":true}}),
            None,
        )?;
        let entries = value
            .get("entries")
            .and_then(Value::as_array)
            .context("CDP localStorage response lacks entries")?;
        ensure!(
            entries.len() <= MAX_LOCAL_STORAGE_ITEMS,
            "Browser localStorage item count exceeds bound"
        );
        let mut total = 0usize;
        let mut output = Vec::with_capacity(entries.len());
        for entry in entries {
            let pair = entry
                .as_array()
                .context("CDP localStorage entry is invalid")?;
            let [key, value] = pair.as_slice() else {
                bail!("CDP localStorage entry is invalid");
            };
            let key = key.as_str().context("CDP localStorage key is not text")?;
            let value = value
                .as_str()
                .context("CDP localStorage value is not text")?;
            ensure!(
                key.len() <= MAX_LOCAL_STORAGE_KEY_BYTES,
                "Browser localStorage key exceeds bound"
            );
            ensure!(
                value.len() <= MAX_LOCAL_STORAGE_VALUE_BYTES,
                "Browser localStorage value exceeds bound"
            );
            total = total
                .checked_add(key.len() + value.len())
                .context("Browser localStorage size overflow")?;
            ensure!(
                total <= MAX_LOCAL_STORAGE_TOTAL_BYTES,
                "Browser localStorage total exceeds bound"
            );
            output.push(LocalStorageEntry {
                key: key.to_owned(),
                value: value.to_owned(),
            });
        }
        output.sort_by(|left, right| left.key.cmp(&right.key));
        ensure!(
            output.windows(2).all(|pair| pair[0].key != pair[1].key),
            "CDP localStorage contains duplicate keys"
        );
        Ok(output)
    }

    /// Restores bounded localStorage to the already-established exact origin,
    /// then reloads the document so the application performs its normal
    /// bootstrap. The caller owns the fresh-profile lifecycle.
    pub fn restore_local_storage(&mut self, entries: &[LocalStorageEntry]) -> Result<()> {
        validate_local_storage(entries)?;
        self.cdp.call(
            "DOMStorage.clear",
            json!({"storageId":{"securityOrigin":self.origin,"isLocalStorage":true}}),
            None,
        )?;
        for entry in entries {
            self.cdp.call("DOMStorage.setDOMStorageItem", json!({"storageId":{"securityOrigin":self.origin,"isLocalStorage":true},"key":entry.key,"value":entry.value}), None)?;
        }
        self.cdp.call(
            "Page.reload",
            json!({"ignoreCache":true}),
            Some(&self.session_id),
        )?;
        wait_for_document_ready(&mut self.cdp, &self.session_id)
    }

    /// Stops the process before removing its profile. The explicit ordering is
    /// part of the hosted Run cleanup contract.
    pub fn stop(mut self) -> Result<()> {
        self.stop_inner()
    }

    fn stop_inner(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        let _ = self
            .cdp
            .call("Target.closeTarget", json!({}), Some(&self.session_id));
        stop_child(&mut self.child)?;
        remove_private_profile(&self.profile)?;
        self.stopped = true;
        Ok(())
    }
}

fn validate_local_storage(entries: &[LocalStorageEntry]) -> Result<()> {
    ensure!(
        entries.len() <= MAX_LOCAL_STORAGE_ITEMS,
        "Browser localStorage item count exceeds bound"
    );
    let mut previous = None;
    let mut total = 0usize;
    for entry in entries {
        ensure!(
            entry.key.len() <= MAX_LOCAL_STORAGE_KEY_BYTES,
            "Browser localStorage key exceeds bound"
        );
        ensure!(
            entry.value.len() <= MAX_LOCAL_STORAGE_VALUE_BYTES,
            "Browser localStorage value exceeds bound"
        );
        if let Some(previous) = previous {
            ensure!(
                previous < entry.key.as_str(),
                "Browser localStorage entries must be sorted and unique"
            );
        }
        previous = Some(entry.key.as_str());
        total = total
            .checked_add(entry.key.len() + entry.value.len())
            .context("Browser localStorage size overflow")?;
        ensure!(
            total <= MAX_LOCAL_STORAGE_TOTAL_BYTES,
            "Browser localStorage total exceeds bound"
        );
    }
    Ok(())
}

impl Drop for BrowserHost {
    fn drop(&mut self) {
        let _ = self.stop_inner();
    }
}

fn validate_runtime_dir(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute() && path.is_dir(),
        "Browser runtime directory must be an absolute existing directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            fs::metadata(path)?.permissions().mode() & 0o077 == 0,
            "Browser runtime directory must be owner-only"
        );
    }
    Ok(())
}

fn restrict_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn wait_for_bootstrap(path: &Path) -> Result<BrowserRuntimeBootstrap> {
    let deadline = Instant::now() + DISCOVERY_WAIT;
    loop {
        if path.is_file() {
            let bytes = fs::read(path).context("read Browser Adapter discovery")?;
            let bootstrap: BrowserRuntimeBootstrap =
                serde_json::from_slice(&bytes).context("decode Browser Adapter discovery")?;
            validate_bootstrap(&bootstrap)?;
            return Ok(bootstrap);
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for Browser Adapter discovery");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn validate_bootstrap(bootstrap: &BrowserRuntimeBootstrap) -> Result<()> {
    ensure!(
        bootstrap.protocol == ato_adapter_browser::BROWSER_PROTOCOL_ID,
        "Browser Adapter protocol mismatch"
    );
    ensure!(
        !bootstrap.channel_credential.is_empty() && !bootstrap.browser_session.is_empty(),
        "Browser Adapter credentials are empty"
    );
    let control = Url::parse(&bootstrap.control_url)?;
    ensure!(
        control.scheme() == "ws"
            && matches!(control.host_str(), Some("127.0.0.1") | Some("localhost"))
            && control.port().is_some(),
        "Browser Adapter control must be loopback WebSocket"
    );
    validate_target(&bootstrap.expected_origin, &bootstrap.expected_origin)
}

fn wait_for_bridge_ready(bootstrap_path: &Path) -> Result<()> {
    let ready_path = bootstrap_path.with_extension("ready");
    let deadline = Instant::now() + DISCOVERY_WAIT;
    loop {
        if fs::read(&ready_path).ok().as_deref() == Some(b"ready") {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for Browser bridge handshake");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn validate_target(target: &str, expected_origin: &str) -> Result<()> {
    let target = Url::parse(target)?;
    ensure!(
        matches!(target.scheme(), "http" | "https")
            && target.origin().ascii_serialization() == expected_origin,
        "Browser target must have exact expected origin"
    );
    Ok(())
}

fn validate_auxiliary_target(value: &str) -> Result<()> {
    let url = Url::parse(value).context("parse Browser auxiliary target URL")?;
    ensure!(
        url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"))
            && url.port().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "Browser auxiliary target must be credential-free loopback HTTP"
    );
    Ok(())
}

fn launch_chrome(chrome: &Path, profile: &Path, headless: bool) -> Result<Child> {
    let stderr_path = profile.join("chrome-stderr.log");
    let stderr = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&stderr_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stderr_path, fs::Permissions::from_mode(0o600))?;
    }
    let mut command = Command::new(chrome);
    command
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--remote-debugging-address=127.0.0.1")
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
            .arg("--disable-dev-shm-usage");
    }
    command.spawn().context("launch private Chrome")
}

fn stop_child(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_none() {
        child.kill().context("stop Browser Chrome")?;
        child.wait().context("wait for Browser Chrome")?;
    }
    Ok(())
}

fn remove_private_profile(profile: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match fs::remove_dir_all(profile) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => return Err(error).context("remove private Browser profile"),
        }
    }
}

struct Cdp {
    websocket: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

fn attach_bridge(
    profile: &Path,
    target_url: &str,
    bootstrap: &BrowserRuntimeBootstrap,
    child: &mut Child,
) -> Result<(Cdp, String, String)> {
    let websocket = connect_cdp(&wait_for_debugger_url(profile, child)?)?;
    let mut cdp = Cdp {
        websocket,
        next_id: 1,
    };
    let context = cdp.call("Target.createBrowserContext", json!({}), None)?;
    let context_id = required_string(&context, "browserContextId")?.to_owned();
    let target = cdp.call(
        "Target.createTarget",
        json!({"url":"about:blank", "browserContextId": context_id}),
        None,
    )?;
    let target_id = required_string(&target, "targetId")?.to_owned();
    let attached = cdp.call(
        "Target.attachToTarget",
        json!({"targetId":target_id, "flatten":true}),
        None,
    )?;
    let session_id = required_string(&attached, "sessionId")?.to_owned();
    cdp.call("Page.enable", json!({}), Some(&session_id))?;
    let source = format!(
        "globalThis.__ATO_BROWSER_BOOTSTRAP__ = {};\n{BRIDGE_SOURCE}",
        serde_json::to_string(bootstrap)?
    );
    cdp.call("Page.addScriptToEvaluateOnNewDocument", json!({"source":source, "worldName":format!("ato.browser.bridge.{}", bootstrap.browser_session)}), Some(&session_id))?;
    let navigation = cdp.call(
        "Page.navigate",
        json!({"url":target_url}),
        Some(&session_id),
    )?;
    if let Some(error) = navigation.get("errorText").and_then(Value::as_str) {
        bail!("Browser navigation failed: {error}");
    }
    wait_for_document_ready(&mut cdp, &session_id)?;
    Ok((cdp, session_id, context_id))
}

impl Cdp {
    fn call(&mut self, method: &str, params: Value, session_id: Option<&str>) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session_id {
            request["sessionId"] = Value::String(session.to_owned());
        }
        self.websocket
            .send(Message::Text(request.to_string().into()))?;
        let deadline = Instant::now() + CDP_WAIT;
        loop {
            let message = self.websocket.read();
            match message {
                Ok(Message::Text(text)) => {
                    let response: Value = serde_json::from_str(&text)?;
                    if response.get("id").and_then(Value::as_u64) != Some(id) {
                        continue;
                    }
                    if let Some(error) = response.get("error") {
                        bail!("CDP {method} failed: {error}");
                    }
                    return response
                        .get("result")
                        .cloned()
                        .context("CDP response missing result");
                }
                Ok(_) => continue,
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) && Instant::now() < deadline =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

fn wait_for_document_ready(cdp: &mut Cdp, session_id: &str) -> Result<()> {
    let deadline = Instant::now() + CDP_WAIT;
    loop {
        let value = cdp.call(
            "Runtime.evaluate",
            json!({"expression":"document.readyState", "returnByValue":true}),
            Some(session_id),
        )?;
        if value.pointer("/result/value").and_then(Value::as_str) == Some("complete") {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("Browser document did not become ready");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_debugger_url(profile: &Path, child: &mut Child) -> Result<String> {
    let deadline = Instant::now() + CDP_WAIT;
    loop {
        if let Ok((port, path)) = read_devtools_port(profile) {
            return Ok(format!("ws://127.0.0.1:{port}{path}"));
        }
        if let Some(status) = child.try_wait()? {
            bail!("Chrome exited before CDP readiness: {status}");
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for loopback CDP");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn read_devtools_port(profile: &Path) -> Result<(u16, String)> {
    let bytes = fs::read(profile.join(DEVTOOLS_ACTIVE_PORT))?;
    ensure!(bytes.len() <= 4096, "DevToolsActivePort is too large");
    let text = std::str::from_utf8(&bytes)?;
    let mut lines = text.lines();
    let port = lines
        .next()
        .context("DevTools port absent")?
        .parse::<u16>()?;
    let path = lines.next().context("DevTools path absent")?;
    ensure!(
        port != 0 && path.starts_with("/devtools/browser/") && !path.chars().any(char::is_control),
        "DevTools endpoint is invalid"
    );
    Ok((port, path.to_owned()))
}

fn connect_cdp(url: &str) -> Result<WebSocket<MaybeTlsStream<TcpStream>>> {
    let url_parsed = Url::parse(url)?;
    let port = url_parsed.port().context("CDP URL lacks port")?;
    let stream =
        TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), CDP_WAIT)?;
    stream.set_read_timeout(Some(CDP_WAIT))?;
    stream.set_write_timeout(Some(CDP_WAIT))?;
    let mut request = url.into_client_request()?;
    request
        .headers_mut()
        .insert("Origin", "http://localhost".parse()?);
    Ok(client(request, MaybeTlsStream::Plain(stream))?.0)
}

fn required_string<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("CDP response missing {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_origin_is_required() {
        assert!(validate_target("https://example.test/path", "https://example.test").is_ok());
        assert!(validate_target("https://other.test", "https://example.test").is_err());
    }

    #[test]
    fn bridge_erases_the_injected_bootstrap_before_opening_a_socket() {
        let erase = BRIDGE_SOURCE
            .find("Reflect.deleteProperty")
            .expect("bridge must erase its injected bootstrap");
        let socket = BRIDGE_SOURCE
            .find("new WebSocket")
            .expect("bridge must open its private control socket");

        assert!(erase < socket, "bootstrap survived until socket creation");
    }

    #[test]
    fn auxiliary_target_is_loopback_and_credential_free() {
        assert!(validate_auxiliary_target("http://127.0.0.1:49152/bootstrap/opaque").is_ok());
        for value in [
            "https://127.0.0.1:49152/bootstrap/opaque",
            "http://example.test:49152/bootstrap/opaque",
            "http://secret@127.0.0.1:49152/bootstrap/opaque",
            "http://127.0.0.1:49152/bootstrap/opaque#secret",
        ] {
            assert!(validate_auxiliary_target(value).is_err());
        }
    }
}

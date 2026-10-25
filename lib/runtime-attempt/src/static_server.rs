//! A loopback static server over an already-verified route table: request
//! path to file and media type. Used by `ato run` for a static application
//! and by a Formation attempt to observe a static candidate over real HTTP.
//! The caller vouches for every file; this only answers requests.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use base64::Engine as _;
use serde::Deserialize;
/// Loopback-only static realization used by `ato run`.
pub struct StaticApplicationServer {
    address: SocketAddr,
    running: Arc<AtomicBool>,
    local_storage: Option<Arc<Mutex<BTreeMap<String, String>>>>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticApplicationState {
    pub persistence_token: String,
    pub local_storage: BTreeMap<String, String>,
    pub assets: Vec<StaticApplicationAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticApplicationAsset {
    pub asset_id: String,
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

struct StaticApplicationRuntimeState {
    persistence_token: String,
    local_storage: Arc<Mutex<BTreeMap<String, String>>>,
    assets: BTreeMap<String, StaticApplicationAsset>,
}

impl StaticApplicationServer {
    /// Serve an already-verified route table: request path to file and media
    /// type. The caller vouches for every file; this only answers requests.
    /// Used by Formation to observe a Static candidate over real HTTP without
    /// a `.capsule` around it.
    pub fn serve_routes(
        routes: BTreeMap<String, (PathBuf, String)>,
        entry_route: String,
        spa_fallback: bool,
    ) -> std::io::Result<Self> {
        Self::listen(routes, entry_route, spa_fallback, None)
    }

    /// [`Self::serve_routes`] with the in-page state bridge of a static
    /// application: `localStorage` and assets the page may read and update.
    pub fn listen(
        routes: BTreeMap<String, (PathBuf, String)>,
        entry_route: String,
        spa_fallback: bool,
        state: Option<StaticApplicationState>,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let state = state.map(|state| {
            let local_storage = Arc::new(Mutex::new(state.local_storage));
            let assets = state
                .assets
                .into_iter()
                .map(|asset| (asset.asset_id.clone(), asset))
                .collect();
            Arc::new(StaticApplicationRuntimeState {
                persistence_token: state.persistence_token,
                local_storage,
                assets,
            })
        });
        let local_storage = state.as_ref().map(|state| Arc::clone(&state.local_storage));
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let thread_state = state;
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            let _ = ready_tx.send(());
            while thread_running.load(Ordering::Acquire) {
                let Ok((stream, _)) = listener.accept() else {
                    continue;
                };
                if !thread_running.load(Ordering::Acquire) {
                    break;
                }
                let _ = serve_request(
                    stream,
                    &routes,
                    &entry_route,
                    spa_fallback,
                    thread_state.as_deref(),
                );
            }
        });
        ready_rx.recv().map_err(|error| {
            std::io::Error::other(format!("static server did not start: {error}"))
        })?;
        Ok(Self {
            address,
            running,
            local_storage,
            thread: Some(thread),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub fn local_storage(&self) -> std::io::Result<Option<BTreeMap<String, String>>> {
        self.local_storage
            .as_ref()
            .map(|state| {
                state.lock().map(|state| state.clone()).map_err(|_| {
                    std::io::Error::other("static application state lock was poisoned")
                })
            })
            .transpose()
    }
}

impl Drop for StaticApplicationServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Ok(mut stream) = TcpStream::connect_timeout(&self.address, Duration::from_secs(1)) {
            let _ = stream.write_all(b"GET /__ato_shutdown__ HTTP/1.1\r\n\r\n");
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_request(
    mut stream: TcpStream,
    routes: &BTreeMap<String, (PathBuf, String)>,
    entry_route: &str,
    spa_fallback: bool,
    state: Option<&StaticApplicationRuntimeState>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader
        .by_ref()
        .take(8 * 1024)
        .read_line(&mut request_line)?;
    let mut fields = request_line.split_whitespace();
    let method = fields.next().unwrap_or_default();
    let raw_path = fields.next().unwrap_or_default();
    let request_path = raw_path.split('?').next().unwrap_or_default();
    if !request_path.starts_with('/') {
        return write_response(
            &mut stream,
            400,
            "text/plain; charset=utf-8",
            b"Bad Request\n",
            method,
        );
    }
    let mut content_length = None;
    let mut content_type = None;
    let mut persistence_token = None;
    let mut header_bytes = request_line.len();
    loop {
        let mut line = String::new();
        let read = reader.by_ref().take(8 * 1024).read_line(&mut line)?;
        if read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        header_bytes += read;
        if header_bytes > 32 * 1024 {
            return write_response(
                &mut stream,
                400,
                "text/plain; charset=utf-8",
                b"Bad Request\n",
                method,
            );
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse::<usize>().ok();
        } else if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-type")
        {
            content_type = Some(value.trim().to_owned());
        } else if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("x-ato-state-token")
        {
            persistence_token = Some(value.trim().to_owned());
        }
    }
    if method == "POST" && request_path == "/__ato/instance-state/local-storage" {
        let Some(state) = state else {
            return write_response(
                &mut stream,
                404,
                "application/json",
                br#"{"error":"not_found"}"#,
                method,
            );
        };
        if persistence_token.as_deref() != Some(state.persistence_token.as_str()) {
            return write_response(
                &mut stream,
                403,
                "application/json",
                br#"{"error":"forbidden"}"#,
                method,
            );
        }
        if content_type.as_deref() != Some("application/json") {
            return write_response(
                &mut stream,
                400,
                "application/json",
                br#"{"error":"invalid_state"}"#,
                method,
            );
        }
        let Some(content_length) = content_length.filter(|length| *length <= 16 * 1024 * 1024)
        else {
            return write_response(
                &mut stream,
                400,
                "application/json",
                br#"{"error":"invalid_state"}"#,
                method,
            );
        };
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body)?;
        let update = serde_json::from_slice::<StaticStateUpdate>(&body).ok();
        let Some(update) = update.filter(|update| update.local_storage.len() <= 4096) else {
            return write_response(
                &mut stream,
                400,
                "application/json",
                br#"{"error":"invalid_state"}"#,
                method,
            );
        };
        let Ok(mut current) = state.local_storage.lock() else {
            return write_response(
                &mut stream,
                500,
                "application/json",
                br#"{"error":"state_unavailable"}"#,
                method,
            );
        };
        *current = update.local_storage;
        return write_response(&mut stream, 200, "application/json", b"{}", method);
    }
    if !matches!(method, "GET" | "HEAD") {
        return write_response(
            &mut stream,
            400,
            "text/plain; charset=utf-8",
            b"Bad Request\n",
            method,
        );
    }
    if let Some(asset_id) = request_path.strip_prefix("/__ato/assets/") {
        let selected = state.and_then(|state| state.assets.get(asset_id));
        let Some(asset) = selected else {
            return write_response(
                &mut stream,
                404,
                "text/plain; charset=utf-8",
                b"Not Found\n",
                method,
            );
        };
        return write_response(&mut stream, 200, &asset.content_type, &asset.bytes, method);
    }
    let route = if request_path == "/" {
        entry_route
    } else {
        request_path
    };
    let selected = routes
        .get(route)
        .or_else(|| spa_fallback.then(|| routes.get(entry_route)).flatten());
    let Some((path, media_type)) = selected else {
        return write_response(
            &mut stream,
            404,
            "text/plain; charset=utf-8",
            b"Not Found\n",
            method,
        );
    };
    let mut body = fs::read(path)?;
    if route == entry_route
        && media_type.starts_with("text/html")
        && let Some(state) = state
    {
        body = inject_static_application_state(&body, state)?;
    }
    write_response(&mut stream, 200, media_type, &body, method)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticStateUpdate {
    local_storage: BTreeMap<String, String>,
}

fn inject_static_application_state(
    body: &[u8],
    state: &StaticApplicationRuntimeState,
) -> std::io::Result<Vec<u8>> {
    let local_storage = state
        .local_storage
        .lock()
        .map_err(|_| std::io::Error::other("static application state lock was poisoned"))?
        .clone();
    let encoded = base64::engine::general_purpose::STANDARD.encode(
        serde_json::to_vec(&local_storage)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
    );
    let persistence_token = &state.persistence_token;
    let bridge = format!(
        r#"<script>(()=>{{
const storage=window.localStorage;
const bytes=Uint8Array.from(atob('{encoded}'),value=>value.charCodeAt(0));
const initial=JSON.parse(new TextDecoder().decode(bytes));
const rawSet=Storage.prototype.setItem;
const rawRemove=Storage.prototype.removeItem;
const rawClear=Storage.prototype.clear;
rawClear.call(storage);
for(const [key,value] of Object.entries(initial)) rawSet.call(storage,key,value);
let queue=Promise.resolve();
function current(){{
  const values={{}};
  for(const key of Object.keys(storage).sort()) values[key]=storage.getItem(key);
  return values;
}}
function persist(){{
  const body=JSON.stringify({{local_storage:current()}});
  queue=queue.then(()=>fetch('/__ato/instance-state/local-storage',{{
    method:'POST',credentials:'same-origin',keepalive:true,
    headers:{{'content-type':'application/json','x-ato-state-token':'{persistence_token}'}},body
  }})).catch(()=>{{}});
  return queue;
}}
Storage.prototype.setItem=function(key,value){{rawSet.call(this,key,value);if(this===storage)persist();}};
Storage.prototype.removeItem=function(key){{rawRemove.call(this,key);if(this===storage)persist();}};
Storage.prototype.clear=function(){{rawClear.call(this);if(this===storage)persist();}};
window.__atoLocalStateFlush=persist;
}})();</script>"#
    );
    let mut html = String::from_utf8(body.to_vec())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let lowercase = html.to_ascii_lowercase();
    let position = lowercase
        .find("<script")
        .or_else(|| lowercase.find("</head>"))
        .unwrap_or(0);
    html.insert_str(position, &bridge);
    Ok(html.into_bytes())
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    media_type: &str,
    body: &[u8],
    method: &str,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {media_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if method != "HEAD" {
        stream.write_all(body)?;
    }
    stream.flush()
}

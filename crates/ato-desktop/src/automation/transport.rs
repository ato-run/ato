use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use capsule_core::common::paths::ato_path_or_workspace_tmp;
use serde_json::Value;
use tracing::{debug, error};

use super::command::{
    AutomationCommand, JsonRpcError, JsonRpcRequest, JsonRpcResponse, PendingAutomationRequest,
};

pub type PendingQueue = Arc<Mutex<Vec<PendingAutomationRequest>>>;
pub type NotifyFn = Arc<dyn Fn() + Send + Sync + 'static>;

/// Returns the socket path for this process.
pub fn socket_path() -> PathBuf {
    let run_dir = dirs_runtime();
    run_dir.join(format!("ato-desktop-{}.sock", std::process::id()))
}

/// Returns the "current instance" JSON file path.
pub fn current_instance_file() -> PathBuf {
    dirs_runtime().join("ato-desktop-current.json")
}

fn dirs_runtime() -> PathBuf {
    ato_path_or_workspace_tmp("run")
}

/// Start the Unix socket listener in a background thread.
/// Writes socket path to `~/.ato/run/ato-desktop-current.json` for discovery.
///
/// `notify`: called each time a new request is pushed to `pending`, to wake the GPUI loop.
#[cfg(unix)]
pub fn start_socket_listener(pending: PendingQueue, notify: NotifyFn) -> std::io::Result<PathBuf> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    let path = socket_path();
    let run_dir = path.parent().unwrap().to_path_buf();
    fs::create_dir_all(&run_dir)?;

    // Remove stale socket if present.
    if path.exists() {
        let _ = fs::remove_file(&path);
    }

    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;

    // Write discovery file.
    let current_file = current_instance_file();
    let discovery = serde_json::json!({
        "pid": std::process::id(),
        "socket": path.to_string_lossy().as_ref(),
    });
    if let Ok(json) = serde_json::to_string(&discovery) {
        let _ = fs::write(&current_file, json);
    }

    let path_clone = path.clone();
    std::thread::Builder::new()
        .name("ato-desktop-automation-listener".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let pending = Arc::clone(&pending);
                        let notify = Arc::clone(&notify);
                        std::thread::spawn(move || {
                            handle_connection(stream, pending, notify);
                        });
                    }
                    Err(e) => {
                        error!("automation socket accept error: {e}");
                        break;
                    }
                }
            }
            debug!("automation socket listener exiting");
        })?;

    Ok(path_clone)
}

#[cfg(test)]
mod tests {
    use super::current_instance_file;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().expect("env lock")
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn current_instance_file_respects_ato_home_override() {
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let ato_home = temp.path().join("ato-home");
        let _guard = EnvVarGuard::set_path("ATO_HOME", &ato_home);

        assert_eq!(
            current_instance_file(),
            PathBuf::from(&ato_home).join("run/ato-desktop-current.json")
        );
    }
}

#[cfg(not(unix))]
pub fn start_socket_listener(
    _pending: PendingQueue,
    _notify: NotifyFn,
) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "automation socket transport is not yet supported on this platform",
    ))
}

#[cfg(unix)]
fn handle_connection(
    stream: std::os::unix::net::UnixStream,
    pending: PendingQueue,
    notify: NotifyFn,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

    let mut writer = stream.try_clone().ok();
    let reader = BufReader::new(stream);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                debug!("automation connection read error: {e}");
                break;
            }
        };
        if line.is_empty() {
            continue;
        }

        let (id, response_json) = match dispatch_request(&line, &pending, &notify) {
            Ok(rx) => {
                // Block waiting for GPUI to process the request (max 35 s).
                match rx.recv_timeout(Duration::from_secs(35)) {
                    Ok(Ok(value)) => {
                        let id = extract_id(&line);
                        (
                            id.clone(),
                            serde_json::to_string(&JsonRpcResponse::ok(id, value)).unwrap(),
                        )
                    }
                    Ok(Err(msg)) => {
                        let id = extract_id(&line);
                        (
                            id.clone(),
                            serde_json::to_string(&JsonRpcResponse::err(id, -32000, msg)).unwrap(),
                        )
                    }
                    Err(_) => {
                        let id = extract_id(&line);
                        (
                            id.clone(),
                            serde_json::to_string(&JsonRpcResponse::err(
                                id,
                                -32000,
                                "automation command timed out",
                            ))
                            .unwrap(),
                        )
                    }
                }
            }
            Err(resp) => {
                let id = extract_id(&line);
                (id, resp)
            }
        };

        let _ = id; // already embedded in response_json

        if let Some(ref mut w) = writer {
            let _ = w.write_all(response_json.as_bytes());
            let _ = w.write_all(b"\n");
            let _ = w.flush();
        }
        // One request per connection — close after responding.
        break;
    }
}

/// Parse and enqueue a JSON-RPC request. Returns the receiver or a pre-built error JSON string.
fn dispatch_request(
    line: &str,
    pending: &PendingQueue,
    notify: &NotifyFn,
) -> Result<std::sync::mpsc::Receiver<Result<Value, String>>, String> {
    let rpc: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            let err_json = serde_json::to_string(&JsonRpcResponse {
                jsonrpc: "2.0",
                id: Value::Null,
                result: None,
                error: Some(JsonRpcError {
                    code: -32700,
                    message: format!("parse error: {e}"),
                }),
            })
            .unwrap();
            return Err(err_json);
        }
    };

    let id = rpc.id.clone().unwrap_or(Value::Null);
    let pane_id = rpc
        .params
        .get("pane_id")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let command = match parse_command(&rpc.method, &rpc.params) {
        Ok(cmd) => cmd,
        Err(msg) => {
            let err_json = serde_json::to_string(&JsonRpcResponse::err(id, -32601, msg)).unwrap();
            return Err(err_json);
        }
    };

    let (tx, rx) = channel();
    let req = PendingAutomationRequest::new(pane_id, command, tx);

    if let Ok(mut q) = pending.lock() {
        q.push(req);
    }
    notify();

    Ok(rx)
}

fn parse_command(method: &str, params: &Value) -> Result<AutomationCommand, String> {
    let s = |key: &str| -> Result<String, String> {
        params
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("missing required param '{key}'"))
    };

    match method {
        "snapshot" => Ok(AutomationCommand::Snapshot),
        "screenshot" => Ok(AutomationCommand::Screenshot),
        "click" => Ok(AutomationCommand::Click { ref_id: s("ref")? }),
        "fill" => Ok(AutomationCommand::Fill {
            ref_id: s("ref")?,
            value: s("value")?,
        }),
        "type" => Ok(AutomationCommand::Type {
            ref_id: s("ref")?,
            text: s("text")?,
        }),
        "select_option" => Ok(AutomationCommand::SelectOption {
            ref_id: s("ref")?,
            value: s("value")?,
        }),
        "check" => Ok(AutomationCommand::Check {
            ref_id: s("ref")?,
            checked: params
                .get("checked")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        }),
        "press_key" => Ok(AutomationCommand::PressKey { key: s("key")? }),
        "navigate" => Ok(AutomationCommand::Navigate { url: s("url")? }),
        "navigate_back" => Ok(AutomationCommand::NavigateBack),
        "navigate_forward" => Ok(AutomationCommand::NavigateForward),
        "wait_for" => Ok(AutomationCommand::WaitFor {
            selector: s("selector")?,
            timeout_ms: params
                .get("timeout")
                .and_then(|v| v.as_u64())
                .unwrap_or(5000),
        }),
        "evaluate" => Ok(AutomationCommand::Evaluate {
            expression: s("expression")?,
        }),
        "console_messages" => Ok(AutomationCommand::ConsoleMessages),
        "verify_text_visible" => Ok(AutomationCommand::VerifyTextVisible { text: s("text")? }),
        "verify_element_visible" => {
            Ok(AutomationCommand::VerifyElementVisible { ref_id: s("ref")? })
        }
        "list_panes" => Ok(AutomationCommand::ListPanes),
        "open_url" => Ok(AutomationCommand::OpenUrl { url: s("url")? }),
        "focus_pane" => Ok(AutomationCommand::FocusPane {
            pane_id: params
                .get("pane_id")
                .and_then(|v| v.as_u64())
                .ok_or("missing required param 'pane_id'")? as usize,
        }),
        other => Err(format!("unknown automation method: {other}")),
    }
}

fn extract_id(line: &str) -> Value {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("id").cloned())
        .unwrap_or(Value::Null)
}

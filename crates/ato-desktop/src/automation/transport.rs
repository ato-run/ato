use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};

use capsule_core::common::paths::ato_path_or_workspace_tmp;
use serde_json::Value;
use tracing::{debug, error};

use super::command::{
    AutomationCommand, JsonRpcError, JsonRpcRequest, JsonRpcResponse, PendingAutomationRequest,
};
use super::policy::AUTOMATION_CONNECTION_IO_TIMEOUT;

pub type PendingQueue = Arc<Mutex<Vec<PendingAutomationRequest>>>;
pub type NotifyFn = Arc<dyn Fn() + Send + Sync + 'static>;
pub type ActivePaneSnapshot = Arc<Mutex<Option<usize>>>;

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

/// Best-effort liveness check for a unix process. Returns `true` when `kill(pid, 0)`
/// succeeds OR fails with `EPERM` (process exists but we can't signal it). Only a
/// hard `ESRCH` is treated as dead — anything else (including transient errors)
/// errs on the side of "still alive" so we don't reap a live socket by mistake.
#[cfg(unix)]
pub(crate) fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: signal 0 performs error checking only; no signal is delivered.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    errno != libc::ESRCH
}

/// Extract `<pid>` from `ato-desktop-<pid>.sock`. Returns `None` for any other shape.
fn parse_socket_filename_pid(name: &str) -> Option<u32> {
    let stem = name.strip_prefix("ato-desktop-")?.strip_suffix(".sock")?;
    stem.parse::<u32>().ok()
}

/// Walk the run directory and remove `ato-desktop-<pid>.sock` files whose PID is
/// no longer alive. Skips the current process's own socket. Idempotent and best
/// effort — failures only surface as debug log lines (#68).
#[cfg(unix)]
fn reap_orphan_sockets(run_dir: &std::path::Path) {
    use std::fs;

    let entries = match fs::read_dir(run_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let self_pid = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Some(pid) = parse_socket_filename_pid(name_str) else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        if pid_is_alive(pid) {
            continue;
        }
        let path = entry.path();
        if let Err(e) = fs::remove_file(&path) {
            debug!(
                "automation socket orphan reap failed for {}: {e}",
                path.display()
            );
        } else {
            debug!(
                "automation socket orphan reaped: {} (pid {pid} dead)",
                path.display()
            );
        }
    }
}

/// Start the Unix socket listener in a background thread.
/// Writes socket path to `~/.ato/run/ato-desktop-current.json` for discovery.
///
/// `notify`: called each time a new request is pushed to `pending`, to wake the GPUI loop.
/// `active_pane`: snapshot updated by `WebViewManager` so MCP `pane_id=0` requests
/// resolve at enqueue time (#67).
#[cfg(unix)]
pub fn start_socket_listener(
    pending: PendingQueue,
    notify: NotifyFn,
    active_pane: ActivePaneSnapshot,
) -> std::io::Result<PathBuf> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    let path = socket_path();
    let run_dir = path.parent().unwrap().to_path_buf();
    fs::create_dir_all(&run_dir)?;

    // Garbage collect orphan sockets from previous PIDs before we publish our own
    // discovery file — otherwise the MCP fallback enumeration in
    // `ato_desktop_mcp::discover_socket` can pick a stale socket from a crashed
    // instance (#68).
    reap_orphan_sockets(&run_dir);

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
                        let active_pane = Arc::clone(&active_pane);
                        std::thread::spawn(move || {
                            handle_connection(stream, pending, notify, active_pane);
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
    use super::{current_instance_file, parse_socket_filename_pid, pid_is_alive};
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock")
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

    #[test]
    fn parse_socket_filename_pid_accepts_canonical_shape() {
        assert_eq!(
            parse_socket_filename_pid("ato-desktop-12345.sock"),
            Some(12345)
        );
        assert_eq!(parse_socket_filename_pid("ato-desktop-1.sock"), Some(1));
    }

    #[test]
    fn parse_socket_filename_pid_rejects_non_pid_shapes() {
        assert_eq!(parse_socket_filename_pid("ato-desktop.sock"), None);
        assert_eq!(parse_socket_filename_pid("ato-desktop-current.json"), None);
        assert_eq!(parse_socket_filename_pid("ato-desktop-foo.sock"), None);
        assert_eq!(parse_socket_filename_pid("ato-desktop--1.sock"), None);
        assert_eq!(parse_socket_filename_pid("other-12345.sock"), None);
    }

    #[test]
    fn pid_is_alive_self_pid_returns_true() {
        assert!(pid_is_alive(std::process::id()));
    }

    #[test]
    fn pid_is_alive_pid_zero_returns_false() {
        assert!(!pid_is_alive(0));
    }

    #[test]
    fn parse_command_set_capsule_secrets_defaults_clear_pending_to_true() {
        let params = serde_json::json!({
            "handle": "github.com/Koh0920/WasedaP2P",
            "secrets": { "PG_PASSWORD": "p", "SECRET_KEY": "s" }
        });
        let cmd = super::parse_command("set_capsule_secrets", &params).expect("parse");
        match cmd {
            super::AutomationCommand::SetCapsuleSecrets {
                handle,
                secrets,
                clear_pending_config,
            } => {
                assert_eq!(handle, "github.com/Koh0920/WasedaP2P");
                assert!(clear_pending_config, "default must be true");
                let mut keys: Vec<_> = secrets.iter().map(|(k, _)| k.clone()).collect();
                keys.sort();
                assert_eq!(keys, vec!["PG_PASSWORD", "SECRET_KEY"]);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn parse_command_set_capsule_secrets_respects_explicit_false() {
        let params = serde_json::json!({
            "handle": "h",
            "secrets": { "K": "v" },
            "clear_pending_config": false,
        });
        let cmd = super::parse_command("set_capsule_secrets", &params).expect("parse");
        match cmd {
            super::AutomationCommand::SetCapsuleSecrets {
                clear_pending_config,
                ..
            } => assert!(!clear_pending_config),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn parse_command_set_capsule_secrets_rejects_empty_secrets() {
        let params = serde_json::json!({"handle": "h", "secrets": {}});
        let err = super::parse_command("set_capsule_secrets", &params).unwrap_err();
        assert!(
            err.contains("at least one"),
            "expected empty-secrets error, got: {err}"
        );
    }

    #[test]
    fn parse_command_set_capsule_secrets_rejects_non_string_value() {
        let params = serde_json::json!({"handle": "h", "secrets": {"K": 42}});
        let err = super::parse_command("set_capsule_secrets", &params).unwrap_err();
        assert!(
            err.contains("must be a string"),
            "expected non-string error, got: {err}"
        );
    }

    #[test]
    fn parse_command_set_capsule_secrets_rejects_missing_handle() {
        let params = serde_json::json!({"secrets": {"K": "v"}});
        let err = super::parse_command("set_capsule_secrets", &params).unwrap_err();
        assert!(
            err.contains("'handle'"),
            "expected handle error, got: {err}"
        );
    }

    #[test]
    fn parse_command_approve_execution_plan_consent_round_trips_handle() {
        let params = serde_json::json!({"handle": "capsule://github.com/Koh0920/WasedaP2P"});
        let cmd = super::parse_command("approve_execution_plan_consent", &params).expect("parse");
        match cmd {
            super::AutomationCommand::ApproveExecutionPlanConsent { handle } => {
                assert_eq!(handle, "capsule://github.com/Koh0920/WasedaP2P");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn parse_command_approve_execution_plan_consent_rejects_missing_handle() {
        let params = serde_json::json!({});
        let err = super::parse_command("approve_execution_plan_consent", &params).unwrap_err();
        assert!(err.contains("'handle'"), "expected handle error: {err}");
    }

    #[test]
    fn parse_command_stop_active_session_takes_no_args() {
        let params = serde_json::json!({});
        let cmd = super::parse_command("stop_active_session", &params).expect("parse");
        assert!(matches!(cmd, super::AutomationCommand::StopActiveSession));
    }

    #[test]
    fn parse_command_stop_active_session_ignores_unknown_params() {
        // The unit variant deliberately ignores any params (including
        // `pane_id`) so callers can pass through the same arg shape
        // they use for browser_* tools. Verify we don't reject extras.
        let params = serde_json::json!({"pane_id": 7, "ignored": "yes"});
        let cmd = super::parse_command("stop_active_session", &params).expect("parse");
        assert!(matches!(cmd, super::AutomationCommand::StopActiveSession));
    }

    #[test]
    fn reap_orphan_sockets_removes_dead_pid_socket_only() {
        use std::fs;
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        let self_pid = std::process::id();

        let live = dir.join(format!("ato-desktop-{self_pid}.sock"));
        // pid 0 is guaranteed-dead per pid_is_alive.
        let dead = dir.join("ato-desktop-0.sock");
        let unrelated = dir.join("other-1.sock");

        fs::write(&live, b"").unwrap();
        fs::write(&dead, b"").unwrap();
        fs::write(&unrelated, b"").unwrap();

        super::reap_orphan_sockets(dir);

        assert!(live.exists(), "self socket must survive");
        assert!(!dead.exists(), "dead-pid socket must be reaped");
        assert!(unrelated.exists(), "non-matching files must be left alone");
    }
}

#[cfg(not(unix))]
pub fn start_socket_listener(
    _pending: PendingQueue,
    _notify: NotifyFn,
    _active_pane: ActivePaneSnapshot,
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
    active_pane: ActivePaneSnapshot,
) {
    let _ = stream.set_read_timeout(Some(AUTOMATION_CONNECTION_IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(AUTOMATION_CONNECTION_IO_TIMEOUT));

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

        let (id, response_json) = match dispatch_request(&line, &pending, &notify, &active_pane) {
            Ok((rx, response_timeout)) => {
                // Block waiting for GPUI to process the request.
                match rx.recv_timeout(response_timeout) {
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
    active_pane: &ActivePaneSnapshot,
) -> Result<
    (
        std::sync::mpsc::Receiver<Result<Value, String>>,
        std::time::Duration,
    ),
    String,
> {
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
    // pane_id=0 (the MCP default) means "active pane". Snapshot the active
    // pane *now* — at request-enqueue time — so a UI focus change between
    // enqueue and dispatch can't redirect the request to a different pane (#67).
    // If no active pane is recorded yet, fall through with 0 and let the
    // dispatcher surface "no active pane".
    let raw_pane_id = rpc
        .params
        .get("pane_id")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let pane_id = if raw_pane_id == 0 {
        active_pane.lock().ok().and_then(|g| *g).unwrap_or(0)
    } else {
        raw_pane_id
    };

    let command = match parse_command(&rpc.method, &rpc.params) {
        Ok(cmd) => cmd,
        Err(msg) => {
            let err_json = serde_json::to_string(&JsonRpcResponse::err(id, -32601, msg)).unwrap();
            return Err(err_json);
        }
    };

    let (tx, rx) = channel();
    let req = PendingAutomationRequest::new(pane_id, command, tx);
    let response_timeout = req.response_timeout();

    if let Ok(mut q) = pending.lock() {
        q.push(req);
    }
    notify();

    Ok((rx, response_timeout))
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
        "set_capsule_secrets" => {
            let handle = s("handle")?;
            let secrets_obj = params
                .get("secrets")
                .and_then(|v| v.as_object())
                .ok_or("missing required param 'secrets' (object of key→string)")?;
            if secrets_obj.is_empty() {
                return Err("'secrets' must contain at least one entry".into());
            }
            let mut secrets = Vec::with_capacity(secrets_obj.len());
            for (key, value) in secrets_obj {
                let value_str = value
                    .as_str()
                    .ok_or_else(|| format!("secret '{key}' must be a string (got {})", value))?;
                secrets.push((key.clone(), value_str.to_string()));
            }
            let clear_pending_config = params
                .get("clear_pending_config")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            Ok(AutomationCommand::SetCapsuleSecrets {
                handle,
                secrets,
                clear_pending_config,
            })
        }
        "approve_execution_plan_consent" => Ok(AutomationCommand::ApproveExecutionPlanConsent {
            handle: s("handle")?,
        }),
        // Unit variant: ignores all params (including `pane_id`,
        // which the dispatcher resolves implicitly to the active
        // pane via `WebViewManager::stop_active_session`). Keeping
        // the surface argumentless mirrors the keybind / omnibar
        // entry points, so MCP and UI exit through the exact same
        // code path (refs #92 AC-step 6).
        "stop_active_session" => Ok(AutomationCommand::StopActiveSession),
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

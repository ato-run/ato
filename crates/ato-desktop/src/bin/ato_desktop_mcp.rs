/// ato-desktop-mcp — MCP stdio bridge for ato-desktop WebView automation.
///
/// Implements the Model Context Protocol (JSON-RPC 2.0 over stdio) and translates
/// MCP `tools/call` requests into automation commands sent to the ato-desktop Unix socket.
///
/// Usage:
///   ato-desktop-mcp [--socket <path>]
///
/// If --socket is omitted, the socket path is read from ${ATO_HOME:-~/.ato}/run/ato-desktop-current.json.
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use capsule_core::common::paths::ato_path_or_workspace_tmp;

// Share the timeout policy with `ato-desktop` so client and server budgets stay
// in lockstep (#69). The lib crate has no published library target, so we
// pull the source in by relative path — the file is small and pure constants.
#[path = "../automation/policy.rs"]
mod policy;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let socket_path = parse_socket_arg(&args).unwrap_or_else(discover_socket);

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    for line in BufReader::new(stdin.lock()).lines() {
        let line = match line {
            Ok(l) if !l.is_empty() => l,
            Ok(_) => continue,
            Err(_) => break,
        };

        let request: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                write_response(
                    &mut out,
                    serde_json::json!({
                        "jsonrpc":"2.0","id":null,
                        "error":{"code":-32700,"message":format!("parse error: {e}")}
                    }),
                );
                continue;
            }
        };

        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");

        // Notifications (no id) — handle silently without responding.
        if request.get("id").is_none() {
            continue;
        }

        let response = match method {
            "initialize" => handle_initialize(id),
            "tools/list" => handle_tools_list(id),
            "tools/call" => handle_tools_call(id, &request, &socket_path),
            _ => serde_json::json!({
                "jsonrpc":"2.0","id":id,
                "error":{"code":-32601,"message":format!("method not found: {method}")}
            }),
        };

        write_response(&mut out, response);
    }
}

fn write_response(out: &mut impl Write, response: serde_json::Value) {
    let _ = out.write_all(response.to_string().as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

fn parse_socket_arg(args: &[String]) -> Option<PathBuf> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--socket" {
            return it.next().map(PathBuf::from);
        }
    }
    None
}

fn discover_socket() -> PathBuf {
    let run_dir = ato_path_or_workspace_tmp("run");
    let current_file = run_dir.join("ato-desktop-current.json");
    if let Ok(data) = std::fs::read_to_string(&current_file) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
            let pid = v.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32);
            // Skip the discovery file's recorded socket if its pid is provably
            // dead. When `pid` is absent we cannot prove liveness either way,
            // so we trust the file and let the connect step surface the error
            // (#68).
            let pid_alive = pid.map(pid_is_alive).unwrap_or(true);
            if pid_alive {
                if let Some(path) = v.get("socket").and_then(|s| s.as_str()) {
                    return PathBuf::from(path);
                }
            }
        }
    }
    // Fallback: enumerate `ato-desktop-<pid>.sock` and pick the first whose
    // pid is alive. This rules out orphan sockets left behind by crashed
    // instances (#68).
    if let Ok(entries) = std::fs::read_dir(&run_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !(name.starts_with("ato-desktop-") && name.ends_with(".sock")) {
                continue;
            }
            let stem = name
                .strip_prefix("ato-desktop-")
                .and_then(|s| s.strip_suffix(".sock"));
            let pid = stem.and_then(|s| s.parse::<u32>().ok());
            // Filenames without an embedded pid (e.g. legacy
            // `ato-desktop.sock`) are kept; canonical `ato-desktop-<pid>.sock`
            // entries are filtered by liveness.
            if pid.map(pid_is_alive).unwrap_or(true) {
                return entry.path();
            }
        }
    }
    // Last resort default.
    run_dir.join("ato-desktop.sock")
}

/// Best-effort liveness check used when picking which discovered socket to
/// trust. Mirrors `automation::transport::pid_is_alive` — duplicated here
/// because the bin crate cannot import private modules from the library
/// binary. Keep the two implementations in sync.
#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: signal 0 performs error checking only; no signal is delivered.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    let errno = std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(0);
    errno != libc::ESRCH
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    true
}

// ── MCP handlers ──────────────────────────────────────────────────────────────

fn handle_initialize(id: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "ato-desktop-mcp",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::discover_socket;
    use std::ffi::OsString;
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
    fn discover_socket_uses_ato_home_run_dir() {
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let ato_home = temp.path().join("isolated-ato-home");
        let fake_home = temp.path().join("real-home");
        let _ato_home = EnvVarGuard::set_path("ATO_HOME", &ato_home);
        let _home = EnvVarGuard::set_path("HOME", &fake_home);

        std::fs::create_dir_all(ato_home.join("run")).expect("create run dir");
        std::fs::create_dir_all(fake_home.join(".ato/run")).expect("create fake home run dir");

        let isolated_socket = ato_home.join("run/ato-desktop-isolated.sock");
        let leaked_socket = fake_home.join(".ato/run/ato-desktop-real.sock");

        std::fs::write(
            ato_home.join("run/ato-desktop-current.json"),
            format!("{{\"socket\":\"{}\"}}", isolated_socket.display()),
        )
        .expect("write isolated discovery file");
        std::fs::write(
            fake_home.join(".ato/run/ato-desktop-current.json"),
            format!("{{\"socket\":\"{}\"}}", leaked_socket.display()),
        )
        .expect("write leaked discovery file");

        assert_eq!(discover_socket(), isolated_socket);
    }

    #[test]
    fn discover_socket_skips_dead_pid_in_current_json_and_falls_back_to_live_sock() {
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let ato_home = temp.path().join("ato-home");
        let _ato_home = EnvVarGuard::set_path("ATO_HOME", &ato_home);
        std::fs::create_dir_all(ato_home.join("run")).expect("create run dir");

        // current.json points at a dead pid → must be ignored.
        let dead_socket = ato_home.join("run/ato-desktop-dead.sock");
        std::fs::write(
            ato_home.join("run/ato-desktop-current.json"),
            format!(
                "{{\"pid\":0,\"socket\":\"{}\"}}",
                dead_socket.display()
            ),
        )
        .expect("write current.json");
        std::fs::write(&dead_socket, b"").expect("touch dead socket");

        // Drop a `.sock` named after this process — its pid is alive, so the
        // fallback enumeration should pick it.
        let live_socket = ato_home
            .join("run")
            .join(format!("ato-desktop-{}.sock", std::process::id()));
        std::fs::write(&live_socket, b"").expect("touch live socket");

        assert_eq!(discover_socket(), live_socket);
    }

    #[test]
    fn discover_socket_trusts_pidless_current_json() {
        // Backward-compat: discovery files written by older instances may
        // omit `pid`. We can't prove liveness then, so trust the file and
        // let connect() surface the error.
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let ato_home = temp.path().join("ato-home");
        let _ato_home = EnvVarGuard::set_path("ATO_HOME", &ato_home);
        std::fs::create_dir_all(ato_home.join("run")).expect("create run dir");

        let socket = ato_home.join("run/ato-desktop-legacy.sock");
        std::fs::write(
            ato_home.join("run/ato-desktop-current.json"),
            format!("{{\"socket\":\"{}\"}}", socket.display()),
        )
        .expect("write current.json");

        assert_eq!(discover_socket(), socket);
    }
}

fn handle_tools_list(id: serde_json::Value) -> serde_json::Value {
    let tools: serde_json::Value = serde_json::from_str(TOOLS).expect("TOOLS is valid JSON");
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "tools": tools
        }
    })
}

fn handle_tools_call(
    id: serde_json::Value,
    request: &serde_json::Value,
    socket_path: &Path,
) -> serde_json::Value {
    let params = request.get("params").cloned().unwrap_or_default();
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or_default();

    let (method, rpc_params) = match map_tool_to_command(tool_name, &args) {
        Ok(pair) => pair,
        Err(msg) => {
            return mcp_error(id, &msg);
        }
    };

    match send_automation_command(socket_path, &method, rpc_params) {
        Ok(result) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": result.to_string() }],
                "isError": false
            }
        }),
        Err(msg) => mcp_error(id, &msg),
    }
}

fn mcp_error(id: serde_json::Value, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": message }],
            "isError": true
        }
    })
}

// ── Tool → command mapping ────────────────────────────────────────────────────

fn map_tool_to_command(
    tool: &str,
    args: &serde_json::Value,
) -> Result<(String, serde_json::Value), String> {
    let s = |key: &str| -> Result<String, String> {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| format!("missing required argument '{key}'"))
    };

    let pane_id = args.get("pane_id").and_then(|v| v.as_u64()).unwrap_or(0);

    let (method, mut params): (&str, serde_json::Value) = match tool {
        "browser_snapshot" => ("snapshot", serde_json::json!({})),
        "browser_take_screenshot" => ("screenshot", serde_json::json!({})),
        "browser_click" => ("click", serde_json::json!({ "ref": s("ref")? })),
        "browser_fill" => (
            "fill",
            serde_json::json!({ "ref": s("ref")?, "value": s("value")? }),
        ),
        "browser_type" => (
            "type",
            serde_json::json!({ "ref": s("ref")?, "text": s("text")? }),
        ),
        "browser_select_option" => (
            "select_option",
            serde_json::json!({ "ref": s("ref")?, "value": s("value")? }),
        ),
        "browser_check" => {
            let checked = args
                .get("checked")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            (
                "check",
                serde_json::json!({ "ref": s("ref")?, "checked": checked }),
            )
        }
        "browser_uncheck" => (
            "check",
            serde_json::json!({ "ref": s("ref")?, "checked": false }),
        ),
        "browser_press_key" => ("press_key", serde_json::json!({ "key": s("key")? })),
        // browser_navigate → open_url (app-level navigate_to_url, works even without an active pane).
        "browser_navigate" => ("open_url", serde_json::json!({ "url": s("url")? })),
        "browser_navigate_back" => ("navigate_back", serde_json::json!({})),
        "browser_navigate_forward" => ("navigate_forward", serde_json::json!({})),
        "browser_wait_for" => {
            let timeout = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5000);
            (
                "wait_for",
                serde_json::json!({ "selector": s("selector")?, "timeout": timeout }),
            )
        }
        "browser_evaluate" => (
            "evaluate",
            serde_json::json!({ "expression": s("expression")? }),
        ),
        "browser_console_messages" => ("console_messages", serde_json::json!({})),
        "browser_verify_text_visible" => (
            "verify_text_visible",
            serde_json::json!({ "text": s("text")? }),
        ),
        "browser_verify_element_visible" => (
            "verify_element_visible",
            serde_json::json!({ "ref": s("ref")? }),
        ),
        "browser_tabs" => ("list_panes", serde_json::json!({})),
        "browser_tab_focus" => {
            let target = args
                .get("pane_id")
                .and_then(|v| v.as_u64())
                .ok_or("missing required argument 'pane_id'")?;
            ("focus_pane", serde_json::json!({ "pane_id": target }))
        }
        other => return Err(format!("unknown tool: {other}")),
    };

    // Attach pane_id to all params.
    if let serde_json::Value::Object(ref mut map) = params {
        map.insert("pane_id".into(), serde_json::json!(pane_id));
    }

    Ok((method.into(), params))
}

// ── Socket communication ──────────────────────────────────────────────────────

#[cfg(unix)]
fn send_automation_command(
    socket_path: &Path,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use std::io::{BufRead, BufReader, ErrorKind, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path).map_err(|e| match e.kind() {
        ErrorKind::NotFound => format!(
            "no ato-desktop instance is running (no socket at {})",
            socket_path.display()
        ),
        ErrorKind::PermissionDenied => format!(
            "ato-desktop socket {} is owned by another user; only the owner can connect",
            socket_path.display()
        ),
        ErrorKind::ConnectionRefused => format!(
            "ato-desktop socket {} is stale (no listener) — start ato-desktop or remove the file",
            socket_path.display()
        ),
        _ => format!("cannot connect to ato-desktop socket {}: {e}", socket_path.display()),
    })?;
    stream
        .set_read_timeout(Some(policy::AUTOMATION_CLIENT_RESPONSE_TIMEOUT))
        .ok();
    stream
        .set_write_timeout(Some(policy::AUTOMATION_CLIENT_WRITE_TIMEOUT))
        .ok();

    let rpc = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    });
    let mut line = serde_json::to_string(&rpc).unwrap();
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|e| format!("send failed: {e}"))?;

    let mut response_line = String::new();
    BufReader::new(stream)
        .read_line(&mut response_line)
        .map_err(|e| format!("receive failed: {e}"))?;

    let response: serde_json::Value =
        serde_json::from_str(&response_line).map_err(|e| format!("invalid JSON response: {e}"))?;

    if let Some(error) = response.get("error") {
        let msg = error
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("automation error");
        return Err(msg.to_string());
    }

    Ok(response
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

#[cfg(not(unix))]
fn send_automation_command(
    _socket_path: &Path,
    _method: &str,
    _params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    Err("automation socket transport is not supported on this platform".into())
}

// ── Tool definitions ──────────────────────────────────────────────────────────

static TOOLS: &str = r#"[
  {"name":"browser_snapshot","description":"Returns an accessibility tree snapshot of the active WebView page.","inputSchema":{"type":"object","properties":{"pane_id":{"type":"integer","description":"Target pane ID (0 = active pane)"}},"required":[]}},
  {"name":"browser_take_screenshot","description":"Takes a PNG screenshot of the active WebView (macOS only).","inputSchema":{"type":"object","properties":{"pane_id":{"type":"integer"}},"required":[]}},
  {"name":"browser_click","description":"Clicks an element by its stable ref from browser_snapshot.","inputSchema":{"type":"object","properties":{"ref":{"type":"string"},"pane_id":{"type":"integer"}},"required":["ref"]}},
  {"name":"browser_fill","description":"Sets the value of an input by ref (clears first).","inputSchema":{"type":"object","properties":{"ref":{"type":"string"},"value":{"type":"string"},"pane_id":{"type":"integer"}},"required":["ref","value"]}},
  {"name":"browser_type","description":"Types text character-by-character into an element.","inputSchema":{"type":"object","properties":{"ref":{"type":"string"},"text":{"type":"string"},"pane_id":{"type":"integer"}},"required":["ref","text"]}},
  {"name":"browser_select_option","description":"Selects an option in a <select> element by value or text.","inputSchema":{"type":"object","properties":{"ref":{"type":"string"},"value":{"type":"string"},"pane_id":{"type":"integer"}},"required":["ref","value"]}},
  {"name":"browser_check","description":"Checks a checkbox or radio button.","inputSchema":{"type":"object","properties":{"ref":{"type":"string"},"checked":{"type":"boolean"},"pane_id":{"type":"integer"}},"required":["ref"]}},
  {"name":"browser_uncheck","description":"Unchecks a checkbox.","inputSchema":{"type":"object","properties":{"ref":{"type":"string"},"pane_id":{"type":"integer"}},"required":["ref"]}},
  {"name":"browser_press_key","description":"Presses a key on the currently focused element.","inputSchema":{"type":"object","properties":{"key":{"type":"string","description":"Key name, e.g. Enter, Escape, ArrowDown"},"pane_id":{"type":"integer"}},"required":["key"]}},
  {"name":"browser_navigate","description":"Navigates the WebView to a URL.","inputSchema":{"type":"object","properties":{"url":{"type":"string"},"pane_id":{"type":"integer"}},"required":["url"]}},
  {"name":"browser_navigate_back","description":"Goes back in the WebView history.","inputSchema":{"type":"object","properties":{"pane_id":{"type":"integer"}},"required":[]}},
  {"name":"browser_navigate_forward","description":"Goes forward in the WebView history.","inputSchema":{"type":"object","properties":{"pane_id":{"type":"integer"}},"required":[]}},
  {"name":"browser_wait_for","description":"Waits until a CSS selector matches an element.","inputSchema":{"type":"object","properties":{"selector":{"type":"string"},"timeout":{"type":"integer","description":"Timeout in ms (default 5000)"},"pane_id":{"type":"integer"}},"required":["selector"]}},
  {"name":"browser_evaluate","description":"Evaluates a JavaScript expression and returns the result.","inputSchema":{"type":"object","properties":{"expression":{"type":"string"},"pane_id":{"type":"integer"}},"required":["expression"]}},
  {"name":"browser_console_messages","description":"Returns buffered console messages and clears the buffer.","inputSchema":{"type":"object","properties":{"pane_id":{"type":"integer"}},"required":[]}},
  {"name":"browser_verify_text_visible","description":"Checks whether the given text appears in the page content.","inputSchema":{"type":"object","properties":{"text":{"type":"string"},"pane_id":{"type":"integer"}},"required":["text"]}},
  {"name":"browser_verify_element_visible","description":"Checks whether the element with the given ref is visible.","inputSchema":{"type":"object","properties":{"ref":{"type":"string"},"pane_id":{"type":"integer"}},"required":["ref"]}},
  {"name":"browser_tabs","description":"Lists all open WebView panes with their IDs.","inputSchema":{"type":"object","properties":{},"required":[]}},
  {"name":"browser_tab_focus","description":"Focuses a specific WebView pane by ID.","inputSchema":{"type":"object","properties":{"pane_id":{"type":"integer"}},"required":["pane_id"]}}
]"#;

/// ato-desktop-mcp — MCP stdio bridge for ato-desktop WebView automation.
///
/// Implements the Model Context Protocol (JSON-RPC 2.0 over stdio) and translates
/// MCP `tools/call` requests into automation commands sent to the ato-desktop Unix socket.
///
/// Usage:
///   ato-desktop-mcp [--socket <path>]
///
/// If --socket is omitted, the socket path is read from ~/.ato/run/ato-desktop-current.json.
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    let home = dirs_home();
    let current_file = home
        .join(".ato")
        .join("run")
        .join("ato-desktop-current.json");
    if let Ok(data) = std::fs::read_to_string(&current_file) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(path) = v.get("socket").and_then(|s| s.as_str()) {
                return PathBuf::from(path);
            }
        }
    }
    // Fallback: first matching socket in run dir.
    let run_dir = home.join(".ato").join("run");
    if let Ok(entries) = std::fs::read_dir(&run_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("ato-desktop-") && name.ends_with(".sock") {
                return entry.path();
            }
        }
    }
    // Last resort default.
    run_dir.join("ato-desktop.sock")
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
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
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("cannot connect to ato-desktop socket: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(36))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

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

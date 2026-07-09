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

use capsule::common::paths::ato_path_or_workspace_tmp;

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

    #[cfg(unix)]
    {
        // Fallback: enumerate `ato-desktop-<pid>.sock` and pick the first whose
        // pid is alive. This rules out orphan sockets left behind by crashed
        // instances (#68).
        let mut legacy_socket = None;
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
                match pid {
                    Some(pid) if pid_is_alive(pid) => return entry.path(),
                    Some(_) => {}
                    None => {
                        legacy_socket.get_or_insert_with(|| entry.path());
                    }
                }
            }
        }
        if let Some(path) = legacy_socket {
            return path;
        }
        return run_dir.join("ato-desktop.sock");
    }

    #[cfg(windows)]
    {
        // Named pipes have no filesystem representation that can be enumerated.
        // Without a valid discovery JSON we cannot determine which
        // `\\.\pipe\ato-desktop-<pid>` to connect to. Return a sentinel that
        // does not exist, producing a clear "not running" error at connect time.
        PathBuf::from(r"\\.\pipe\ato-desktop-not-running")
    }

    #[cfg(not(any(unix, windows)))]
    {
        run_dir.join("ato-desktop.sock")
    }
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
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    errno != libc::ESRCH
}

#[cfg(windows)]
fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output();
    match output {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            text.contains(&pid.to_string())
        }
        Err(_) => true,
    }
}

#[cfg(not(any(unix, windows)))]
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
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                unsafe {
                    std::env::set_var(self.key, value);
                }
            } else {
                unsafe {
                    std::env::remove_var(self.key);
                }
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
            format!("{{\"pid\":0,\"socket\":\"{}\"}}", dead_socket.display()),
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
    fn tools_list_includes_set_capsule_secrets_with_required_fields() {
        let tools: serde_json::Value =
            serde_json::from_str(super::TOOLS).expect("TOOLS is valid JSON");
        let arr = tools.as_array().expect("array");
        let entry = arr
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("set_capsule_secrets"))
            .expect("set_capsule_secrets registered");
        let required = entry
            .get("inputSchema")
            .and_then(|s| s.get("required"))
            .and_then(|r| r.as_array())
            .expect("required[]");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"handle"), "handle required");
        assert!(names.contains(&"secrets"), "secrets required");
    }

    #[test]
    fn tools_list_includes_approve_execution_plan_consent_with_required_handle() {
        let tools: serde_json::Value =
            serde_json::from_str(super::TOOLS).expect("TOOLS is valid JSON");
        let arr = tools.as_array().expect("array");
        let entry = arr
            .iter()
            .find(|t| {
                t.get("name").and_then(|v| v.as_str()) == Some("approve_execution_plan_consent")
            })
            .expect("approve_execution_plan_consent registered");
        let required = entry
            .get("inputSchema")
            .and_then(|s| s.get("required"))
            .and_then(|r| r.as_array())
            .expect("required[]");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"handle"), "handle required");
    }

    #[test]
    fn map_tool_passes_through_secrets_and_handle() {
        let args = serde_json::json!({
            "handle": "github.com/Koh0920/WasedaP2P",
            "secrets": {"PG_PASSWORD": "p", "SECRET_KEY": "s"},
        });
        let (method, params) =
            super::map_tool_to_command("set_capsule_secrets", &args).expect("map");
        assert_eq!(method, "set_capsule_secrets");
        assert_eq!(
            params.get("handle").and_then(|v| v.as_str()),
            Some("github.com/Koh0920/WasedaP2P")
        );
        assert!(
            params.get("secrets").and_then(|v| v.as_object()).is_some(),
            "secrets must be passed through as object"
        );
        // clear_pending_config is omitted on input → not forwarded; backend
        // applies its own default (true).
        assert!(
            params.get("clear_pending_config").is_none(),
            "absent flag must not be synthesised"
        );
    }

    #[test]
    fn map_tool_forwards_clear_pending_config_when_present() {
        let args = serde_json::json!({
            "handle": "h",
            "secrets": {"K": "v"},
            "clear_pending_config": false,
        });
        let (_method, params) =
            super::map_tool_to_command("set_capsule_secrets", &args).expect("map");
        assert_eq!(
            params.get("clear_pending_config").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn map_tool_set_capsule_secrets_rejects_missing_handle() {
        let args = serde_json::json!({"secrets": {"K": "v"}});
        let err = super::map_tool_to_command("set_capsule_secrets", &args).unwrap_err();
        assert!(err.contains("'handle'"), "expected handle error: {err}");
    }

    #[test]
    fn map_tool_approve_consent_passes_handle_through() {
        let args = serde_json::json!({"handle": "capsule://github.com/Koh0920/WasedaP2P"});
        let (method, params) =
            super::map_tool_to_command("approve_execution_plan_consent", &args).expect("map");
        assert_eq!(method, "approve_execution_plan_consent");
        assert_eq!(
            params.get("handle").and_then(|v| v.as_str()),
            Some("capsule://github.com/Koh0920/WasedaP2P")
        );
    }

    #[test]
    fn map_tool_approve_consent_rejects_missing_handle() {
        let args = serde_json::json!({});
        let err = super::map_tool_to_command("approve_execution_plan_consent", &args).unwrap_err();
        assert!(err.contains("'handle'"), "expected handle error: {err}");
    }

    #[test]
    fn client_timeout_stays_default_for_non_wait_commands() {
        let timeout = super::automation_client_response_timeout("snapshot", &serde_json::json!({}));
        assert_eq!(timeout, super::policy::AUTOMATION_CLIENT_RESPONSE_TIMEOUT);
    }

    #[test]
    fn client_timeout_extends_for_long_wait_for_calls() {
        let timeout = super::automation_client_response_timeout(
            "wait_for",
            &serde_json::json!({"selector": "body", "timeout": 120000}),
        );
        assert!(
            timeout >= std::time::Duration::from_secs(135),
            "long wait_for must outlive the default 45s client read timeout"
        );
    }

    #[test]
    fn tools_list_includes_stop_active_session_with_no_required_args() {
        let tools: serde_json::Value =
            serde_json::from_str(super::TOOLS).expect("TOOLS is valid JSON");
        let arr = tools.as_array().expect("array");
        let entry = arr
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("stop_active_session"))
            .expect("stop_active_session registered");
        let required = entry
            .get("inputSchema")
            .and_then(|s| s.get("required"))
            .and_then(|r| r.as_array())
            .expect("required[]");
        assert!(
            required.is_empty(),
            "stop_active_session must take no required args, got: {required:?}"
        );
    }

    #[test]
    fn tools_list_includes_restart_active_session_with_no_required_args() {
        let tools: serde_json::Value =
            serde_json::from_str(super::TOOLS).expect("TOOLS is valid JSON");
        let arr = tools.as_array().expect("array");
        let entry = arr
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("restart_active_session"))
            .expect("restart_active_session registered");
        let required = entry
            .get("inputSchema")
            .and_then(|s| s.get("required"))
            .and_then(|r| r.as_array())
            .expect("required[]");
        assert!(
            required.is_empty(),
            "restart_active_session must take no required args, got: {required:?}"
        );
    }

    #[test]
    fn tools_list_includes_host_take_screenshot_with_optional_region() {
        let tools: serde_json::Value =
            serde_json::from_str(super::TOOLS).expect("TOOLS is valid JSON");
        let arr = tools.as_array().expect("array");
        let entry = arr
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("host_take_screenshot"))
            .expect("host_take_screenshot registered");
        let required = entry
            .get("inputSchema")
            .and_then(|s| s.get("required"))
            .and_then(|r| r.as_array())
            .expect("required[]");
        assert!(
            required.is_empty(),
            "host_take_screenshot takes no required args, got: {required:?}"
        );
        let region_prop = entry
            .get("inputSchema")
            .and_then(|s| s.get("properties"))
            .and_then(|p| p.get("region"));
        assert!(region_prop.is_some(), "region property must exist");
    }

    #[test]
    fn tools_list_includes_host_dispatch_action_with_url_property() {
        let tools: serde_json::Value =
            serde_json::from_str(super::TOOLS).expect("TOOLS is valid JSON");
        let arr = tools.as_array().expect("array");
        let entry = arr
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("host_dispatch_action"))
            .expect("host_dispatch_action registered");
        let required = entry
            .get("inputSchema")
            .and_then(|s| s.get("required"))
            .and_then(|r| r.as_array())
            .expect("required[]");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"action"), "action required");
        assert!(
            !names.contains(&"url"),
            "url is required only for selected actions"
        );
        let url_prop = entry
            .get("inputSchema")
            .and_then(|s| s.get("properties"))
            .and_then(|p| p.get("url"));
        assert!(url_prop.is_some(), "url property must exist");
    }

    #[test]
    fn is_valid_region_accepts_four_int_csv() {
        assert!(super::is_valid_region("0,0,800,600"));
        assert!(super::is_valid_region("12, 34, 567, 890"));
    }

    #[test]
    fn is_valid_region_rejects_garbage() {
        assert!(!super::is_valid_region(""));
        assert!(!super::is_valid_region("0,0,800"));
        assert!(!super::is_valid_region("0,0,800,abc"));
        assert!(!super::is_valid_region("a,b,c,d"));
    }

    #[test]
    fn map_tool_stop_active_session_emits_method_with_default_pane_id() {
        let args = serde_json::json!({});
        let (method, params) =
            super::map_tool_to_command("stop_active_session", &args).expect("map");
        assert_eq!(method, "stop_active_session");
        // The mapping layer attaches the (default) pane_id for shape
        // symmetry with browser_*; transport::parse_command discards it
        // so this is harmless. Asserting it's present catches a regression
        // where a future cleanup deletes the trailing
        // `map.insert("pane_id", ...)` and breaks browser_* tools.
        assert_eq!(params.get("pane_id").and_then(|v| v.as_u64()), Some(0));
    }

    #[test]
    fn map_tool_stop_active_session_does_not_require_handle() {
        // Regression guard: keep `stop_active_session` argument-less so
        // an autonomous AODD harness can call it without first having to
        // resolve the active capsule's handle (which the desktop already
        // knows from active_pane_id → views[pane_id].launched_session).
        let result = super::map_tool_to_command("stop_active_session", &serde_json::json!({}));
        assert!(
            result.is_ok(),
            "stop_active_session must accept empty args, got: {result:?}"
        );
    }

    #[test]
    fn map_tool_restart_active_session_emits_method_with_default_pane_id() {
        let args = serde_json::json!({});
        let (method, params) =
            super::map_tool_to_command("restart_active_session", &args).expect("map");
        assert_eq!(method, "restart_active_session");
        assert_eq!(params.get("pane_id").and_then(|v| v.as_u64()), Some(0));
    }

    #[test]
    fn map_tool_restart_active_session_does_not_require_handle() {
        let result = super::map_tool_to_command("restart_active_session", &serde_json::json!({}));
        assert!(
            result.is_ok(),
            "restart_active_session must accept empty args, got: {result:?}"
        );
    }

    #[test]
    fn map_tool_host_dispatch_action_passes_installed_app_url() {
        let args = serde_json::json!({
            "action": "NavigateToUrl",
            "url": "ato://app/ipk_82508d85640a941525c49106efdd4071",
        });
        let (method, params) =
            super::map_tool_to_command("host_dispatch_action", &args).expect("map");
        assert_eq!(method, "host_dispatch_action");
        assert_eq!(
            params.get("action").and_then(|v| v.as_str()),
            Some("NavigateToUrl")
        );
        assert_eq!(
            params.get("url").and_then(|v| v.as_str()),
            Some("ato://app/ipk_82508d85640a941525c49106efdd4071")
        );
        assert_eq!(params.get("pane_id").and_then(|v| v.as_u64()), Some(0));
    }

    #[test]
    fn map_tool_host_dispatch_action_rejects_missing_url_for_navigate() {
        let err = super::map_tool_to_command(
            "host_dispatch_action",
            &serde_json::json!({"action": "NavigateToUrl"}),
        )
        .unwrap_err();
        assert!(
            err.contains("NavigateToUrl requires"),
            "expected NavigateToUrl url error: {err}"
        );
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

    #[test]
    fn auth_status_does_not_expose_token() {
        let tools: serde_json::Value =
            serde_json::from_str(super::TOOLS).expect("TOOLS is valid JSON");
        let arr = tools.as_array().expect("array");
        let entry = arr
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("auth_status"))
            .expect("auth_status registered");
        let schema = entry.get("inputSchema").expect("inputSchema");
        let props = schema.get("properties").expect("properties");
        assert!(
            props.get("token").is_none(),
            "auth_status must not expose token field"
        );
    }

    #[test]
    fn auth_status_tool_registered_with_empty_input_schema() {
        let tools: serde_json::Value =
            serde_json::from_str(super::TOOLS).expect("TOOLS is valid JSON");
        let arr = tools.as_array().expect("array");
        let entry = arr
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("auth_status"))
            .expect("auth_status registered");
        let required = entry
            .get("inputSchema")
            .and_then(|s| s.get("required"))
            .and_then(|r| r.as_array())
            .expect("required[]");
        assert!(required.is_empty(), "auth_status takes no required args");
    }

    #[test]
    fn map_auth_status_maps_to_method_with_empty_params() {
        let (method, params) =
            super::map_tool_to_command("auth_status", &serde_json::json!({})).expect("map");
        assert_eq!(method, "auth_status");
        assert!(
            params.as_object().is_some(),
            "params must be object (pane_id injected)"
        );
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

    // Host-side tools (GPUI surface AODD primitives) bypass the
    // automation socket entirely — they shell out to OS-level
    // utilities from inside the MCP process.
    if tool_name == "host_take_screenshot" {
        return handle_host_take_screenshot(id, &args);
    }
    if tool_name == "host_activate_app" {
        return handle_host_activate_app(id, &args);
    }
    if tool_name == "host_press_key" {
        return handle_host_press_key(id, &args);
    }
    if tool_name == "cleanup_host_resources" {
        return handle_cleanup_host_resources(id, &args);
    }

    let (method, rpc_params) = match map_tool_to_command(tool_name, &args) {
        Ok(pair) => pair,
        Err(msg) => {
            return mcp_error(id, &msg);
        }
    };
    let response_timeout = automation_client_response_timeout(&method, &rpc_params);

    match send_automation_command(socket_path, &method, rpc_params, response_timeout) {
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

// ── Host-surface AODD primitives ─────────────────────────────────────────────

/// `host_take_screenshot` — shells out to `screencapture` to capture
/// the GPUI host surface, which is invisible to the `browser_*` tools
/// (those only see WKWebView page content).
fn handle_host_take_screenshot(
    id: serde_json::Value,
    args: &serde_json::Value,
) -> serde_json::Value {
    let path = match aodd_screenshot_path() {
        Ok(p) => p,
        Err(e) => return mcp_error(id, &format!("failed to allocate screenshot path: {e}")),
    };

    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("screencapture");
        cmd.args(["-t", "png", "-x"]);
        if let Some(region) = args.get("region").and_then(|v| v.as_str()) {
            if !is_valid_region(region) {
                return mcp_error(
                    id,
                    &format!("invalid region '{region}': expected 'x,y,w,h' integers"),
                );
            }
            cmd.arg("-R").arg(region);
        }
        cmd.arg(&path);
        match cmd.output() {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return mcp_error(
                    id,
                    &format!(
                        "screencapture exited with status {}: {}",
                        out.status, stderr
                    ),
                );
            }
            Err(e) => return mcp_error(id, &format!("failed to spawn screencapture: {e}")),
        }
    }

    #[cfg(windows)]
    {
        let region = args.get("region").and_then(|v| v.as_str());
        let (x, y, w, h) = if let Some(region) = region {
            if !is_valid_region(region) {
                return mcp_error(
                    id,
                    &format!("invalid region '{region}': expected 'x,y,w,h' integers"),
                );
            }
            let parts: Vec<i64> = region
                .split(',')
                .map(|part| part.trim().parse::<i64>().unwrap())
                .collect();
            (parts[0], parts[1], parts[2], parts[3])
        } else {
            (0, 0, -1, -1)
        };

        let ps_path = escape_ps_single_quoted(&path.display().to_string());
        let script = if w < 0 {
            format!(
                r#"Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing;
$s=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds;
$bmp=New-Object System.Drawing.Bitmap($s.Width,$s.Height);
$g=[System.Drawing.Graphics]::FromImage($bmp);
$g.CopyFromScreen($s.Left,$s.Top,0,0,$s.Size);
$bmp.Save('{ps_path}',[System.Drawing.Imaging.ImageFormat]::Png);
$g.Dispose();$bmp.Dispose()"#
            )
        } else {
            format!(
                r#"Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing;
$bmp=New-Object System.Drawing.Bitmap({w},{h});
$g=[System.Drawing.Graphics]::FromImage($bmp);
$g.CopyFromScreen({x},{y},0,0,(New-Object System.Drawing.Size({w},{h})));
$bmp.Save('{ps_path}',[System.Drawing.Imaging.ImageFormat]::Png);
$g.Dispose();$bmp.Dispose()"#
            )
        };

        match std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
        {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return mcp_error(
                    id,
                    &format!("PowerShell screenshot failed ({}): {}", out.status, stderr),
                );
            }
            Err(e) => return mcp_error(id, &format!("failed to spawn powershell: {e}")),
        }
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        return mcp_error(id, "host_take_screenshot is not supported on this platform");
    }

    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{
                "type": "text",
                "text": serde_json::json!({
                    "ok": true,
                    "path": path.display().to_string(),
                })
                .to_string()
            }],
            "isError": false
        }
    })
}

fn aodd_screenshot_path() -> std::io::Result<PathBuf> {
    let dir = ato_path_or_workspace_tmp("aodd");
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let pid = std::process::id();
    Ok(dir.join(format!("host-{stamp}-{pid}.png")))
}

fn is_valid_region(spec: &str) -> bool {
    let parts: Vec<&str> = spec.split(',').collect();
    if parts.len() != 4 {
        return false;
    }
    parts.iter().all(|p| p.trim().parse::<i64>().is_ok())
}

/// `host_activate_app` — bring `process_name` to the foreground so
/// subsequent `host_press_key` calls land in the right app.
fn handle_host_activate_app(id: serde_json::Value, args: &serde_json::Value) -> serde_json::Value {
    let process_name = args
        .get("process_name")
        .and_then(|v| v.as_str())
        .unwrap_or("ato-desktop");
    if !is_safe_applescript_literal(process_name) {
        return mcp_error(id, "process_name contains forbidden characters");
    }

    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "tell application \"System Events\" to set frontmost of (first process whose name is \"{}\") to true",
            process_name
        );
        match std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
        {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return mcp_error(
                    id,
                    &format!(
                        "osascript failed to activate {process_name} (status {}): {}",
                        out.status, stderr
                    ),
                );
            }
            Err(e) => return mcp_error(id, &format!("failed to spawn osascript: {e}")),
        }
    }

    #[cfg(windows)]
    {
        let ps_process_name = escape_ps_single_quoted(process_name);
        let script = format!(
            "(New-Object -ComObject WScript.Shell).AppActivate((Get-Process | Where-Object {{ $_.ProcessName -eq '{ps_process_name}' }} | Select-Object -First 1 -ExpandProperty Id))",
        );
        match std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
        {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return mcp_error(
                    id,
                    &format!(
                        "failed to activate {process_name} ({}): {}",
                        out.status, stderr
                    ),
                );
            }
            Err(e) => return mcp_error(id, &format!("failed to spawn powershell: {e}")),
        }
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        return mcp_error(id, "host_activate_app is not supported on this platform");
    }

    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{
                "type": "text",
                "text": serde_json::json!({"ok": true, "process_name": process_name}).to_string()
            }],
            "isError": false
        }
    })
}

/// `host_press_key` — synthesise a keystroke to the foreground app.
/// Pair with `host_activate_app` first.
fn handle_host_press_key(id: serde_json::Value, args: &serde_json::Value) -> serde_json::Value {
    let key = match args.get("key").and_then(|v| v.as_str()) {
        Some(k) if !k.is_empty() => k,
        _ => return mcp_error(id, "missing required argument 'key'"),
    };
    let modifiers: Vec<String> = args
        .get("modifiers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    #[cfg(target_os = "macos")]
    {
        let modifier_clause = match build_modifier_clause(&modifiers) {
            Ok(s) => s,
            Err(e) => return mcp_error(id, &e),
        };

        let script = match build_keystroke_script(key, &modifier_clause) {
            Ok(s) => s,
            Err(e) => return mcp_error(id, &e),
        };

        match std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
        {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return mcp_error(
                    id,
                    &format!(
                        "osascript keystroke failed (status {}): {}",
                        out.status, stderr
                    ),
                );
            }
            Err(e) => return mcp_error(id, &format!("failed to spawn osascript: {e}")),
        }
    }

    #[cfg(windows)]
    {
        let sendkeys_key = match key {
            "Escape" | "escape" | "Esc" | "esc" => "{ESC}".to_string(),
            "Return" | "return" | "Enter" | "enter" => "{ENTER}".to_string(),
            "Tab" | "tab" => "{TAB}".to_string(),
            "Space" | "space" => " ".to_string(),
            "Backspace" | "backspace" | "Delete" | "delete" => "{BACKSPACE}".to_string(),
            "Up" | "up" | "ArrowUp" => "{UP}".to_string(),
            "Down" | "down" | "ArrowDown" => "{DOWN}".to_string(),
            "Left" | "left" | "ArrowLeft" => "{LEFT}".to_string(),
            "Right" | "right" | "ArrowRight" => "{RIGHT}".to_string(),
            k if k.len() == 1 => {
                let escaped = match k {
                    "+" | "^" | "%" | "~" | "(" | ")" | "{" | "}" | "[" | "]" => {
                        format!("{{{k}}}")
                    }
                    _ => k.to_string(),
                };
                escaped
            }
            other => return mcp_error(id, &format!("unsupported key for Windows: {other}")),
        };

        let mut prefix = String::new();
        for modifier in &modifiers {
            match modifier.to_lowercase().as_str() {
                "shift" => prefix.push('+'),
                "control" | "ctrl" => prefix.push('^'),
                "alt" | "option" => prefix.push('%'),
                "command" | "cmd" => {}
                other => return mcp_error(id, &format!("unknown modifier: {other}")),
            }
        }

        let send_sequence = format!("{prefix}{sendkeys_key}");
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait('{}')",
            send_sequence.replace('\'', "''")
        );
        match std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
        {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return mcp_error(id, &format!("SendKeys failed ({}): {}", out.status, stderr));
            }
            Err(e) => return mcp_error(id, &format!("failed to spawn powershell: {e}")),
        }
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        return mcp_error(id, "host_press_key is not supported on this platform");
    }

    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{
                "type": "text",
                "text": serde_json::json!({"ok": true, "key": key, "modifiers": modifiers}).to_string()
            }],
            "isError": false
        }
    })
}

fn build_modifier_clause(modifiers: &[String]) -> Result<String, String> {
    if modifiers.is_empty() {
        return Ok(String::new());
    }
    let mut parts = Vec::with_capacity(modifiers.len());
    for m in modifiers {
        let token = match m.to_lowercase().as_str() {
            "command" | "cmd" => "command down",
            "shift" => "shift down",
            "option" | "alt" => "option down",
            "control" | "ctrl" => "control down",
            other => return Err(format!("unknown modifier: {other}")),
        };
        parts.push(token);
    }
    Ok(format!(" using {{{}}}", parts.join(", ")))
}

/// `cleanup_host_resources` — kills stale provider processes (TERM only)
/// and frees System V shared memory owned by the current user.
fn handle_cleanup_host_resources(
    id: serde_json::Value,
    args: &serde_json::Value,
) -> serde_json::Value {
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut report = serde_json::json!({
        "dry_run": dry_run,
        "postgres_killed": 0u64,
        "bun_servers_killed": 0u64,
        "port_1111_cleared": false,
        "shm_freed": 0u64,
    });

    // Find + optionally kill postgres workers
    let postgres_pids = find_pids("postgres.*ato");
    report["postgres_found"] = serde_json::json!(postgres_pids.len() as u64);
    if !dry_run && !postgres_pids.is_empty() {
        report["postgres_killed"] = serde_json::json!(kill_pids_mcp(&postgres_pids) as u64);
    }

    // Find + optionally kill bun server processes
    let bun_pids = find_pids("bun.*dist/index");
    report["bun_servers_found"] = serde_json::json!(bun_pids.len() as u64);
    if !dry_run && !bun_pids.is_empty() {
        report["bun_servers_killed"] = serde_json::json!(kill_pids_mcp(&bun_pids) as u64);
    }

    // Find + optionally kill port 1111 listeners
    let port_pids = find_port_pids_mcp(1111);
    report["port_1111_pids"] = serde_json::json!(port_pids.len() as u64);
    if !dry_run && !port_pids.is_empty() {
        let killed = kill_pids_mcp(&port_pids);
        report["port_1111_killed"] = serde_json::json!(killed as u64);
    }

    // Count + optionally free SysV shared memory
    let shm = count_shm_mcp();
    report["shm_segments"] = serde_json::json!(shm as u64);
    if !dry_run && shm > 0 {
        report["shm_freed"] = serde_json::json!(free_shm_mcp() as u64);
    }

    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": report.to_string() }],
            "isError": false
        }
    })
}

fn find_pids(pattern: &str) -> Vec<u32> {
    #[cfg(unix)]
    {
        let output = match std::process::Command::new("pgrep")
            .arg("-f")
            .arg(pattern)
            .output()
        {
            Ok(o) => o,
            Err(_) => return Vec::new(),
        };
        if !output.status.success() {
            return Vec::new();
        }
        return String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .collect();
    }

    #[cfg(windows)]
    {
        let escaped = pattern.replace('\'', "''");
        let script = format!(
            "Get-Process | Where-Object {{ $_.ProcessName -match '{escaped}' -or ($_.Path -ne $null -and $_.Path -match '{escaped}') }} | Select-Object -ExpandProperty Id"
        );
        let output = match std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
        {
            Ok(o) => o,
            Err(_) => return Vec::new(),
        };
        if !output.status.success() {
            return Vec::new();
        }
        return String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .collect();
    }

    #[cfg(not(any(unix, windows)))]
    {
        Vec::new()
    }
}

#[cfg(unix)]
fn kill_pids_mcp(pids: &[u32]) -> usize {
    let current = std::process::id();
    let mut killed = 0usize;
    for &pid in pids {
        if pid == current {
            continue;
        }
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        killed += 1;
    }
    killed
}

#[cfg(windows)]
fn kill_pids_mcp(pids: &[u32]) -> usize {
    let current = std::process::id();
    let mut killed = 0usize;
    for &pid in pids {
        if pid == current {
            continue;
        }
        let ok = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if ok {
            killed += 1;
        }
    }
    killed
}

#[cfg(not(any(unix, windows)))]
fn kill_pids_mcp(_pids: &[u32]) -> usize {
    0
}

fn find_port_pids_mcp(port: u16) -> Vec<u32> {
    #[cfg(unix)]
    {
        let output = match std::process::Command::new("lsof")
            .arg("-ti")
            .arg(format!(":{port}"))
            .output()
        {
            Ok(o) => o,
            Err(_) => return Vec::new(),
        };
        if !output.status.success() {
            return Vec::new();
        }
        return String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .collect();
    }

    #[cfg(windows)]
    {
        let output = match std::process::Command::new("netstat")
            .args(["-ano"])
            .output()
        {
            Ok(o) => o,
            Err(_) => return Vec::new(),
        };
        let port_str = format!(":{port} ");
        return String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                line.contains(&port_str)
                    && (line.contains("LISTENING") || line.contains("ESTABLISHED"))
            })
            .filter_map(|line| line.split_whitespace().last()?.trim().parse::<u32>().ok())
            .collect::<std::collections::HashSet<u32>>()
            .into_iter()
            .collect();
    }

    #[cfg(not(any(unix, windows)))]
    {
        Vec::new()
    }
}

fn count_shm_mcp() -> usize {
    #[cfg(unix)]
    {
        let output = match std::process::Command::new("ipcs").arg("-m").output() {
            Ok(o) => o,
            Err(_) => return 0,
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let user = std::env::var("USER").unwrap_or_default();
        return text.lines().filter(|line| line.contains(&user)).count();
    }

    #[cfg(not(unix))]
    {
        0
    }
}

fn free_shm_mcp() -> usize {
    #[cfg(unix)]
    {
        let output = match std::process::Command::new("ipcs").arg("-m").output() {
            Ok(o) => o,
            Err(_) => return 0,
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let user = std::env::var("USER").unwrap_or_default();
        let mut freed = 0usize;
        for line in text.lines() {
            if !line.contains(&user) {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(id_str) = parts.get(1) {
                if let Ok(id) = id_str.parse::<u32>() {
                    let _ = std::process::Command::new("ipcrm")
                        .arg("-m")
                        .arg(id.to_string())
                        .output();
                    freed += 1;
                }
            }
        }
        return freed;
    }

    #[cfg(not(unix))]
    {
        0
    }
}

/// Map either a single character or a named key to the AppleScript
/// statement that produces it. `keystroke` works for printable
/// characters; `key code` is required for non-printable keys
/// (Escape, Return, Tab, Space, arrows).
fn build_keystroke_script(key: &str, modifier_clause: &str) -> Result<String, String> {
    let named: Option<u32> = match key {
        "Escape" | "escape" | "Esc" | "esc" => Some(53),
        "Return" | "return" | "Enter" | "enter" => Some(36),
        "Tab" | "tab" => Some(48),
        "Space" | "space" => Some(49),
        "Backspace" | "backspace" | "Delete" | "delete" => Some(51),
        "Up" | "up" | "ArrowUp" => Some(126),
        "Down" | "down" | "ArrowDown" => Some(125),
        "Left" | "left" | "ArrowLeft" => Some(123),
        "Right" | "right" | "ArrowRight" => Some(124),
        _ => None,
    };
    if let Some(code) = named {
        let clause = if modifier_clause.is_empty() {
            String::new()
        } else {
            modifier_clause.to_string()
        };
        return Ok(format!(
            "tell application \"System Events\" to key code {code}{clause}"
        ));
    }
    // Printable character path: keystroke "<char>" using {modifiers}.
    if key.chars().count() == 0 {
        return Err("empty key".into());
    }
    if !is_safe_applescript_literal(key) {
        return Err("key contains forbidden characters".into());
    }
    Ok(format!(
        "tell application \"System Events\" to keystroke \"{key}\"{modifier_clause}"
    ))
}

/// Reject backslashes, quotes, and newlines that would break out of
/// the AppleScript string literal we embed into.
fn is_safe_applescript_literal(s: &str) -> bool {
    !s.chars().any(|c| matches!(c, '"' | '\\' | '\n' | '\r'))
}

/// Escapes a string for safe embedding inside a PowerShell single-quoted literal.
/// In PS single-quoted strings, only `'` is special and must be doubled as `''`.
fn escape_ps_single_quoted(s: &str) -> String {
    s.replace('\'', "''")
}

fn automation_client_response_timeout(
    method: &str,
    params: &serde_json::Value,
) -> std::time::Duration {
    use std::cmp::max;
    use std::time::Duration;

    if method == "wait_for" {
        let timeout_ms = params
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(5000);
        return max(
            policy::AUTOMATION_CLIENT_RESPONSE_TIMEOUT,
            Duration::from_millis(timeout_ms)
                .saturating_add(policy::AUTOMATION_CONNECTION_IO_TIMEOUT)
                .saturating_add(policy::AUTOMATION_CLIENT_WRITE_TIMEOUT),
        );
    }

    policy::AUTOMATION_CLIENT_RESPONSE_TIMEOUT
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
        "browser_click_at" => {
            let x = args
                .get("x")
                .and_then(|v| v.as_f64())
                .ok_or("missing required argument 'x'")?;
            let y = args
                .get("y")
                .and_then(|v| v.as_f64())
                .ok_or("missing required argument 'y'")?;
            ("click_at", serde_json::json!({ "x": x, "y": y }))
        }
        "browser_close_tab" => {
            let pane_id = args
                .get("pane_id")
                .and_then(|v| v.as_u64())
                .ok_or("missing required argument 'pane_id'")?;
            ("close_pane", serde_json::json!({ "pane_id": pane_id }))
        }
        "set_capsule_secrets" => {
            let handle = s("handle")?;
            let secrets = args
                .get("secrets")
                .cloned()
                .ok_or("missing required argument 'secrets'")?;
            let mut params = serde_json::json!({
                "handle": handle,
                "secrets": secrets,
            });
            // Pass through `clear_pending_config` only when the caller set it;
            // the backend defaults to true.
            if let Some(flag) = args.get("clear_pending_config") {
                if let serde_json::Value::Object(ref mut map) = params {
                    map.insert("clear_pending_config".into(), flag.clone());
                }
            }
            ("set_capsule_secrets", params)
        }
        "approve_execution_plan_consent" => {
            let handle = s("handle")?;
            (
                "approve_execution_plan_consent",
                serde_json::json!({ "handle": handle }),
            )
        }
        // No required arguments — the unit variant always targets the
        // active pane on the desktop side, mirroring Cmd+Shift+W and the
        // omnibar "stop session" suggestion. We still attach the
        // (default) `pane_id` below for shape-symmetry with browser_*,
        // but `transport::parse_command` discards it.
        "auth_status" => ("auth_status", serde_json::json!({})),
        "stop_active_session" => ("stop_active_session", serde_json::json!({})),
        "restart_active_session" => ("restart_active_session", serde_json::json!({})),
        "host_dispatch_action" => {
            let action = s("action")?;
            let url = args["url"].as_str().map(|s| s.to_string());
            // Validate against all known host actions so the MCP caller
            // gets an immediate error instead of a silent no-op.
            const KNOWN_ACTIONS: &[&str] = &[
                "BrokerNegativeTest",
                "CloseAppWindow",
                "CompleteOnboarding",
                "FocusControlBarInput",
                "ForceApprovePending",
                "GithubRunFindCandidates",
                "GithubRunProceedToConsent",
                "NavigateToTestCapsule",
                "NavigateToTestHttp",
                "NavigateToUrl",
                "OpenAppWindowExperiment",
                "OpenCapsuleSettingsDemo",
                "OpenCardSwitcher",
                "OpenDockWindow",
                "OpenIdentityMenu",
                "OpenImportBlinko",
                "OpenLaunchBoot",
                "OpenLaunchConsent",
                "OpenLaunchConsentConfigPanel",
                "OpenStartWindow",
                "OpenGithubRunWindow",
                "OpenStoreWindow",
                "RunImportBlinko",
                "CheckImportState",
                "ScrollLaunchConsentConfigPanelBottom",
                "ShowAtoHome",
                "ShowSettings",
                "SkipOnboarding",
            ];
            if !KNOWN_ACTIONS.contains(&action.as_str()) {
                return Err(format!(
                    "unknown action '{}'; valid actions: {:?}",
                    action, KNOWN_ACTIONS,
                ));
            }
            if action == "NavigateToUrl" && url.is_none() {
                return Err("NavigateToUrl requires a `url` parameter".into());
            }
            if action == "GithubRunFindCandidates" && url.is_none() {
                return Err(
                    "GithubRunFindCandidates requires a `url` parameter (owner/repo or GitHub URL)"
                        .into(),
                );
            }
            if action == "GithubRunProceedToConsent" && url.is_none() {
                return Err("GithubRunProceedToConsent requires a `url` parameter (owner/repo or GitHub URL)".into());
            }
            let mut params = serde_json::json!({ "action": action });
            if let Some(ref u) = url {
                params["url"] = serde_json::Value::String(u.clone());
            }
            ("host_dispatch_action", params)
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
    response_timeout: std::time::Duration,
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
        _ => format!(
            "cannot connect to ato-desktop socket {}: {e}",
            socket_path.display()
        ),
    })?;
    stream.set_read_timeout(Some(response_timeout)).ok();
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

#[cfg(windows)]
fn send_automation_command(
    socket_path: &Path,
    method: &str,
    params: serde_json::Value,
    response_timeout: std::time::Duration,
) -> Result<serde_json::Value, String> {
    use std::io::{BufRead as _, Write};
    use std::time::Instant;

    let deadline = Instant::now() + response_timeout;
    let mut file = loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(socket_path)
        {
            Ok(file) => break file,
            Err(error) if error.raw_os_error() == Some(231) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "named pipe {} is busy (all instances occupied)",
                        socket_path.display()
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "no ato-desktop instance is running (no pipe at {})",
                    socket_path.display()
                ));
            }
            Err(error) => {
                return Err(format!(
                    "cannot connect to ato-desktop pipe {}: {error}",
                    socket_path.display()
                ));
            }
        }
    };

    let rpc = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    });
    let mut line = serde_json::to_string(&rpc).unwrap();
    line.push('\n');
    file.write_all(line.as_bytes())
        .map_err(|e| format!("send failed: {e}"))?;

    // `std::fs::File` (named pipe) has no `set_read_timeout`. Bound the
    // receive with a helper thread + channel timeout instead.
    let timeout_remaining = deadline.saturating_duration_since(Instant::now());
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    std::thread::spawn(move || {
        let mut response_line = String::new();
        let result = std::io::BufReader::new(file)
            .read_line(&mut response_line)
            .map(|_| response_line)
            .map_err(|e| e.to_string());
        let _ = tx.send(result);
    });
    let response_line = match rx.recv_timeout(timeout_remaining) {
        Ok(Ok(line)) => line,
        Ok(Err(e)) => return Err(format!("receive failed: {e}")),
        Err(_) => {
            return Err(format!(
                "timed out waiting for ato-desktop response ({}ms)",
                response_timeout.as_millis()
            ));
        }
    };

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

#[cfg(not(any(unix, windows)))]
fn send_automation_command(
    _socket_path: &Path,
    _method: &str,
    _params: serde_json::Value,
    _response_timeout: std::time::Duration,
) -> Result<serde_json::Value, String> {
    Err("automation socket transport is not supported on this platform".into())
}

// ── Tool definitions ──────────────────────────────────────────────────────────

static TOOLS: &str = r#"[
  {"name":"browser_snapshot","description":"Returns an accessibility tree snapshot of the active WebView page.","inputSchema":{"type":"object","properties":{"pane_id":{"type":"integer","description":"Target pane ID (0 = active pane)"}},"required":[]}},
  {"name":"browser_take_screenshot","description":"Takes a PNG screenshot of the active WebView (macOS via WKWebView snapshotting; Windows via WebView2 CapturePreview).","inputSchema":{"type":"object","properties":{"pane_id":{"type":"integer"}},"required":[]}},
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
  {"name":"browser_tab_focus","description":"Focuses a specific WebView pane by ID.","inputSchema":{"type":"object","properties":{"pane_id":{"type":"integer"}},"required":["pane_id"]}},
  {"name":"browser_click_at","description":"Clicks at a specific (x, y) coordinate in the active WebView.","inputSchema":{"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"},"pane_id":{"type":"integer"}},"required":["x","y"]}},
  {"name":"browser_close_tab","description":"Closes a WebView pane by ID.","inputSchema":{"type":"object","properties":{"pane_id":{"type":"integer"}},"required":["pane_id"]}},
  {"name":"set_capsule_secrets","description":"Persist one or more secrets for a capsule handle, grant them to that handle, and (default) dismiss any open `missing_required_env` (E103) modal so the launch re-arms with the freshly stored secrets. Mirrors the modal Save handler — disk-write failures (e.g. ~/.ato/secrets.json mode/parent-dir errors) are returned as MCP errors instead of being silently swallowed.","inputSchema":{"type":"object","properties":{"handle":{"type":"string","description":"Capsule handle as it appears in pending_config / launch state (e.g. 'github.com/Koh0920/WasedaP2P')."},"secrets":{"type":"object","description":"Map of env-var-name → secret value (strings only).","additionalProperties":{"type":"string"}},"clear_pending_config":{"type":"boolean","description":"If true (default), clears AppState.pending_config when its handle matches, re-arming the launch."}},"required":["handle","secrets"]}},
  {"name":"approve_execution_plan_consent","description":"Approve the open ExecutionPlan consent modal for `handle`. Goes through the same handler as the UI's Approve button — `apply_capsule_consent` invokes `ato internal consent approve-execution-plan` (CLI owns the JSONL append; desktop never writes the consent file directly), records the per-handle retry-once budget, and clears `pending_consent` so `ensure_pending_local_launch` re-arms the launch on the next render. Errors surface as MCP errors when no matching pending_consent exists or the CLI write fails — the modal is left open so the caller can retry.","inputSchema":{"type":"object","properties":{"handle":{"type":"string","description":"Capsule handle as it appears in pending_consent (the same handle the user typed in the omnibar / ato-desktop opened, e.g. 'capsule://github.com/Koh0920/WasedaP2P')."}},"required":["handle"]}},
  {"name":"stop_active_session","description":"Stop the active pane's underlying capsule session, mirroring the `Cmd+Shift+W` keybind and the omnibar 'stop session' suggestion. Routes through `WebViewManager::stop_active_session` — the same method `DesktopShell::on_stop_active_session` dispatches — so providers (postgres, etc.) and consumers (uvicorn / vite / ...) shut down via `ato app session stop` exactly as a UI-initiated stop would. Returns `{ok:true, stopped, had_active_session, session_id, handle}`; `stopped:false` with `had_active_session:false` means there was nothing to stop (idempotent), `stopped:false` with `had_active_session:true` means the underlying `stop_guest_session` returned a non-success outcome and the caller should inspect ports/processes (refs #92 AC-step 6).","inputSchema":{"type":"object","properties":{},"required":[]}},
  {"name":"restart_active_session","description":"Stop and restart the active Focus-mode capsule session with the same route and launch configs. Restricted to CapsuleHandle/CapsuleUrl routes. The root Focus View window stays open. Returns `{ok:true, restarted, had_active_session, session_id, handle}`; `restarted:false` with `had_active_session:false` means nothing was running (idempotent). If stop fails, reopen is skipped and an error is returned.","inputSchema":{"type":"object","properties":{},"required":[]}},
  {"name":"auth_status","description":"Returns the current ato-desktop sign-in state for AODD agents. Does NOT expose the session token.","inputSchema":{"type":"object","properties":{},"required":[]}},
  {"name":"host_take_screenshot","description":"Captures the full display as PNG on macOS (via `screencapture`) and Windows (via PowerShell + .NET Graphics), writing it under `${ATO_HOME:-~/.ato}/aodd/`. Returns `{ok:true, path:'<abs>'}`. This is the AODD visual-inspection primitive for ato-desktop's GPUI host surfaces, which are not reachable via the `browser_*` tools because those target embedded WebView content only.","inputSchema":{"type":"object","properties":{"region":{"type":"string","description":"Optional region in 'x,y,w,h' format. When omitted captures the whole main display."}},"required":[]}},
  {"name":"host_activate_app","description":"Brings an application to the foreground on macOS (via osascript + System Events) and Windows (via PowerShell AppActivate). Required before `host_press_key` so keystrokes route to ato-desktop, not whatever else has focus. Returns `{ok:true}` on success.","inputSchema":{"type":"object","properties":{"process_name":{"type":"string","description":"Process name as shown in Activity Monitor or Task Manager (default: 'ato-desktop')."}},"required":[]}},
  {"name":"host_press_key","description":"Sends a keyboard event to the currently focused application on macOS (osascript/System Events) and Windows (PowerShell SendKeys). Pair with `host_activate_app` first. `key` accepts a single character (e.g. 'n', '1') or a named key ('Escape', 'Return', 'Tab', 'Space'). `modifiers` is an array drawn from ['command','shift','option','control'] (Windows ignores command). Returns `{ok:true}` on success.","inputSchema":{"type":"object","properties":{"key":{"type":"string","description":"Single character or named key."},"modifiers":{"type":"array","items":{"type":"string"},"description":"Modifier keys to hold."}},"required":["key"]}},
  {"name":"cleanup_host_resources","description":"Preview and clean up stale provider processes (postgres, bun) owned by the current user. On Unix also removes System V shared memory segments; on Unix sends SIGTERM (no SIGKILL), on Windows uses `taskkill /F` (force-kill, no graceful shutdown). Set dry_run=true to only preview without killing.","inputSchema":{"type":"object","properties":{"dry_run":{"type":"boolean","description":"If true, only preview without killing (default: false)"}},"required":[]}},
  {"name":"host_dispatch_action","description":"Queues a host-level GPUI action by name onto the ato-desktop automation socket. Valid actions: 'NavigateToUrl' (requires `url` parameter), 'ForceApprovePending', 'FocusControlBarInput', 'OpenStartWindow', 'OpenStoreWindow', 'OpenCardSwitcher', 'CompleteOnboarding', 'SkipOnboarding', 'ShowSettings', and others. The desktop drains the queue on its next render pass and invokes the matching handler in-process. Returns `{ok:true, queued_action:'<name>'}` when queued. For `NavigateToUrl` with `ato://app/<install_profile_key>`, invalid or degraded installed profiles return `{ok:false, action:'NavigateToUrl', url, reason, detail?}` and are not queued.","inputSchema":{"type":"object","properties":{"action":{"type":"string","description":"Action name to dispatch."},"url":{"type":"string","description":"URL parameter required for NavigateToUrl action (e.g. 'capsule://github.com/Koh0920/hello-capsule' or 'ato://app/ipk_...')."}},"required":["action"]}}
]"#;

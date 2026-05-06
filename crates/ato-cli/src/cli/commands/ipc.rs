//! `ato ipc` subcommand — IPC service management.
//!
//! ## Subcommands
//!
//! - `ato ipc status` — List running IPC services and their status.
//! - `ato ipc start`  — Start an IPC service from a capsule directory.
//! - `ato ipc stop`   — Stop a running IPC service by name.
//! - `ato ipc invoke` — Validate and send a JSON-RPC invoke request.

#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::Shutdown;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use capsule_core::execution_plan::error::AtoExecutionError;
use serde_json::Value;

use crate::ipc::broker::IpcBroker;
use crate::ipc::jsonrpc::{InvokeParams, JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use crate::ipc::types::{IpcMethodDescriptor, IpcRuntimeKind, IpcServiceInfo, IpcTransport};

/// Run `ato ipc status`.
///
/// Displays a table of all running IPC services with their:
/// - Name, sharing mode, reference count
/// - Transport, endpoint, runtime
/// - Uptime, PID
pub fn run_ipc_status(json_output: bool) -> Result<()> {
    // Create a broker pointing to the default socket directory
    let socket_dir = default_socket_dir();
    let broker = IpcBroker::new(socket_dir);

    let snapshot = broker.registry.status_snapshot();

    if json_output {
        let json = serde_json::to_string_pretty(&snapshot)?;
        println!("{}", json);
        return Ok(());
    }

    if snapshot.is_empty() {
        println!("No IPC services running.");
        println!();
        println!("Hint: Run a capsule with [ipc.exports] in its capsule.toml to start a service.");
        return Ok(());
    }

    // Table header
    println!(
        "{:<20} {:<12} {:<10} {:<10} {:<30} {:<8} {:<10}",
        "SERVICE", "MODE", "REFCOUNT", "TRANSPORT", "ENDPOINT", "RUNTIME", "UPTIME"
    );
    println!("{}", "-".repeat(100));

    for svc in &snapshot {
        let uptime = format_uptime(svc.uptime_secs);
        let mode = format!("{:?}", svc.mode).to_lowercase();

        println!(
            "{:<20} {:<12} {:<10} {:<10} {:<30} {:<8} {:<10}",
            svc.name,
            mode,
            svc.ref_count,
            svc.transport,
            truncate(&svc.endpoint, 28),
            format!("{}", svc.runtime),
            uptime,
        );
    }

    println!();
    println!("{} service(s) running.", snapshot.len());

    Ok(())
}

/// Run `ato ipc start`.
///
/// Starts an IPC service from a capsule directory. The capsule must have
/// `[ipc.exports]` configured in its `capsule.toml`.
pub fn run_ipc_start(path: PathBuf, json_output: bool) -> Result<()> {
    let capsule_root = if path.is_file() {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        path.clone()
    };

    let manifest_path = capsule_root.join("capsule.toml");
    if !manifest_path.exists() {
        anyhow::bail!(
            "capsule.toml not found at {}. Provide a capsule directory with [ipc.exports].",
            manifest_path.display()
        );
    }

    // Parse [ipc] section from capsule.toml
    let raw_text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let raw: toml::Value = toml::from_str(&raw_text)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
    let diagnostics =
        crate::ipc::validate::validate_manifest(&raw, &capsule_root).map_err(|err| {
            AtoExecutionError::execution_contract_invalid(
                format!("IPC validation failed: {err}"),
                None,
                None,
            )
        })?;
    if crate::ipc::validate::has_errors(&diagnostics) {
        return Err(AtoExecutionError::execution_contract_invalid(
            crate::ipc::validate::format_diagnostics(&diagnostics),
            None,
            None,
        )
        .into());
    }

    let ipc_config = parse_ipc_section(&raw)?;

    let service_name = ipc_config
        .as_ref()
        .and_then(|c| c.exports.as_ref())
        .and_then(|e| e.name.clone())
        .unwrap_or_else(|| {
            capsule_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unnamed")
                .to_string()
        });

    let socket_dir = default_socket_dir();
    let broker = IpcBroker::new(socket_dir);

    // Check if already running
    if broker.registry.lookup(&service_name).is_some() {
        if json_output {
            println!(
                "{}",
                serde_json::json!({"error": "already_running", "service": service_name})
            );
        } else {
            println!(
                "⚠️  Service '{}' is already running. Use `ato ipc stop --name {}` first.",
                service_name, service_name
            );
        }
        return Ok(());
    }

    // Register the service (actual process launch delegated to broker/executors)
    let socket_path = broker.socket_path(&service_name);
    let info = IpcServiceInfo {
        name: service_name.clone(),
        pid: None, // Will be set after process spawn
        endpoint: IpcTransport::UnixSocket(socket_path),
        capabilities: ipc_config
            .as_ref()
            .and_then(|c| c.exports.as_ref())
            .map(|e| e.methods.iter().map(|m| m.name.clone()).collect())
            .unwrap_or_default(),
        ref_count: 0,
        started_at: Some(Instant::now()),
        runtime_kind: IpcRuntimeKind::Source,
        sharing_mode: ipc_config
            .as_ref()
            .and_then(|c| c.exports.as_ref())
            .map(|e| e.sharing.mode)
            .unwrap_or_default(),
    };
    broker.registry.register(info);

    if json_output {
        println!(
            "{}",
            serde_json::json!({
                "status": "registered",
                "service": service_name,
                "capsule_root": capsule_root.display().to_string(),
                "warnings": diagnostics
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>(),
            })
        );
    } else {
        println!("🚀 IPC service '{}' registered.", service_name);
        println!("   Capsule: {}", capsule_root.display());
        println!("   Note: Full process launch requires `ato run` with IPC integration.");
        for diagnostic in diagnostics {
            println!("   {}", diagnostic.to_string().replace('\n', "\n   "));
        }
    }

    Ok(())
}

/// Run `ato ipc stop`.
///
/// Stops a running IPC service by name.
pub fn run_ipc_stop(name: String, force: bool, json_output: bool) -> Result<()> {
    let socket_dir = default_socket_dir();
    let broker = IpcBroker::new(socket_dir);

    let info = broker.registry.lookup(&name);
    if info.is_none() {
        if json_output {
            println!(
                "{}",
                serde_json::json!({"error": "not_found", "service": name})
            );
        } else {
            eprintln!("❌ Service '{}' is not running.", name);
            eprintln!("   Use `ato ipc status` to list running services.");
        }
        return Ok(());
    }

    let info = info.unwrap();

    // Send signal to process if it has a PID
    if let Some(pid) = info.pid {
        #[cfg(unix)]
        {
            let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
            let signal_name = if force { "SIGKILL" } else { "SIGTERM" };

            let ret = unsafe { libc::kill(pid as i32, signal) };
            if ret != 0 {
                let errno = std::io::Error::last_os_error();
                if json_output {
                    println!(
                        "{}",
                        serde_json::json!({
                            "warning": "signal_failed",
                            "service": name,
                            "pid": pid,
                            "error": errno.to_string(),
                        })
                    );
                } else {
                    eprintln!(
                        "⚠️  Failed to send {} to PID {}: {}",
                        signal_name, pid, errno
                    );
                }
            }
        }

        #[cfg(windows)]
        {
            let mut command = std::process::Command::new("taskkill");
            command.arg("/PID").arg(pid.to_string());
            if force {
                command.arg("/F");
            }

            let status = command.status();
            match status {
                Ok(code) if code.success() => {}
                Ok(code) => {
                    if json_output {
                        println!(
                            "{}",
                            serde_json::json!({
                                "warning": "signal_failed",
                                "service": name,
                                "pid": pid,
                                "error": format!("taskkill exited with {}", code),
                            })
                        );
                    } else {
                        eprintln!(
                            "⚠️  Failed to stop PID {} (taskkill exited with {}).",
                            pid, code
                        );
                    }
                }
                Err(err) => {
                    if json_output {
                        println!(
                            "{}",
                            serde_json::json!({
                                "warning": "signal_failed",
                                "service": name,
                                "pid": pid,
                                "error": err.to_string(),
                            })
                        );
                    } else {
                        eprintln!("⚠️  Failed to run taskkill for PID {}: {}", pid, err);
                    }
                }
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = (pid, force);
        }
    }

    // Remove from registry
    broker.registry.unregister(&name);

    // Clean up socket file
    let socket_path = broker.socket_path(&name);
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    if json_output {
        println!(
            "{}",
            serde_json::json!({"status": "stopped", "service": name})
        );
    } else {
        println!("⏹  Service '{}' stopped.", name);
    }

    Ok(())
}

/// Run `ato ipc invoke`.
///
/// Performs JSON-RPC request validation, input schema validation, and message
/// size enforcement before attempting transport.
pub fn run_ipc_invoke(
    path: PathBuf,
    service: Option<String>,
    method: String,
    args: String,
    id: String,
    max_message_size: Option<usize>,
    json_output: bool,
) -> Result<()> {
    let request_id = Value::String(id);
    let target = load_invoke_target(&path, service.as_deref(), &method)?;
    let args_value: Value = match serde_json::from_str(&args) {
        Ok(value) => value,
        Err(err) => exit_with_invoke_error(
            request_id.clone(),
            JsonRpcError::invalid_params(
                &format!("Invalid JSON for --args: {}", err),
                "Pass a valid JSON object/string to --args.",
            ),
            json_output,
        ),
    };

    if let Some(schema_path) = &target.method.input_schema {
        if let Err(err) =
            crate::ipc::schema::validate_input(schema_path, &target.capsule_root, &args_value)
        {
            exit_with_invoke_error(
                request_id,
                JsonRpcError::from_schema_error(&err),
                json_output,
            );
        }
    }

    let broker = IpcBroker::new(default_socket_dir());
    let token = broker
        .token_manager
        .generate(target.capabilities.clone())
        .value;
    let params = InvokeParams {
        service: target.service_name.clone(),
        method,
        token,
        args: args_value,
    };
    let request = JsonRpcRequest::new(
        "capsule/invoke",
        Some(serde_json::to_value(&params)?),
        request_id.clone(),
    );

    if let Err(err) = request.validate() {
        exit_with_invoke_error(request_id, err, json_output);
    }

    let request_bytes = serde_json::to_vec(&request)?;
    if let Err(err) = crate::ipc::schema::check_message_size(
        &request_bytes,
        max_message_size.unwrap_or(crate::ipc::schema::DEFAULT_MAX_MESSAGE_SIZE),
    ) {
        exit_with_invoke_error(
            request_id,
            JsonRpcError::from_schema_error(&err),
            json_output,
        );
    }

    #[cfg(unix)]
    {
        let response: JsonRpcResponse = match &target.endpoint {
            IpcTransport::UnixSocket(socket_path) => invoke_over_unix_socket(socket_path, &request)
                .unwrap_or_else(|err| exit_with_invoke_error(request_id.clone(), err, json_output)),
            transport => exit_with_invoke_error(
                request_id,
                JsonRpcError::service_unavailable(&format!(
                    "Transport '{}' is not supported by `ato ipc invoke` yet.",
                    transport.endpoint_display()
                )),
                json_output,
            ),
        };

        if let Some(error) = response.error.clone() {
            exit_with_invoke_error(response.id.clone(), error, json_output);
        }

        if json_output {
            println!("{}", serde_json::to_string_pretty(&response)?);
        } else if let Some(result) = response.result {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!("{}", serde_json::to_string_pretty(&response)?);
        }

        Ok(())
    }

    #[cfg(not(unix))]
    {
        match &target.endpoint {
            IpcTransport::UnixSocket(_) => exit_with_invoke_error(
                request_id,
                JsonRpcError::service_unavailable(
                    "Unix socket transport is unavailable on this platform.",
                ),
                json_output,
            ),
            transport => exit_with_invoke_error(
                request_id,
                JsonRpcError::service_unavailable(&format!(
                    "Transport '{}' is not supported by `ato ipc invoke` yet.",
                    transport.endpoint_display()
                )),
                json_output,
            ),
        }
    }
}

/// Parse the `[ipc]` section from a raw TOML value.
fn parse_ipc_section(raw: &toml::Value) -> Result<Option<crate::ipc::types::IpcConfig>> {
    if let Some(ipc_table) = raw.get("ipc") {
        let ipc_str = toml::to_string(ipc_table).context("Failed to serialize [ipc] section")?;
        let config: crate::ipc::types::IpcConfig =
            toml::from_str(&ipc_str).context("Failed to parse [ipc] section")?;
        Ok(Some(config))
    } else {
        Ok(None)
    }
}

/// Format uptime seconds into a human-readable string.
fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Truncate a string to max length with ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

/// Default IPC socket directory.
fn default_socket_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ATO_SOCKET_DIR") {
        return PathBuf::from(dir);
    }
    capsule_core::common::paths::ato_path_or_workspace_tmp("run/capsule-ipc")
}

#[derive(Debug, Clone)]
struct InvokeTarget {
    capsule_root: PathBuf,
    service_name: String,
    endpoint: IpcTransport,
    capabilities: Vec<String>,
    method: IpcMethodDescriptor,
}

fn load_invoke_target(
    path: &Path,
    service_override: Option<&str>,
    method_name: &str,
) -> Result<InvokeTarget> {
    let manifest_path = if path.is_file() {
        path.to_path_buf()
    } else {
        path.join("capsule.toml")
    };
    if !manifest_path.exists() {
        anyhow::bail!(
            "capsule.toml not found at {}. Pass a capsule directory or manifest path.",
            manifest_path.display()
        );
    }

    let raw_text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let raw: toml::Value = toml::from_str(&raw_text)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
    let ipc_config = parse_ipc_section(&raw)?.ok_or_else(|| {
        anyhow::anyhow!("`ato ipc invoke` requires [ipc.exports] in capsule.toml")
    })?;
    let exports = ipc_config.exports.ok_or_else(|| {
        anyhow::anyhow!("`ato ipc invoke` requires [ipc.exports] in capsule.toml")
    })?;

    let service_name = service_override
        .map(ToOwned::to_owned)
        .or(exports.name.clone())
        .or_else(|| {
            manifest_path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "unnamed".to_string());

    let method = exports
        .methods
        .iter()
        .find(|descriptor| descriptor.name == method_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!(JsonRpcError::method_not_found(method_name).message))?;

    let broker = IpcBroker::new(default_socket_dir());
    let endpoint = broker
        .registry
        .lookup(&service_name)
        .map(|info| info.endpoint)
        .unwrap_or_else(|| IpcTransport::UnixSocket(broker.socket_path(&service_name)));
    let capsule_root = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    Ok(InvokeTarget {
        capsule_root,
        service_name,
        endpoint,
        capabilities: exports.methods.into_iter().map(|m| m.name).collect(),
        method,
    })
}

#[cfg(unix)]
fn invoke_over_unix_socket(
    socket_path: &Path,
    request: &JsonRpcRequest,
) -> std::result::Result<JsonRpcResponse, JsonRpcError> {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    if !socket_path.exists() {
        return Err(JsonRpcError::service_unavailable(&format!(
            "Socket not found at {}",
            socket_path.display()
        )));
    }

    let mut stream = UnixStream::connect(socket_path).map_err(|err| {
        JsonRpcError::service_unavailable(&format!(
            "Failed to connect to {}: {}",
            socket_path.display(),
            err
        ))
    })?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let payload = serde_json::to_vec(request).map_err(|err| {
        JsonRpcError::invalid_params(
            &format!("Failed to serialize JSON-RPC request: {}", err),
            "Check that request parameters are JSON-serializable.",
        )
    })?;

    stream.write_all(&payload).map_err(|err| {
        JsonRpcError::service_unavailable(&format!(
            "Failed to write request to {}: {}",
            socket_path.display(),
            err
        ))
    })?;
    stream.write_all(b"\n").map_err(|err| {
        JsonRpcError::service_unavailable(&format!(
            "Failed to finalize request to {}: {}",
            socket_path.display(),
            err
        ))
    })?;
    let _ = stream.shutdown(Shutdown::Write);

    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|err| {
        JsonRpcError::service_unavailable(&format!(
            "Failed to read response from {}: {}",
            socket_path.display(),
            err
        ))
    })?;

    if response.trim().is_empty() {
        return Err(JsonRpcError::service_unavailable(
            "Service closed the connection without a JSON-RPC response.",
        ));
    }

    serde_json::from_str(response.trim()).map_err(|err| {
        JsonRpcError::service_unavailable(&format!(
            "Service returned an invalid JSON-RPC payload: {}",
            err
        ))
    })
}

fn exit_with_invoke_error(id: Value, error: JsonRpcError, json_output: bool) -> ! {
    let response = JsonRpcResponse::error(id, error.clone());
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&response)
                .unwrap_or_else(|_| "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32603,\"message\":\"Failed to render JSON-RPC error\"},\"id\":null}".to_string())
        );
    } else {
        eprintln!("capsule/invoke failed ({}): {}", error.code, error.message);
        if let Some(data) = error.data {
            eprintln!("hint: {}", data.hint);
        }
    }
    std::process::exit(1);
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_uptime_seconds() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(30), "30s");
    }

    #[test]
    fn test_format_uptime_minutes() {
        assert_eq!(format_uptime(90), "1m30s");
        assert_eq!(format_uptime(300), "5m0s");
    }

    #[test]
    fn test_format_uptime_hours() {
        assert_eq!(format_uptime(3661), "1h1m");
        assert_eq!(format_uptime(7200), "2h0m");
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let long = "unix:///tmp/capsule-ipc/greeter-service.sock";
        let truncated = truncate(long, 20);
        assert!(truncated.len() <= 20);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn test_default_socket_dir() {
        let dir = default_socket_dir();
        let s = dir.to_str().unwrap();
        // Must end under .ato/run/capsule-ipc (or ATO_SOCKET_DIR override)
        assert!(
            s.contains("capsule-ipc"),
            "expected capsule-ipc in path, got: {s}"
        );
    }
}

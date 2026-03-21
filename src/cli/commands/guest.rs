use crate::guest_protocol::{
    decode_payload_base64, encode_payload_base64, GuestAction, GuestContextRole, GuestError,
    GuestErrorCode, GuestMode, GuestPermission, GuestRequest, GuestResponse,
    GUEST_PROTOCOL_VERSION,
};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use wasi_common::pipe::{ReadPipe, WritePipe};
use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimitsBuilder};
use wasmtime_wasi::sync::{ambient_authority, Dir, WasiCtxBuilder};
use zip::{write::FileOptions, ZipArchive, ZipWriter};

struct WasiState {
    wasi: wasmtime_wasi::WasiCtx,
    limits: wasmtime::StoreLimits,
}

#[derive(Clone, serde::Deserialize)]
struct GuestManifest {
    #[serde(default)]
    policy: GuestManifestPolicy,
    #[serde(default)]
    permissions: GuestManifestPermissions,
    #[serde(default)]
    ownership: GuestManifestOwnership,
}

#[derive(Clone, Default, serde::Deserialize)]
struct GuestManifestPolicy {
    #[serde(default = "default_policy_timeout")]
    timeout: u64,
}

#[derive(Clone, Default, serde::Deserialize)]
struct GuestManifestPermissions {
    #[serde(default)]
    allow_hosts: Vec<String>,
    #[serde(default)]
    allow_env: Vec<String>,
}

#[derive(Clone, Default, serde::Deserialize)]
struct GuestManifestOwnership {
    #[serde(default)]
    write_allowed: bool,
}

fn default_policy_timeout() -> u64 {
    30
}

pub struct GuestArgs {
    pub sync_path: PathBuf,
}

pub fn execute(args: GuestArgs) -> Result<()> {
    let mut stdin = String::new();
    std::io::stdin().read_to_string(&mut stdin)?;
    let payload = stdin.trim();

    if payload.is_empty() {
        write_response(GuestResponse {
            version: GUEST_PROTOCOL_VERSION.to_string(),
            request_id: "unknown".to_string(),
            ok: false,
            result: None,
            error: Some(GuestError::new(
                GuestErrorCode::InvalidRequest,
                "Empty request",
            )),
        })?;
        return Ok(());
    }

    let request: GuestRequest = match serde_json::from_str(payload) {
        Ok(request) => request,
        Err(err) => {
            write_response(GuestResponse {
                version: GUEST_PROTOCOL_VERSION.to_string(),
                request_id: "unknown".to_string(),
                ok: false,
                result: None,
                error: Some(GuestError::new(
                    GuestErrorCode::InvalidRequest,
                    err.to_string(),
                )),
            })?;
            return Ok(());
        }
    };

    let response = handle_request(&args.sync_path, &request);
    write_response(response)?;

    Ok(())
}

fn handle_request(sync_path: &PathBuf, request: &GuestRequest) -> GuestResponse {
    let request_id = request.request_id.clone();

    if request.version != GUEST_PROTOCOL_VERSION {
        return error_response(
            &request_id,
            GuestErrorCode::ProtocolError,
            format!("Unsupported protocol version: {}", request.version),
        );
    }

    if let Err(err) = validate_env_context(request) {
        return error_response(&request_id, err.code, err.message);
    }

    let requested_sync_path = request.context.sync_path.clone();
    let resolved_sync_path = sync_path.to_string_lossy().to_string();
    if requested_sync_path != resolved_sync_path {
        return error_response(
            &request_id,
            GuestErrorCode::InvalidRequest,
            "sync_path mismatch",
        );
    }

    let permissions = match effective_permissions(sync_path, &request.context.permissions) {
        Ok(perms) => perms,
        Err(err) => return error_response(&request_id, err.code, err.message),
    };

    if let Err(err) = ensure_permissions(request, &permissions) {
        return error_response(&request_id, err.code, err.message);
    }

    match request.action {
        GuestAction::ReadPayload => match read_payload(sync_path) {
            Ok(payload) => ok_response(&request_id, Some(Value::String(payload))),
            Err(err) => error_response(&request_id, err.code, err.message),
        },
        GuestAction::ReadContext => match read_context(sync_path) {
            Ok(context) => ok_response(&request_id, Some(context)),
            Err(err) => error_response(&request_id, err.code, err.message),
        },
        GuestAction::WritePayload => match write_payload(sync_path, &request.input) {
            Ok(()) => ok_response(&request_id, None),
            Err(err) => error_response(&request_id, err.code, err.message),
        },
        GuestAction::UpdatePayload => match update_payload(sync_path, &request.input, &permissions)
        {
            Ok(()) => ok_response(&request_id, None),
            Err(err) => error_response(&request_id, err.code, err.message),
        },
        GuestAction::WriteContext => match write_context(sync_path, &request.input) {
            Ok(()) => ok_response(&request_id, None),
            Err(err) => error_response(&request_id, err.code, err.message),
        },
        GuestAction::ExecuteWasm => match execute_wasm(sync_path, &permissions, None) {
            Ok(output) => ok_response(
                &request_id,
                Some(Value::String(encode_payload_base64(&output))),
            ),
            Err(err) => error_response(&request_id, err.code, err.message),
        },
    }
}

fn ensure_permissions(
    request: &GuestRequest,
    permissions: &GuestPermission,
) -> Result<(), GuestError> {
    if matches!(request.context.role, GuestContextRole::Consumer) {
        match request.action {
            GuestAction::ReadPayload | GuestAction::ReadContext => {}
            _ => {
                return Err(GuestError::new(
                    GuestErrorCode::PermissionDenied,
                    "Owner context required",
                ))
            }
        }
    }

    match request.action {
        GuestAction::ReadPayload => {
            if !permissions.can_read_payload {
                return Err(GuestError::new(
                    GuestErrorCode::PermissionDenied,
                    "read payload not allowed",
                ));
            }
        }
        GuestAction::ReadContext => {
            if !permissions.can_read_context {
                return Err(GuestError::new(
                    GuestErrorCode::PermissionDenied,
                    "read context not allowed",
                ));
            }
        }
        GuestAction::WritePayload | GuestAction::UpdatePayload => {
            if !permissions.can_write_payload {
                return Err(GuestError::new(
                    GuestErrorCode::PermissionDenied,
                    "write payload not allowed",
                ));
            }
        }
        GuestAction::WriteContext => {
            if !permissions.can_write_context {
                return Err(GuestError::new(
                    GuestErrorCode::PermissionDenied,
                    "write context not allowed",
                ));
            }
        }
        GuestAction::ExecuteWasm => {
            if !permissions.can_execute_wasm {
                return Err(GuestError::new(
                    GuestErrorCode::PermissionDenied,
                    "execute wasm not allowed",
                ));
            }
        }
    }

    Ok(())
}

fn validate_env_context(request: &GuestRequest) -> Result<(), GuestError> {
    if let Ok(protocol) = std::env::var("CAPSULE_GUEST_PROTOCOL") {
        if protocol != request.version {
            return Err(GuestError::new(
                GuestErrorCode::ProtocolError,
                "CAPSULE_GUEST_PROTOCOL mismatch",
            ));
        }
    }

    if let Ok(mode) = std::env::var("GUEST_MODE") {
        let expected = match request.context.mode {
            GuestMode::Widget => "widget",
            GuestMode::Headless => "headless",
        };
        if mode.to_ascii_lowercase() != expected {
            return Err(GuestError::new(
                GuestErrorCode::InvalidRequest,
                "GUEST_MODE mismatch",
            ));
        }
    }

    if let Ok(role) = std::env::var("GUEST_ROLE") {
        let expected = match request.context.role {
            GuestContextRole::Consumer => "consumer",
            GuestContextRole::Owner => "owner",
        };
        if role.to_ascii_lowercase() != expected {
            return Err(GuestError::new(
                GuestErrorCode::InvalidRequest,
                "GUEST_ROLE mismatch",
            ));
        }
    }

    if let Ok(sync_path) = std::env::var("SYNC_PATH") {
        if sync_path != request.context.sync_path {
            return Err(GuestError::new(
                GuestErrorCode::InvalidRequest,
                "SYNC_PATH mismatch",
            ));
        }
    }

    let widget_bounds = std::env::var("GUEST_WIDGET_BOUNDS").ok();
    match request.context.mode {
        GuestMode::Widget => {
            let value = widget_bounds.ok_or_else(|| {
                GuestError::new(
                    GuestErrorCode::InvalidRequest,
                    "GUEST_WIDGET_BOUNDS is required for widget mode",
                )
            })?;
            parse_widget_bounds(&value)?;
        }
        GuestMode::Headless => {
            if widget_bounds.is_some() {
                return Err(GuestError::new(
                    GuestErrorCode::InvalidRequest,
                    "GUEST_WIDGET_BOUNDS is not allowed in headless mode",
                ));
            }
        }
    }

    Ok(())
}

fn parse_widget_bounds(value: &str) -> Result<(u32, u32, u32, u32), GuestError> {
    let parts: Vec<&str> = value.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(GuestError::new(
            GuestErrorCode::InvalidRequest,
            "GUEST_WIDGET_BOUNDS must be x,y,width,height",
        ));
    }

    let x = parts[0]
        .parse::<u32>()
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))?;
    let y = parts[1]
        .parse::<u32>()
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))?;
    let width = parts[2]
        .parse::<u32>()
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))?;
    let height = parts[3]
        .parse::<u32>()
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))?;

    if width == 0 || height == 0 {
        return Err(GuestError::new(
            GuestErrorCode::InvalidRequest,
            "GUEST_WIDGET_BOUNDS width and height must be > 0",
        ));
    }

    Ok((x, y, width, height))
}

fn effective_permissions(
    sync_path: &PathBuf,
    requested: &GuestPermission,
) -> Result<GuestPermission, GuestError> {
    let manifest = load_manifest(sync_path)?;
    let manifest_permissions = manifest.permissions;

    let mut permissions = requested.clone();
    permissions.allowed_env =
        intersect_allowlist(&requested.allowed_env, &manifest_permissions.allow_env);
    permissions.allowed_hosts =
        intersect_allowlist(&requested.allowed_hosts, &manifest_permissions.allow_hosts);

    if let Ok(value) = std::env::var("ALLOW_ENV") {
        let allow_env = parse_allowlist_env(&value);
        permissions.allowed_env = intersect_allowlist(&permissions.allowed_env, &allow_env);
    }

    if let Ok(value) = std::env::var("ALLOW_HOSTS") {
        let allow_hosts = parse_allowlist_env(&value);
        permissions.allowed_hosts = intersect_allowlist(&permissions.allowed_hosts, &allow_hosts);
    }

    Ok(permissions)
}

fn intersect_allowlist(host: &[String], manifest: &[String]) -> Vec<String> {
    if host.is_empty() || manifest.is_empty() {
        return Vec::new();
    }

    let manifest_set: HashSet<&str> = manifest.iter().map(String::as_str).collect();
    host.iter()
        .filter(|item| manifest_set.contains(item.as_str()))
        .cloned()
        .collect()
}

fn parse_allowlist_env(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| item.to_string())
        .collect()
}

fn read_payload(sync_path: &PathBuf) -> Result<String, GuestError> {
    let payload = read_payload_bytes(sync_path)?;
    Ok(encode_payload_base64(&payload))
}

fn read_payload_bytes(sync_path: &PathBuf) -> Result<Vec<u8>, GuestError> {
    read_zip_entry_bytes(sync_path, "payload")
}

fn read_zip_entry_bytes(sync_path: &PathBuf, entry_name: &str) -> Result<Vec<u8>, GuestError> {
    let file = File::open(sync_path)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))?;
    let mut entry = archive
        .by_name(entry_name)
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))?;
    let mut buffer = Vec::new();
    entry
        .read_to_end(&mut buffer)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;
    Ok(buffer)
}

fn read_context(sync_path: &PathBuf) -> Result<Value, GuestError> {
    let file = File::open(sync_path)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))?;

    let mut context_file = match archive.by_name("context.json") {
        Ok(file) => file,
        Err(_) => return Ok(Value::Null),
    };

    let mut buffer = Vec::new();
    context_file
        .read_to_end(&mut buffer)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;

    serde_json::from_slice(&buffer)
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))
}

fn write_payload(sync_path: &PathBuf, input: &Value) -> Result<(), GuestError> {
    let payload = input.as_str().ok_or_else(|| {
        GuestError::new(GuestErrorCode::InvalidRequest, "payload must be a string")
    })?;

    let decoded = decode_payload_base64(payload)?;

    update_zip_entry(sync_path, "payload", &decoded)
}

fn update_payload(
    sync_path: &PathBuf,
    input: &Value,
    permissions: &GuestPermission,
) -> Result<(), GuestError> {
    let payload = input.as_str().ok_or_else(|| {
        GuestError::new(GuestErrorCode::InvalidRequest, "payload must be a string")
    })?;
    let decoded = decode_payload_base64(payload)?;

    let manifest = load_manifest(sync_path)?;

    if !manifest.ownership.write_allowed {
        return Err(GuestError::new(
            GuestErrorCode::PermissionDenied,
            "write-back not allowed by ownership policy",
        ));
    }

    write_payload(sync_path, &Value::String(payload.to_string()))?;

    if !permissions.can_execute_wasm {
        return Ok(());
    }

    if read_optional_zip_entry_bytes(sync_path, "sync.wasm")?.is_some() {
        execute_wasm(sync_path, permissions, Some(decoded))?;
    }

    Ok(())
}

fn write_context(sync_path: &PathBuf, input: &Value) -> Result<(), GuestError> {
    let bytes = serde_json::to_vec(input)
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))?;
    update_zip_entry(sync_path, "context.json", &bytes)
}

fn execute_wasm(
    sync_path: &PathBuf,
    permissions: &GuestPermission,
    stdin_payload: Option<Vec<u8>>,
) -> Result<Vec<u8>, GuestError> {
    let wasm_bytes = read_zip_entry_bytes(sync_path, "sync.wasm")
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.message))?;
    let payload = read_payload_bytes(sync_path)?;
    let context =
        read_optional_zip_entry_bytes(sync_path, "context.json")?.unwrap_or_else(|| b"{}".to_vec());

    let manifest = load_manifest(sync_path)?;

    let mut config = Config::new();
    config.wasm_component_model(false);
    config.async_support(false);
    config.max_wasm_stack(1024 * 1024);
    config.epoch_interruption(true);

    let engine = Engine::new(&config)
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;
    let module = Module::from_binary(&engine, &wasm_bytes)
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;

    let temp_dir =
        TempDir::new().map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;
    let payload_path = temp_dir.path().join("payload");
    let context_path = temp_dir.path().join("context.json");

    fs::write(&payload_path, payload)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;
    fs::write(&context_path, context)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;

    if stdin_payload.is_none() {
        set_readonly(&payload_path)?;
        set_readonly(&context_path)?;
    }

    let mut wasi_builder = WasiCtxBuilder::new();

    let stdout_pipe = WritePipe::new_in_memory();
    let stderr_pipe = WritePipe::new_in_memory();
    let stdout_handle = stdout_pipe.clone();
    let stderr_handle = stderr_pipe.clone();

    wasi_builder
        .arg("sync.wasm")
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;
    wasi_builder
        .env("SYNC_PATH", "/sync")
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;
    wasi_builder
        .env("SYNC_PAYLOAD", "/sync/payload")
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;
    wasi_builder
        .env("SYNC_CONTEXT", "/sync/context.json")
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;
    wasi_builder
        .env("ALLOW_HOSTS", &permissions.allowed_hosts.join(","))
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;
    wasi_builder
        .env("ALLOW_ENV", &permissions.allowed_env.join(","))
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;
    let sync_mode = if stdin_payload.is_some() {
        "push"
    } else {
        "pull"
    };
    let stdin_data = stdin_payload.unwrap_or_default();
    wasi_builder
        .env("SYNC_MODE", sync_mode)
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;
    wasi_builder
        .stdin(Box::new(ReadPipe::from(stdin_data)))
        .stdout(Box::new(stdout_pipe))
        .stderr(Box::new(stderr_pipe));

    for env_var in &permissions.allowed_env {
        if let Ok(value) = std::env::var(env_var) {
            let _ = wasi_builder.env(env_var, &value);
        }
    }

    let dir = Dir::open_ambient_dir(temp_dir.path(), ambient_authority())
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;
    let _ = wasi_builder.preopened_dir(dir, "/sync");

    let wasi = wasi_builder.build();

    let mut linker = Linker::new(&engine);
    wasmtime_wasi::add_to_linker(&mut linker, |state: &mut WasiState| &mut state.wasi)
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;

    let memory_limit_mb = std::env::var("GUEST_MEMORY_LIMIT_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(64);
    let limits = StoreLimitsBuilder::new()
        .memory_size(memory_limit_mb.saturating_mul(1024 * 1024))
        .build();

    let mut store = Store::new(&engine, WasiState { wasi, limits });
    store.limiter(|state| &mut state.limits);

    let timeout_secs = std::env::var("GUEST_CPU_LIMIT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|ms| ms.div_ceil(1000))
        .unwrap_or(manifest.policy.timeout);

    if timeout_secs > 0 {
        store.set_epoch_deadline(1);
        store.epoch_deadline_trap();
        let engine_for_timer = engine.clone();
        let timeout = std::time::Duration::from_secs(timeout_secs);
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            engine_for_timer.increment_epoch();
        });
    }
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;

    let start = instance
        .get_typed_func::<(), ()>(&mut store, "_start")
        .map_err(|err| GuestError::new(GuestErrorCode::ExecutionFailed, err.to_string()))?;

    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| start.call(&mut store, ())));

    drop(store);

    if result.is_err() {
        return Err(GuestError::new(
            GuestErrorCode::ExecutionFailed,
            "Wasm execution panicked",
        ));
    }

    if let Ok(Err(err)) = result {
        let stderr = stderr_handle
            .try_into_inner()
            .map_err(|_| {
                GuestError::new(
                    GuestErrorCode::ExecutionFailed,
                    "stderr handle still in use",
                )
            })?
            .into_inner();
        let stderr_msg = String::from_utf8_lossy(&stderr);
        let message = if stderr_msg.is_empty() {
            err.to_string()
        } else {
            format!("{} (stderr: {})", err, stderr_msg)
        };
        return Err(GuestError::new(GuestErrorCode::ExecutionFailed, message));
    }

    let output = stdout_handle
        .try_into_inner()
        .map_err(|_| {
            GuestError::new(
                GuestErrorCode::ExecutionFailed,
                "stdout handle still in use",
            )
        })?
        .into_inner();

    Ok(output)
}

#[cfg(unix)]
fn set_readonly(path: &Path) -> Result<(), GuestError> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = fs::Permissions::from_mode(0o444);
    fs::set_permissions(path, permissions)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))
}

#[cfg(not(unix))]
fn set_readonly(_path: &Path) -> Result<(), GuestError> {
    Ok(())
}

fn read_optional_zip_entry_bytes(
    sync_path: &PathBuf,
    entry_name: &str,
) -> Result<Option<Vec<u8>>, GuestError> {
    let file = File::open(sync_path)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))?;

    let mut entry = match archive.by_name(entry_name) {
        Ok(entry) => entry,
        Err(_) => return Ok(None),
    };

    let mut buffer = Vec::new();
    entry
        .read_to_end(&mut buffer)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;

    Ok(Some(buffer))
}

fn load_manifest(sync_path: &PathBuf) -> Result<GuestManifest, GuestError> {
    let manifest_bytes = read_zip_entry_bytes(sync_path, "manifest.toml")?;
    let manifest_text = std::str::from_utf8(&manifest_bytes)
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))?;
    toml::from_str(manifest_text)
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))
}

fn update_zip_entry(
    sync_path: &PathBuf,
    entry_name: &str,
    content: &[u8],
) -> Result<(), GuestError> {
    let archive_path = sync_path;
    let file = File::open(archive_path)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))?;

    let temp_path = archive_path.with_extension("sync.tmp");
    let temp_file = File::create(&temp_path)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;
    let mut temp_zip = ZipWriter::new(temp_file);
    let options: FileOptions<()> =
        FileOptions::default().compression_method(zip::CompressionMethod::Stored);

    let entries_to_skip = HashSet::from([entry_name]);

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|err| GuestError::new(GuestErrorCode::InvalidRequest, err.to_string()))?;
        let name = file.name().to_string();

        if entries_to_skip.contains(name.as_str()) {
            continue;
        }

        temp_zip
            .start_file(&name, options)
            .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;

        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;
        temp_zip
            .write_all(&buffer)
            .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;
    }

    temp_zip
        .start_file(entry_name, options)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;
    temp_zip
        .write_all(content)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;
    temp_zip
        .finish()
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;

    fs::rename(&temp_path, archive_path)
        .map_err(|err| GuestError::new(GuestErrorCode::IoError, err.to_string()))?;

    Ok(())
}

fn ok_response(request_id: &str, result: Option<Value>) -> GuestResponse {
    GuestResponse {
        version: GUEST_PROTOCOL_VERSION.to_string(),
        request_id: request_id.to_string(),
        ok: true,
        result,
        error: None,
    }
}

fn error_response(
    request_id: &str,
    code: GuestErrorCode,
    message: impl Into<String>,
) -> GuestResponse {
    GuestResponse {
        version: GUEST_PROTOCOL_VERSION.to_string(),
        request_id: request_id.to_string(),
        ok: false,
        result: None,
        error: Some(GuestError::new(code, message)),
    }
}

fn write_response(response: GuestResponse) -> Result<()> {
    let json = serde_json::to_string(&response)?;
    let mut stdout = std::io::stdout();
    stdout.write_all(json.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

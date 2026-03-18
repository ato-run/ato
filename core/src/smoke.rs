use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::error::CapsuleError;

const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 2000;
const PORT_RETRY_TIMEOUT: Duration = Duration::from_secs(30);
const PORT_RETRY_INTERVAL: Duration = Duration::from_millis(500);
const STDERR_TAIL_MAX_BYTES: usize = 8192;
const STDERR_CAPTURE_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(unix)]
const PROCESS_TERMINATE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeOptions {
    pub startup_timeout_ms: u64,
    pub check_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeSummary {
    pub startup_timeout_ms: u64,
    pub required_port: Option<u16>,
    pub checked_commands: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmokeFailureClass {
    SpawnFailed,
    ProcessExitedEarly,
    StartupTimeout,
    RequiredPortUnavailable,
    RequiredPortUnreachable,
    CheckCommandFailed,
    ManifestInvalid,
    ConfigInvalid,
}

impl SmokeFailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpawnFailed => "spawn_failed",
            Self::ProcessExitedEarly => "process_exited_early",
            Self::StartupTimeout => "startup_timeout",
            Self::RequiredPortUnavailable => "required_port_unavailable",
            Self::RequiredPortUnreachable => "required_port_unreachable",
            Self::CheckCommandFailed => "check_command_failed",
            Self::ManifestInvalid => "manifest_invalid",
            Self::ConfigInvalid => "config_invalid",
        }
    }
}

impl fmt::Display for SmokeFailureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeFailureReport {
    pub class: SmokeFailureClass,
    pub message: String,
    pub stderr_tail: String,
    pub exit_status: Option<i32>,
}

impl SmokeFailureReport {
    fn new(
        class: SmokeFailureClass,
        message: impl Into<String>,
        stderr_tail: impl Into<String>,
        exit_status: Option<i32>,
    ) -> Self {
        Self {
            class,
            message: message.into(),
            stderr_tail: trim_utf8_by_bytes(&stderr_tail.into(), STDERR_TAIL_MAX_BYTES),
            exit_status,
        }
    }
}

impl fmt::Display for SmokeFailureReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.stderr_tail.trim().is_empty() {
            write!(f, "{} ({})", self.message, self.class)
        } else {
            write!(
                f,
                "{} ({})\nstderr tail:\n{}",
                self.message, self.class, self.stderr_tail
            )
        }
    }
}

impl std::error::Error for SmokeFailureReport {}

#[derive(Debug, Clone)]
struct MainService {
    executable: String,
    args: Vec<String>,
    cwd: String,
    env: HashMap<String, String>,
    ports: HashMap<String, u16>,
    health_port: Option<String>,
}

struct StderrTailCapture {
    buffer: Arc<Mutex<Vec<u8>>>,
    done_rx: Receiver<()>,
    join_handle: Option<JoinHandle<()>>,
}

impl StderrTailCapture {
    fn from_child(child: &mut Child) -> Option<Self> {
        let reader = child.stderr.take()?;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let buffer_ref = Arc::clone(&buffer);
        let (done_tx, done_rx) = mpsc::channel();
        let join_handle = std::thread::spawn(move || {
            let mut reader = reader;
            let mut chunk = [0_u8; 1024];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(len) => {
                        if let Ok(mut guard) = buffer_ref.lock() {
                            guard.extend_from_slice(&chunk[..len]);
                            if guard.len() > STDERR_TAIL_MAX_BYTES {
                                let overflow = guard.len() - STDERR_TAIL_MAX_BYTES;
                                guard.drain(..overflow);
                            }
                        } else {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = done_tx.send(());
        });
        Some(Self {
            buffer,
            done_rx,
            join_handle: Some(join_handle),
        })
    }

    fn snapshot(&self) -> String {
        let bytes = self
            .buffer
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).trim().to_string()
    }

    fn finish(&mut self) -> String {
        let completed = self
            .done_rx
            .recv_timeout(STDERR_CAPTURE_WAIT_TIMEOUT)
            .is_ok();
        if completed {
            if let Some(handle) = self.join_handle.take() {
                let _ = handle.join();
            }
        } else {
            let _ = self.join_handle.take();
        }
        self.snapshot()
    }
}

pub fn run_capsule_smoke(
    capsule_path: &Path,
    target_label: &str,
) -> std::result::Result<SmokeSummary, SmokeFailureReport> {
    let extract_dir = tempfile::tempdir().map_err(|err| {
        SmokeFailureReport::new(
            SmokeFailureClass::ConfigInvalid,
            format!("failed to create smoke tempdir: {err}"),
            "",
            None,
        )
    })?;
    extract_capsule(capsule_path, extract_dir.path()).map_err(|err| {
        SmokeFailureReport::new(SmokeFailureClass::ConfigInvalid, err.to_string(), "", None)
    })?;

    let manifest_path = extract_dir.path().join("capsule.toml");
    let manifest = std::fs::read_to_string(&manifest_path).map_err(|err| {
        SmokeFailureReport::new(
            SmokeFailureClass::ManifestInvalid,
            format!("failed to read extracted manifest: {err}"),
            "",
            None,
        )
    })?;
    let manifest_raw: toml::Value = toml::from_str(&manifest).map_err(|err| {
        SmokeFailureReport::new(
            SmokeFailureClass::ManifestInvalid,
            format!("manifest parse failed: {err}"),
            "",
            None,
        )
    })?;

    let options = parse_smoke_options(&manifest_raw, target_label).map_err(|err| {
        SmokeFailureReport::new(
            SmokeFailureClass::ManifestInvalid,
            err.to_string(),
            "",
            None,
        )
    })?;
    let service = load_main_service(extract_dir.path()).map_err(|err| {
        SmokeFailureReport::new(SmokeFailureClass::ConfigInvalid, err.to_string(), "", None)
    })?;
    let required_port =
        resolve_required_port(&manifest_raw, target_label, &service).map_err(|err| {
            SmokeFailureReport::new(
                SmokeFailureClass::ManifestInvalid,
                err.to_string(),
                "",
                None,
            )
        })?;
    ensure_required_port_is_free_before_start(required_port).map_err(|err| {
        SmokeFailureReport::new(
            SmokeFailureClass::RequiredPortUnavailable,
            err.to_string(),
            "",
            None,
        )
    })?;

    let mut child = spawn_main_service(extract_dir.path(), &service).map_err(|err| {
        SmokeFailureReport::new(SmokeFailureClass::SpawnFailed, err.to_string(), "", None)
    })?;
    let mut stderr_capture = StderrTailCapture::from_child(&mut child);
    let startup_timeout = Duration::from_millis(options.startup_timeout_ms);
    let deadline = Instant::now() + startup_timeout;

    loop {
        if let Some(status) = child.try_wait().map_err(|err| {
            SmokeFailureReport::new(
                SmokeFailureClass::ProcessExitedEarly,
                format!("failed to poll smoke process: {err}"),
                capture_current_stderr(&stderr_capture),
                None,
            )
        })? {
            if status.success() && required_port.is_none() && options.check_commands.is_empty() {
                if let Some(capture) = stderr_capture.as_mut() {
                    let _ = capture.finish();
                }
                return Ok(SmokeSummary {
                    startup_timeout_ms: options.startup_timeout_ms,
                    required_port,
                    checked_commands: options.check_commands.len(),
                });
            }

            let stderr_tail = finish_capture(&mut stderr_capture);
            return Err(SmokeFailureReport::new(
                SmokeFailureClass::ProcessExitedEarly,
                format!("Smoke failed: process exited before startup timeout (status: {status})"),
                stderr_tail,
                status.code(),
            ));
        }

        if Instant::now() >= deadline {
            break;
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    if required_port.is_none() && options.check_commands.is_empty() {
        kill_child(&mut child).map_err(|err| {
            SmokeFailureReport::new(
                SmokeFailureClass::StartupTimeout,
                err.to_string(),
                capture_current_stderr(&stderr_capture),
                None,
            )
        })?;
        let _ = finish_capture(&mut stderr_capture);
        return Ok(SmokeSummary {
            startup_timeout_ms: options.startup_timeout_ms,
            required_port,
            checked_commands: 0,
        });
    }

    if let Some(port) = required_port {
        if let Err(mut report) =
            wait_for_required_port_with_retry(&mut child, port, PORT_RETRY_TIMEOUT, &stderr_capture)
        {
            let _ = kill_child(&mut child);
            report.stderr_tail =
                combine_stderr(report.stderr_tail, finish_capture(&mut stderr_capture));
            return Err(report);
        }
    }

    if let Err(mut report) = run_check_commands(extract_dir.path(), &service, &options) {
        let _ = kill_child(&mut child);
        report.stderr_tail =
            combine_stderr(finish_capture(&mut stderr_capture), report.stderr_tail);
        return Err(report);
    }

    kill_child(&mut child).map_err(|err| {
        SmokeFailureReport::new(
            SmokeFailureClass::StartupTimeout,
            err.to_string(),
            capture_current_stderr(&stderr_capture),
            None,
        )
    })?;
    let _ = finish_capture(&mut stderr_capture);

    Ok(SmokeSummary {
        startup_timeout_ms: options.startup_timeout_ms,
        required_port,
        checked_commands: options.check_commands.len(),
    })
}

fn extract_capsule(capsule_path: &Path, out_dir: &Path) -> Result<(), CapsuleError> {
    let mut archive = std::fs::File::open(capsule_path).map_err(CapsuleError::Io)?;
    let mut outer = tar::Archive::new(&mut archive);
    outer.unpack(out_dir).map_err(CapsuleError::Io)?;

    crate::capsule_v3::unpack_payload_from_capsule_root(out_dir, out_dir)?;

    Ok(())
}

pub(crate) fn parse_smoke_options(
    manifest: &toml::Value,
    target_label: &str,
) -> Result<SmokeOptions, CapsuleError> {
    let target = manifest
        .get("targets")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(target_label))
        .and_then(|v| v.as_table())
        .ok_or_else(|| {
            CapsuleError::Pack(format!("targets.{target_label} is missing in manifest"))
        })?;

    let mut startup_timeout_ms = DEFAULT_STARTUP_TIMEOUT_MS;
    let mut check_commands = Vec::new();

    if let Some(smoke) = target.get("smoke") {
        let smoke = smoke.as_table().ok_or_else(|| {
            CapsuleError::Pack(format!("targets.{target_label}.smoke must be a table"))
        })?;

        if let Some(timeout) = smoke.get("startup_timeout_ms") {
            let timeout = timeout.as_integer().ok_or_else(|| {
                CapsuleError::Pack(format!(
                    "targets.{target_label}.smoke.startup_timeout_ms must be an integer"
                ))
            })?;
            if timeout <= 0 {
                return Err(CapsuleError::Pack(format!(
                    "targets.{target_label}.smoke.startup_timeout_ms must be greater than 0"
                )));
            }
            startup_timeout_ms = timeout as u64;
        }

        if let Some(commands) = smoke.get("check_commands") {
            let commands = commands.as_array().ok_or_else(|| {
                CapsuleError::Pack(format!(
                    "targets.{target_label}.smoke.check_commands must be an array"
                ))
            })?;
            for (idx, cmd) in commands.iter().enumerate() {
                let cmd = cmd.as_str().ok_or_else(|| {
                    CapsuleError::Pack(format!(
                        "targets.{target_label}.smoke.check_commands[{idx}] must be a string"
                    ))
                })?;
                if cmd.trim().is_empty() {
                    return Err(CapsuleError::Pack(format!(
                        "targets.{target_label}.smoke.check_commands[{idx}] must not be empty"
                    )));
                }
                check_commands.push(cmd.to_string());
            }
        }
    }

    Ok(SmokeOptions {
        startup_timeout_ms,
        check_commands,
    })
}

fn load_main_service(extract_dir: &Path) -> Result<MainService, CapsuleError> {
    let config_path = extract_dir.join("config.json");
    let raw = std::fs::read_to_string(&config_path).map_err(CapsuleError::Io)?;
    let json: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| CapsuleError::Pack(format!("config.json parse failed: {e}")))?;

    let main = json
        .get("services")
        .and_then(|v| v.get("main"))
        .and_then(|v| v.as_object())
        .ok_or_else(|| CapsuleError::Pack("config.json requires services.main".to_string()))?;

    let executable = main
        .get("executable")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
        .ok_or_else(|| CapsuleError::Pack("services.main.executable is required".to_string()))?;

    let args = main
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let cwd = main
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("source")
        .to_string();

    let env = main
        .get("env")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let ports = main
        .get("ports")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_u64().map(|p| (k.clone(), p as u16)))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let health_port = main
        .get("health_check")
        .and_then(|v| v.get("port"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(MainService {
        executable,
        args,
        cwd,
        env,
        ports,
        health_port,
    })
}

fn resolve_required_port(
    manifest: &toml::Value,
    target_label: &str,
    service: &MainService,
) -> Result<Option<u16>, CapsuleError> {
    let target_port = manifest
        .get("targets")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(target_label))
        .and_then(|v| v.as_table())
        .and_then(|target| target.get("port"))
        .map(|v| {
            v.as_integer().ok_or_else(|| {
                CapsuleError::Pack(format!("targets.{target_label}.port must be an integer"))
            })
        })
        .transpose()?;

    if let Some(port) = target_port {
        if !(1..=65535).contains(&port) {
            return Err(CapsuleError::Pack(format!(
                "targets.{target_label}.port must be between 1 and 65535"
            )));
        }
        return Ok(Some(port as u16));
    }

    if let Some(port) = service.health_port.as_deref() {
        if let Ok(num) = port.parse::<u16>() {
            if num == 0 {
                return Err(CapsuleError::Pack(
                    "services.main.health_check.port must be > 0".to_string(),
                ));
            }
            return Ok(Some(num));
        }
        if let Some(num) = service.ports.get(port) {
            return Ok(Some(*num));
        }
        return Err(CapsuleError::Pack(format!(
            "services.main.health_check.port '{port}' is not numeric and not found in services.main.ports"
        )));
    }

    Ok(None)
}

fn spawn_main_service(root: &Path, service: &MainService) -> std::io::Result<Child> {
    let cwd_path = resolve_path(root, &service.cwd);
    let executable = resolve_path_with_cwd(root, &cwd_path, &service.executable);
    let mut cmd = Command::new(&executable);
    let args = service
        .args
        .iter()
        .map(|a| resolve_arg(root, a))
        .collect::<Vec<_>>();
    cmd.args(args);
    cmd.current_dir(&cwd_path);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    for (k, v) in &service.env {
        cmd.env(k, resolve_env_value(root, v));
    }
    for (k, v) in &service.ports {
        cmd.env(k, v.to_string());
    }

    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    cmd.spawn().map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "failed to start process '{}' for smoke: {e}",
                executable.display()
            ),
        )
    })
}

fn run_check_commands(
    root: &Path,
    service: &MainService,
    options: &SmokeOptions,
) -> std::result::Result<(), SmokeFailureReport> {
    if options.check_commands.is_empty() {
        return Ok(());
    }

    let cwd_path = resolve_path(root, &service.cwd);
    for command in &options.check_commands {
        let parts = shell_words::split(command).map_err(|e| {
            SmokeFailureReport::new(
                SmokeFailureClass::CheckCommandFailed,
                format!("invalid smoke.check_commands entry '{command}': {e}"),
                "",
                None,
            )
        })?;
        if parts.is_empty() {
            return Err(SmokeFailureReport::new(
                SmokeFailureClass::CheckCommandFailed,
                "smoke command must not be empty",
                "",
                None,
            ));
        }

        let mut cmd = Command::new(&parts[0]);
        cmd.args(parts.iter().skip(1));
        cmd.current_dir(&cwd_path);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());
        for (k, v) in &service.env {
            cmd.env(k, resolve_env_value(root, v));
        }
        for (k, v) in &service.ports {
            cmd.env(k, v.to_string());
        }

        let output = cmd.output().map_err(|e| {
            SmokeFailureReport::new(
                SmokeFailureClass::CheckCommandFailed,
                format!("failed to execute smoke command '{command}': {e}"),
                "",
                None,
            )
        })?;
        if !output.status.success() {
            return Err(SmokeFailureReport::new(
                SmokeFailureClass::CheckCommandFailed,
                format!(
                    "smoke command failed (status {}): {}",
                    output.status, command
                ),
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
                output.status.code(),
            ));
        }
    }

    Ok(())
}

fn kill_child(child: &mut Child) -> Result<(), CapsuleError> {
    #[cfg(unix)]
    {
        let pgid = child.id() as i32;
        unsafe {
            libc::killpg(pgid, libc::SIGTERM);
        }
        let terminate_deadline = Instant::now() + PROCESS_TERMINATE_TIMEOUT;
        while Instant::now() < terminate_deadline {
            if child.try_wait().map_err(CapsuleError::Io)?.is_some() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
        let kill_deadline = Instant::now() + PROCESS_TERMINATE_TIMEOUT;
        while Instant::now() < kill_deadline {
            if child.try_wait().map_err(CapsuleError::Io)?.is_some() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

fn can_connect_localhost(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok()
}

fn ensure_required_port_is_free_before_start(
    required_port: Option<u16>,
) -> Result<(), CapsuleError> {
    let Some(port) = required_port else {
        return Ok(());
    };
    if can_connect_localhost(port) {
        return Err(CapsuleError::Pack(format!(
            "Smoke failed: required port {port} is already in use before launch; stop the existing process and retry"
        )));
    }
    Ok(())
}

fn wait_for_required_port_with_retry(
    child: &mut Child,
    port: u16,
    timeout: Duration,
    stderr_capture: &Option<StderrTailCapture>,
) -> std::result::Result<(), SmokeFailureReport> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|err| {
            SmokeFailureReport::new(
                SmokeFailureClass::RequiredPortUnreachable,
                format!("failed to poll smoke process while waiting for port {port}: {err}"),
                capture_current_stderr(stderr_capture),
                None,
            )
        })? {
            return Err(SmokeFailureReport::new(
                SmokeFailureClass::ProcessExitedEarly,
                format!(
                    "Smoke failed: process exited while waiting for port {port} (status: {status})"
                ),
                capture_current_stderr(stderr_capture),
                status.code(),
            ));
        }

        if can_connect_localhost(port) {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(SmokeFailureReport::new(
                SmokeFailureClass::RequiredPortUnreachable,
                format!(
                    "Port {port} did not open within {} seconds. Check logs.",
                    timeout.as_secs()
                ),
                capture_current_stderr(stderr_capture),
                None,
            ));
        }

        std::thread::sleep(PORT_RETRY_INTERVAL);
    }
}

fn resolve_path(root: &Path, raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed.starts_with('/') {
        return PathBuf::from(trimmed);
    }
    if !trimmed.contains('/') {
        if let Ok(found) = which::which(trimmed) {
            return found;
        }
    }
    root.join(trimmed)
}

fn resolve_path_with_cwd(root: &Path, cwd: &Path, raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed.starts_with('/') {
        return PathBuf::from(trimmed);
    }
    if trimmed.starts_with("source/") || trimmed.starts_with("runtime/") {
        return root.join(trimmed);
    }
    if trimmed.starts_with("./") {
        return cwd.join(trimmed.trim_start_matches("./"));
    }
    if !trimmed.contains('/') {
        let with_cwd = cwd.join(trimmed);
        if with_cwd.exists() {
            return with_cwd;
        }
        if let Ok(found) = which::which(trimmed) {
            return found;
        }
    }
    root.join(trimmed)
}

fn resolve_arg(root: &Path, arg: &str) -> String {
    let trimmed = arg.trim();
    if trimmed.starts_with("source/") || trimmed.starts_with("runtime/") {
        return root.join(trimmed).to_string_lossy().to_string();
    }
    trimmed.to_string()
}

fn resolve_env_value(root: &Path, raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("source/") || trimmed.starts_with("runtime/") {
        return root.join(trimmed).to_string_lossy().to_string();
    }
    trimmed.to_string()
}

fn trim_utf8_by_bytes(value: &str, max_bytes: usize) -> String {
    let encoded = value.as_bytes();
    if encoded.len() <= max_bytes {
        return value.trim().to_string();
    }

    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].trim().to_string()
}

fn capture_current_stderr(stderr_capture: &Option<StderrTailCapture>) -> String {
    stderr_capture
        .as_ref()
        .map(|capture| capture.snapshot())
        .unwrap_or_default()
}

fn finish_capture(stderr_capture: &mut Option<StderrTailCapture>) -> String {
    stderr_capture
        .as_mut()
        .map(|capture| capture.finish())
        .unwrap_or_default()
}

fn combine_stderr(primary: String, secondary: String) -> String {
    match (primary.trim(), secondary.trim()) {
        ("", "") => String::new(),
        ("", _) => trim_utf8_by_bytes(&secondary, STDERR_TAIL_MAX_BYTES),
        (_, "") => trim_utf8_by_bytes(&primary, STDERR_TAIL_MAX_BYTES),
        _ if primary == secondary => trim_utf8_by_bytes(&primary, STDERR_TAIL_MAX_BYTES),
        _ => trim_utf8_by_bytes(
            &format!("{}\n{}", primary.trim_end(), secondary.trim_start()),
            STDERR_TAIL_MAX_BYTES,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_smoke_defaults() {
        let manifest: toml::Value = toml::from_str(
            r#"
[targets.cli]
runtime = "source"
entrypoint = "main.py"
"#,
        )
        .unwrap();
        let opts = parse_smoke_options(&manifest, "cli").unwrap();
        assert_eq!(opts.startup_timeout_ms, DEFAULT_STARTUP_TIMEOUT_MS);
        assert!(opts.check_commands.is_empty());
    }

    #[test]
    fn parse_smoke_invalid_timeout() {
        let manifest: toml::Value = toml::from_str(
            r#"
[targets.cli]
runtime = "source"
entrypoint = "main.py"

[targets.cli.smoke]
startup_timeout_ms = 0
"#,
        )
        .unwrap();
        let err = parse_smoke_options(&manifest, "cli").unwrap_err();
        assert!(err.to_string().contains("startup_timeout_ms"));
    }

    #[test]
    fn reject_required_port_already_in_use_before_start() {
        let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", 0)) else {
            return;
        };
        let port = listener.local_addr().unwrap().port();

        let err = ensure_required_port_is_free_before_start(Some(port)).unwrap_err();
        assert!(err.to_string().contains("already in use"));
    }

    #[test]
    fn trims_utf8_tail_without_splitting_codepoints() {
        let repeated = "あ".repeat(5000);
        let trimmed = trim_utf8_by_bytes(&repeated, 8192);
        assert!(trimmed.len() < repeated.len());
        assert!(trimmed.is_char_boundary(trimmed.len()));
    }

    #[test]
    fn combines_unique_stderr_blocks() {
        let combined = combine_stderr("main stderr".to_string(), "check stderr".to_string());
        assert!(combined.contains("main stderr"));
        assert!(combined.contains("check stderr"));
    }

    #[cfg(unix)]
    #[test]
    fn kill_child_terminates_process_group_and_releases_stderr() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("(sleep 30) >&2 & exec sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().expect("spawn smoke fixture");

        let mut capture = StderrTailCapture::from_child(&mut child);
        kill_child(&mut child).expect("kill child process group");

        let started = Instant::now();
        let _ = finish_capture(&mut capture);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "stderr capture should finish without hanging"
        );
    }
}

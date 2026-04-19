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

use crate::common::paths::workspace_artifacts_dir;
use crate::error::CapsuleError;
use crate::isolation::HostIsolationContext;

const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 2000;
const PORT_RETRY_TIMEOUT: Duration = Duration::from_secs(120);
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
    let isolated_env = prepare_isolated_host_environment(extract_dir.path())?;
    prepare_smoke_working_directory(extract_dir.path(), &service, &isolated_env)?;

    let mut child =
        spawn_main_service(extract_dir.path(), &service, &isolated_env).map_err(|err| {
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

    if let Err(mut report) =
        run_check_commands(extract_dir.path(), &service, &options, &isolated_env)
    {
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

fn spawn_main_service(
    root: &Path,
    service: &MainService,
    isolated_env: &HostIsolationContext,
) -> std::io::Result<Child> {
    let cwd_path = resolve_path(root, &service.cwd);
    // Resolve `npm:X` → `node_modules/.bin/X` so ato's package-bin references
    // work inside the smoke's isolated environment.
    let executable = if let Some(package_bin) = service.executable.trim().strip_prefix("npm:") {
        let bin = cwd_path.join("node_modules").join(".bin").join(package_bin);
        if bin.exists() {
            bin
        } else {
            resolve_path_with_cwd(root, &cwd_path, &service.executable)
        }
    } else {
        resolve_path_with_cwd(root, &cwd_path, &service.executable)
    };
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
    apply_isolated_command_env(&mut cmd, root, service, isolated_env);
    cmd.env("COREPACK_ENABLE_STRICT", "0");
    cmd.env("npm_config_manage_package_manager_versions", "false");

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

fn prepare_smoke_working_directory(
    root: &Path,
    service: &MainService,
    isolated_env: &HostIsolationContext,
) -> std::result::Result<(), SmokeFailureReport> {
    let cwd_path = resolve_path(root, &service.cwd);
    if service.executable.trim() == "uv" && cwd_path.join("uv.lock").exists() {
        let install_dir = cwd_path.join(".ato-smoke-site-packages");
        if !install_dir.exists() {
            let mut command = Command::new("uv");
            command.args([
                "pip",
                "sync",
                "uv.lock",
                "--python",
                "python3",
                "--target",
                ".ato-smoke-site-packages",
            ]);
            if let Some(uv_cache_dir) = resolve_bundled_uv_cache_dir(root, service) {
                command.args(["--cache-dir", &uv_cache_dir]);
            }
            command.current_dir(&cwd_path);
            command.stdin(Stdio::null());
            command.stdout(Stdio::null());
            command.stderr(Stdio::piped());
            apply_isolated_command_env(&mut command, root, service, isolated_env);

            let output = command.output().map_err(|err| {
                SmokeFailureReport::new(
                    SmokeFailureClass::SpawnFailed,
                    format!("failed to prepare python smoke dependencies: {err}"),
                    "",
                    None,
                )
            })?;
            if !output.status.success() {
                return Err(SmokeFailureReport::new(
                    SmokeFailureClass::SpawnFailed,
                    format!(
                        "smoke python dependency preparation failed (status {}): uv pip sync",
                        output.status
                    ),
                    String::from_utf8_lossy(&output.stderr).trim().to_string(),
                    output.status.code(),
                ));
            }
        }
    }

    let package_json = cwd_path.join("package.json");
    let node_modules = cwd_path.join("node_modules");
    if !package_json.exists() || node_modules.exists() {
        return Ok(());
    }

    let install = if cwd_path.join("pnpm-lock.yaml").exists() {
        // Write a project-level .npmrc that disables pnpm's auto-version-switch so the
        // system pnpm binary is used as-is even if package.json pins a different version.
        let npmrc_path = cwd_path.join(".npmrc");
        if !npmrc_path.exists() {
            let _ = std::fs::write(
                &npmrc_path,
                "manage-package-manager-versions=false\n",
            );
        } else if let Ok(existing) = std::fs::read_to_string(&npmrc_path) {
            if !existing.contains("manage-package-manager-versions") {
                let _ = std::fs::write(
                    &npmrc_path,
                    format!("{existing}\nmanage-package-manager-versions=false\n"),
                );
            }
        }
        Some(("pnpm", vec!["install"]))
    } else if cwd_path.join("package-lock.json").exists()
        || cwd_path.join("npm-shrinkwrap.json").exists()
    {
        Some(("npm", vec!["install", "--legacy-peer-deps"]))
    } else if cwd_path.join("yarn.lock").exists() {
        Some(("yarn", vec!["install"]))
    } else if cwd_path.join("bun.lock").exists() || cwd_path.join("bun.lockb").exists() {
        Some(("bun", vec!["install"]))
    } else {
        None
    };

    let Some((program, args)) = install else {
        return Ok(());
    };
    let joined_args = args.join(" ");

    let mut command = Command::new(program);
    command.args(&args);
    command.current_dir(&cwd_path);
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::piped());
    // apply_isolated_command_env calls env_clear() internally; set package-manager
    // compat vars AFTER it so they are not wiped.
    apply_isolated_command_env(&mut command, root, service, isolated_env);
    // Prevent corepack from enforcing the `packageManager` version pin, which may
    // refer to a version not yet cached on the host machine.
    command.env("COREPACK_ENABLE_STRICT", "0");
    // Disable pnpm 10's auto-manage-package-manager-versions: without this, pnpm
    // attempts to download and switch to the version pinned in packageManager, which
    // fails in offline/isolated smoke environments.
    // "false" (string) is truthy in JS — use "0" so pnpm's bool parser disables the feature.
    command.env("npm_config_manage_package_manager_versions", "0");
    // Auto-approve pnpm build scripts without interactive prompt.
    command.env("npm_config_approve_builds", "on");
    // Skip git-hooks managers (husky, lefthook, etc.): the smoke workspace has no .git dir.
    command.env("HUSKY", "0");
    command.env("LEFTHOOK", "0");
    // Use the bundled pnpm content-addressable store (fetched during build) so the
    // smoke install resolves from cache rather than downloading from the network.
    if program == "pnpm" {
        if let Some(bundled_store) = resolve_bundled_pnpm_store_dir(root) {
            command.env("pnpm_config_store_dir", &bundled_store);
            command.arg("--store-dir");
            command.arg(&bundled_store);
            command.arg("--prefer-offline");
        }
    }

    let output = command.output().map_err(|err| {
        SmokeFailureReport::new(
            SmokeFailureClass::SpawnFailed,
            format!(
                "failed to prepare smoke dependencies with '{program} {}': {err}",
                joined_args
            ),
            "",
            None,
        )
    })?;
    if !output.status.success() {
        return Err(SmokeFailureReport::new(
            SmokeFailureClass::SpawnFailed,
            format!(
                "smoke dependency preparation failed (status {}): {} {}",
                output.status, program, joined_args
            ),
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
            output.status.code(),
        ));
    }

    Ok(())
}

fn run_check_commands(
    root: &Path,
    service: &MainService,
    options: &SmokeOptions,
    isolated_env: &HostIsolationContext,
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
        apply_isolated_command_env(&mut cmd, root, service, isolated_env);
        cmd.env("COREPACK_ENABLE_STRICT", "0");
        cmd.env("npm_config_manage_package_manager_versions", "false");

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

fn prepare_isolated_host_environment(
    root: &Path,
) -> std::result::Result<HostIsolationContext, SmokeFailureReport> {
    HostIsolationContext::new(root, "smoke").map_err(|err| {
        SmokeFailureReport::new(
            SmokeFailureClass::ConfigInvalid,
            format!("failed to prepare isolated smoke environment: {err}"),
            "",
            None,
        )
    })
}

fn apply_isolated_command_env(
    command: &mut Command,
    root: &Path,
    service: &MainService,
    isolated_env: &HostIsolationContext,
) {
    let service_python_path = service
        .env
        .get("PYTHONPATH")
        .map(|value| resolve_env_value(root, value));
    let extra_env = service
        .env
        .iter()
        .map(|(key, value)| (key.clone(), resolve_env_value(root, value)))
        .chain(
            service
                .ports
                .iter()
                .map(|(key, value)| (key.clone(), value.to_string())),
        )
        .collect::<Vec<_>>();
    isolated_env.apply_to_command(command, extra_env);

    if let Some(uv_cache_dir) = resolve_bundled_uv_cache_dir(root, service) {
        command.env("UV_CACHE_DIR", uv_cache_dir);
    }
    if let Some(site_packages) = resolve_smoke_site_packages_dir(root, service) {
        let python_path = service_python_path
            .map(|existing| format!("{}:{}", site_packages.display(), existing))
            .unwrap_or_else(|| site_packages.display().to_string());
        command.env("PYTHONPATH", python_path);
    }
}

fn resolve_bundled_uv_cache_dir(root: &Path, service: &MainService) -> Option<String> {
    let executable = service.executable.trim();
    if executable != "uv" {
        return None;
    }

    let cwd_path = resolve_path(root, &service.cwd);
    for base in [
        workspace_artifacts_dir(&cwd_path),
        cwd_path.join("artifacts"),
        workspace_artifacts_dir(root),
        root.join("artifacts"),
    ] {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path().join("uv-cache");
            if candidate.is_dir() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }

    None
}

/// Look for the pnpm content-addressable store bundled as an artifact in the
/// capsule (populated by `pnpm fetch` during the build phase). Using it avoids
/// a full network download during smoke test dependency preparation.
fn resolve_bundled_pnpm_store_dir(root: &Path) -> Option<PathBuf> {
    for base in [workspace_artifacts_dir(root), root.join("artifacts")] {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path().join("pnpm-store");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }
    None
}

fn resolve_smoke_site_packages_dir(root: &Path, service: &MainService) -> Option<PathBuf> {
    let cwd_path = resolve_path(root, &service.cwd);
    let site_packages = cwd_path.join(".ato-smoke-site-packages");
    site_packages.is_dir().then_some(site_packages)
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
    // Check both IPv4 (127.0.0.1) and IPv6 (::1) since dev servers like Vite 5+
    // may bind exclusively to the IPv6 loopback address.
    let ipv4 = SocketAddr::from(([127, 0, 0, 1], port));
    let ipv6 = SocketAddr::from(([0u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], port));
    TcpStream::connect_timeout(&ipv4, Duration::from_millis(100)).is_ok()
        || TcpStream::connect_timeout(&ipv6, Duration::from_millis(100)).is_ok()
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
    use std::fs;

    struct PathGuard {
        original: Option<std::ffi::OsString>,
    }

    impl PathGuard {
        fn prepend(path: &Path) -> Self {
            let original = std::env::var_os("PATH");
            let mut parts = vec![path.to_path_buf()];
            if let Some(existing) = &original {
                parts.extend(std::env::split_paths(existing));
            }
            let joined = std::env::join_paths(parts).expect("join PATH");
            std::env::set_var("PATH", &joined);
            Self { original }
        }
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            if let Some(original) = &self.original {
                std::env::set_var("PATH", original);
            } else {
                std::env::remove_var("PATH");
            }
        }
    }

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

    #[test]
    fn prepare_smoke_working_directory_installs_pnpm_dependencies_when_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        fs::create_dir_all(&source_dir).expect("mkdir source");
        fs::write(source_dir.join("package.json"), "{}\n").expect("write package.json");
        fs::write(
            source_dir.join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\n",
        )
        .expect("write pnpm lock");

        let bin_dir = temp.path().join("bin");
        fs::create_dir_all(&bin_dir).expect("mkdir bin");
        let pnpm_path = bin_dir.join("pnpm");
        let env_capture_path = temp.path().join("captured-env.txt");
        fs::write(
            &pnpm_path,
            format!(
                "#!/bin/sh\n{{\nprintf 'HOME=%s\\n' \"$HOME\"\nprintf 'TMPDIR=%s\\n' \"$TMPDIR\"\nprintf 'npm_config_cache=%s\\n' \"$npm_config_cache\"\nprintf 'pnpm_config_store_dir=%s\\n' \"$pnpm_config_store_dir\"\nprintf 'SECRET_HOST_TOKEN=%s\\n' \"$SECRET_HOST_TOKEN\"\n}} > '{}'\nmkdir -p node_modules\nexit 0\n",
                env_capture_path.display()
            ),
        )
        .expect("write fake pnpm");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&pnpm_path).expect("stat pnpm").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&pnpm_path, perms).expect("chmod pnpm");
        }

        let _path_guard = PathGuard::prepend(&bin_dir);
        let service = MainService {
            executable: "sh".to_string(),
            args: vec!["-c".to_string(), "echo ok".to_string()],
            cwd: "source".to_string(),
            env: HashMap::new(),
            ports: HashMap::new(),
            health_port: None,
        };

        std::env::set_var("SECRET_HOST_TOKEN", "do-not-leak");
        let isolated_env = prepare_isolated_host_environment(temp.path()).expect("isolated env");

        prepare_smoke_working_directory(temp.path(), &service, &isolated_env)
            .expect("prepare deps");

        assert!(source_dir.join("node_modules").is_dir());
        let captured = fs::read_to_string(&env_capture_path).expect("read captured env");
        let isolated_home = temp
            .path()
            .join(".ato-smoke-host")
            .join("home")
            .to_string_lossy()
            .to_string();
        let isolated_tmp = temp
            .path()
            .join(".ato-smoke-host")
            .join("tmp")
            .to_string_lossy()
            .to_string();
        let isolated_npm_cache = temp
            .path()
            .join(".ato-smoke-host")
            .join("cache")
            .join("npm")
            .to_string_lossy()
            .to_string();
        let isolated_pnpm_store = temp
            .path()
            .join(".ato-smoke-host")
            .join("cache")
            .join("pnpm-store")
            .to_string_lossy()
            .to_string();

        assert!(captured.contains(&format!("HOME={isolated_home}")));
        assert!(captured.contains(&format!("TMPDIR={isolated_tmp}")));
        assert!(captured.contains(&format!("npm_config_cache={isolated_npm_cache}")));
        assert!(captured.contains(&format!("pnpm_config_store_dir={isolated_pnpm_store}")));
        assert!(captured.contains("SECRET_HOST_TOKEN="));
        assert!(!captured.contains("do-not-leak"));

        std::env::remove_var("SECRET_HOST_TOKEN");
    }

    #[test]
    fn apply_isolated_command_env_prefers_bundled_uv_cache() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let cache_dir = workspace_artifacts_dir(&source_dir)
            .join("app")
            .join("uv-cache");
        fs::create_dir_all(&cache_dir).expect("mkdir uv-cache");

        let service = MainService {
            executable: "uv".to_string(),
            args: vec!["run".to_string(), "--offline".to_string()],
            cwd: "source".to_string(),
            env: HashMap::new(),
            ports: HashMap::new(),
            health_port: None,
        };

        let isolated_env = prepare_isolated_host_environment(temp.path()).expect("isolated env");
        let mut command = Command::new("env");

        apply_isolated_command_env(&mut command, temp.path(), &service, &isolated_env);

        let envs = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().to_string(),
                    value.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect::<HashMap<_, _>>();

        assert_eq!(
            envs.get("UV_CACHE_DIR").and_then(|value| value.clone()),
            Some(cache_dir.to_string_lossy().to_string())
        );
    }

    #[test]
    fn apply_isolated_command_env_adds_smoke_site_packages_to_pythonpath() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let site_packages = source_dir.join(".ato-smoke-site-packages");
        fs::create_dir_all(&site_packages).expect("mkdir site-packages");

        let service = MainService {
            executable: "uv".to_string(),
            args: vec!["run".to_string(), "--offline".to_string()],
            cwd: "source".to_string(),
            env: HashMap::new(),
            ports: HashMap::new(),
            health_port: None,
        };

        let isolated_env = prepare_isolated_host_environment(temp.path()).expect("isolated env");
        let mut command = Command::new("env");

        apply_isolated_command_env(&mut command, temp.path(), &service, &isolated_env);

        let envs = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().to_string(),
                    value.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect::<HashMap<_, _>>();

        assert_eq!(
            envs.get("PYTHONPATH").and_then(|value| value.clone()),
            Some(site_packages.to_string_lossy().to_string())
        );
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

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::error::{CapsuleError, Result};
use crate::tsnet::TsnetEndpoint;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const ENV_SIDECAR_PATH: &str = "ATO_TSNETD_PATH";
const ENV_CONTROL_URL: &str = "ATO_TSNET_CONTROL_URL";
const ENV_AUTH_KEY: &str = "ATO_TSNET_AUTH_KEY";
const ENV_HOSTNAME: &str = "ATO_TSNET_HOSTNAME";
const ENV_SOCKS_PORT: &str = "ATO_TSNET_SOCKS_PORT";

#[cfg(unix)]
const ENV_GRPC_SOCKET: &str = "ATO_TSNET_GRPC_SOCKET";
#[cfg(windows)]
const ENV_GRPC_PIPE: &str = "ATO_TSNET_GRPC_PIPE";

#[cfg(windows)]
const DEFAULT_SIDECAR_BINARY: &str = "ato-tsnetd.exe";
#[cfg(not(windows))]
const DEFAULT_SIDECAR_BINARY: &str = "ato-tsnetd";

#[derive(Debug, Default, Clone)]
pub struct SidecarRequest {
    pub explicit_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SidecarBaseConfig {
    pub control_url: String,
    pub auth_key: String,
    pub hostname: String,
    pub socks_port: u16,
}

#[derive(Debug)]
pub struct SidecarSpawnConfig {
    pub endpoint: TsnetEndpoint,
    pub base_config: Option<SidecarBaseConfig>,
    pub stdout: Stdio,
    pub stderr: Stdio,
}

impl SidecarSpawnConfig {
    pub fn new(endpoint: TsnetEndpoint) -> Self {
        Self {
            endpoint,
            base_config: None,
            stdout: Stdio::inherit(),
            stderr: Stdio::inherit(),
        }
    }

    pub fn with_base_config(mut self, base_config: SidecarBaseConfig) -> Self {
        self.base_config = Some(base_config);
        self
    }

    pub fn with_stdio(mut self, stdout: Stdio, stderr: Stdio) -> Self {
        self.stdout = stdout;
        self.stderr = stderr;
        self
    }
}

pub fn discover_sidecar(req: SidecarRequest) -> Result<PathBuf> {
    // 1) Explicit CLI flag
    if let Some(path) = req.explicit_path {
        return validate_sidecar_path(path);
    }

    // 2) Environment override
    if let Ok(env_path) = std::env::var(ENV_SIDECAR_PATH) {
        if !env_path.trim().is_empty() {
            return validate_sidecar_path(PathBuf::from(env_path));
        }
    }

    // 3) Portable mode: look next to capsule binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(DEFAULT_SIDECAR_BINARY);
            if candidate.exists() {
                return validate_sidecar_path(candidate);
            }
        }
    }

    Err(CapsuleError::NotFound(
        "ato-tsnetd not found. Set ATO_TSNETD_PATH or place the binary next to the capsule binary"
            .to_string(),
    ))
}

pub fn spawn_sidecar(path: &Path, config: SidecarSpawnConfig) -> Result<Child> {
    let mut cmd = Command::new(path);
    cmd.stdout(config.stdout).stderr(config.stderr);

    match config.endpoint {
        #[cfg(unix)]
        TsnetEndpoint::Uds(socket_path) => {
            cmd.env(ENV_GRPC_SOCKET, socket_path);
        }
        #[cfg(windows)]
        TsnetEndpoint::NamedPipe(pipe) => {
            cmd.env(ENV_GRPC_PIPE, pipe);
        }
    }

    if let Some(base) = config.base_config {
        cmd.env(ENV_CONTROL_URL, base.control_url)
            .env(ENV_AUTH_KEY, base.auth_key)
            .env(ENV_HOSTNAME, base.hostname)
            .env(ENV_SOCKS_PORT, base.socks_port.to_string());
    }

    cmd.spawn()
        .map_err(|err| CapsuleError::ProcessStart(format!("failed to spawn ato-tsnetd: {err}")))
}

fn validate_sidecar_path(path: PathBuf) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .map_err(|err| CapsuleError::Config(format!("Failed to resolve sidecar path: {err}")))?;

    let meta = std::fs::metadata(&canonical)
        .map_err(|err| CapsuleError::Config(format!("Failed to stat sidecar path: {err}")))?;

    if !meta.is_file() {
        return Err(CapsuleError::Config(format!(
            "Sidecar path is not a file: {}",
            canonical.display()
        )));
    }

    #[cfg(unix)]
    {
        let mode = meta.permissions().mode();
        if (mode & 0o111) == 0 {
            return Err(CapsuleError::Config(format!(
                "Sidecar is not executable: {}",
                canonical.display()
            )));
        }
    }

    Ok(canonical)
}

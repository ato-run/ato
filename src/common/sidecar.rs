#[cfg(unix)]
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};
use capsule_core::{
    discover_sidecar, spawn_sidecar, wait_for_ready, SidecarBaseConfig, SidecarRequest,
    SidecarSpawnConfig, TsnetClient, TsnetConfig, TsnetEndpoint, TsnetHandle, TsnetState,
    TsnetWaitConfig,
};

const ENV_CONTROL_URL: &str = "ATO_TSNET_CONTROL_URL";
const ENV_AUTH_KEY: &str = "ATO_TSNET_AUTH_KEY";
const ENV_HOSTNAME: &str = "ATO_TSNET_HOSTNAME";
const ENV_SOCKS_PORT: &str = "ATO_TSNET_SOCKS_PORT";

#[cfg(unix)]
const ENV_GRPC_SOCKET: &str = "ATO_TSNET_GRPC_SOCKET";
#[cfg(windows)]
const ENV_GRPC_PIPE: &str = "ATO_TSNET_GRPC_PIPE";

pub fn maybe_start_sidecar() -> Result<Option<SidecarHandle>> {
    if std::env::var("CAPSULE_WATCH_MODE").is_ok() {
        return Ok(None);
    }

    let control_url = read_env(ENV_CONTROL_URL);
    let auth_key = read_env(ENV_AUTH_KEY);
    let hostname = read_env(ENV_HOSTNAME);

    if control_url.is_none() || auth_key.is_none() || hostname.is_none() {
        return Ok(None);
    }

    let socks_port = read_env(ENV_SOCKS_PORT)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);

    let endpoint = resolve_endpoint()?;
    let sidecar_path = discover_sidecar(SidecarRequest {
        explicit_path: None,
    })
    .context("ato-tsnetd not found")?;

    let base = SidecarBaseConfig {
        control_url: control_url.clone().unwrap(),
        auth_key: auth_key.clone().unwrap(),
        hostname: hostname.clone().unwrap(),
        socks_port,
    };

    let spawn_config = SidecarSpawnConfig::new(endpoint.clone())
        .with_base_config(base)
        .with_stdio(Stdio::inherit(), Stdio::inherit());

    let child = spawn_sidecar(&sidecar_path, spawn_config)?;

    let runtime = tokio::runtime::Runtime::new()?;
    let status = runtime.block_on(start_and_wait(
        endpoint.clone(),
        control_url.unwrap(),
        auth_key.unwrap(),
        hostname.unwrap(),
        socks_port,
    ))?;

    let resolved_port = status.socks_port.unwrap_or(socks_port);
    if resolved_port == 0 {
        anyhow::bail!("sidecar started without socks_port");
    }

    std::env::set_var(ENV_SOCKS_PORT, resolved_port.to_string());

    Ok(Some(SidecarHandle {
        endpoint,
        child: Some(child),
    }))
}

#[derive(Debug)]
pub struct SidecarHandle {
    endpoint: TsnetEndpoint,
    child: Option<Child>,
}

impl SidecarHandle {
    pub fn stop(mut self) -> Result<()> {
        let client = TsnetClient::from_endpoint(self.endpoint)?;
        let runtime = tokio::runtime::Runtime::new()?;
        let _ = runtime.block_on(client.stop());

        if let Some(child) = self.child.as_mut() {
            if child.try_wait()?.is_none() {
                let _ = child.kill();
            }
        }
        Ok(())
    }
}

async fn start_and_wait(
    endpoint: TsnetEndpoint,
    control_url: String,
    auth_key: String,
    hostname: String,
    socks_port: u16,
) -> Result<capsule_core::TsnetStatus> {
    let endpoint_for_client = endpoint.clone();
    let client = TsnetClient::from_endpoint(endpoint_for_client)?;
    let config = TsnetConfig {
        control_url,
        auth_key,
        hostname,
        socks_port,
        allow_net: vec![],
        endpoint,
    };

    let mut attempts = 0usize;
    let status = loop {
        match client.start(config.clone()).await {
            Ok(status) => break status,
            Err(capsule_core::CapsuleError::SidecarIpc(_)) if attempts < 10 => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(err) => return Err(err.into()),
        }
    };

    if status.state == TsnetState::Ready {
        return Ok(status);
    }

    let wait_config = TsnetWaitConfig::new(Duration::from_millis(200), Duration::from_secs(5));
    Ok(wait_for_ready(&client, wait_config).await?)
}

fn read_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn resolve_endpoint() -> Result<TsnetEndpoint> {
    #[cfg(unix)]
    {
        let socket = match std::env::var(ENV_GRPC_SOCKET) {
            Ok(value) if !value.trim().is_empty() => PathBuf::from(value.trim()),
            _ => {
                let path =
                    std::env::temp_dir().join(format!("ato-tsnetd-{}.sock", std::process::id()));
                std::env::set_var(ENV_GRPC_SOCKET, &path);
                path
            }
        };
        Ok(TsnetEndpoint::Uds(socket))
    }

    #[cfg(windows)]
    {
        let pipe = match std::env::var(ENV_GRPC_PIPE) {
            Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => {
                let pipe = format!("\\\\.\\pipe\\ato-tsnetd-{}", std::process::id());
                std::env::set_var(ENV_GRPC_PIPE, &pipe);
                pipe
            }
        };
        Ok(TsnetEndpoint::NamedPipe(pipe))
    }
}

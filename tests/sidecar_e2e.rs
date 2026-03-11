use std::process::{Child, Command, Stdio};
use std::time::Duration;

use capsule_core::{
    wait_for_ready, TsnetClient, TsnetConfig, TsnetEndpoint, TsnetHandle, TsnetState,
    TsnetWaitConfig,
};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const ENV_CONTROL_URL: &str = "ATO_TSNET_CONTROL_URL";
const ENV_AUTH_KEY: &str = "ATO_TSNET_AUTH_KEY";
const ENV_HOSTNAME: &str = "ATO_TSNET_HOSTNAME";
const ENV_SOCKS_PORT: &str = "ATO_TSNET_SOCKS_PORT";
const ENV_SIDECAR_PATH: &str = "ATO_TSNETD_PATH";

#[cfg(unix)]
const ENV_GRPC_SOCKET: &str = "ATO_TSNET_GRPC_SOCKET";
#[cfg(windows)]
const ENV_GRPC_PIPE: &str = "ATO_TSNET_GRPC_PIPE";

fn read_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn required_envs() -> Option<(String, String, String)> {
    let control = read_env(ENV_CONTROL_URL)?;
    let auth = read_env(ENV_AUTH_KEY)?;
    let hostname = read_env(ENV_HOSTNAME)?;
    Some((control, auth, hostname))
}

fn sidecar_path() -> Option<String> {
    read_env(ENV_SIDECAR_PATH)
}

fn spawn_sidecar(temp_dir: &TempDir) -> Option<Child> {
    let path = sidecar_path()?;
    let mut cmd = Command::new(path);
    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

    #[cfg(unix)]
    {
        let socket = temp_dir.path().join("ato-tsnetd.sock");
        cmd.env(ENV_GRPC_SOCKET, socket);
    }
    #[cfg(windows)]
    {
        let pipe = format!("\\\\.\\pipe\\ato-tsnetd-{}", std::process::id());
        cmd.env(ENV_GRPC_PIPE, pipe);
    }

    cmd.spawn().ok()
}

fn endpoint_from_tempdir(temp_dir: &TempDir) -> TsnetEndpoint {
    #[cfg(unix)]
    {
        let socket = temp_dir.path().join("ato-tsnetd.sock");
        TsnetEndpoint::Uds(socket)
    }
    #[cfg(windows)]
    {
        let pipe = std::env::var(ENV_GRPC_PIPE)
            .unwrap_or_else(|_| format!("\\\\.\\pipe\\ato-tsnetd-{}", std::process::id()));
        return TsnetEndpoint::NamedPipe(pipe);
    }
}

async fn start_sidecar_for_test(temp_dir: &TempDir) -> (Child, TsnetClient, TsnetEndpoint) {
    let child = spawn_sidecar(temp_dir).expect("failed to spawn ato-tsnetd");
    let endpoint = endpoint_from_tempdir(temp_dir);
    let client =
        TsnetClient::from_endpoint(endpoint.clone()).expect("failed to create tsnet client");
    (child, client, endpoint)
}

#[tokio::test]
async fn sidecar_starts_and_responds_to_socks5() {
    let Some((control_url, auth_key, hostname)) = required_envs() else {
        eprintln!("[skip] sidecar e2e requires ATO_TSNET_CONTROL_URL/ATO_TSNET_AUTH_KEY/ATO_TSNET_HOSTNAME");
        return;
    };

    let Some(_) = sidecar_path() else {
        eprintln!("[skip] sidecar e2e requires ATO_TSNETD_PATH");
        return;
    };

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let (mut child, client, endpoint) = start_sidecar_for_test(&temp_dir).await;

    let socks_port = read_env(ENV_SOCKS_PORT)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);

    let config = TsnetConfig {
        control_url,
        auth_key,
        hostname,
        socks_port,
        allow_net: vec![],
        endpoint,
    };

    let status = client.start(config).await.expect("start failed");
    let status = if status.state == TsnetState::Ready {
        status
    } else {
        let wait_config = TsnetWaitConfig::new(Duration::from_millis(200), Duration::from_secs(5));
        wait_for_ready(&client, wait_config)
            .await
            .expect("wait_for_ready failed")
    };

    let port = status.socks_port.expect("missing socks port");

    let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .expect("failed to connect to socks port");
    stream
        .write_all(&[0x05, 0x01, 0x00])
        .await
        .expect("write failed");

    let mut buf = [0u8; 2];
    stream.read_exact(&mut buf).await.expect("read failed");
    assert_eq!(buf, [0x05, 0x00]);

    let _ = client.stop().await;
    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test]
async fn sidecar_serve_lifecycle() {
    let Some((control_url, auth_key, hostname)) = required_envs() else {
        eprintln!("[skip] sidecar e2e requires ATO_TSNET_CONTROL_URL/ATO_TSNET_AUTH_KEY/ATO_TSNET_HOSTNAME");
        return;
    };

    let Some(_) = sidecar_path() else {
        eprintln!("[skip] sidecar e2e requires ATO_TSNETD_PATH");
        return;
    };

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let (mut child, client, endpoint) = start_sidecar_for_test(&temp_dir).await;

    let config = TsnetConfig {
        control_url,
        auth_key,
        hostname,
        socks_port: 0,
        allow_net: vec![],
        endpoint,
    };

    let status = client.start(config).await.expect("start failed");
    if status.state != TsnetState::Ready {
        let wait_config = TsnetWaitConfig::new(Duration::from_millis(200), Duration::from_secs(5));
        wait_for_ready(&client, wait_config)
            .await
            .expect("wait_for_ready failed");
    }

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind target listener");
    let target_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = listener.accept().await;
    });

    let serve = client
        .start_serve(format!("127.0.0.1:{target_port}"), 0)
        .await
        .expect("start_serve failed");
    assert!(serve.running);
    assert!(serve.listen_port.is_some());

    let status = client.serve_status().await.expect("serve_status failed");
    assert!(status.running);

    let stopped = client.stop_serve().await.expect("stop_serve failed");
    assert!(!stopped.running);

    let _ = client.stop().await;
    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test]
async fn sidecar_serve_rejects_non_loopback_target() {
    let Some((control_url, auth_key, hostname)) = required_envs() else {
        eprintln!("[skip] sidecar e2e requires ATO_TSNET_CONTROL_URL/ATO_TSNET_AUTH_KEY/ATO_TSNET_HOSTNAME");
        return;
    };

    let Some(_) = sidecar_path() else {
        eprintln!("[skip] sidecar e2e requires ATO_TSNETD_PATH");
        return;
    };

    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let mut child = spawn_sidecar(&temp_dir).expect("failed to spawn ato-tsnetd");

    let endpoint = endpoint_from_tempdir(&temp_dir);
    let client =
        TsnetClient::from_endpoint(endpoint.clone()).expect("failed to create tsnet client");

    let socks_port = read_env(ENV_SOCKS_PORT)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);

    let config = TsnetConfig {
        control_url,
        auth_key,
        hostname,
        socks_port,
        allow_net: vec![],
        endpoint,
    };

    let status = client.start(config).await.expect("start failed");
    if status.state != TsnetState::Ready {
        let wait_config = TsnetWaitConfig::new(Duration::from_millis(200), Duration::from_secs(5));
        wait_for_ready(&client, wait_config)
            .await
            .expect("wait_for_ready failed");
    }

    let err = client
        .start_serve("10.0.0.1:80".to_string(), 0)
        .await
        .expect_err("non-loopback target should be rejected");
    assert!(err.to_string().contains("target_addr"));

    let _ = client.stop().await;
    let _ = child.kill();
    let _ = child.wait();
}

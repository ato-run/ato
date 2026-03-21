use std::io::{BufRead, BufReader, Write};

use anyhow::Result;
use capsule_core::types::ReadinessProbe;

use super::spawn::ExternalCapsuleChild;
use super::{EXTERNAL_READY_INTERVAL, EXTERNAL_READY_TIMEOUT};

pub(super) fn wait_for_dependency_readiness(
    alias: &str,
    child: &mut ExternalCapsuleChild,
    port: Option<u16>,
    readiness_probe: Option<ReadinessProbe>,
) -> Result<()> {
    let deadline = std::time::Instant::now() + EXTERNAL_READY_TIMEOUT;
    loop {
        if let Some(status) = child.child.try_wait()? {
            anyhow::bail!(
                "external capsule dependency '{}' exited before becoming ready (exit code: {})",
                alias,
                status.code().unwrap_or(1)
            );
        }

        if let Some(port) = port {
            if let Some(probe) = readiness_probe.as_ref() {
                if readiness_probe_ok(probe, port)? {
                    return Ok(());
                }
            } else if tcp_probe("127.0.0.1", port) {
                return Ok(());
            }
        } else if readiness_probe.is_none()
            && std::time::Instant::now() + EXTERNAL_READY_INTERVAL >= deadline
        {
            return Ok(());
        }

        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "external capsule dependency '{}' readiness check timed out after {}s",
                alias,
                EXTERNAL_READY_TIMEOUT.as_secs()
            );
        }

        std::thread::sleep(EXTERNAL_READY_INTERVAL);
    }
}

fn readiness_probe_ok(probe: &ReadinessProbe, port: u16) -> Result<bool> {
    if let Some(path) = probe
        .http_get
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Ok(http_probe(path, port));
    }
    if let Some(target) = probe
        .tcp_connect
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Ok(matches!(target, "$PORT" | "PORT") && tcp_probe("127.0.0.1", port));
    }
    anyhow::bail!("readiness_probe must define http_get or tcp_connect")
}

fn http_probe(path: &str, port: u16) -> bool {
    if path.starts_with("http://") || path.starts_with("https://") {
        return false;
    }
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };

    let Ok(mut stream) = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_secs(1),
    ) else {
        return false;
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        path
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).is_ok()
        && (status_line.contains(" 200 ")
            || status_line.contains(" 201 ")
            || status_line.contains(" 204 "))
}

fn tcp_probe(host: &str, port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("{}:{}", host, port)
            .parse()
            .unwrap_or(std::net::SocketAddr::from(([127, 0, 0, 1], port))),
        std::time::Duration::from_secs(1),
    )
    .is_ok()
}

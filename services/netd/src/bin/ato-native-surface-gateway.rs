//! Runner sidecar: one bounded stdin configuration, ready JSON, stop on owner EOF.
//! Keys use the existing surface assertion keyring and never enter argv or output.

#[cfg(unix)]
mod unix {
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::{BufRead, Write};
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use ato_formation::native_surface::NativeSurfaceAdapter;
    use ato_ipc::session_surface::SurfaceExecutionScope;
    use netd::native_probe::{NativeProbeTarget, capture_and_start_native_probe};
    use netd::native_surface::{
        NativeSurfaceConfig, NativeSurfaceProbe, NativeSurfaceReady, start_ready_native_surface,
    };
    use netd::pixel_authorization::{HmacSurfaceAccessAuthorizer, SurfaceAssertionKeyring};
    use netd::pixel_gateway::PixelGatewayConfig;
    use netd::surface_authorization::SurfaceGatewayScope;
    use netd::terminal_gateway::TerminalGatewayConfig;
    use serde::Deserialize;
    use zeroize::Zeroizing;

    // Eight bounded byte-array operations may exceed 64KiB as JSON even
    // though their aggregate raw input is only 32KiB. Keep the whole config
    // bounded while admitting the shared catalog's maximum encoding.
    const MAX_CONFIG_BYTES: usize = 256 * 1024;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Config {
        #[serde(default)]
        execution: Option<SurfaceExecutionScope>,
        session_id: String,
        surface_id: String,
        listen_addr: SocketAddr,
        connect_url: String,
        origin: String,
        assertion_keys: BTreeMap<String, String>,
        idle_timeout_secs: u64,
        readiness_timeout_secs: u64,
        target: Target,
        #[serde(default)]
        application_probe: Option<ApplicationProbe>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ApplicationProbe {
        listen_addr: SocketAddr,
        adapter: NativeSurfaceAdapter,
    }
    #[derive(Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    enum Target {
        Pixel { private_rfb_addr: SocketAddr },
        Terminal { firecracker_vsock_uds: PathBuf },
    }

    fn read_config(reader: &mut impl BufRead) -> anyhow::Result<Config> {
        let mut line = Zeroizing::new(Vec::new());
        // take() bounds allocation even when an owner never writes a newline.
        std::io::Read::take(reader, (MAX_CONFIG_BYTES + 1) as u64).read_until(b'\n', &mut line)?;
        anyhow::ensure!(
            line.len() <= MAX_CONFIG_BYTES,
            "surface configuration exceeds limit"
        );
        let config: Config = serde_json::from_slice(&line)
            .map_err(|_| anyhow::anyhow!("invalid surface configuration"))?;
        anyhow::ensure!(
            config.listen_addr.ip().is_loopback() && config.listen_addr.port() != 0,
            "surface sidecar requires an assigned loopback listener"
        );
        anyhow::ensure!(
            (1..=120).contains(&config.readiness_timeout_secs),
            "invalid surface readiness timeout"
        );
        anyhow::ensure!(
            (1..=86_400).contains(&config.idle_timeout_secs),
            "invalid surface idle timeout"
        );
        anyhow::ensure!(
            config.application_probe.is_none() || config.execution.is_some(),
            "native application observations require an execution scope"
        );
        if let Some(execution) = &config.execution {
            for id in [&execution.run_id, &execution.lease_id, &execution.runner_id] {
                anyhow::ensure!(
                    !id.is_empty()
                        && id.len() <= 160
                        && id
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
                    "invalid surface execution scope"
                );
            }
        }
        Ok(config)
    }

    pub async fn run() -> anyhow::Result<()> {
        let mut owner = std::io::BufReader::new(std::io::stdin());
        let config = read_config(&mut owner)?;
        let scope = SurfaceGatewayScope {
            session_id: config.session_id,
            surface_id: config.surface_id,
        };
        if config.execution.is_some() {
            anyhow::ensure!(
                config.assertion_keys.len() == 1
                    && config.assertion_keys.contains_key("ato.runner-surface@1"),
                "execution requires the scoped Runner key"
            );
        }
        let keys = Arc::new(SurfaceAssertionKeyring::new(config.assertion_keys)?);
        let mut authorizer = HmacSurfaceAccessAuthorizer::new(Arc::clone(&keys));
        if let Some(execution) = config.execution.clone() {
            authorizer = authorizer.with_execution_scope(execution);
        }
        let allowed_origins = BTreeSet::from([config.origin.clone()]);
        let timeout = Duration::from_secs(config.readiness_timeout_secs);
        let probe_target = match &config.target {
            Target::Pixel { private_rfb_addr } => NativeProbeTarget::Pixel(*private_rfb_addr),
            Target::Terminal {
                firecracker_vsock_uds,
            } => NativeProbeTarget::Terminal(firecracker_vsock_uds.clone()),
        };
        let target = match config.target {
            Target::Pixel { private_rfb_addr } => NativeSurfaceConfig::Pixel(PixelGatewayConfig {
                listen_addr: config.listen_addr,
                private_rfb_addr,
                private_rfb_connect_timeout: timeout,
                scope: scope.clone(),
                allowed_origins,
            }),
            Target::Terminal {
                firecracker_vsock_uds,
            } => NativeSurfaceConfig::Terminal(TerminalGatewayConfig {
                listen_addr: config.listen_addr,
                firecracker_vsock_uds,
                guest_connect_timeout: timeout,
                scope: scope.clone(),
                allowed_origins,
            }),
        };
        match &target {
            NativeSurfaceConfig::Pixel(config) => config.validate()?,
            NativeSurfaceConfig::Terminal(config) => config.validate()?,
        }
        netd::rfb_probe::pixel_gateway_probe_url(&config.connect_url)?;
        // Watch ownership from the beginning of startup, including bounded
        // application actions and Surface readiness. Cancellation drops all
        // partial handles; it must not continue input after its owner exits.
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        std::thread::spawn(move || {
            let mut command = [0_u8; 1];
            let _ = std::io::Read::read(&mut owner, &mut command);
            let _ = tx.blocking_send(());
        });
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let startup = async {
            let application_probe = match config.application_probe {
                Some(probe) => Some(
                    capture_and_start_native_probe(probe.listen_addr, probe_target, &probe.adapter)
                        .await?,
                ),
                None => None,
            };
            // Issue after capture so the short readiness assertion cannot
            // expire while an authored application operation is running.
            let assertion =
                keys.issue_readiness_assertion_for_execution(&scope, config.execution)?;
            let handle = start_ready_native_surface(
                target,
                Arc::new(authorizer),
                NativeSurfaceProbe {
                    connect_url: &config.connect_url,
                    origin: &config.origin,
                    assertion: assertion.as_str(),
                },
                timeout,
                Duration::from_secs(config.idle_timeout_secs),
            )
            .await?;
            Ok::<_, anyhow::Error>((application_probe, handle))
        };
        let (mut application_probe, handle) = tokio::select! {
            biased;
            _ = rx.recv() => return Ok(()),
            _ = term.recv() => return Ok(()),
            _ = tokio::signal::ctrl_c() => return Ok(()),
            result = startup => result?,
        };
        let (kind, viewport) = match handle.ready {
            NativeSurfaceReady::Pixel(frame) => ("pixel_stream", Some((frame.width, frame.height))),
            NativeSurfaceReady::Terminal => ("terminal", None),
        };
        let mut ready = serde_json::json!({ "surface_ready": true, "kind": kind,
            "viewport": viewport, "contract_verified": false });
        if let Some(probe) = &application_probe {
            ready["application_probe_addr"] = serde_json::json!(probe.local_addr());
        }
        println!("{ready}");
        std::io::stdout().flush()?;
        loop {
            tokio::select! {
                _ = rx.recv() => break,
                _ = term.recv() => break,
                _ = tokio::signal::ctrl_c() => break,
                _ = tokio::time::sleep(Duration::from_millis(250)) => if handle.idle_expired() { break; },
            }
        }
        handle.stop().await?;
        if let Some(probe) = &mut application_probe {
            probe.stop().await?;
        }
        println!(
            "{}",
            serde_json::json!({"surface_stopped": true, "idle_expired": handle.idle_expired()})
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn configuration_is_bounded_and_parse_errors_do_not_echo_credentials() {
            let bytes = vec![b'x'; MAX_CONFIG_BYTES + 1];
            assert!(read_config(&mut std::io::Cursor::new(bytes)).is_err());
            let error = read_config(&mut std::io::Cursor::new(
                b"{\"secret-value\": \"operator-key\"}\n",
            ))
            .err()
            .unwrap();
            assert_eq!(error.to_string(), "invalid surface configuration");
            assert!(!error.to_string().contains("operator-key"));
        }
    }
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    unix::run().await
}

#[cfg(not(unix))]
fn main() {
    eprintln!("native surface sidecar requires a Unix Runner");
    std::process::exit(69);
}

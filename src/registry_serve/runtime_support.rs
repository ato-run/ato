use super::*;

pub(super) fn format_bind_error(addr: SocketAddr, err: &std::io::Error) -> String {
    let mut message = format!("Failed to bind {}: {}", addr, err);
    match err.kind() {
        ErrorKind::AddrInUse => {
            message.push_str(". Another process is already listening on that port. Try a different `--port` or inspect listeners with `lsof -nP -iTCP:<port> -sTCP:LISTEN`.");
        }
        ErrorKind::AddrNotAvailable => {
            message.push_str(". The requested host is not available on this machine.");
        }
        ErrorKind::PermissionDenied => {
            message.push_str(". Permission was denied while opening the socket.");
        }
        _ => {}
    }
    message
}

pub(super) async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

pub(super) fn spawn_registry_gc_worker(data_dir: PathBuf) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        let mut ticks: u64 = 0;
        loop {
            interval.tick().await;
            let store = match RegistryStore::open(&data_dir) {
                Ok(store) => store,
                Err(err) => {
                    tracing::warn!(
                        "registry gc worker failed to open store path={} error={}",
                        data_dir.display(),
                        err
                    );
                    continue;
                }
            };

            let now = Utc::now().to_rfc3339();
            match store.gc_tick(&now, 32) {
                Ok(tick) => {
                    if tick.deleted > 0 {
                        let vacuum_pages = (tick.deleted.saturating_mul(2)).max(1);
                        if let Err(err) = store.incremental_vacuum(vacuum_pages) {
                            tracing::warn!(
                                "registry gc incremental vacuum failed path={} error={}",
                                data_dir.display(),
                                err
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        "registry gc tick failed path={} error={}",
                        data_dir.display(),
                        err
                    );
                }
            }

            ticks = ticks.saturating_add(1);
            if ticks.is_multiple_of(60) {
                if let Err(err) = store.checkpoint_wal_truncate() {
                    tracing::warn!(
                        "registry gc checkpoint failed path={} error={}",
                        data_dir.display(),
                        err
                    );
                }
            }
        }
    });
}

//! Optional retention sweep.
//!
//! When `--retention-seconds` is set, a background task periodically removes
//! objects (and abandoned partial uploads) older than the retention window.
//! Retention is a deployment decision: by default capsulehub is persistent.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::watch;
use tracing::{info, warn};

use crate::storage::{BlobStore, StorageError, SweepReport};

const MINIMUM_INTERVAL: Duration = Duration::from_secs(1);
const MAXIMUM_INTERVAL: Duration = Duration::from_secs(300);

/// How often to sweep for a given retention window.
pub fn sweep_interval(retention: Duration) -> Duration {
    (retention / 2).clamp(MINIMUM_INTERVAL, MAXIMUM_INTERVAL)
}

/// Runs one sweep at the current time.
pub async fn sweep_once(
    store: &BlobStore,
    retention: Duration,
) -> Result<SweepReport, StorageError> {
    sweep_at(store, retention, SystemTime::now()).await
}

/// Runs one sweep as if it were `now` (used by tests to age objects without
/// touching their modification times).
pub async fn sweep_at(
    store: &BlobStore,
    retention: Duration,
    now: SystemTime,
) -> Result<SweepReport, StorageError> {
    store.sweep(retention, now).await
}

/// Background sweep loop. Stops when the shutdown channel signals.
pub async fn run_sweeper(
    store: Arc<BlobStore>,
    retention: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(sweep_interval(retention));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = ticker.tick() => match sweep_once(&store, retention).await {
                Ok(report) if report.removed_objects > 0 || report.removed_uploads > 0 => {
                    info!(
                        removed_objects = report.removed_objects,
                        removed_uploads = report.removed_uploads,
                        removed_bytes = report.removed_bytes,
                        "capsulehub retention sweep removed objects"
                    );
                }
                Ok(_) => {}
                Err(error) => warn!(%error, "capsulehub retention sweep failed"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::storage::ObjectKey;

    #[test]
    fn sweep_interval_is_bounded() {
        assert_eq!(sweep_interval(Duration::from_secs(3600)), MAXIMUM_INTERVAL);
        assert_eq!(sweep_interval(Duration::from_millis(10)), MINIMUM_INTERVAL);
        assert_eq!(
            sweep_interval(Duration::from_secs(120)),
            Duration::from_secs(60)
        );
    }

    #[tokio::test]
    async fn sweep_at_uses_the_supplied_clock() {
        let directory = tempfile::tempdir().unwrap();
        let store = BlobStore::open(directory.path(), 1024, None).unwrap();
        let payload = b"retained";
        let key =
            ObjectKey::parse(&format!("sha256:{}", hex::encode(Sha256::digest(payload)))).unwrap();
        let mut upload = store.begin_upload(&key).await.unwrap();
        upload.write(payload).await.unwrap();
        upload.finish().await.unwrap();

        let retention = Duration::from_secs(30);
        let now = SystemTime::now();
        assert_eq!(
            sweep_at(&store, retention, now).await.unwrap(),
            SweepReport::default()
        );
        let future = now + Duration::from_secs(60);
        assert_eq!(
            sweep_at(&store, retention, future)
                .await
                .unwrap()
                .removed_objects,
            1
        );
    }
}

//! Reference counting and idle-timeout management for shared IPC services.
//!
//! Each `SharedService` tracks how many clients are currently using it.
//! When the reference count drops to zero, an idle timer starts.
//! If no new client connects before `idle_timeout`, the service is stopped.
//!
//! ## Sharing Modes
//!
//! - **Singleton**: One instance shared by all clients. Idle timeout applies.
//! - **Exclusive**: One instance per client. Stopped when client disconnects.
//! - **Daemon**: Long-lived process. Idle timeout is ignored.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info};

use super::types::SharingMode;

/// State of a shared IPC service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// Service is running and has active clients.
    Active,
    /// Service is running but idle (no clients, timer ticking).
    Idle,
    /// Service has been requested to stop.
    Stopping,
    /// Service is not running.
    Stopped,
}

/// Reference-counted shared service handle.
///
/// Thread-safe: uses `AtomicU32` for the counter and `Notify` for
/// signaling idle-timeout cancellation.
#[derive(Debug)]
pub struct SharedService {
    /// Service name (for logging).
    pub name: String,
    /// Current reference count.
    ref_count: Arc<AtomicU32>,
    /// Sharing mode.
    pub sharing_mode: SharingMode,
    /// Idle timeout duration.
    pub idle_timeout: Duration,
    /// Notifier to cancel idle timer when a new client connects.
    idle_cancel: Arc<Notify>,
    /// Current state.
    state: ServiceState,
}

impl SharedService {
    /// Create a new shared service handle.
    pub fn new(name: String, sharing_mode: SharingMode, idle_timeout: Duration) -> Self {
        Self {
            name,
            ref_count: Arc::new(AtomicU32::new(0)),
            sharing_mode,
            idle_timeout,
            idle_cancel: Arc::new(Notify::new()),
            state: ServiceState::Active,
        }
    }

    /// Acquire a reference (client connected).
    ///
    /// Increments the reference count and cancels any pending idle timer.
    /// Returns the new reference count.
    pub fn acquire(&mut self) -> u32 {
        let prev = self.ref_count.fetch_add(1, Ordering::SeqCst);
        let new_count = prev + 1;
        debug!(
            service = %self.name,
            ref_count = new_count,
            "IPC service reference acquired"
        );

        // Cancel any pending idle timer
        self.idle_cancel.notify_one();
        self.state = ServiceState::Active;

        new_count
    }

    /// Release a reference (client disconnected).
    ///
    /// Decrements the reference count. If it reaches zero:
    /// - **Exclusive**: Immediately returns `true` (should stop).
    /// - **Singleton**: Starts idle timer. Returns `false` (caller should
    ///   call `wait_for_idle_timeout()`).
    /// - **Daemon**: Returns `false` (never stops due to idle).
    ///
    /// Returns `true` if the service should be stopped immediately.
    pub fn release(&mut self) -> bool {
        let prev = self.ref_count.fetch_sub(1, Ordering::SeqCst);
        let new_count = prev.saturating_sub(1);
        debug!(
            service = %self.name,
            ref_count = new_count,
            "IPC service reference released"
        );

        if new_count == 0 {
            match self.sharing_mode {
                SharingMode::Exclusive => {
                    self.state = ServiceState::Stopping;
                    return true;
                }
                SharingMode::Singleton => {
                    self.state = ServiceState::Idle;
                    // Caller should call wait_for_idle_timeout()
                }
                SharingMode::Daemon => {
                    // Daemon mode: never stop due to idle
                    self.state = ServiceState::Active;
                }
            }
        }

        false
    }

    /// Wait for the idle timeout to expire.
    ///
    /// Returns `true` if the timeout expired (service should be stopped).
    /// Returns `false` if a new client connected (timer cancelled).
    ///
    /// Only meaningful for `Singleton` mode when ref_count == 0.
    pub async fn wait_for_idle_timeout(&self) -> bool {
        if self.sharing_mode == SharingMode::Daemon {
            return false;
        }

        let cancel = self.idle_cancel.clone();
        let timeout = self.idle_timeout;
        let name = self.name.clone();

        info!(
            service = %name,
            timeout_secs = timeout.as_secs(),
            "IPC service idle, starting shutdown timer"
        );

        tokio::select! {
            _ = tokio::time::sleep(timeout) => {
                // Check if still idle
                if self.ref_count.load(Ordering::SeqCst) == 0 {
                    info!(service = %name, "IPC service idle timeout expired, stopping");
                    true
                } else {
                    debug!(service = %name, "IPC service got new client during timeout");
                    false
                }
            }
            _ = cancel.notified() => {
                debug!(service = %name, "IPC service idle timer cancelled (new client)");
                false
            }
        }
    }

    /// Get the current reference count.
    pub fn current_ref_count(&self) -> u32 {
        self.ref_count.load(Ordering::SeqCst)
    }

    /// Get the current state.
    pub fn state(&self) -> ServiceState {
        self.state
    }

    /// Mark the service as stopped.
    pub fn mark_stopped(&mut self) {
        self.state = ServiceState::Stopped;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acquire_release_singleton() {
        let mut svc = SharedService::new(
            "test-svc".to_string(),
            SharingMode::Singleton,
            Duration::from_secs(30),
        );

        assert_eq!(svc.current_ref_count(), 0);

        let count = svc.acquire();
        assert_eq!(count, 1);
        assert_eq!(svc.state(), ServiceState::Active);

        let count = svc.acquire();
        assert_eq!(count, 2);

        let should_stop = svc.release();
        assert!(!should_stop);
        assert_eq!(svc.current_ref_count(), 1);

        let should_stop = svc.release();
        assert!(!should_stop);
        assert_eq!(svc.current_ref_count(), 0);
        assert_eq!(svc.state(), ServiceState::Idle);
    }

    #[test]
    fn test_exclusive_mode_stops_immediately() {
        let mut svc = SharedService::new(
            "exclusive-svc".to_string(),
            SharingMode::Exclusive,
            Duration::from_secs(30),
        );

        svc.acquire();
        let should_stop = svc.release();
        assert!(should_stop, "Exclusive mode should stop immediately");
        assert_eq!(svc.state(), ServiceState::Stopping);
    }

    #[test]
    fn test_daemon_mode_never_stops() {
        let mut svc = SharedService::new(
            "daemon-svc".to_string(),
            SharingMode::Daemon,
            Duration::from_secs(30),
        );

        svc.acquire();
        let should_stop = svc.release();
        assert!(!should_stop, "Daemon mode should never stop due to idle");
        assert_eq!(svc.state(), ServiceState::Active);
    }

    #[tokio::test]
    async fn test_idle_timeout_expires() {
        let svc = SharedService::new(
            "idle-test".to_string(),
            SharingMode::Singleton,
            Duration::from_millis(50),
        );

        let expired = svc.wait_for_idle_timeout().await;
        assert!(expired, "Timeout should expire when ref_count is 0");
    }

    #[tokio::test]
    async fn test_idle_timeout_cancelled() {
        let mut svc = SharedService::new(
            "cancel-test".to_string(),
            SharingMode::Singleton,
            Duration::from_secs(60),
        );

        let idle_cancel = svc.idle_cancel.clone();

        // Spawn a task that cancels the timer quickly
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            idle_cancel.notify_one();
        });

        let expired = svc.wait_for_idle_timeout().await;
        assert!(!expired, "Timeout should be cancelled");

        // Acquire to update state
        svc.acquire();
        assert_eq!(svc.state(), ServiceState::Active);
    }

    #[test]
    fn test_mark_stopped() {
        let mut svc = SharedService::new(
            "stop-test".to_string(),
            SharingMode::Singleton,
            Duration::from_secs(30),
        );

        svc.mark_stopped();
        assert_eq!(svc.state(), ServiceState::Stopped);
    }
}

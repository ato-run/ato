//! Terminal session manager for interactive PTY sessions.
//!
//! Enforces:
//! - Maximum 4 concurrent terminal sessions
//! - 30-minute idle timeout (no input received)
//! - 4-hour absolute session lifetime
//! - 1000 msg/sec input rate limit (token bucket per session)
//! - Audit-log lifecycle events via tracing

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{info, warn};

const MAX_SESSIONS: usize = 4;
const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_SESSION_LIFETIME: Duration = Duration::from_secs(4 * 60 * 60);
/// Token bucket capacity and refill rate (messages per second)
const RATE_LIMIT_CAPACITY: u32 = 1000;
const RATE_LIMIT_REFILL_PER_SEC: f64 = 1000.0;

#[derive(Debug, Clone)]
pub struct TerminalSession {
    pub session_id: String,
    pub capsule_handle: String,
    pub cols: u16,
    pub rows: u16,
    pub created_at: Instant,
    pub last_input_at: Instant,
    /// Token bucket counter (fractional tokens accumulated)
    rate_tokens: f64,
    /// Last time rate tokens were refilled
    rate_last_refill: Instant,
}

impl TerminalSession {
    fn new(session_id: String, capsule_handle: String, cols: u16, rows: u16) -> Self {
        let now = Instant::now();
        Self {
            session_id,
            capsule_handle,
            cols,
            rows,
            created_at: now,
            last_input_at: now,
            rate_tokens: RATE_LIMIT_CAPACITY as f64,
            rate_last_refill: now,
        }
    }

    /// Return true if this session has exceeded its idle or absolute lifetime.
    pub fn is_expired(&self) -> bool {
        let now = Instant::now();
        now.duration_since(self.last_input_at) > IDLE_TIMEOUT
            || now.duration_since(self.created_at) > MAX_SESSION_LIFETIME
    }

    /// Consume one input token. Returns false (and drops the message) if rate-limited.
    pub fn consume_rate_token(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.rate_last_refill).as_secs_f64();
        self.rate_tokens = (self.rate_tokens + elapsed * RATE_LIMIT_REFILL_PER_SEC)
            .min(RATE_LIMIT_CAPACITY as f64);
        self.rate_last_refill = now;

        if self.rate_tokens >= 1.0 {
            self.rate_tokens -= 1.0;
            self.last_input_at = now;
            true
        } else {
            false
        }
    }

    /// Record input activity (without consuming a rate token — for resize/signal).
    pub fn touch(&mut self) {
        self.last_input_at = Instant::now();
    }
}

/// Thread-safe terminal session registry.
#[derive(Clone, Default)]
pub struct TerminalSessionManager {
    inner: Arc<Mutex<TerminalSessionManagerInner>>,
}

#[derive(Default)]
struct TerminalSessionManagerInner {
    sessions: HashMap<String, TerminalSession>,
}

impl TerminalSessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new session. Returns `Err` if the session limit is reached.
    pub fn create_session(
        &self,
        session_id: String,
        capsule_handle: String,
        cols: u16,
        rows: u16,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        // Evict expired sessions first
        inner.evict_expired();

        if inner.sessions.len() >= MAX_SESSIONS {
            warn!(
                session_id = %session_id,
                "Terminal session limit ({}) reached; rejecting new session",
                MAX_SESSIONS
            );
            return Err(format!("Terminal session limit ({MAX_SESSIONS}) reached"));
        }

        info!(
            session_id = %session_id,
            capsule = %capsule_handle,
            cols,
            rows,
            "Terminal session created"
        );
        inner.sessions.insert(
            session_id.clone(),
            TerminalSession::new(session_id, capsule_handle, cols, rows),
        );
        Ok(())
    }

    /// Remove a session and emit audit log.
    pub fn remove_session(&self, session_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(session) = inner.sessions.remove(session_id) {
            let lifetime = Instant::now().duration_since(session.created_at);
            info!(
                session_id = %session_id,
                capsule = %session.capsule_handle,
                lifetime_secs = lifetime.as_secs(),
                "Terminal session ended"
            );
        }
    }

    /// Consume one rate-limit token for the given session.
    /// Returns `false` if the message should be dropped (rate limited or session not found).
    pub fn consume_input_token(&self, session_id: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if let Some(session) = inner.sessions.get_mut(session_id) {
            if session.is_expired() {
                warn!(session_id = %session_id, "Terminal session expired; dropping input");
                inner.sessions.remove(session_id);
                return false;
            }
            let allowed = session.consume_rate_token();
            if !allowed {
                warn!(session_id = %session_id, "Terminal input rate-limited; dropping message");
            }
            allowed
        } else {
            false
        }
    }

    /// Record activity for resize/signal without rate limiting.
    pub fn touch_session(&self, session_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(session) = inner.sessions.get_mut(session_id) {
            session.touch();
        }
    }

    /// Update cols/rows after a resize.
    pub fn update_size(&self, session_id: &str, cols: u16, rows: u16) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(session) = inner.sessions.get_mut(session_id) {
            session.cols = cols;
            session.rows = rows;
            session.touch();
        }
    }

    /// Returns true if the session exists and is not expired.
    pub fn is_active(&self, session_id: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if let Some(session) = inner.sessions.get(session_id) {
            if session.is_expired() {
                warn!(session_id = %session_id, "Terminal session has expired (checked)");
                inner.sessions.remove(session_id);
                return false;
            }
            true
        } else {
            false
        }
    }

    /// Return count of active sessions.
    pub fn active_count(&self) -> usize {
        let mut inner = self.inner.lock().unwrap();
        inner.evict_expired();
        inner.sessions.len()
    }
}

impl TerminalSessionManagerInner {
    fn evict_expired(&mut self) {
        let expired: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.is_expired())
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            if let Some(session) = self.sessions.remove(id) {
                warn!(
                    session_id = %id,
                    capsule = %session.capsule_handle,
                    "Terminal session evicted due to timeout"
                );
            }
        }
    }
}

/// Result of non-blocking output polling from a terminal session.
pub enum TryRecvOutput {
    /// Base64-encoded output chunk.
    Data(String),
    /// No output available right now.
    Empty,
    /// Output channel is closed.
    Disconnected,
}

/// Core terminal I/O contract independent from the UI surface.
pub trait TerminalCore: Send {
    fn session_id(&self) -> &str;
    fn send_input(&self, data: Vec<u8>) -> bool;
    fn send_resize(&self, cols: u16, rows: u16) -> bool;
    fn try_recv_output(&self) -> TryRecvOutput;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_remove_session() {
        let mgr = TerminalSessionManager::new();
        mgr.create_session("s1".into(), "myapp".into(), 80, 24)
            .unwrap();
        assert!(mgr.is_active("s1"));
        mgr.remove_session("s1");
        assert!(!mgr.is_active("s1"));
    }

    #[test]
    fn test_session_limit() {
        let mgr = TerminalSessionManager::new();
        for i in 0..MAX_SESSIONS {
            mgr.create_session(format!("s{i}"), "app".into(), 80, 24)
                .unwrap();
        }
        let result = mgr.create_session("overflow".into(), "app".into(), 80, 24);
        assert!(result.is_err());
    }

    #[test]
    fn test_rate_limit_allows_burst() {
        let mgr = TerminalSessionManager::new();
        mgr.create_session("s1".into(), "app".into(), 80, 24)
            .unwrap();
        // First 1000 tokens should succeed
        for _ in 0..100 {
            assert!(mgr.consume_input_token("s1"));
        }
    }

    #[test]
    fn test_unknown_session_returns_false() {
        let mgr = TerminalSessionManager::new();
        assert!(!mgr.consume_input_token("nonexistent"));
    }

    #[test]
    fn test_update_size() {
        let mgr = TerminalSessionManager::new();
        mgr.create_session("s1".into(), "app".into(), 80, 24)
            .unwrap();
        mgr.update_size("s1", 132, 50);
        // No panic; session still active
        assert!(mgr.is_active("s1"));
    }
}

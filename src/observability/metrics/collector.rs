//! Pull-based metrics collector for Engine observability.
//!
//! This replaces the old push-based UsageReporter that sent data to Coordinator.
//! Metrics are now exposed via `/metrics` endpoint for Prometheus scraping.

use super::MetricsCollector as PrometheusMetrics;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;
use tracing::{debug, warn};

/// Usage record for a single capsule session
#[derive(Debug, Clone)]
pub struct UsageRecord {
    pub capsule_id: String,
    pub user_id: Option<String>,
    pub start_time: SystemTime,
    pub end_time: Option<SystemTime>,
    pub gpu_hours: f64,
}

/// MetricsCollector replaces UsageReporter for pull-based observability.
///
/// Instead of pushing usage reports to Coordinator, this:
/// 1. Tracks capsule usage locally
/// 2. Exposes metrics via Prometheus `/metrics` endpoint
/// 3. Stores usage history for audit/billing queries (optional)
pub struct MetricsCollector {
    prometheus: Arc<PrometheusMetrics>,
    active_sessions: RwLock<HashMap<String, UsageRecord>>,
    completed_sessions: RwLock<Vec<UsageRecord>>,
    /// Maximum completed sessions to retain in memory (for audit queries)
    max_history: usize,
}

impl MetricsCollector {
    /// Create a new MetricsCollector
    pub fn new() -> Self {
        let prometheus = PrometheusMetrics::new().unwrap_or_else(|e| {
            warn!(
                "Failed to initialize Prometheus metrics: {}. Using fallback.",
                e
            );
            // Create a minimal fallback - in production this should not happen
            PrometheusMetrics::new().expect("Fallback metrics init failed")
        });

        Self {
            prometheus: Arc::new(prometheus),
            active_sessions: RwLock::new(HashMap::new()),
            completed_sessions: RwLock::new(Vec::new()),
            max_history: 1000, // Retain last 1000 completed sessions
        }
    }

    /// Start tracking a capsule session
    pub fn start_tracking(&self, capsule_id: &str, user_id: Option<String>) {
        let record = UsageRecord {
            capsule_id: capsule_id.to_string(),
            user_id,
            start_time: SystemTime::now(),
            end_time: None,
            gpu_hours: 0.0,
        };

        if let Ok(mut sessions) = self.active_sessions.write() {
            sessions.insert(capsule_id.to_string(), record);
            self.prometheus.inc_capsule_count();
            self.prometheus
                .set_capsule_status(capsule_id, "running", 1.0);
            debug!("Started tracking capsule: {}", capsule_id);
        }
    }

    /// Stop tracking a capsule session and compute usage
    pub fn stop_tracking(&self, capsule_id: &str) -> Option<UsageRecord> {
        let mut record = {
            let mut sessions = self.active_sessions.write().ok()?;
            sessions.remove(capsule_id)?
        };

        let end_time = SystemTime::now();
        record.end_time = Some(end_time);

        // Calculate GPU hours
        if let Ok(duration) = end_time.duration_since(record.start_time) {
            record.gpu_hours = duration.as_secs_f64() / 3600.0;
        }

        // Update Prometheus metrics
        self.prometheus.dec_capsule_count();
        self.prometheus
            .set_capsule_status(capsule_id, "running", 0.0);
        self.prometheus
            .set_capsule_status(capsule_id, "stopped", 1.0);

        // Store in completed sessions
        if let Ok(mut completed) = self.completed_sessions.write() {
            completed.push(record.clone());
            // Trim history if needed
            if completed.len() > self.max_history {
                completed.remove(0);
            }
        }

        debug!(
            "Stopped tracking capsule: {}, gpu_hours: {:.4}",
            capsule_id, record.gpu_hours
        );

        Some(record)
    }

    /// Update GPU metrics from hardware detection
    pub fn update_gpu_metrics(&self, total_bytes: u64, used_bytes: u64) {
        let available = total_bytes.saturating_sub(used_bytes);
        self.prometheus.set_gpu_vram_metrics(
            total_bytes as f64,
            used_bytes as f64,
            available as f64,
        );
    }

    /// Get Prometheus metrics in text format for `/metrics` endpoint
    pub fn gather_prometheus(&self) -> Result<String> {
        self.prometheus.gather()
    }

    /// Get active session count
    pub fn active_session_count(&self) -> usize {
        self.active_sessions.read().map(|s| s.len()).unwrap_or(0)
    }

    /// Get completed sessions for billing/audit queries
    pub fn get_completed_sessions(&self, limit: Option<usize>) -> Vec<UsageRecord> {
        self.completed_sessions
            .read()
            .map(|sessions| {
                let limit = limit.unwrap_or(sessions.len());
                sessions.iter().rev().take(limit).cloned().collect()
            })
            .unwrap_or_default()
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn test_tracking_lifecycle() {
        let collector = MetricsCollector::new();

        // Start tracking
        collector.start_tracking("test-capsule-1", Some("user-123".to_string()));
        assert_eq!(collector.active_session_count(), 1);

        // Small delay to ensure measurable duration
        sleep(Duration::from_millis(10));

        // Stop tracking
        let record = collector.stop_tracking("test-capsule-1");
        assert!(record.is_some());
        assert_eq!(collector.active_session_count(), 0);

        let record = record.unwrap();
        assert_eq!(record.capsule_id, "test-capsule-1");
        assert!(record.gpu_hours > 0.0);
    }

    #[test]
    fn test_prometheus_gather() {
        let collector = MetricsCollector::new();
        collector.start_tracking("test-capsule", None);

        let metrics = collector.gather_prometheus();
        assert!(metrics.is_ok());

        let text = metrics.unwrap();
        assert!(text.contains("capsule_count"));
    }
}

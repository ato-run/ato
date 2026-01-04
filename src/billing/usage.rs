//! Usage Tracking for Billing
//!
//! Tracks API usage metrics for billing purposes:
//! - Token counts (input/output)
//! - Request counts
//! - GPU time (minutes)
//! - Model usage

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock as StdRwLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Usage metrics for a single request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestUsage {
    /// Unique request ID
    pub request_id: String,
    /// Customer ID
    pub customer_id: String,
    /// Model/Capsule used
    pub model: String,
    /// Input tokens
    pub input_tokens: u64,
    /// Output tokens
    pub output_tokens: u64,
    /// Total tokens
    pub total_tokens: u64,
    /// Request duration in milliseconds
    pub duration_ms: u64,
    /// Whether this was a cloud request
    pub is_cloud: bool,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// Aggregated usage for a customer
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CustomerUsage {
    /// Total input tokens
    pub total_input_tokens: u64,
    /// Total output tokens
    pub total_output_tokens: u64,
    /// Total requests
    pub total_requests: u64,
    /// Total GPU time in seconds
    pub total_gpu_seconds: u64,
    /// Usage by model
    pub by_model: HashMap<String, ModelUsage>,
    /// Period start (Unix epoch)
    pub period_start: u64,
    /// Period end (Unix epoch)
    pub period_end: u64,
}

/// Usage for a specific model
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub requests: u64,
    pub gpu_seconds: u64,
}

/// Active capsule session for GPU time tracking
#[derive(Debug, Clone)]
struct ActiveSession {
    _capsule_id: String,
    customer_id: String,
    model: String,
    started_at: Instant,
    _start_timestamp: u64,
}

/// Simple tracking info for sync API (capsule_manager compatibility)
#[derive(Debug, Clone)]
struct SimpleSession {
    started_at: SystemTime,
}

/// Usage tracker that records and aggregates usage metrics
pub struct UsageTracker {
    /// Active capsule sessions (async API)
    active_sessions: RwLock<HashMap<String, ActiveSession>>,
    /// Simple session tracking (sync API for capsule_manager)
    simple_sessions: StdRwLock<HashMap<String, SimpleSession>>,
    /// Request history (bounded buffer)
    request_history: RwLock<Vec<RequestUsage>>,
    /// Aggregated usage by customer
    customer_usage: RwLock<HashMap<String, CustomerUsage>>,
    /// Maximum history size
    max_history_size: usize,
}

impl UsageTracker {
    pub fn new() -> Self {
        Self::with_capacity(10000)
    }

    pub fn with_capacity(max_history_size: usize) -> Self {
        Self {
            active_sessions: RwLock::new(HashMap::new()),
            simple_sessions: StdRwLock::new(HashMap::new()),
            request_history: RwLock::new(Vec::with_capacity(max_history_size)),
            customer_usage: RwLock::new(HashMap::new()),
            max_history_size,
        }
    }

    // ==========================================
    // Sync API (for capsule_manager compatibility)
    // ==========================================

    /// Start tracking a capsule session (sync version for capsule_manager)
    pub fn start_tracking(&self, capsule_id: String) {
        let session = SimpleSession {
            started_at: SystemTime::now(),
        };
        if let Ok(mut sessions) = self.simple_sessions.write() {
            sessions.insert(capsule_id.clone(), session);
            debug!("Started simple tracking for capsule {}", capsule_id);
        }
    }

    /// Stop tracking a capsule session (sync version for capsule_manager)
    /// Returns the start time for reporting
    pub fn stop_tracking(&self, capsule_id: &str) -> Option<SystemTime> {
        if let Ok(mut sessions) = self.simple_sessions.write() {
            let session = sessions.remove(capsule_id)?;
            debug!("Stopped simple tracking for capsule {}", capsule_id);
            Some(session.started_at)
        } else {
            None
        }
    }

    // ==========================================
    // Async API (for advanced usage tracking)
    // ==========================================

    /// Start tracking a capsule session (for GPU time)
    pub async fn start_session(&self, session_id: &str, customer_id: &str, model: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let session = ActiveSession {
            _capsule_id: session_id.to_string(),
            customer_id: customer_id.to_string(),
            model: model.to_string(),
            started_at: Instant::now(),
            _start_timestamp: now,
        };

        self.active_sessions
            .write()
            .await
            .insert(session_id.to_string(), session);

        debug!(
            "Started session {} for customer {}",
            session_id, customer_id
        );
    }

    /// Stop tracking a capsule session and return GPU seconds used
    pub async fn stop_session(&self, session_id: &str) -> Option<u64> {
        let session = self.active_sessions.write().await.remove(session_id)?;
        let duration = session.started_at.elapsed();
        let gpu_seconds = duration.as_secs();

        // Update customer usage
        let mut usage = self.customer_usage.write().await;
        let customer_usage = usage
            .entry(session.customer_id.clone())
            .or_insert_with(CustomerUsage::default);

        customer_usage.total_gpu_seconds += gpu_seconds;

        let model_usage = customer_usage
            .by_model
            .entry(session.model.clone())
            .or_insert_with(ModelUsage::default);
        model_usage.gpu_seconds += gpu_seconds;

        info!(
            "Stopped session {} for customer {}: {} GPU seconds",
            session_id, session.customer_id, gpu_seconds
        );

        Some(gpu_seconds)
    }

    /// Record a completed request
    pub async fn record_request(&self, usage: RequestUsage) {
        let customer_id = usage.customer_id.clone();
        let model = usage.model.clone();

        // Update aggregated usage
        {
            let mut customer_usage = self.customer_usage.write().await;
            let agg = customer_usage
                .entry(customer_id.clone())
                .or_insert_with(CustomerUsage::default);

            agg.total_input_tokens += usage.input_tokens;
            agg.total_output_tokens += usage.output_tokens;
            agg.total_requests += 1;

            let model_agg = agg
                .by_model
                .entry(model.clone())
                .or_insert_with(ModelUsage::default);
            model_agg.input_tokens += usage.input_tokens;
            model_agg.output_tokens += usage.output_tokens;
            model_agg.requests += 1;
        }

        debug!(
            "Recorded request for customer {}: {} input, {} output tokens",
            customer_id, usage.input_tokens, usage.output_tokens
        );

        // Add to history (with bounded size)
        {
            let mut history = self.request_history.write().await;
            if history.len() >= self.max_history_size {
                history.remove(0);
            }
            history.push(usage);
        }
    }

    /// Get usage for a customer
    pub async fn get_customer_usage(&self, customer_id: &str) -> Option<CustomerUsage> {
        self.customer_usage.read().await.get(customer_id).cloned()
    }

    /// Get all customer usage (for reporting)
    pub async fn get_all_usage(&self) -> HashMap<String, CustomerUsage> {
        self.customer_usage.read().await.clone()
    }

    /// Get recent requests for a customer
    pub async fn get_recent_requests(&self, customer_id: &str, limit: usize) -> Vec<RequestUsage> {
        self.request_history
            .read()
            .await
            .iter()
            .filter(|r| r.customer_id == customer_id)
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    /// Reset usage for a customer (e.g., at billing period start)
    pub async fn reset_customer_usage(&self, customer_id: &str) {
        self.customer_usage.write().await.remove(customer_id);
        info!("Reset usage for customer {}", customer_id);
    }

    /// Get active session count
    pub async fn active_session_count(&self) -> usize {
        self.active_sessions.read().await.len()
    }

    /// Export usage data for billing
    pub async fn export_for_billing(&self, customer_id: &str) -> Option<BillingExport> {
        let usage = self.get_customer_usage(customer_id).await?;

        Some(BillingExport {
            customer_id: customer_id.to_string(),
            total_tokens: usage.total_input_tokens + usage.total_output_tokens,
            input_tokens: usage.total_input_tokens,
            output_tokens: usage.total_output_tokens,
            requests: usage.total_requests,
            gpu_minutes: usage.total_gpu_seconds / 60,
            period_start: usage.period_start,
            period_end: usage.period_end,
            exported_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        })
    }
}

impl Default for UsageTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Billing export data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingExport {
    pub customer_id: String,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub requests: u64,
    pub gpu_minutes: u64,
    pub period_start: u64,
    pub period_end: u64,
    pub exported_at: u64,
}

/// Builder for creating RequestUsage
pub struct RequestUsageBuilder {
    request_id: String,
    customer_id: String,
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    started_at: Instant,
    is_cloud: bool,
}

impl RequestUsageBuilder {
    pub fn new(
        request_id: impl Into<String>,
        customer_id: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            customer_id: customer_id.into(),
            model: model.into(),
            input_tokens: 0,
            output_tokens: 0,
            started_at: Instant::now(),
            is_cloud: false,
        }
    }

    pub fn input_tokens(mut self, tokens: u64) -> Self {
        self.input_tokens = tokens;
        self
    }

    pub fn output_tokens(mut self, tokens: u64) -> Self {
        self.output_tokens = tokens;
        self
    }

    pub fn is_cloud(mut self, is_cloud: bool) -> Self {
        self.is_cloud = is_cloud;
        self
    }

    pub fn build(self) -> RequestUsage {
        let duration = self.started_at.elapsed();
        RequestUsage {
            request_id: self.request_id,
            customer_id: self.customer_id,
            model: self.model,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            total_tokens: self.input_tokens + self.output_tokens,
            duration_ms: duration.as_millis() as u64,
            is_cloud: self.is_cloud,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_record_request() {
        let tracker = UsageTracker::new();

        let usage = RequestUsage {
            request_id: "req_1".to_string(),
            customer_id: "cus_1".to_string(),
            model: "gpt-4".to_string(),
            input_tokens: 100,
            output_tokens: 200,
            total_tokens: 300,
            duration_ms: 500,
            is_cloud: true,
            timestamp: 1234567890,
        };

        tracker.record_request(usage).await;

        let customer_usage = tracker.get_customer_usage("cus_1").await.unwrap();
        assert_eq!(customer_usage.total_input_tokens, 100);
        assert_eq!(customer_usage.total_output_tokens, 200);
        assert_eq!(customer_usage.total_requests, 1);
    }

    #[tokio::test]
    async fn test_session_tracking() {
        let tracker = UsageTracker::new();

        tracker
            .start_session("session_1", "cus_1", "llama-70b")
            .await;
        assert_eq!(tracker.active_session_count().await, 1);

        // Simulate some time passing
        tokio::time::sleep(Duration::from_millis(100)).await;

        let gpu_seconds = tracker.stop_session("session_1").await;
        assert!(gpu_seconds.is_some());
        assert_eq!(tracker.active_session_count().await, 0);
    }

    #[tokio::test]
    async fn test_multiple_requests_same_customer() {
        let tracker = UsageTracker::new();

        for i in 0..5 {
            let usage = RequestUsage {
                request_id: format!("req_{}", i),
                customer_id: "cus_1".to_string(),
                model: "gpt-4".to_string(),
                input_tokens: 100,
                output_tokens: 200,
                total_tokens: 300,
                duration_ms: 500,
                is_cloud: false,
                timestamp: 1234567890 + i,
            };
            tracker.record_request(usage).await;
        }

        let customer_usage = tracker.get_customer_usage("cus_1").await.unwrap();
        assert_eq!(customer_usage.total_input_tokens, 500);
        assert_eq!(customer_usage.total_output_tokens, 1000);
        assert_eq!(customer_usage.total_requests, 5);
    }

    #[tokio::test]
    async fn test_usage_by_model() {
        let tracker = UsageTracker::new();

        // Request with model A
        tracker
            .record_request(RequestUsage {
                request_id: "req_1".to_string(),
                customer_id: "cus_1".to_string(),
                model: "model-a".to_string(),
                input_tokens: 100,
                output_tokens: 100,
                total_tokens: 200,
                duration_ms: 100,
                is_cloud: false,
                timestamp: 0,
            })
            .await;

        // Request with model B
        tracker
            .record_request(RequestUsage {
                request_id: "req_2".to_string(),
                customer_id: "cus_1".to_string(),
                model: "model-b".to_string(),
                input_tokens: 200,
                output_tokens: 200,
                total_tokens: 400,
                duration_ms: 100,
                is_cloud: false,
                timestamp: 0,
            })
            .await;

        let usage = tracker.get_customer_usage("cus_1").await.unwrap();
        assert_eq!(usage.by_model.len(), 2);
        assert_eq!(usage.by_model["model-a"].requests, 1);
        assert_eq!(usage.by_model["model-b"].requests, 1);
    }

    #[tokio::test]
    async fn test_request_usage_builder() {
        let usage = RequestUsageBuilder::new("req_1", "cus_1", "gpt-4")
            .input_tokens(100)
            .output_tokens(200)
            .is_cloud(true)
            .build();

        assert_eq!(usage.request_id, "req_1");
        assert_eq!(usage.customer_id, "cus_1");
        assert_eq!(usage.model, "gpt-4");
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 200);
        assert_eq!(usage.total_tokens, 300);
        assert!(usage.is_cloud);
    }

    #[tokio::test]
    async fn test_export_for_billing() {
        let tracker = UsageTracker::new();

        tracker
            .record_request(RequestUsage {
                request_id: "req_1".to_string(),
                customer_id: "cus_1".to_string(),
                model: "gpt-4".to_string(),
                input_tokens: 1000,
                output_tokens: 2000,
                total_tokens: 3000,
                duration_ms: 1000,
                is_cloud: true,
                timestamp: 0,
            })
            .await;

        let export = tracker.export_for_billing("cus_1").await.unwrap();
        assert_eq!(export.customer_id, "cus_1");
        assert_eq!(export.total_tokens, 3000);
        assert_eq!(export.input_tokens, 1000);
        assert_eq!(export.output_tokens, 2000);
        assert_eq!(export.requests, 1);
    }

    #[tokio::test]
    async fn test_reset_customer_usage() {
        let tracker = UsageTracker::new();

        tracker
            .record_request(RequestUsage {
                request_id: "req_1".to_string(),
                customer_id: "cus_1".to_string(),
                model: "gpt-4".to_string(),
                input_tokens: 100,
                output_tokens: 100,
                total_tokens: 200,
                duration_ms: 100,
                is_cloud: false,
                timestamp: 0,
            })
            .await;

        assert!(tracker.get_customer_usage("cus_1").await.is_some());

        tracker.reset_customer_usage("cus_1").await;

        assert!(tracker.get_customer_usage("cus_1").await.is_none());
    }
}

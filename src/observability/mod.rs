//! Observability modules (UARC L5)
//!
//! This module provides observability and monitoring capabilities:
//! - audit: Cryptographically signed audit logging
//! - job_history: Job execution history persistence
//! - logs: Log collection and streaming
//! - metrics: Prometheus metrics collection

pub mod audit;
pub mod job_history;
pub mod logs;
pub mod metrics;

// Re-export commonly used types
pub use audit::{AuditLogger, AuditOperation, AuditStatus};
pub use job_history::{JobHistory, JobPhase, JobRecord, SqliteJobHistoryStore};
pub use logs::{LogCollector, LogEntry, LogStream};
pub use metrics::MetricsCollector;

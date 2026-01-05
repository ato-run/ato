mod prometheus_metrics;
pub mod collector;

pub use prometheus_metrics::{register_metrics, MetricsCollector};

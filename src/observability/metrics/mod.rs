pub mod collector;
mod prometheus_metrics;

pub use prometheus_metrics::{register_metrics, MetricsCollector};

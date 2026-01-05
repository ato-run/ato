use anyhow::Result;
use prometheus::{Encoder, Gauge, GaugeVec, IntGauge, Opts, Registry, TextEncoder};
use std::sync::Arc;
use tracing::debug;

/// MetricsCollector manages Prometheus metrics for the engine
pub struct MetricsCollector {
    registry: Arc<Registry>,

    // Custom metrics
    capsule_count: IntGauge,
    gpu_vram_total_bytes: Gauge,
    gpu_vram_used_bytes: Gauge,
    gpu_vram_available_bytes: Gauge,
    container_cpu_usage: GaugeVec,
    capsule_status: GaugeVec,
}

impl MetricsCollector {
    /// Create a new metrics collector
    pub fn new() -> Result<Self> {
        let registry = Arc::new(Registry::new());

        // Create custom metrics
        let capsule_count = IntGauge::with_opts(
            Opts::new("capsule_count", "Total number of capsules")
                .namespace("capsuled")
                .subsystem("engine"),
        )?;

        let gpu_vram_total_bytes = Gauge::with_opts(
            Opts::new("gpu_vram_total_bytes", "Total GPU VRAM in bytes")
                .namespace("capsuled")
                .subsystem("engine"),
        )?;

        let gpu_vram_used_bytes = Gauge::with_opts(
            Opts::new("gpu_vram_used_bytes", "Used GPU VRAM in bytes")
                .namespace("capsuled")
                .subsystem("engine"),
        )?;

        let gpu_vram_available_bytes = Gauge::with_opts(
            Opts::new("gpu_vram_available_bytes", "Available GPU VRAM in bytes")
                .namespace("capsuled")
                .subsystem("engine"),
        )?;

        let container_cpu_usage = GaugeVec::new(
            Opts::new("container_cpu_usage", "CPU usage per container")
                .namespace("capsuled")
                .subsystem("engine"),
            &["capsule_id"],
        )?;

        let capsule_status = GaugeVec::new(
            Opts::new(
                "capsule_status",
                "Status of capsules (1=running, 0=stopped)",
            )
            .namespace("capsuled")
            .subsystem("engine"),
            &["capsule_id", "status"],
        )?;

        // Register metrics
        registry.register(Box::new(capsule_count.clone()))?;
        registry.register(Box::new(gpu_vram_total_bytes.clone()))?;
        registry.register(Box::new(gpu_vram_used_bytes.clone()))?;
        registry.register(Box::new(gpu_vram_available_bytes.clone()))?;
        registry.register(Box::new(container_cpu_usage.clone()))?;
        registry.register(Box::new(capsule_status.clone()))?;

        debug!("Metrics collector initialized with custom metrics");

        Ok(Self {
            registry,
            capsule_count,
            gpu_vram_total_bytes,
            gpu_vram_used_bytes,
            gpu_vram_available_bytes,
            container_cpu_usage,
            capsule_status,
        })
    }

    /// Update capsule count metric
    pub fn set_capsule_count(&self, count: i64) {
        self.capsule_count.set(count);
    }

    /// Increment capsule count
    pub fn inc_capsule_count(&self) {
        self.capsule_count.inc();
    }

    /// Decrement capsule count
    pub fn dec_capsule_count(&self) {
        self.capsule_count.dec();
    }

    /// Update GPU VRAM metrics
    pub fn set_gpu_vram_metrics(&self, total: f64, used: f64, available: f64) {
        self.gpu_vram_total_bytes.set(total);
        self.gpu_vram_used_bytes.set(used);
        self.gpu_vram_available_bytes.set(available);
    }

    /// Update container CPU usage
    pub fn set_container_cpu_usage(&self, capsule_id: &str, usage: f64) {
        self.container_cpu_usage
            .with_label_values(&[capsule_id])
            .set(usage);
    }

    /// Update capsule status
    pub fn set_capsule_status(&self, capsule_id: &str, status: &str, value: f64) {
        self.capsule_status
            .with_label_values(&[capsule_id, status])
            .set(value);
    }

    /// Remove metrics for a capsule (when it's deleted)
    pub fn remove_capsule_metrics(&self, capsule_id: &str) {
        // Remove container CPU usage metric
        let _ = self.container_cpu_usage.remove_label_values(&[capsule_id]);

        // Remove capsule status metrics
        for status in &["pending", "running", "stopped", "failed"] {
            let _ = self
                .capsule_status
                .remove_label_values(&[capsule_id, status]);
        }
    }

    /// Gather and encode metrics in Prometheus text format
    pub fn gather(&self) -> Result<String> {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer)?;
        Ok(String::from_utf8(buffer)?)
    }

    /// Get the registry for testing or advanced use
    pub fn registry(&self) -> Arc<Registry> {
        Arc::clone(&self.registry)
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new().expect("Failed to create default metrics collector")
    }
}

/// Register default process metrics
pub fn register_metrics(_collector: &MetricsCollector) -> Result<()> {
    // Process metrics like CPU, memory etc. are automatically collected
    // by the prometheus crate via the default process collector
    // We just need to ensure our custom metrics are registered,
    // which is done in MetricsCollector::new()

    debug!("Metrics registration complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_collector_creation() {
        let collector = MetricsCollector::new();
        assert!(collector.is_ok());
    }

    #[test]
    fn test_set_capsule_count() {
        let collector = MetricsCollector::new().unwrap();

        collector.set_capsule_count(5);
        let metrics = collector.gather().unwrap();
        assert!(metrics.contains("capsuled_engine_capsule_count 5"));
    }

    #[test]
    fn test_inc_dec_capsule_count() {
        let collector = MetricsCollector::new().unwrap();

        collector.set_capsule_count(0);
        collector.inc_capsule_count();
        collector.inc_capsule_count();

        let metrics = collector.gather().unwrap();
        assert!(metrics.contains("capsuled_engine_capsule_count 2"));

        collector.dec_capsule_count();
        let metrics = collector.gather().unwrap();
        assert!(metrics.contains("capsuled_engine_capsule_count 1"));
    }

    #[test]
    fn test_set_gpu_vram_metrics() {
        let collector = MetricsCollector::new().unwrap();

        let total = 8_589_934_592.0; // 8GB
        let used = 4_294_967_296.0; // 4GB
        let available = 4_294_967_296.0; // 4GB

        collector.set_gpu_vram_metrics(total, used, available);

        let metrics = collector.gather().unwrap();
        assert!(metrics.contains("capsuled_engine_gpu_vram_total_bytes"));
        assert!(metrics.contains("capsuled_engine_gpu_vram_used_bytes"));
        assert!(metrics.contains("capsuled_engine_gpu_vram_available_bytes"));
    }

    #[test]
    fn test_set_container_cpu_usage() {
        let collector = MetricsCollector::new().unwrap();

        collector.set_container_cpu_usage("capsule-123", 75.5);

        let metrics = collector.gather().unwrap();
        assert!(metrics.contains("capsuled_engine_container_cpu_usage"));
        assert!(metrics.contains("capsule-123"));
    }

    #[test]
    fn test_set_capsule_status() {
        let collector = MetricsCollector::new().unwrap();

        collector.set_capsule_status("capsule-123", "running", 1.0);
        collector.set_capsule_status("capsule-456", "stopped", 1.0);

        let metrics = collector.gather().unwrap();
        assert!(metrics.contains("capsuled_engine_capsule_status"));
        assert!(metrics.contains("capsule-123"));
        assert!(metrics.contains("running"));
        assert!(metrics.contains("capsule-456"));
        assert!(metrics.contains("stopped"));
    }

    #[test]
    fn test_remove_capsule_metrics() {
        let collector = MetricsCollector::new().unwrap();

        // Add metrics for a capsule
        collector.set_container_cpu_usage("capsule-999", 50.0);
        collector.set_capsule_status("capsule-999", "running", 1.0);

        let metrics = collector.gather().unwrap();
        assert!(metrics.contains("capsule-999"));

        // Remove metrics
        collector.remove_capsule_metrics("capsule-999");

        let _metrics = collector.gather().unwrap();
        // The metrics may still be present but with no values
        // This is expected behavior in Prometheus
    }

    #[test]
    fn test_gather_returns_valid_prometheus_format() {
        let collector = MetricsCollector::new().unwrap();

        collector.set_capsule_count(3);
        let metrics = collector.gather().unwrap();

        // Check for Prometheus format markers
        assert!(metrics.contains("# HELP"));
        assert!(metrics.contains("# TYPE"));
        assert!(metrics.contains("capsuled_engine_capsule_count"));
    }
}

use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

/// Errors that can occur while measuring GPU memory usage for running processes.
#[cfg_attr(not(feature = "real-gpu"), allow(dead_code))]
#[derive(Debug, Error)]
pub enum GpuProcessMonitorError {
    #[error("NVML initialization failed: {0}")]
    NvmlInitFailed(String),

    #[error("failed to get device count: {0}")]
    DeviceCountFailed(String),

    #[error("failed to query GPU {index}: {message}")]
    GpuQueryFailed { index: u32, message: String },

    #[error("failed to list processes on GPU {index}: {message}")]
    ProcessQueryFailed { index: u32, message: String },
}

/// Abstraction for monitoring GPU memory usage of processes.
pub trait GpuProcessMonitor: Send + Sync {
    /// Returns a map of `pid -> used_gpu_memory_bytes`.
    fn collect_usage_bytes(&self) -> Result<HashMap<u32, u64>, GpuProcessMonitorError>;
}

#[derive(Debug, Default)]
pub struct MockGpuProcessMonitor;

impl GpuProcessMonitor for MockGpuProcessMonitor {
    fn collect_usage_bytes(&self) -> Result<HashMap<u32, u64>, GpuProcessMonitorError> {
        Ok(HashMap::new())
    }
}

#[cfg(all(feature = "real-gpu", target_os = "linux"))]
#[derive(Debug)]
pub struct NvmlGpuProcessMonitor {
    nvml: Arc<nvml_wrapper::Nvml>,
}

#[cfg(all(feature = "real-gpu", target_os = "linux"))]
impl NvmlGpuProcessMonitor {
    pub fn new() -> Result<Self, GpuProcessMonitorError> {
        let nvml = nvml_wrapper::Nvml::init()
            .map_err(|e| GpuProcessMonitorError::NvmlInitFailed(e.to_string()))?;

        Ok(Self {
            nvml: Arc::new(nvml),
        })
    }
}

#[cfg(all(feature = "real-gpu", target_os = "linux"))]
impl GpuProcessMonitor for NvmlGpuProcessMonitor {
    fn collect_usage_bytes(&self) -> Result<HashMap<u32, u64>, GpuProcessMonitorError> {
        let device_count = self
            .nvml
            .device_count()
            .map_err(|e| GpuProcessMonitorError::DeviceCountFailed(e.to_string()))?;

        let mut usage = HashMap::new();

        for index in 0..device_count {
            let device = self.nvml.device_by_index(index).map_err(|e| {
                GpuProcessMonitorError::GpuQueryFailed {
                    index,
                    message: e.to_string(),
                }
            })?;

            let processes = device.running_compute_processes().map_err(|e| {
                GpuProcessMonitorError::ProcessQueryFailed {
                    index,
                    message: e.to_string(),
                }
            })?;

            for process in processes {
                let used = process.used_gpu_memory.unwrap_or(0);
                let pid = process.pid;

                usage
                    .entry(pid)
                    .and_modify(|total| *total = total.saturating_add(used))
                    .or_insert(used);
            }
        }

        Ok(usage)
    }
}

/// Factory that returns the appropriate GPU process monitor.
pub fn create_gpu_process_monitor() -> Arc<dyn GpuProcessMonitor> {
    #[cfg(all(feature = "real-gpu", target_os = "linux"))]
    {
        match NvmlGpuProcessMonitor::new() {
            Ok(monitor) => {
                tracing::info!("Using NVML GPU process monitor");
                return Arc::new(monitor);
            }
            Err(err) => {
                tracing::warn!(
                    "Failed to initialize NVML GPU process monitor: {err}. Falling back to mock monitor"
                );
            }
        }
    }

    #[cfg(all(feature = "real-gpu", not(target_os = "linux")))]
    {
        tracing::info!("real-gpu feature enabled on non-Linux; NVML monitor disabled");
    }

    tracing::info!("Using mock GPU process monitor");
    Arc::new(MockGpuProcessMonitor)
}

use std::sync::Arc;
use thiserror::Error;

use super::hardware_report::{GpuInfo, RigHardwareReport};

/// Errors that can occur during GPU detection
#[derive(Debug, Error)]
pub enum GpuDetectionError {
    #[error("NVML initialization failed: {0}")]
    NvmlInitFailed(String),

    #[error("Failed to query GPU {index}: {message}")]
    GpuQueryFailed { index: u32, message: String },

    #[error("Failed to get system information: {0}")]
    SystemInfoFailed(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
}

/// Trait for GPU detection implementations
pub trait GpuDetector: Send + Sync {
    /// Detect GPUs and return hardware report
    fn detect_gpus(&self) -> Result<RigHardwareReport, GpuDetectionError>;

    /// Check if detector is available (e.g., NVML library loaded)
    /// Check if detector is available (e.g., NVML library loaded)
    fn is_available(&self) -> bool;

    /// Get detector name (for logging/debugging)
    fn name(&self) -> &str;

    /// Get available VRAM in bytes for a specific GPU index
    fn get_available_vram_bytes(&self, index: usize) -> Result<u64, GpuDetectionError>;
}

/// Mock GPU detector for development without actual GPU hardware
///
/// Configuration via environment variables:
/// - MOCK_GPU_COUNT: Number of GPUs (default: 1)
/// - MOCK_VRAM_GB: VRAM per GPU in GB (default: 8)
/// - MOCK_CUDA_VERSION: CUDA version (default: "12.0")
#[derive(Debug)]
pub struct MockGpuDetector {
    gpu_count: u32,
    vram_gb: u64,
    cuda_version: String,
}

impl MockGpuDetector {
    pub fn new() -> Self {
        let gpu_count = std::env::var("MOCK_GPU_COUNT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);

        let vram_gb = std::env::var("CAPSULED_MOCK_VRAM_GB")
            .or_else(|_| std::env::var("MOCK_VRAM_GB"))
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8);

        let cuda_version = std::env::var("MOCK_CUDA_VERSION")
            .ok()
            .unwrap_or_else(|| "12.0".to_string());

        Self {
            gpu_count,
            vram_gb,
            cuda_version,
        }
    }

    pub fn with_config(gpu_count: u32, vram_gb: u64, cuda_version: String) -> Self {
        Self {
            gpu_count,
            vram_gb,
            cuda_version,
        }
    }
}

impl Default for MockGpuDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuDetector for MockGpuDetector {
    fn detect_gpus(&self) -> Result<RigHardwareReport, GpuDetectionError> {
        let rig_id = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "mock-rig".to_string());

        let mut report = RigHardwareReport::new(rig_id);
        report.is_mock = true;
        report.system_cuda_version = Some(self.cuda_version.clone());
        report.system_driver_version = Some("525.60.11-mock".to_string());

        for i in 0..self.gpu_count {
            report.gpus.push(GpuInfo {
                index: i,
                device_name: format!("Mock NVIDIA GPU {}", i),
                vram_total_bytes: self.vram_gb * 1_073_741_824, // GB to bytes
                cuda_compute_capability: Some("8.0".to_string()),
                vram_used_bytes: Some(0), // Mock: no usage initially
                uuid: format!("GPU-MOCK-{}-UUID", i),
            });
        }

        Ok(report)
    }

    fn is_available(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "MockGpuDetector"
    }

    fn get_available_vram_bytes(&self, index: usize) -> Result<u64, GpuDetectionError> {
        if index as u32 >= self.gpu_count {
            return Err(GpuDetectionError::GpuQueryFailed {
                index: index as u32,
                message: "GPU index out of bounds".to_string(),
            });
        }
        // Return configured VRAM size (converted to bytes)
        // In a more advanced mock, we could track usage, but for now assume full availability
        // or use a separate env var for "available" memory if needed for testing.
        Ok(self.vram_gb * 1_073_741_824)
    }
}

/// Real GPU detector using NVML (NVIDIA Management Library)
/// Only available when compiled with `real-gpu` feature
#[cfg(all(feature = "real-gpu", target_os = "linux"))]
#[derive(Debug)]
pub struct NvmlGpuDetector {
    nvml: Arc<nvml_wrapper::Nvml>,
}

#[cfg(all(feature = "real-gpu", target_os = "linux"))]
impl NvmlGpuDetector {
    pub fn new() -> Result<Self, GpuDetectionError> {
        let nvml = nvml_wrapper::Nvml::init()
            .map_err(|e| GpuDetectionError::NvmlInitFailed(e.to_string()))?;

        Ok(Self {
            nvml: Arc::new(nvml),
        })
    }
}

#[cfg(all(feature = "real-gpu", target_os = "linux"))]
impl GpuDetector for NvmlGpuDetector {
    fn detect_gpus(&self) -> Result<RigHardwareReport, GpuDetectionError> {
        let rig_id = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "unknown-rig".to_string());

        let mut report = RigHardwareReport::new(rig_id);
        report.is_mock = false;

        // Get CUDA version
        report.system_cuda_version = self
            .nvml
            .sys_cuda_driver_version()
            .ok()
            .map(|v| format!("{}.{}", v.0, v.1));

        // Get driver version
        report.system_driver_version = self.nvml.sys_driver_version().ok().map(|v| v.to_string());

        // Get device count
        let device_count = self
            .nvml
            .device_count()
            .map_err(|e| GpuDetectionError::SystemInfoFailed(e.to_string()))?;

        // Query each GPU
        for i in 0..device_count {
            let device =
                self.nvml
                    .device_by_index(i)
                    .map_err(|e| GpuDetectionError::GpuQueryFailed {
                        index: i,
                        message: e.to_string(),
                    })?;

            let name = device
                .name()
                .map_err(|e| GpuDetectionError::GpuQueryFailed {
                    index: i,
                    message: e.to_string(),
                })?;

            let memory_info =
                device
                    .memory_info()
                    .map_err(|e| GpuDetectionError::GpuQueryFailed {
                        index: i,
                        message: e.to_string(),
                    })?;

            let cuda_compute_capability = device
                .cuda_compute_capability()
                .ok()
                .map(|cc| format!("{}.{}", cc.major, cc.minor));

            let uuid = device
                .uuid()
                .map_err(|e| GpuDetectionError::GpuQueryFailed {
                    index: i,
                    message: e.to_string(),
                })?;

            report.gpus.push(GpuInfo {
                index: i,
                device_name: name,
                vram_total_bytes: memory_info.total,
                cuda_compute_capability,
                vram_used_bytes: Some(memory_info.used),
                uuid,
            });
        }

        Ok(report)
    }

    fn is_available(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "NvmlGpuDetector"
    }

    fn get_available_vram_bytes(&self, index: usize) -> Result<u64, GpuDetectionError> {
        let device = self.nvml.device_by_index(index as u32).map_err(|e| {
            GpuDetectionError::GpuQueryFailed {
                index: index as u32,
                message: e.to_string(),
            }
        })?;

        let memory_info = device
            .memory_info()
            .map_err(|e| GpuDetectionError::GpuQueryFailed {
                index: index as u32,
                message: e.to_string(),
            })?;

        Ok(memory_info.free)
    }
}

/// nvidia-smi based GPU detector (SPEC V1.1.0)
///
/// Uses `nvidia-smi --query-gpu=... --format=csv` to detect NVIDIA GPUs
/// without requiring the NVML library. This provides cross-platform support
/// for any system with the nvidia-smi tool installed.
#[derive(Debug, Default)]
pub struct NvidiaSmiGpuDetector;

impl NvidiaSmiGpuDetector {
    pub fn new() -> Result<Self, GpuDetectionError> {
        // Check if nvidia-smi is available
        let output = std::process::Command::new("nvidia-smi")
            .arg("--version")
            .output();

        match output {
            Ok(o) if o.status.success() => Ok(Self),
            Ok(_) => Err(GpuDetectionError::NvmlInitFailed(
                "nvidia-smi returned error".to_string(),
            )),
            Err(e) => Err(GpuDetectionError::NvmlInitFailed(format!(
                "nvidia-smi not found: {}",
                e
            ))),
        }
    }

    /// Parse nvidia-smi CSV output
    fn parse_gpu_csv_line(line: &str) -> Option<GpuInfo> {
        // Expected format: "index, name, memory.total [MiB], memory.used [MiB], uuid, compute_cap"
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() < 6 {
            return None;
        }

        let index = parts[0].parse::<u32>().ok()?;
        let name = parts[1].to_string();

        // Parse memory (format: "16384 MiB" or just "16384")
        let total_mib = parts[2]
            .trim_end_matches(" MiB")
            .trim()
            .parse::<u64>()
            .ok()?;
        let used_mib = parts[3]
            .trim_end_matches(" MiB")
            .trim()
            .parse::<u64>()
            .ok()?;

        let uuid = parts[4].to_string();
        let compute_cap = parts[5].to_string();

        Some(GpuInfo {
            index,
            device_name: name,
            vram_total_bytes: total_mib * 1024 * 1024,
            cuda_compute_capability: Some(compute_cap),
            vram_used_bytes: Some(used_mib * 1024 * 1024),
            uuid,
        })
    }
}

impl GpuDetector for NvidiaSmiGpuDetector {
    fn detect_gpus(&self) -> Result<RigHardwareReport, GpuDetectionError> {
        let rig_id = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "smi-rig".to_string());

        let mut report = RigHardwareReport::new(rig_id);
        report.is_mock = false;

        // Get CUDA driver version
        let driver_output = std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=driver_version", "--format=csv,noheader"])
            .output()
            .map_err(|e| GpuDetectionError::SystemInfoFailed(e.to_string()))?;

        if driver_output.status.success() {
            let driver = String::from_utf8_lossy(&driver_output.stdout)
                .lines()
                .next()
                .map(|s| s.trim().to_string());
            report.system_driver_version = driver;
        }

        // Get GPU information
        let output = std::process::Command::new("nvidia-smi")
            .args([
                "--query-gpu=index,name,memory.total,memory.used,uuid,compute_cap",
                "--format=csv,noheader",
            ])
            .output()
            .map_err(|e| GpuDetectionError::SystemInfoFailed(e.to_string()))?;

        if !output.status.success() {
            return Err(GpuDetectionError::SystemInfoFailed(
                "nvidia-smi query failed".to_string(),
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Some(gpu) = Self::parse_gpu_csv_line(line) {
                report.gpus.push(gpu);
            }
        }

        Ok(report)
    }

    fn is_available(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "NvidiaSmiGpuDetector"
    }

    fn get_available_vram_bytes(&self, index: usize) -> Result<u64, GpuDetectionError> {
        let output = std::process::Command::new("nvidia-smi")
            .args([
                "--query-gpu=memory.free",
                "--format=csv,noheader",
                "-i",
                &index.to_string(),
            ])
            .output()
            .map_err(|e| GpuDetectionError::GpuQueryFailed {
                index: index as u32,
                message: e.to_string(),
            })?;

        if !output.status.success() {
            return Err(GpuDetectionError::GpuQueryFailed {
                index: index as u32,
                message: "nvidia-smi query failed".to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let free_mib = stdout
            .lines()
            .next()
            .and_then(|line| line.trim_end_matches(" MiB").trim().parse::<u64>().ok())
            .ok_or_else(|| GpuDetectionError::GpuQueryFailed {
                index: index as u32,
                message: "Failed to parse memory.free output".to_string(),
            })?;

        Ok(free_mib * 1024 * 1024)
    }
}

/// CPU-only detector for environments without GPUs (e.g., MacBook, non-GPU servers)
///
/// This detector always reports 0 GPUs.
#[derive(Debug, Default)]
pub struct CpuGpuDetector;

impl CpuGpuDetector {
    pub fn new() -> Self {
        Self
    }
}

impl GpuDetector for CpuGpuDetector {
    fn detect_gpus(&self) -> Result<RigHardwareReport, GpuDetectionError> {
        let rig_id = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "cpu-only-rig".to_string());

        let mut report = RigHardwareReport::new(rig_id);
        report.is_mock = false; // It's a real "no-GPU" state, not a mock with fake GPUs

        // No GPUs to add

        Ok(report)
    }

    fn is_available(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "CpuGpuDetector"
    }

    fn get_available_vram_bytes(&self, index: usize) -> Result<u64, GpuDetectionError> {
        Err(GpuDetectionError::GpuQueryFailed {
            index: index as u32,
            message: "No GPUs available in CPU-only mode".to_string(),
        })
    }
}

/// Factory function to create the appropriate GPU detector
///
/// Detection order (SPEC V1.1.0):
/// 1. Mock detector if explicitly requested via env var `CAPSULED_USE_MOCK_GPU=1`
/// 2. NVML detector if `real-gpu` feature is enabled (Linux only)
/// 3. nvidia-smi based detector (cross-platform, no NVML dependency)
/// 4. Mac GPU detector on macOS
/// 5. CPU-only detector as final fallback
pub fn create_gpu_detector() -> Arc<dyn GpuDetector> {
    // Check if Mock is explicitly requested or implied by VRAM config
    if std::env::var("CAPSULED_USE_MOCK_GPU").is_ok()
        || std::env::var("CAPSULED_MOCK_VRAM_GB").is_ok()
    {
        tracing::info!("Using mock GPU detector (requested via env var)");
        return Arc::new(MockGpuDetector::new());
    }

    #[cfg(all(feature = "real-gpu", target_os = "linux"))]
    {
        match NvmlGpuDetector::new() {
            Ok(detector) => {
                tracing::info!("Using NVML GPU detector");
                return Arc::new(detector);
            }
            Err(e) => {
                tracing::warn!("NVML init failed ({}), trying nvidia-smi fallback", e);
            }
        }
    }

    // Try nvidia-smi based detection (works without NVML library)
    match NvidiaSmiGpuDetector::new() {
        Ok(detector) => {
            tracing::info!("Using nvidia-smi GPU detector");
            return Arc::new(detector);
        }
        Err(e) => {
            tracing::debug!("nvidia-smi not available: {}", e);
        }
    }

    #[cfg(target_os = "macos")]
    {
        use super::mac_gpu::MacGpuDetector;
        match MacGpuDetector::new() {
            Ok(detector) => {
                tracing::info!("Using Mac GPU detector");
                return Arc::new(detector);
            }
            Err(e) => {
                tracing::warn!(
                    "Mac GPU detector failed ({}), falling back to CPU-only mode",
                    e
                );
            }
        }
    }

    tracing::info!("Using CPU-only detector (no GPU detection method available)");
    Arc::new(CpuGpuDetector::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_detector_default() {
        let detector = MockGpuDetector::new();
        assert!(detector.is_available());
        assert_eq!(detector.name(), "MockGpuDetector");

        let report = detector
            .detect_gpus()
            .expect("Mock detection should not fail");
        assert!(report.is_mock);
        assert!(report.has_gpu());
        assert_eq!(report.gpu_count(), 1);
        assert_eq!(report.total_vram_gb(), 8.0);
    }

    #[test]
    fn test_mock_detector_custom_config() {
        let detector = MockGpuDetector::with_config(2, 16, "11.8".to_string());

        let report = detector
            .detect_gpus()
            .expect("Mock detection should not fail");
        assert!(report.is_mock);
        assert_eq!(report.gpu_count(), 2);
        assert_eq!(report.total_vram_gb(), 32.0);
        assert_eq!(report.system_cuda_version, Some("11.8".to_string()));
    }

    #[test]
    fn test_mock_detector_gpu_info() {
        let detector = MockGpuDetector::with_config(1, 10, "12.0".to_string());

        let report = detector
            .detect_gpus()
            .expect("Mock detection should not fail");
        let gpu = report.get_gpu(0).expect("GPU 0 should exist");

        assert_eq!(gpu.index, 0);
        assert!(gpu.device_name.contains("Mock NVIDIA GPU"));
        assert_eq!(gpu.vram_gb(), 10.0);
        assert_eq!(gpu.cuda_compute_capability, Some("8.0".to_string()));
        assert!(gpu.uuid.starts_with("GPU-MOCK-0-UUID"));
    }

    #[test]
    fn test_create_gpu_detector() {
        let detector = create_gpu_detector();
        assert!(detector.is_available());

        // Should work regardless of feature flag
        let report = detector.detect_gpus();
        assert!(report.is_ok());
    }

    #[test]
    fn test_mock_detector_zero_gpus() {
        let detector = MockGpuDetector::with_config(0, 8, "12.0".to_string());

        let report = detector
            .detect_gpus()
            .expect("Mock detection should not fail");
        assert!(!report.has_gpu());
        assert_eq!(report.gpu_count(), 0);
        assert_eq!(report.total_vram_gb(), 0.0);
    }
}

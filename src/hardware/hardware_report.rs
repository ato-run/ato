use serde::{Deserialize, Serialize};

/// Information about a single GPU device
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GpuInfo {
    /// GPU index (0-based)
    pub index: u32,

    /// Device name (e.g., "NVIDIA GeForce RTX 4090")
    pub device_name: String,

    /// Total VRAM in bytes
    pub vram_total_bytes: u64,

    /// CUDA compute capability (e.g., "8.9")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cuda_compute_capability: Option<String>,

    /// Current VRAM usage in bytes (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_used_bytes: Option<u64>,

    /// GPU UUID (Unique Identifier)
    pub uuid: String,
}

impl GpuInfo {
    /// Get VRAM in GB (rounded to 2 decimal places)
    pub fn vram_gb(&self) -> f64 {
        (self.vram_total_bytes as f64 / 1_073_741_824.0 * 100.0).round() / 100.0
    }

    /// Get available VRAM in bytes (total - used)
    pub fn vram_available_bytes(&self) -> u64 {
        match self.vram_used_bytes {
            Some(used) => self.vram_total_bytes.saturating_sub(used),
            None => self.vram_total_bytes,
        }
    }
}

/// Hardware report for a Rig (Agent node)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RigHardwareReport {
    /// Rig identifier (hostname or custom ID)
    pub rig_id: String,

    /// List of detected GPUs
    pub gpus: Vec<GpuInfo>,

    /// System-wide CUDA version (e.g., "12.0")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_cuda_version: Option<String>,

    /// NVIDIA driver version (e.g., "525.60.11")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_driver_version: Option<String>,

    /// Whether this is a mock report (for testing)
    #[serde(default)]
    pub is_mock: bool,
}

impl RigHardwareReport {
    /// Create a new hardware report
    pub fn new(rig_id: String) -> Self {
        Self {
            rig_id,
            gpus: Vec::new(),
            system_cuda_version: None,
            system_driver_version: None,
            is_mock: false,
        }
    }

    /// Get total VRAM across all GPUs in bytes
    pub fn total_vram_bytes(&self) -> u64 {
        self.gpus.iter().map(|gpu| gpu.vram_total_bytes).sum()
    }

    /// Get total VRAM in GB (rounded to 2 decimal places)
    pub fn total_vram_gb(&self) -> f64 {
        (self.total_vram_bytes() as f64 / 1_073_741_824.0 * 100.0).round() / 100.0
    }

    /// Get number of GPUs
    pub fn gpu_count(&self) -> usize {
        self.gpus.len()
    }

    /// Check if system has any GPUs
    pub fn has_gpu(&self) -> bool {
        !self.gpus.is_empty()
    }

    /// Get GPU by index
    pub fn get_gpu(&self, index: u32) -> Option<&GpuInfo> {
        self.gpus.iter().find(|gpu| gpu.index == index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gpu_info_vram_gb() {
        let gpu = GpuInfo {
            index: 0,
            device_name: "Mock GPU".to_string(),
            vram_total_bytes: 8_589_934_592, // 8 GB
            cuda_compute_capability: Some("8.0".to_string()),
            vram_used_bytes: None,
            uuid: "GPU-MOCK-0".to_string(),
        };
        assert_eq!(gpu.vram_gb(), 8.0);
    }

    #[test]
    fn test_gpu_info_vram_available() {
        let gpu = GpuInfo {
            index: 0,
            device_name: "Mock GPU".to_string(),
            vram_total_bytes: 8_589_934_592, // 8 GB
            cuda_compute_capability: None,
            vram_used_bytes: Some(4_294_967_296), // 4 GB used
            uuid: "GPU-MOCK-0".to_string(),
        };
        assert_eq!(gpu.vram_available_bytes(), 4_294_967_296); // 4 GB available
    }

    #[test]
    fn test_hardware_report_total_vram() {
        let mut report = RigHardwareReport::new("test-rig".to_string());
        report.gpus.push(GpuInfo {
            index: 0,
            device_name: "GPU 0".to_string(),
            vram_total_bytes: 8_589_934_592, // 8 GB
            cuda_compute_capability: None,
            vram_used_bytes: None,
            uuid: "GPU-MOCK-0".to_string(),
        });
        report.gpus.push(GpuInfo {
            index: 1,
            device_name: "GPU 1".to_string(),
            vram_total_bytes: 8_589_934_592, // 8 GB
            cuda_compute_capability: None,
            vram_used_bytes: None,
            uuid: "GPU-MOCK-1".to_string(),
        });

        assert_eq!(report.gpu_count(), 2);
        assert_eq!(report.total_vram_gb(), 16.0);
        assert!(report.has_gpu());
    }

    #[test]
    fn test_hardware_report_get_gpu() {
        let mut report = RigHardwareReport::new("test-rig".to_string());
        report.gpus.push(GpuInfo {
            index: 0,
            device_name: "GPU 0".to_string(),
            vram_total_bytes: 8_589_934_592,
            cuda_compute_capability: None,
            vram_used_bytes: None,
            uuid: "GPU-MOCK-0".to_string(),
        });

        assert!(report.get_gpu(0).is_some());
        assert_eq!(report.get_gpu(0).unwrap().device_name, "GPU 0");
        assert!(report.get_gpu(1).is_none());
    }
}

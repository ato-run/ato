use super::gpu_detector::{GpuDetectionError, GpuDetector};
use super::hardware_report::{GpuInfo, RigHardwareReport};
use serde_json::Value;
use std::process::Command;

#[derive(Debug)]
pub struct MacGpuDetector;

impl MacGpuDetector {
    pub fn new() -> Result<Self, GpuDetectionError> {
        // Verify we are on macOS (runtime check not strictly needed if guarded by cfg, but good for safety)
        if cfg!(target_os = "macos") {
            Ok(Self)
        } else {
            Err(GpuDetectionError::SystemInfoFailed(
                "Not running on macOS".to_string(),
            ))
        }
    }

    fn get_system_memory_bytes() -> Result<u64, GpuDetectionError> {
        let output = Command::new("sysctl")
            .arg("-n")
            .arg("hw.memsize")
            .output()
            .map_err(|e| {
                GpuDetectionError::SystemInfoFailed(format!("Failed to execute sysctl: {}", e))
            })?;

        if !output.status.success() {
            return Err(GpuDetectionError::SystemInfoFailed(
                "sysctl failed".to_string(),
            ));
        }

        let output_str = String::from_utf8(output.stdout).map_err(|e| {
            GpuDetectionError::SystemInfoFailed(format!("Invalid UTF-8 in sysctl output: {}", e))
        })?;

        output_str.trim().parse::<u64>().map_err(|e| {
            GpuDetectionError::SystemInfoFailed(format!("Failed to parse memory size: {}", e))
        })
    }

    fn get_serial_number() -> Option<String> {
        let output = Command::new("system_profiler")
            .args(["SPHardwareDataType", "-json"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let output_str = String::from_utf8(output.stdout).ok()?;
        let v: Value = serde_json::from_str(&output_str).ok()?;

        v.get("SPHardwareDataType")
            .and_then(|items| items.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("serial_number"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    }

    fn generate_stable_uuid(model: &str) -> String {
        // Try to get serial number for stable UUID
        if let Some(serial) = Self::get_serial_number() {
            // Hash serial + model for deterministic UUID
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};

            let mut hasher = DefaultHasher::new();
            serial.hash(&mut hasher);
            model.hash(&mut hasher);
            let hash = hasher.finish();

            format!("GPU-{:016X}", hash)
        } else {
            // Fallback to model-based UUID
            format!("GPU-{}-UNIFIED", model.replace(" ", "-").to_uppercase())
        }
    }

    fn estimate_vram_used_bytes(total_memory: u64) -> u64 {
        // For unified memory systems, estimate usage based on vm_stat
        // Parse vm_stat output to get memory statistics
        let output = Command::new("vm_stat").output().ok();

        if let Some(output) = output {
            if output.status.success() {
                if let Ok(output_str) = String::from_utf8(output.stdout) {
                    // Parse page size and calculate used memory
                    // vm_stat shows statistics in pages, typically 4096 bytes
                    let page_size = 4096u64; // Default macOS page size

                    // Look for "Pages active:" and "Pages wired down:"
                    let mut active_pages = 0u64;
                    let mut wired_pages = 0u64;

                    for line in output_str.lines() {
                        if line.contains("Pages active:") {
                            if let Some(num) = line.split_whitespace().nth(2) {
                                active_pages = num.trim_end_matches('.').parse().unwrap_or(0);
                            }
                        } else if line.contains("Pages wired down:") {
                            if let Some(num) = line.split_whitespace().nth(3) {
                                wired_pages = num.trim_end_matches('.').parse().unwrap_or(0);
                            }
                        }
                    }

                    // Estimate used memory as active + wired pages
                    // Apply a conservative 70% multiplier to estimate GPU portion
                    let used_memory = (active_pages + wired_pages) * page_size;
                    let estimated_gpu_used = (used_memory as f64 * 0.3) as u64; // 30% assumed for GPU

                    return estimated_gpu_used;
                }
            }
        }

        // Fallback: Use 20% of total memory as conservative estimate
        (total_memory as f64 * 0.2) as u64
    }
}

impl GpuDetector for MacGpuDetector {
    fn detect_gpus(&self) -> Result<RigHardwareReport, GpuDetectionError> {
        let rig_id = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "mac-rig".to_string());

        let mut report = RigHardwareReport::new(rig_id);
        report.is_mock = false;

        // 1. Get System Memory (Unified Memory)
        let total_memory = Self::get_system_memory_bytes()?;

        // 2. Get GPU Info via system_profiler
        let output = Command::new("system_profiler")
            .args(["SPDisplaysDataType", "-json"])
            .output()
            .map_err(|e| {
                GpuDetectionError::SystemInfoFailed(format!(
                    "Failed to execute system_profiler: {}",
                    e
                ))
            })?;

        if !output.status.success() {
            return Err(GpuDetectionError::SystemInfoFailed(
                "system_profiler failed".to_string(),
            ));
        }

        let output_str = String::from_utf8(output.stdout).map_err(|e| {
            GpuDetectionError::SystemInfoFailed(format!(
                "Invalid UTF-8 in system_profiler output: {}",
                e
            ))
        })?;

        let v: Value = serde_json::from_str(&output_str).map_err(|e| {
            GpuDetectionError::SystemInfoFailed(format!("Failed to parse JSON: {}", e))
        })?;

        // Parse JSON: { "SPDisplaysDataType": [ { "sppci_model": "Apple M1", ... } ] }
        if let Some(items) = v.get("SPDisplaysDataType").and_then(|i| i.as_array()) {
            // We assume the first GPU is the main one on Apple Silicon
            if let Some(item) = items.first() {
                let model = item
                    .get("sppci_model")
                    .and_then(|s| s.as_str())
                    .unwrap_or("Unknown Apple GPU")
                    .to_string();

                // Check if it's Apple Silicon
                let is_apple_silicon = model.contains("Apple")
                    || model.contains("M1")
                    || model.contains("M2")
                    || model.contains("M3");

                if is_apple_silicon {
                    // Generate a stable UUID based on serial number + model
                    let uuid = Self::generate_stable_uuid(&model);

                    // Estimate VRAM usage for unified memory
                    let vram_used = Self::estimate_vram_used_bytes(total_memory);

                    report.gpus.push(GpuInfo {
                        index: 0,
                        device_name: model,
                        vram_total_bytes: total_memory, // Unified memory
                        cuda_compute_capability: None,  // Not CUDA
                        vram_used_bytes: Some(vram_used), // Estimated usage
                        uuid,
                    });
                }
            }
        }

        Ok(report)
    }

    fn is_available(&self) -> bool {
        // Only available on macOS
        cfg!(target_os = "macos")
    }

    fn name(&self) -> &str {
        "MacGpuDetector"
    }

    fn get_available_vram_bytes(&self, _index: usize) -> Result<u64, GpuDetectionError> {
        // For now, return total system memory as "available"
        // In reality, we should check 'vm_stat' or similar
        Self::get_system_memory_bytes()
    }
}

//! Provision receipt and marker types for GPU runner provisioning.
//!
//! These types record the outcome of `ato runner provision` so that:
//!
//! - The control plane (via heartbeat capabilities) can know which
//!   runners are GPU-capable and at what CUDA/driver level.
//! - `ato runner doctor` can display the last known provision state.
//! - A `--resume` re-run after reboot can pick up where it left off
//!   via the [`ProvisionMarker`] intermediate state.

use serde::{Deserialize, Serialize};

use super::host_gpu::{GpuDevice, OsInfo};

// ─────────────────────────────────────────────
// Receipt
// ─────────────────────────────────────────────

/// Outcome of the Dockerless GPU smoke test (`vulkaninfo --summary`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeResult {
    /// Smoke passed; an NVIDIA Vulkan device was visible.
    Pass,
    /// Smoke failed; no NVIDIA Vulkan device or the probe failed.
    Fail,
    /// Smoke was skipped (e.g. `--dry-run` or post-reboot resume).
    Skipped,
}

/// A compact GPU summary stored in the receipt (no PCIe bus IDs or
/// UUIDs — those are only needed during provisioning, not for
/// capability advertisement).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuDeviceSummary {
    pub name: String,
    pub vram_bytes: u64,
}

impl From<&GpuDevice> for GpuDeviceSummary {
    fn from(d: &GpuDevice) -> Self {
        Self {
            name: d.name.clone(),
            vram_bytes: d.vram_bytes,
        }
    }
}

/// The full outcome of a successful `ato runner provision` run.
///
/// Written to `~/.ato/runner/provision-receipt.json` (0600 on Unix).
/// Read by `ato runner doctor` and (future) the heartbeat path to
/// advertise GPU capabilities to the control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionReceipt {
    /// Unix timestamp (seconds) when the receipt was written.
    pub timestamp_unix: u64,
    /// Profile name that was provisioned, e.g. `"nvidia-ubuntu"`.
    pub profile: String,
    pub os: OsInfo,
    pub kernel_version: String,
    pub secure_boot_enabled: bool,
    pub driver_version: Option<String>,
    pub cuda_driver_api_version: Option<String>,
    pub gpu_count: usize,
    pub gpu_devices: Vec<GpuDeviceSummary>,
    /// Whether the Vulkan loader library was present after provisioning.
    pub vulkan_loader_present: bool,
    /// Whether the `vulkaninfo` tool was available after provisioning.
    pub vulkaninfo_available: bool,
    /// Whether an NVIDIA Vulkan ICD manifest was present after provisioning.
    pub nvidia_vulkan_icd_present: bool,
    /// Whether `vulkaninfo` reported a usable NVIDIA Vulkan device.
    pub vulkan_nvidia_device_visible: bool,
    /// Result of the Dockerless GPU smoke (`vulkaninfo --summary`).
    pub gpu_smoke_result: SmokeResult,
    /// Number of GPUs detected (nvidia-smi) when the smoke passed.
    pub smoke_gpu_count_detected: Option<usize>,
    /// `true` when a reboot was needed and the user was told to
    /// `--resume`. The receipt is only final when this is `false`.
    pub reboot_required: bool,
    pub warnings: Vec<String>,
    pub ato_cli_version: String,

    // ── CUDA / SGLang fields (profile = "nvidia-cuda" only) ──
    // All optional + `#[serde(default)]` so an existing `nvidia-ubuntu` receipt
    // (which never set these) still deserializes, and a CUDA receipt round-trips.
    /// Whether the driver exposed a CUDA driver-API version after provisioning
    /// (the host-side "CUDA runtime present" signal). `None` on a Vulkan receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cuda_runtime_present: Option<bool>,
    /// The `python3 --version` string captured after provisioning (the host
    /// interpreter the managed sglang venv is built from). `None` on a Vulkan
    /// receipt, or when python3 could not be queried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python3_version: Option<String>,
    /// The pinned sglang wheel version the managed venv targets. `None` on a
    /// Vulkan receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sglang_version: Option<String>,
    /// Whether `import sglang` succeeded in the managed venv (the CUDA smoke).
    /// `None` on a Vulkan receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sglang_import_ok: Option<bool>,
    /// Largest detected GPU's VRAM in bytes (the CUDA capacity hint). `None` on
    /// a Vulkan receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_gpu_vram_bytes: Option<u64>,
}

// ─────────────────────────────────────────────
// Marker (intermediate state for --resume)
// ─────────────────────────────────────────────

/// The phase of provisioning that was in progress when a marker was
/// written. Used by `--resume` to skip already-completed steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisionPhase {
    /// NVIDIA driver was just installed; a reboot is required before
    /// the module can load (especially with Secure Boot / MOK).
    PostDriverInstall,
    /// A reboot was required for the driver to load; `--resume` should
    /// re-check the driver and continue to the Vulkan runtime + smoke test.
    RebootRequired,
}

/// Intermediate provisioning state, written to
/// `~/.ato/runner/provision-marker.json` so that `--resume` can
/// pick up after a reboot or MOK enrollment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionMarker {
    pub profile: String,
    pub phase: ProvisionPhase,
    pub secure_boot_enabled: bool,
    /// Unix timestamp (seconds) when the marker was written.
    pub timestamp_unix: u64,
}

// ─────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::host_gpu::OsInfo;
    use super::*;

    #[test]
    fn smoke_result_serializes_as_snake_case() {
        let json = serde_json::to_string(&SmokeResult::Pass).unwrap();
        assert_eq!(json, "\"pass\"");
        let json = serde_json::to_string(&SmokeResult::Fail).unwrap();
        assert_eq!(json, "\"fail\"");
        let json = serde_json::to_string(&SmokeResult::Skipped).unwrap();
        assert_eq!(json, "\"skipped\"");
    }

    #[test]
    fn provision_phase_serializes_as_snake_case() {
        let json = serde_json::to_string(&ProvisionPhase::PostDriverInstall).unwrap();
        assert_eq!(json, "\"post_driver_install\"");
        let json = serde_json::to_string(&ProvisionPhase::RebootRequired).unwrap();
        assert_eq!(json, "\"reboot_required\"");
    }

    #[test]
    fn receipt_round_trips_through_json() {
        let receipt = ProvisionReceipt {
            timestamp_unix: 1718700000,
            profile: "nvidia-ubuntu".to_string(),
            os: OsInfo {
                distro: "ubuntu".to_string(),
                version: "22.04".to_string(),
                kernel: "5.15.0-91-generic".to_string(),
            },
            kernel_version: "5.15.0-91-generic".to_string(),
            secure_boot_enabled: false,
            driver_version: Some("575.57.08".to_string()),
            cuda_driver_api_version: Some("12.4".to_string()),
            gpu_count: 2,
            gpu_devices: vec![
                GpuDeviceSummary {
                    name: "NVIDIA GeForce RTX 3060".to_string(),
                    vram_bytes: 12 * 1024 * 1024 * 1024,
                },
                GpuDeviceSummary {
                    name: "NVIDIA GeForce RTX 3060".to_string(),
                    vram_bytes: 12 * 1024 * 1024 * 1024,
                },
            ],
            vulkan_loader_present: true,
            vulkaninfo_available: true,
            nvidia_vulkan_icd_present: true,
            vulkan_nvidia_device_visible: true,
            gpu_smoke_result: SmokeResult::Pass,
            smoke_gpu_count_detected: Some(2),
            reboot_required: false,
            warnings: vec![],
            ato_cli_version: "0.7.0-dev".to_string(),
            cuda_runtime_present: None,
            python3_version: None,
            sglang_version: None,
            sglang_import_ok: None,
            max_gpu_vram_bytes: None,
        };
        let json = serde_json::to_string(&receipt).unwrap();
        let decoded: ProvisionReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, receipt);
        assert_eq!(decoded.gpu_count, 2);
        assert_eq!(decoded.gpu_smoke_result, SmokeResult::Pass);
    }

    #[test]
    fn cuda_receipt_round_trips_with_sglang_fields() {
        let receipt = ProvisionReceipt {
            timestamp_unix: 1718700000,
            profile: "nvidia-cuda".to_string(),
            os: OsInfo {
                distro: "ubuntu".to_string(),
                version: "24.04".to_string(),
                kernel: "6.8.0-31-generic".to_string(),
            },
            kernel_version: "6.8.0-31-generic".to_string(),
            secure_boot_enabled: false,
            driver_version: Some("570.124.06".to_string()),
            cuda_driver_api_version: Some("12.4".to_string()),
            gpu_count: 1,
            gpu_devices: vec![GpuDeviceSummary {
                name: "NVIDIA RTX A6000".to_string(),
                vram_bytes: 48 * 1024 * 1024 * 1024,
            }],
            vulkan_loader_present: false,
            vulkaninfo_available: false,
            nvidia_vulkan_icd_present: false,
            vulkan_nvidia_device_visible: false,
            gpu_smoke_result: SmokeResult::Pass,
            smoke_gpu_count_detected: Some(1),
            reboot_required: false,
            warnings: vec![],
            ato_cli_version: "0.7.0-dev".to_string(),
            cuda_runtime_present: Some(true),
            python3_version: Some("Python 3.12.3".to_string()),
            sglang_version: Some("0.4.10.post2".to_string()),
            sglang_import_ok: Some(true),
            max_gpu_vram_bytes: Some(48 * 1024 * 1024 * 1024),
        };
        let json = serde_json::to_string(&receipt).unwrap();
        let decoded: ProvisionReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, receipt);
        assert_eq!(decoded.sglang_version.as_deref(), Some("0.4.10.post2"));
        assert_eq!(decoded.sglang_import_ok, Some(true));
    }

    #[test]
    fn legacy_vulkan_receipt_without_cuda_fields_still_deserializes() {
        // A receipt written by the pre-CUDA build (no sglang/cuda fields). The
        // new optional fields must default to `None` so old receipts load.
        let legacy = r#"{
            "timestamp_unix": 1718700000,
            "profile": "nvidia-ubuntu",
            "os": {"distro":"ubuntu","version":"22.04","kernel":"5.15.0-91-generic"},
            "kernel_version": "5.15.0-91-generic",
            "secure_boot_enabled": false,
            "driver_version": "575.57.08",
            "cuda_driver_api_version": "12.4",
            "gpu_count": 1,
            "gpu_devices": [{"name":"NVIDIA GeForce RTX 3060","vram_bytes":12884901888}],
            "vulkan_loader_present": true,
            "vulkaninfo_available": true,
            "nvidia_vulkan_icd_present": true,
            "vulkan_nvidia_device_visible": true,
            "gpu_smoke_result": "pass",
            "smoke_gpu_count_detected": 1,
            "reboot_required": false,
            "warnings": [],
            "ato_cli_version": "0.6.0"
        }"#;
        let decoded: ProvisionReceipt = serde_json::from_str(legacy).unwrap();
        assert_eq!(decoded.profile, "nvidia-ubuntu");
        assert_eq!(decoded.cuda_runtime_present, None);
        assert_eq!(decoded.sglang_version, None);
        assert_eq!(decoded.sglang_import_ok, None);
        assert_eq!(decoded.max_gpu_vram_bytes, None);
    }

    #[test]
    fn marker_round_trips_through_json() {
        let marker = ProvisionMarker {
            profile: "nvidia-ubuntu".to_string(),
            phase: ProvisionPhase::PostDriverInstall,
            secure_boot_enabled: true,
            timestamp_unix: 1718700000,
        };
        let json = serde_json::to_string(&marker).unwrap();
        let decoded: ProvisionMarker = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, marker);
        assert_eq!(decoded.phase, ProvisionPhase::PostDriverInstall);
        assert!(decoded.secure_boot_enabled);
    }

    #[test]
    fn gpu_device_summary_from_gpu_device_strips_uuid_and_pcie() {
        let device = GpuDevice {
            index: 0,
            name: "NVIDIA GeForce RTX 3060".to_string(),
            uuid: Some("GPU-abc".to_string()),
            vram_bytes: 12 * 1024 * 1024 * 1024,
            pcie_bus_id: Some("0000:01:00.0".to_string()),
        };
        let summary = GpuDeviceSummary::from(&device);
        assert_eq!(summary.name, "NVIDIA GeForce RTX 3060");
        assert_eq!(summary.vram_bytes, 12 * 1024 * 1024 * 1024);
    }
}

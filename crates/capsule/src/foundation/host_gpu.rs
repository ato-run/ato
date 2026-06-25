//! Host GPU profile detection for runner provisioning.
//!
//! Read-only probes that collect a complete picture of the host's GPU,
//! driver, CUDA driver API, and Vulkan state. Used by `ato runner doctor`
//! (health check) and `ato runner provision` (Dockerless install flow) to
//! decide what needs to be installed and to generate a provision receipt.
//! This is the Dockerless GPU path — no Docker / Podman / nvidia-container-toolkit.
//!
//! This module is **detection only** — it never mutates host state.
//! The companion [`hardware`](super::hardware) module remains the
//! recipe-gated probe used by the OCI executor at launch time.

use std::process::Command;

use serde::{Deserialize, Serialize};

use super::error::Result;

// ─────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────

/// Operating system identification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsInfo {
    /// Distribution name, e.g. `"ubuntu"`, `"debian"`, `"fedora"`.
    pub distro: String,
    /// Version string, e.g. `"22.04"`, `"24.04"`.
    pub version: String,
    /// Kernel release string, e.g. `"5.15.0-91-generic"`.
    pub kernel: String,
}

/// A single GPU device detected on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuDevice {
    /// Zero-based index as reported by nvidia-smi.
    pub index: u32,
    /// Human-readable model name, e.g. `"NVIDIA GeForce RTX 3060"`.
    pub name: String,
    /// GPU UUID when available.
    pub uuid: Option<String>,
    /// Total VRAM in bytes.
    pub vram_bytes: u64,
    /// PCIe bus identifier, e.g. `"0000:01:00.0"`.
    pub pcie_bus_id: Option<String>,
}

/// NVIDIA kernel driver information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverInfo {
    /// Driver version string, e.g. `"575.57.08"`.
    pub version: String,
    /// Whether `nvidia-smi` is available on PATH and functional.
    pub nvidia_smi_available: bool,
}

/// CUDA runtime information visible from the driver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CudaInfo {
    /// CUDA driver API version reported by nvidia-smi, e.g. `"12.4"`.
    pub driver_api_version: String,
    /// CUDA toolkit version from `nvcc --version`, when installed.
    pub toolkit_version: Option<String>,
}

/// Vulkan runtime state — the Dockerless GPU path for native-inference. On
/// NVIDIA hosts the Vulkan ICD ships with the driver's userspace (`nvidia_icd.json`
/// and `libGLX_nvidia`); `vulkaninfo` confirms a usable device. These four signals
/// are tracked separately so provisioning/doctor can pinpoint the exact gap (a
/// present loader does NOT imply the `vulkaninfo` tool, and a present `vulkaninfo`
/// does NOT imply a working NVIDIA ICD).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VulkanInfo {
    /// Whether the Vulkan loader library (`libvulkan.so.1`) is present.
    pub loader_present: bool,
    /// Whether the `vulkaninfo` tool (from `vulkan-tools`) is on PATH.
    pub vulkaninfo_available: bool,
    /// Whether an NVIDIA Vulkan ICD manifest (`nvidia_icd.json`) is installed
    /// in a standard search dir.
    pub nvidia_icd_present: bool,
    /// Whether `vulkaninfo` reports at least one NVIDIA physical device.
    pub nvidia_device_visible: bool,
}

/// CUDA runtime readiness for the SGLang native-inference path. Distinct from
/// [`CudaInfo`] (the driver-reported CUDA *driver-API* version): these are the
/// host-side signals that a CUDA *runtime* is usable Dockerlessly — a CUDA
/// driver-API version is visible (the driver exposes CUDA), and a Python 3
/// interpreter with the `venv` module is present (SGLang installs into a managed
/// venv). The deeper `import sglang` + device-visibility smoke is the
/// `nvidia-cuda` doctor's job; these are the cheap, read-only host signals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CudaRuntimeInfo {
    /// Whether the driver reports a CUDA driver-API version (CUDA is exposed).
    pub cuda_runtime_present: bool,
    /// Whether a `python3` interpreter is on PATH.
    pub python3_ok: bool,
    /// Whether `python3 -m venv` is importable (the stdlib `venv` module).
    pub venv_module_ok: bool,
    /// Whether the CUDA toolkit's `nvcc` compiler is on PATH. SGLang 0.5.x
    /// JIT-compiles CUDA kernels at runtime (tvm_ffi → ninja → nvcc → g++), so
    /// `nvcc` is required for a green `nvidia-cuda` doctor — `provision` installs
    /// the CUDA toolkit. Read-only here; not part of the host-readiness floor.
    pub nvcc_ok: bool,
    /// Whether the `ninja` build tool is on PATH. SGLang's runtime JIT invokes
    /// `ninja` to drive `nvcc`; `provision` installs `ninja-build`. Read-only.
    pub ninja_ok: bool,
    /// Max VRAM in bytes across detected GPUs (0 when none detected).
    pub max_gpu_vram_bytes: u64,
}

/// Complete host GPU profile — the result of all detection probes.
///
/// Fields are `Option` when the corresponding component is not installed
/// or cannot be probed. A fully provisioned Ubuntu+NVIDIA host will have
/// every field populated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostGpuProfile {
    pub os: OsInfo,
    pub secure_boot_enabled: Option<bool>,
    pub gpus: Vec<GpuDevice>,
    pub driver: Option<DriverInfo>,
    /// CUDA driver-API info — informational only (kept for a future CUDA
    /// source-build path); not required for the Vulkan native-inference path.
    pub cuda: Option<CudaInfo>,
    pub vulkan: Option<VulkanInfo>,
    /// CUDA runtime readiness signals for the SGLang native-inference path.
    /// `None` when not probed (e.g. on non-Linux hosts or older receipts).
    #[serde(default)]
    pub cuda_runtime: Option<CudaRuntimeInfo>,
}

impl HostGpuProfile {
    /// `true` when at least one NVIDIA GPU is visible via nvidia-smi.
    pub fn has_gpu(&self) -> bool {
        !self.gpus.is_empty()
    }

    /// `true` when the NVIDIA driver is loaded and nvidia-smi works.
    pub fn driver_installed(&self) -> bool {
        self.driver
            .as_ref()
            .map(|d| d.nvidia_smi_available)
            .unwrap_or(false)
    }

    /// `true` when the Vulkan loader library is present on the host.
    pub fn vulkan_loader_present(&self) -> bool {
        self.vulkan
            .as_ref()
            .map(|v| v.loader_present)
            .unwrap_or(false)
    }

    /// `true` when the `vulkaninfo` tool (the smoke tool) is available.
    pub fn vulkaninfo_available(&self) -> bool {
        self.vulkan
            .as_ref()
            .map(|v| v.vulkaninfo_available)
            .unwrap_or(false)
    }

    /// `true` when an NVIDIA Vulkan ICD manifest is installed.
    pub fn nvidia_vulkan_icd_present(&self) -> bool {
        self.vulkan
            .as_ref()
            .map(|v| v.nvidia_icd_present)
            .unwrap_or(false)
    }

    /// `true` when `vulkaninfo` reports a usable NVIDIA Vulkan device.
    pub fn vulkan_nvidia_device_visible(&self) -> bool {
        self.vulkan
            .as_ref()
            .map(|v| v.nvidia_device_visible)
            .unwrap_or(false)
    }

    /// `true` when the host can run a Vulkan-accelerated native-inference engine
    /// Dockerlessly: an NVIDIA GPU + working driver + the Vulkan loader + the
    /// `vulkaninfo` tool + a visible NVIDIA Vulkan device. (Device visibility
    /// implies a working ICD; loader + tool are required so doctor/provision
    /// never report "ready" with the smoke tool missing.)
    pub fn native_inference_vulkan_ready(&self) -> bool {
        self.has_gpu()
            && self.driver_installed()
            && self.vulkan_loader_present()
            && self.vulkaninfo_available()
            && self.vulkan_nvidia_device_visible()
    }

    /// Max VRAM in bytes across all detected GPUs (0 when none detected).
    pub fn max_gpu_vram_bytes(&self) -> u64 {
        self.gpus.iter().map(|g| g.vram_bytes).max().unwrap_or(0)
    }

    /// `true` when the largest detected GPU has at least `need_bytes` of VRAM.
    /// Used by the `nvidia-cuda` doctor as a (non-fatal) headroom hint for the
    /// AWQ-quantized weights SGLang loads; `false` when no GPU was detected.
    pub fn gpu_vram_meets(&self, need_bytes: u64) -> bool {
        self.max_gpu_vram_bytes() >= need_bytes
    }

    /// `true` when the host can run the SGLang (CUDA) native-inference engine
    /// Dockerlessly: an NVIDIA GPU + working driver + a detectable CUDA runtime
    /// (the driver exposes a CUDA driver-API version) + a Python 3 interpreter
    /// with the `venv` module (SGLang installs into a managed venv).
    ///
    /// Honest by construction: when CUDA, the driver, or Python/venv cannot be
    /// detected this returns `false`, so doctor/provision never report "ready"
    /// with a missing prerequisite. The deeper `import sglang` + GPU-visibility
    /// smoke is the `nvidia-cuda` doctor's gate; this is the host-readiness floor.
    pub fn native_inference_cuda_ready(&self) -> bool {
        self.has_gpu()
            && self.driver_installed()
            && self
                .cuda_runtime
                .as_ref()
                .map(|c| c.cuda_runtime_present && c.python3_ok && c.venv_module_ok)
                .unwrap_or(false)
    }

    /// `true` when the OS is Ubuntu 22.04 or 24.04 (the v0 supported set).
    pub fn os_supported(&self) -> bool {
        self.os.distro == "ubuntu" && (self.os.version == "22.04" || self.os.version == "24.04")
    }
}

// ─────────────────────────────────────────────
// Detection
// ─────────────────────────────────────────────

/// Run all detection probes and return a complete [`HostGpuProfile`].
///
/// Each probe is independent — a failure in one (e.g. nvidia-smi not
/// installed) does not prevent the others from running. Missing
/// components surface as `None` or empty `Vec`s.
pub fn detect_host_gpu_profile() -> Result<HostGpuProfile> {
    let os = detect_os_info();
    let secure_boot_enabled = detect_secure_boot();
    let gpus = detect_gpu_devices().unwrap_or_default();
    let driver = detect_driver_info();
    let cuda = detect_cuda_info();
    let vulkan = detect_vulkan_info();
    let cuda_runtime = detect_cuda_runtime_info(&gpus, cuda.as_ref());

    Ok(HostGpuProfile {
        os,
        secure_boot_enabled,
        gpus,
        driver,
        cuda,
        vulkan,
        cuda_runtime,
    })
}

/// Probe the SGLang CUDA-runtime readiness signals: a CUDA driver-API version is
/// visible (the driver exposes CUDA), and a `python3` interpreter with the
/// stdlib `venv` module is present (SGLang installs into a managed venv). These
/// are cheap, read-only host signals — the heavy `import sglang` + device smoke
/// is the `nvidia-cuda` doctor's job. Returns `None` only when there is nothing
/// CUDA-relevant to report (no GPU AND no CUDA info), so a probed Linux host
/// always carries the (possibly all-false) signals.
fn detect_cuda_runtime_info(gpus: &[GpuDevice], cuda: Option<&CudaInfo>) -> Option<CudaRuntimeInfo> {
    let cuda_runtime_present = cuda
        .map(|c| !c.driver_api_version.trim().is_empty())
        .unwrap_or(false);
    if gpus.is_empty() && !cuda_runtime_present {
        // Nothing CUDA-relevant on this host — leave the field unprobed.
        return None;
    }
    let python3_ok = which::which("python3").is_ok();
    // `python3 -c "import venv"` confirms the stdlib venv module is importable
    // (some minimal distros ship python3 without it). Only meaningful when
    // python3 exists.
    let venv_module_ok = python3_ok
        && Command::new("python3")
            .args(["-c", "import venv"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
    // The SGLang runtime JIT toolchain (tvm_ffi → ninja → nvcc → g++). These are
    // cheap PATH probes; `provision --profile nvidia-cuda` installs the CUDA
    // toolkit (nvcc) + ninja-build. Surfaced as their own doctor rows so a green
    // doctor implies sglang can actually compile kernels.
    // nvcc may live under the CUDA toolkit prefix (`/usr/local/cuda-XX/bin`, where
    // `provision --profile nvidia-cuda` installs it and points CUDA_HOME) without
    // being on the default PATH, so check CUDA_HOME/bin and any /usr/local/cuda*/bin
    // in addition to PATH — otherwise a correctly-provisioned host reports FAIL.
    let nvcc_ok = which::which("nvcc").is_ok()
        || std::env::var("CUDA_HOME")
            .map(|h| std::path::Path::new(&h).join("bin/nvcc").is_file())
            .unwrap_or(false)
        || std::fs::read_dir("/usr/local")
            .map(|entries| {
                entries.flatten().any(|e| {
                    let p = e.path();
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("cuda"))
                        .unwrap_or(false)
                        && p.join("bin/nvcc").is_file()
                })
            })
            .unwrap_or(false);
    let ninja_ok = which::which("ninja").is_ok();
    let max_gpu_vram_bytes = gpus.iter().map(|g| g.vram_bytes).max().unwrap_or(0);

    Some(CudaRuntimeInfo {
        cuda_runtime_present,
        python3_ok,
        venv_module_ok,
        nvcc_ok,
        ninja_ok,
        max_gpu_vram_bytes,
    })
}

/// Parse `/etc/os-release` and `uname -r` to identify the OS.
fn detect_os_info() -> OsInfo {
    let (distro, version) = parse_os_release().unwrap_or_else(|| (String::new(), String::new()));
    let kernel = Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();

    OsInfo {
        distro,
        version,
        kernel,
    }
}

fn parse_os_release() -> Option<(String, String)> {
    let content = std::fs::read_to_string("/etc/os-release").ok()?;
    let mut distro = String::new();
    let mut version = String::new();
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("ID=") {
            distro = val.trim_matches('"').to_string();
        } else if let Some(val) = line.strip_prefix("VERSION_ID=") {
            version = val.trim_matches('"').to_string();
        }
    }
    if distro.is_empty() {
        None
    } else {
        Some((distro, version))
    }
}

/// Check Secure Boot status via `mokutil --sb-state`.
///
/// Returns `None` when `mokutil` is not installed (common on non-UEFI
/// systems or before driver installation).
fn detect_secure_boot() -> Option<bool> {
    if which::which("mokutil").is_err() {
        return None;
    }
    let output = Command::new("mokutil").arg("--sb-state").output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.contains("SecureBoot enabled") {
        Some(true)
    } else if stdout.contains("SecureBoot disabled") {
        Some(false)
    } else {
        None
    }
}

/// Query `nvidia-smi` for individual GPU devices.
///
/// Returns an empty `Vec` when nvidia-smi is not installed or reports
/// no GPUs. Unlike `hardware::detect_nvidia_gpus`, this returns
/// per-device details (name, UUID, PCIe bus) rather than just a count
/// and average VRAM.
fn detect_gpu_devices() -> Result<Vec<GpuDevice>> {
    if which::which("nvidia-smi").is_err() {
        return Ok(Vec::new());
    }

    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=index,name,uuid,memory.total,pci.bus_id",
            "--format=csv,noheader,nounits",
        ])
        .output()?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut devices = Vec::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() < 4 {
            continue;
        }
        let index = parts[0].parse::<u32>().unwrap_or(0);
        let name = parts[1].to_string();
        let uuid = if parts[2].is_empty() || parts[2].eq_ignore_ascii_case("N/A") {
            None
        } else {
            Some(parts[2].to_string())
        };
        let vram_mib: u64 = parts[3].parse().unwrap_or(0);
        let vram_bytes = vram_mib * 1024 * 1024;
        let pcie_bus_id = parts.get(4).map(|s| s.to_string());

        devices.push(GpuDevice {
            index,
            name,
            uuid,
            vram_bytes,
            pcie_bus_id,
        });
    }

    Ok(devices)
}

/// Detect NVIDIA driver version from `nvidia-smi` header output.
fn detect_driver_info() -> Option<DriverInfo> {
    if which::which("nvidia-smi").is_err() {
        return None;
    }

    let output = Command::new("nvidia-smi").output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    if !output.status.success() || stdout.is_empty() {
        return Some(DriverInfo {
            version: String::new(),
            nvidia_smi_available: false,
        });
    }

    let version = stdout
        .lines()
        .find_map(|line| {
            line.split("Driver Version:")
                .nth(1)
                .map(|s| s.split_whitespace().next().unwrap_or("").to_string())
        })
        .unwrap_or_default();

    Some(DriverInfo {
        version,
        nvidia_smi_available: true,
    })
}

/// Detect CUDA driver API version from nvidia-smi and toolkit version
/// from `nvcc --version` when available.
fn detect_cuda_info() -> Option<CudaInfo> {
    if which::which("nvidia-smi").is_err() {
        return None;
    }

    let output = Command::new("nvidia-smi").output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    if !output.status.success() {
        return None;
    }

    let driver_api_version = stdout
        .lines()
        .find_map(|line| {
            line.split("CUDA Version:")
                .nth(1)
                .map(|s| s.split_whitespace().next().unwrap_or("").to_string())
        })
        .unwrap_or_default();

    let toolkit_version = if which::which("nvcc").is_ok() {
        Command::new("nvcc")
            .arg("--version")
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    let nvcc_out = String::from_utf8_lossy(&o.stdout);
                    nvcc_out.lines().rev().find_map(|line| {
                        line.split("release ").nth(1).map(|s| {
                            s.split_whitespace()
                                .next()
                                .map(|v| v.trim_end_matches(',').to_string())
                                .unwrap_or_default()
                        })
                    })
                } else {
                    None
                }
            })
    } else {
        None
    };

    Some(CudaInfo {
        driver_api_version,
        toolkit_version,
    })
}

/// Standard search dirs for Vulkan ICD manifests (loader convention).
const VULKAN_ICD_DIRS: &[&str] = &["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"];

/// `true` when the Vulkan loader library (`libvulkan.so.1`) is on disk. This is
/// independent of the `vulkaninfo` tool (which ships separately in `vulkan-tools`).
fn vulkan_loader_lib_present() -> bool {
    [
        "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
        "/usr/lib/aarch64-linux-gnu/libvulkan.so.1",
        "/lib/x86_64-linux-gnu/libvulkan.so.1",
        "/usr/lib64/libvulkan.so.1",
    ]
    .iter()
    .any(|p| std::path::Path::new(p).exists())
}

/// `true` when an NVIDIA Vulkan ICD manifest (`*nvidia*icd*.json`) is installed
/// in a standard search dir. A present manifest is necessary (not sufficient —
/// device visibility is the real proof) for NVIDIA Vulkan.
fn nvidia_vulkan_icd_on_disk() -> bool {
    for dir in VULKAN_ICD_DIRS {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.contains("nvidia") && name.ends_with(".json") {
                return true;
            }
        }
    }
    false
}

/// Detect the Vulkan runtime as four independent signals: the loader library,
/// the `vulkaninfo` tool, an NVIDIA ICD manifest, and a visible NVIDIA device.
/// This is the Dockerless GPU readiness signal for the native-inference Vulkan
/// engine variant.
fn detect_vulkan_info() -> Option<VulkanInfo> {
    let loader_present = vulkan_loader_lib_present();
    let vulkaninfo_available = which::which("vulkaninfo").is_ok();
    let nvidia_icd_present = nvidia_vulkan_icd_on_disk();

    // `vulkaninfo --summary` lists deviceName lines; an NVIDIA device confirms
    // the driver's Vulkan ICD is actually usable. Only meaningful when the tool
    // exists.
    let nvidia_device_visible = vulkaninfo_available
        && Command::new("vulkaninfo")
            .arg("--summary")
            .output()
            .map(|o| {
                o.status.success()
                    && String::from_utf8_lossy(&o.stdout)
                        .to_lowercase()
                        .contains("nvidia")
            })
            .unwrap_or(false);

    Some(VulkanInfo {
        loader_present,
        vulkaninfo_available,
        nvidia_icd_present,
        nvidia_device_visible,
    })
}

// ─────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_info_supported_ubuntu_22_04() {
        let os = OsInfo {
            distro: "ubuntu".to_string(),
            version: "22.04".to_string(),
            kernel: "5.15.0-91-generic".to_string(),
        };
        let profile = HostGpuProfile {
            os: os.clone(),
            secure_boot_enabled: None,
            gpus: vec![],
            driver: None,
            cuda: None,
            vulkan: None,
            cuda_runtime: None,
        };
        assert!(profile.os_supported());
        assert_eq!(os.distro, "ubuntu");
    }

    #[test]
    fn os_info_supported_ubuntu_24_04() {
        let os = OsInfo {
            distro: "ubuntu".to_string(),
            version: "24.04".to_string(),
            kernel: "6.8.0-31-generic".to_string(),
        };
        assert!(
            HostGpuProfile {
                os,
                secure_boot_enabled: None,
                gpus: vec![],
                driver: None,
                cuda: None,
                vulkan: None,
                cuda_runtime: None,
            }
            .os_supported()
        );
    }

    #[test]
    fn os_info_unsupported_debian() {
        let os = OsInfo {
            distro: "debian".to_string(),
            version: "12".to_string(),
            kernel: "6.1.0-10".to_string(),
        };
        assert!(
            !HostGpuProfile {
                os,
                secure_boot_enabled: None,
                gpus: vec![],
                driver: None,
                cuda: None,
                vulkan: None,
                cuda_runtime: None,
            }
            .os_supported()
        );
    }

    #[test]
    fn os_info_unsupported_ubuntu_20_04() {
        let os = OsInfo {
            distro: "ubuntu".to_string(),
            version: "20.04".to_string(),
            kernel: "5.4.0-169".to_string(),
        };
        assert!(
            !HostGpuProfile {
                os,
                secure_boot_enabled: None,
                gpus: vec![],
                driver: None,
                cuda: None,
                vulkan: None,
                cuda_runtime: None,
            }
            .os_supported()
        );
    }

    #[test]
    fn has_gpu_true_with_devices() {
        let profile = HostGpuProfile {
            os: OsInfo {
                distro: "ubuntu".to_string(),
                version: "22.04".to_string(),
                kernel: "5.15.0".to_string(),
            },
            secure_boot_enabled: None,
            gpus: vec![GpuDevice {
                index: 0,
                name: "NVIDIA GeForce RTX 3060".to_string(),
                uuid: Some("GPU-1234".to_string()),
                vram_bytes: 12 * 1024 * 1024 * 1024,
                pcie_bus_id: None,
            }],
            driver: None,
            cuda: None,
            vulkan: None,
            cuda_runtime: None,
        };
        assert!(profile.has_gpu());
    }

    #[test]
    fn has_gpu_false_without_devices() {
        let profile = HostGpuProfile {
            os: OsInfo {
                distro: "ubuntu".to_string(),
                version: "22.04".to_string(),
                kernel: "5.15.0".to_string(),
            },
            secure_boot_enabled: None,
            gpus: vec![],
            driver: None,
            cuda: None,
            vulkan: None,
            cuda_runtime: None,
        };
        assert!(!profile.has_gpu());
    }

    #[test]
    fn driver_installed_true_when_smi_available() {
        let profile = HostGpuProfile {
            os: OsInfo {
                distro: "ubuntu".to_string(),
                version: "22.04".to_string(),
                kernel: "5.15.0".to_string(),
            },
            secure_boot_enabled: None,
            gpus: vec![],
            driver: Some(DriverInfo {
                version: "575.57.08".to_string(),
                nvidia_smi_available: true,
            }),
            cuda: None,
            vulkan: None,
            cuda_runtime: None,
        };
        assert!(profile.driver_installed());
    }

    #[test]
    fn vulkan_ready_requires_gpu_driver_loader_tool_and_device() {
        let mk =
            |gpus: Vec<GpuDevice>, driver_ok: bool, loader: bool, tool: bool, vk_device: bool| {
                HostGpuProfile {
                    os: OsInfo {
                        distro: "ubuntu".to_string(),
                        version: "22.04".to_string(),
                        kernel: "5.15.0".to_string(),
                    },
                    secure_boot_enabled: None,
                    gpus,
                    driver: driver_ok.then(|| DriverInfo {
                        version: "575.57.08".to_string(),
                        nvidia_smi_available: true,
                    }),
                    cuda: None,
                    vulkan: Some(VulkanInfo {
                        loader_present: loader,
                        vulkaninfo_available: tool,
                        nvidia_icd_present: vk_device,
                        nvidia_device_visible: vk_device,
                    }),
                    cuda_runtime: None,
                }
            };
        let gpu = GpuDevice {
            index: 0,
            name: "NVIDIA".to_string(),
            uuid: None,
            vram_bytes: 0,
            pcie_bus_id: None,
        };
        assert!(mk(vec![gpu.clone()], true, true, true, true).native_inference_vulkan_ready());
        // Missing ANY of GPU / driver / loader / vulkaninfo tool / device → not ready.
        assert!(!mk(vec![], true, true, true, true).native_inference_vulkan_ready());
        assert!(!mk(vec![gpu.clone()], false, true, true, true).native_inference_vulkan_ready());
        assert!(!mk(vec![gpu.clone()], true, false, true, true).native_inference_vulkan_ready());
        // libvulkan present but vulkaninfo tool missing → NOT ready (was the bug).
        assert!(!mk(vec![gpu.clone()], true, true, false, true).native_inference_vulkan_ready());
        assert!(!mk(vec![gpu], true, true, true, false).native_inference_vulkan_ready());
    }

    #[test]
    fn cuda_ready_requires_gpu_driver_and_cuda_runtime() {
        let mk = |gpus: Vec<GpuDevice>,
                  driver_ok: bool,
                  cuda_runtime: Option<CudaRuntimeInfo>| {
            HostGpuProfile {
                os: OsInfo {
                    distro: "ubuntu".to_string(),
                    version: "22.04".to_string(),
                    kernel: "5.15.0".to_string(),
                },
                secure_boot_enabled: None,
                gpus,
                driver: driver_ok.then(|| DriverInfo {
                    version: "575.57.08".to_string(),
                    nvidia_smi_available: true,
                }),
                cuda: None,
                vulkan: None,
                cuda_runtime,
            }
        };
        let gpu = GpuDevice {
            index: 0,
            name: "NVIDIA RTX A6000".to_string(),
            uuid: None,
            vram_bytes: 48 * 1024 * 1024 * 1024,
            pcie_bus_id: None,
        };
        let ready_rt = || {
            Some(CudaRuntimeInfo {
                cuda_runtime_present: true,
                python3_ok: true,
                venv_module_ok: true,
                nvcc_ok: true,
                ninja_ok: true,
                max_gpu_vram_bytes: 48 * 1024 * 1024 * 1024,
            })
        };
        // Fully ready.
        assert!(mk(vec![gpu.clone()], true, ready_rt()).native_inference_cuda_ready());
        // Missing ANY prerequisite → not ready (honest fail-closed).
        assert!(!mk(vec![], true, ready_rt()).native_inference_cuda_ready());
        assert!(!mk(vec![gpu.clone()], false, ready_rt()).native_inference_cuda_ready());
        assert!(!mk(vec![gpu.clone()], true, None).native_inference_cuda_ready());
        // CUDA runtime not visible → not ready.
        assert!(
            !mk(
                vec![gpu.clone()],
                true,
                Some(CudaRuntimeInfo {
                    cuda_runtime_present: false,
                    python3_ok: true,
                    venv_module_ok: true,
                    nvcc_ok: false,
                    ninja_ok: false,
                    max_gpu_vram_bytes: 0,
                })
            )
            .native_inference_cuda_ready()
        );
        // python3/venv missing → not ready (SGLang needs a managed venv).
        assert!(
            !mk(
                vec![gpu],
                true,
                Some(CudaRuntimeInfo {
                    cuda_runtime_present: true,
                    python3_ok: true,
                    venv_module_ok: false,
                    nvcc_ok: false,
                    ninja_ok: false,
                    max_gpu_vram_bytes: 0,
                })
            )
            .native_inference_cuda_ready()
        );
    }

    #[test]
    fn vulkan_helpers_reflect_profile() {
        let profile = HostGpuProfile {
            os: OsInfo {
                distro: "ubuntu".to_string(),
                version: "22.04".to_string(),
                kernel: "5.15.0".to_string(),
            },
            secure_boot_enabled: None,
            gpus: vec![],
            driver: None,
            cuda: None,
            vulkan: Some(VulkanInfo {
                loader_present: true,
                vulkaninfo_available: false,
                nvidia_icd_present: false,
                nvidia_device_visible: false,
            }),
            cuda_runtime: None,
        };
        assert!(profile.vulkan_loader_present());
        assert!(!profile.vulkaninfo_available());
        assert!(!profile.nvidia_vulkan_icd_present());
        assert!(!profile.vulkan_nvidia_device_visible());
    }

    #[test]
    fn parse_os_release_extracts_distro_and_version() {
        let sample = "NAME=\"Ubuntu\"\nVERSION=\"22.04 LTS\"\nID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"22.04\"\n";
        // Simulate the parsing logic inline since parse_os_release reads a file.
        let mut distro = String::new();
        let mut version = String::new();
        for line in sample.lines() {
            if let Some(val) = line.strip_prefix("ID=") {
                distro = val.trim_matches('"').to_string();
            } else if let Some(val) = line.strip_prefix("VERSION_ID=") {
                version = val.trim_matches('"').to_string();
            }
        }
        assert_eq!(distro, "ubuntu");
        assert_eq!(version, "22.04");
    }
}

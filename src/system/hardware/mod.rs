/// Hardware detection module
///
/// Provides abstraction for detecting GPU hardware with mock and real implementations.
/// The mock implementation is enabled by default for development without GPU hardware.
///
/// # Feature Flags
/// - `real-gpu`: Enable NVML-based real GPU detection (requires NVIDIA GPU and drivers)
///
/// # Environment Variables (Mock Mode)
/// - `MOCK_GPU_COUNT`: Number of mock GPUs (default: 1)
/// - `MOCK_VRAM_GB`: VRAM per GPU in GB (default: 8)
/// - `MOCK_CUDA_VERSION`: Mock CUDA version (default: "12.0")
pub mod gpu_detector;
pub mod gpu_process_monitor;
pub mod hardware_report;

pub mod mac_gpu;

pub use gpu_detector::{create_gpu_detector, GpuDetectionError, GpuDetector};
pub use gpu_process_monitor::{create_gpu_process_monitor, GpuProcessMonitor};
pub use hardware_report::{GpuInfo, RigHardwareReport};

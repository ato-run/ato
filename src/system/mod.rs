//! System-level modules for hardware and network management
//!
//! This module contains low-level system interactions:
//! - Hardware: GPU detection, monitoring, hardware reports
//! - Network: Service registry, mDNS, Tailscale, Traefik integration

pub mod hardware;
pub mod network;

// Re-export commonly used types
pub use hardware::{create_gpu_detector, GpuDetector, GpuInfo, RigHardwareReport};
pub use network::service_registry::ServiceRegistry;

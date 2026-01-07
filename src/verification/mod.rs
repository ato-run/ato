//! Verification and security modules
//!
//! This module contains UARC verification layers:
//! - verifier: Manifest verification and policy analysis
//! - signing: Cryptographic signature verification  
//! - egress_policy: Network egress policy enforcement
//! - egress_proxy: Egress proxy implementation
//! - path: Path validation and security
//! - dns_monitor: DNS request monitoring
//! - vram: GPU memory security
//!
//! Note: Audit logging has been moved to the `observability` module.

pub mod dns_monitor;
pub mod egress_policy;
pub mod egress_proxy;
pub mod path;
pub mod signing;
pub mod verifier;
pub mod vram;

// Re-export audit from observability for backward compatibility
pub use crate::observability::audit;
pub use crate::observability::audit::*;

pub use dns_monitor::*;
pub use egress_policy::*;
pub use egress_proxy::*;
pub use path::*;
// pub use signing::*; // Avoid conflict with legacy signing if needed?
pub use verifier::*;
pub use vram::*;

// Re-export common constants
pub const ENV_KEY_EGRESS_TOKEN: &str = "CAPSULED_EGRESS_TOKEN";

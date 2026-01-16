//! Verification and security modules
//!
//! v3.0: This module now contains only OS-native sandbox enforcement.
//! Validation and policy resolution have been moved to capsule-cli.
//!
//! Remaining components:
//! - sandbox: OS-native process sandboxing (Landlock/Seatbelt)
//! - dns_monitor: DNS request monitoring
//! - egress_proxy: Egress proxy implementation  
//! - path: Path validation and security
//! - vram: GPU memory security
//!
//! Moved to capsule-cli:
//! - verifier: L1 Source Policy + L2 Signature Verification
//! - signing: Ed25519 signature generation
//! - egress_policy: L4 Egress policy resolution (domain → IP)
//!
//! Note: Audit logging is handled by the caller (capsule-cli).

pub mod dns_monitor;
pub mod egress_policy;
pub mod egress_proxy;
pub mod path;
pub mod sandbox;
pub mod signing;
pub mod verifier;
pub mod vram;

pub use dns_monitor::*;
pub use egress_policy::*;
pub use egress_proxy::*;
pub use path::*;
pub use signing::*;
pub use verifier::*;
pub use vram::*;

// ENV constant kept for runtime use
pub const ENV_KEY_EGRESS_TOKEN: &str = "NACELLE_EGRESS_TOKEN";

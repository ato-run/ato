// Capsule type definitions (extracted from capsule-core to eliminate external dependency)
// This module provides UARC V1.1.0 compliant types used by both nacelle and CLI.

pub mod error;
pub mod identity;
pub mod license;
pub mod manifest;
pub mod orchestration;
pub mod profile;
pub mod runplan;
pub mod signing;
pub mod utils;

// Re-export commonly used types
pub use error::*;
pub use identity::*;
pub use license::*;
pub use manifest::*;
pub use orchestration::*;
pub use profile::*;
pub use runplan::*;
pub use signing::*;
pub use utils::*;

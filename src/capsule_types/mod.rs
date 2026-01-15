// Capsule type definitions (extracted from capsule-core to eliminate external dependency)
// This module provides UARC V1.1.0 compliant types used by both nacelle and CLI.

pub mod capsule_v1;
pub mod error;
pub mod runplan;
pub mod signing;
pub mod utils;

// Re-export commonly used types
pub use capsule_v1::*;
pub use error::*;
pub use runplan::*;
pub use signing::*;
pub use utils::*;

// Capsule type definitions (extracted from capsule to eliminate external dependency)
// This module provides UARC V1.1.0 compliant types used by both nacelle and CLI.

pub mod bridge;
pub mod command_spec;
pub mod error;
pub mod identity;
pub mod license;
pub mod manifest;
pub mod assets;/// `schema_version = "1"` — the authoring surface an Execution Identity may be
/// minted from. Deliberately NOT convertible from v0.3 (ADR-015 §1).
pub mod manifest_v1;
pub mod oci;
pub mod orchestration;
pub mod profile;
pub mod ready_state;
pub mod runplan;
pub mod signing;
pub mod utils;

// Re-export commonly used types
pub use bridge::*;
pub use command_spec::*;
pub use error::*;
pub use identity::*;
pub use license::*;
pub use manifest::*;
pub use oci::*;
pub use orchestration::*;
pub use profile::*;
pub use ready_state::*;
pub use runplan::*;
pub use signing::*;
pub use utils::*;

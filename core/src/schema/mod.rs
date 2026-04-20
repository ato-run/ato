//! Canonical schema definitions exported as JSON Schema for external consumers
//! (web-api registry search, SKILL.md, future `ato encap` / `ato validate` lints).
//!
//! See `README.md` for the reconciliation rules between `health.toml` and these
//! capability enums.

pub mod capabilities;

pub use capabilities::{
    Capabilities, FsWrites, Network, SchemaVersion, SideEffects,
};

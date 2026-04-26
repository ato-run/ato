//! Share specification, materialization, and execution.
//!
//! This module contains the types and logic shared between `ato-cli` and
//! `ato-desktop` for working with share URLs (`https://ato.run/s/...`).

pub mod executor;
pub mod types;

pub use executor::{
    execute_share, ShareExecutionMode, ShareExecutionResult, SharePipedSession, ShareRunRequest,
};
pub use types::*;

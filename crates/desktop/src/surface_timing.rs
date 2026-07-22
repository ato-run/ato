//! `SURFACE-TIMING` supervision-stage timing telemetry.
//!
//! Single-sourced in the runner crate (`runner::surface_timing`) — it times
//! execution/launch supervision stages, not GUI rendering, and is self-
//! contained (only `std::time` / `std::env`). Re-exported here so the Desktop
//! shell's existing `crate::surface_timing::…` call sites are unchanged and
//! the timers travel with the supervision code they measure.
//!
//! Phase 1 Step 5 (invert the shell side-paths): the supervision path no
//! longer reaches up into a desktop-crate telemetry module.

pub use runner::surface_timing::{
    ClickOrigin, SurfaceExtras, SurfaceStageTimer, emit_stage, emit_total,
};

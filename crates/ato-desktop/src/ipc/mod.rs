//! `src/ipc` — Desktop host-bridge IPC layer.
//!
//! This module is the single home for all IPC machinery that is shared
//! between system-capsule WebViews, guest-capsule WebViews, and the
//! internal test/debug harness.
//!
//! # Module structure
//!
//! | Sub-module | Responsibility |
//! |------------|---------------|
//! | `protocol` | Wire types: `IpcRequest`, `IpcResponse` |
//! | `principal` | `IpcPrincipal` enum — who is calling? (Phase 3) |
//! | `registry`  | `IpcCommandRegistry` — command → handler map (Phase 2) |
//! | `policy`    | Visibility + capability enforcement (Phase 4) |
//! | `broker`    | `IpcBroker::dispatch` — the single chokepoint (Phase 4) |
//! | `commands`  | Per-namespace handlers (Phase 5) |
//!
//! Phases 2–9 will progressively fill in the stubs below.

pub mod protocol;
pub mod principal;
pub mod policy;
pub mod registry;
pub mod broker;

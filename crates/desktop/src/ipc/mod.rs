//! `src/ipc` — Desktop host-bridge IPC wire types.
//!
//! Currently this module exposes only the wire `protocol` types
//! (`IpcRequest`, `IpcResponse`) consumed by the system-capsule bridge.
//! The earlier `broker`/`registry`/`policy`/`principal`/`commands`
//! scaffolding (Phases 2–9) was removed as dead code; reintroduce it
//! from history when that work is picked up.

pub mod protocol;

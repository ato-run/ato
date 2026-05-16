//! System capsule layer — Phase 1.
//!
//! Architectural framing (locked by the user in the planning round
//! that produced `/Users/.../glistening-wobbling-hellman.md`):
//!
//!   - Desktop core owns the substrate: WebView engine, OS window
//!     lifecycle, security boundary, IPC broker.
//!   - System capsules (`ato-store`, `ato-web-viewer`, `ato-settings`,
//!     `ato-windows`) are the privileged-but-bounded UI surfaces that
//!     ship with the desktop binary. They run inside Wry WebViews,
//!     served from bundled HTML/CSS/JS, and call back into the
//!     desktop via the `CapabilityBroker`. The broker is the *only*
//!     thing that touches `&mut App` privileged operations.
//!   - User capsules are unprivileged guest apps (the existing
//!     `crates/ato-desktop/src/bridge.rs` path). This module does NOT
//!     touch user-capsule bridge code.
//!
//! Stage A scope (this commit): typed scaffolding only. The existing
//! `web_bridge::BridgeAction`-based dispatch in `card_switcher.rs` /
//! `start_window.rs` is wrapped by a shim that translates each
//! `BridgeAction` into a `SystemCommand` and runs it through the
//! broker. UX must remain identical to R42.
//!
//! Stages B–D will (a) replace the HTML envelope with
//! `{capsule, command}`, (b) serve the HTML over a new
//! `capsule://system/<name>` custom protocol, and (c) retire the
//! Launcher window. See the plan file for stage receipts.

pub mod ato_dock;
pub mod ato_identity;
pub mod ato_launch;
pub mod ato_onboarding;
pub mod ato_settings;
pub mod ato_start;
pub mod ato_store;
pub mod ato_web_viewer;
pub mod ato_windows;
pub mod broker;
pub mod ipc;
pub mod manifest;

pub use broker::{BrokerError, Capability, CapabilityBroker, SystemCapsuleId, SystemCommand};

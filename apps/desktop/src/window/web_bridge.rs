//! **Deprecated (Stage B):** legacy `BridgeAction`-based IPC bridge.
//! The Card Switcher and StartWindow now route through
//! `crate::system_capsule::ipc`, which uses a typed
//! `{capsule, command}` envelope and the `CapabilityBroker`. This
//! module is retained for one stage so any out-of-tree experiments
//! using the old shape don't break; Stage C/D will delete it.
//!
//! Original design notes:
//!
//! IPC bridge between the Wry-hosted launcher HTML pages (Card
//! Switcher, StartWindow) and the rust-side GPUI actions. Pattern is
//! borrowed from `automation/transport.rs` + `focus_dispatcher.rs`:
//!
//!   - Wry's `with_ipc_handler` callback runs on whatever thread Wry
//!     chooses (typically the main thread, but we treat it as
//!     untrusted from a threading POV).
//!   - The handler does almost nothing — it parses the JSON message
//!     and pushes it onto a shared `Arc<Mutex<Vec<_>>>`.
//!   - A `foreground_executor` polling task drains that queue every
//!     50ms and dispatches on the GPUI main thread, where it has
//!     `&mut App` and can mutate globals / open / close windows.
//!
//! Keeping the IPC handler thread-free of GPUI mutation means we
//! don't have to reason about whether wry called us from the right
//! thread for any particular gpui call.
//!
//! Action vocabulary is intentionally small — every entry maps to a
//! concrete `&mut App` operation. New buttons inside the HTML add
//! new variants here.

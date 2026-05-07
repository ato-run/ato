//! Modal overlays drawn by the host on top of the stage.
//!
//! Each modal is a self-contained GPUI entity owning its own input
//! state. `DesktopShell` lazy-creates these entities when the
//! corresponding `AppState` "pending request" field flips from `None`
//! to `Some`, and drops them when it flips back. The render tree
//! conditionally inserts the overlay element via `.when(...)`, so the
//! modal participates in the same layout pass as the rest of the
//! shell.

pub(super) mod config_form;
pub(super) mod consent_form;

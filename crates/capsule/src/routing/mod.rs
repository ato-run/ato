//! Layer 3: Routing — manifest routing, input resolution, handle management.
pub mod discovery;
/// Handle/URL classifier. Lives in `ato-protocol` (N2) so `ato-desktop`
/// can consume it without linking capsule's runtime deps; re-exported
/// here so internal callers keep using `crate::routing::handle::*`.
pub mod handle {
    pub use ato_protocol::handle::*;
}
pub mod handle_store;
pub mod importer;
pub mod input_resolver;
pub mod launch_spec;
pub mod router;

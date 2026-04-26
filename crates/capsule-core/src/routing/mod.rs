//! Layer 3: Routing — manifest routing, input resolution, handle management.
pub mod discovery;
/// Handle/URL classifier. Lives in `capsule-wire` (N2) so `ato-desktop`
/// can consume it without linking capsule-core's runtime deps; re-exported
/// here so internal callers keep using `crate::routing::handle::*`.
pub mod handle {
    pub use capsule_wire::handle::*;
}
pub mod handle_store;
pub mod importer;
pub mod input_resolver;
pub mod launch_spec;
pub mod router;

//! Runtime binding for a materialised, host-window-attached system capsule.
//!
//! `SystemCapsuleBinding` is the runtime counterpart to the static
//! `SystemCapsuleDescriptor` in `manifest.rs`.  It is created by
//! window-open code once capsule assets are available on disk and the
//! GPUI window exists.

//! nacelle host↔helper IPC.
//!
//! The wire types (`NacelleEvent`, `TerminalCommand`, `ExportedArtifact`, and
//! the spec-version helpers) are single-sourced in `protocol::nacelle_ipc` and
//! re-exported here so nacelle's existing `crate::internal_api::…` /
//! `nacelle::internal_api::…` paths keep resolving unchanged. The `.emit()`
//! behaviour stays here — stdout I/O has no place in the dependency-light wire
//! crate — as an extension trait.

pub use protocol::nacelle_ipc::{
    CURRENT_SPEC_VERSION, ExportedArtifact, LEGACY_SPEC_VERSION, NEXT_SPEC_VERSION, NacelleEvent,
    TerminalCommand, is_supported_spec_version, validate_spec_version,
};

/// Emit a [`NacelleEvent`] to stdout as one NDJSON line. Lives in nacelle (not
/// the wire crate) because it performs I/O; import this trait to call
/// `event.emit()` as before.
pub trait NacelleEventExt {
    fn emit(&self);
}

impl NacelleEventExt for NacelleEvent {
    fn emit(&self) {
        use std::io::Write;
        if let Ok(json) = serde_json::to_string(self) {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            // Use writeln! instead of println! to avoid panicking on broken pipe.
            if writeln!(lock, "{json}").is_ok() {
                let _ = lock.flush();
            }
        }
    }
}

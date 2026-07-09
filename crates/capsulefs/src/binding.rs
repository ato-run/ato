//! File-binding writeback semantics (requirements §1).
//!
//! A user-selected file binding is attached **after restore**, never sealed.
//! How writes flow back to the host is the security-sensitive part, so it is an
//! explicit policy rather than an implicit default:
//!
//! * [`ReadOnly`](WritebackMode::ReadOnly) — host file mounted read-only (the
//!   default, MustHave).
//! * [`CopyOut`](WritebackMode::CopyOut) — session output copied out explicitly
//!   (MustHave).
//! * [`CopyIn`](WritebackMode::CopyIn) — host file copied in explicitly
//!   (NiceToHave; not implemented in Stage 1).
//! * [`ReadWriteWithExplicitGrant`](WritebackMode::ReadWriteWithExplicitGrant) —
//!   in-place read/write (DecisionNeeded; high accident risk, gated on a
//!   grant/consent design — not implemented in Stage 1).
//!
//! This type lives in `capsulefs` (below `capsule`) so it cannot reuse
//! `capsule::BindingSpec` without a dependency cycle; mapping a manifest
//! `[bindings.*]` to a [`FileBindingSpec`] is a `capsule`/cli-layer concern.

use serde::{Deserialize, Serialize};

/// How a user-selected file binding's writes flow back to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WritebackMode {
    /// Host file mounted read-only (default).
    #[default]
    ReadOnly,
    /// Session output copied out to the host explicitly.
    CopyOut,
    /// Host file copied into the session explicitly (NiceToHave; not implemented).
    CopyIn,
    /// In-place read/write (DecisionNeeded; gated on an explicit grant design).
    ReadWriteWithExplicitGrant,
}

impl WritebackMode {
    /// Whether this mode is implemented in Stage 1 (only `ReadOnly`/`CopyOut`).
    pub fn is_implemented(&self) -> bool {
        matches!(self, WritebackMode::ReadOnly | WritebackMode::CopyOut)
    }

    /// Whether this mode can write (anything but `ReadOnly`).
    pub fn is_writable(&self) -> bool {
        !matches!(self, WritebackMode::ReadOnly)
    }
}

/// A user-selected file binding: a host path bound at a guest target with a
/// writeback policy. Resolved/attached post-restore (never sealed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBindingSpec {
    /// Binding name (matches the manifest `[bindings.<name>]`).
    pub name: String,
    /// Guest mount target.
    pub target: String,
    /// Writeback policy (defaults to [`WritebackMode::ReadOnly`]).
    #[serde(default)]
    pub writeback: WritebackMode,
}

impl FileBindingSpec {
    /// A read-only binding (the safe default).
    pub fn read_only(name: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            target: target.into(),
            writeback: WritebackMode::ReadOnly,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writeback_mode_defaults_to_read_only() {
        assert_eq!(WritebackMode::default(), WritebackMode::ReadOnly);
    }

    #[test]
    fn file_binding_spec_defaults_writeback_to_read_only() {
        let spec: FileBindingSpec =
            serde_json::from_str(r#"{"name":"data","target":"/data"}"#).unwrap();
        assert_eq!(spec.writeback, WritebackMode::ReadOnly);
        assert_eq!(
            FileBindingSpec::read_only("data", "/data").writeback,
            WritebackMode::ReadOnly
        );
    }

    #[test]
    fn writeback_mode_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&WritebackMode::ReadOnly).unwrap(),
            "\"read_only\""
        );
        assert_eq!(
            serde_json::to_string(&WritebackMode::CopyOut).unwrap(),
            "\"copy_out\""
        );
        assert_eq!(
            serde_json::to_string(&WritebackMode::CopyIn).unwrap(),
            "\"copy_in\""
        );
        assert_eq!(
            serde_json::to_string(&WritebackMode::ReadWriteWithExplicitGrant).unwrap(),
            "\"read_write_with_explicit_grant\""
        );
    }

    #[test]
    fn only_read_only_and_copy_out_are_implemented() {
        assert!(WritebackMode::ReadOnly.is_implemented());
        assert!(WritebackMode::CopyOut.is_implemented());
        assert!(!WritebackMode::CopyIn.is_implemented());
        assert!(!WritebackMode::ReadWriteWithExplicitGrant.is_implemented());
        assert!(!WritebackMode::ReadOnly.is_writable());
        assert!(WritebackMode::CopyOut.is_writable());
    }

    #[test]
    fn file_binding_spec_round_trips_through_json() {
        let spec = FileBindingSpec {
            name: "out".into(),
            target: "/out".into(),
            writeback: WritebackMode::CopyOut,
        };
        let json = serde_json::to_string(&spec).unwrap();
        let back: FileBindingSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, spec);
    }
}

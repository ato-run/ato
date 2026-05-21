//! `debug.*` commands — developer tooling, never reachable from a WebView.
//!
//! All commands in this namespace are `InternalOnly`.  The transport adapter
//! must screen them before any registry lookup so their existence is never
//! revealed to untrusted callers.

use super::{spec, IpcCommandSpec};
use crate::ipc::policy::IpcVisibility::InternalOnly;

pub fn specs() -> Vec<IpcCommandSpec> {
    vec![
        spec("debug.reloadSystemCapsule", InternalOnly, &[]),
    ]
}

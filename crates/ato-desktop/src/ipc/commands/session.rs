//! `session.*` commands — capsule session lifecycle, system-capsule only.

use super::{IpcCommandSpec, spec};
use crate::ipc::policy::IpcVisibility::SystemCapsule;
use crate::system_capsule::broker::Capability;

pub fn specs() -> Vec<IpcCommandSpec> {
    vec![
        spec("session.list", SystemCapsule, &[Capability::WindowsList]),
        spec("session.start", SystemCapsule, &[Capability::WebviewCreate]),
        spec("session.stop", SystemCapsule, &[Capability::WindowsClose]),
        spec(
            "session.restart",
            SystemCapsule,
            &[Capability::WebviewCreate],
        ),
        spec(
            "session.activateWindow",
            SystemCapsule,
            &[Capability::WindowsActivate],
        ),
        spec(
            "session.closeWindow",
            SystemCapsule,
            &[Capability::WindowsClose],
        ),
        spec(
            "session.closeTarget",
            SystemCapsule,
            &[Capability::WindowsCloseTarget],
        ),
        spec("session.logs", SystemCapsule, &[]),
    ]
}

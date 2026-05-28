//! `shell.*` commands — browser-like surface, public or system-only.

use super::{spec, IpcCommandSpec};
use crate::ipc::policy::IpcVisibility;
use crate::system_capsule::broker::Capability;

pub fn specs() -> Vec<IpcCommandSpec> {
    use IpcVisibility::{PublicCapsule, SystemCapsule};
    vec![
        // Available to any WebView (guest or system).
        spec("shell.openExternal", PublicCapsule, &[]),
        // System-capsule only: navigation within the desktop shell.
        spec(
            "shell.openCapsuleLink",
            SystemCapsule,
            &[Capability::LaunchSystemCapsule],
        ),
        spec(
            "shell.openStore",
            SystemCapsule,
            &[Capability::LaunchSystemCapsule],
        ),
        spec(
            "shell.openSettings",
            SystemCapsule,
            &[Capability::LaunchSystemCapsule],
        ),
        spec(
            "shell.closeWindow",
            SystemCapsule,
            &[Capability::WindowsClose],
        ),
        spec("shell.setWindowTitle", SystemCapsule, &[]),
    ]
}

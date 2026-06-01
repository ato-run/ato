//! `registry.*` commands — capsule registry browse, system-capsule only.

use super::{IpcCommandSpec, spec};
use crate::ipc::policy::IpcVisibility::SystemCapsule;
use crate::system_capsule::broker::Capability;

pub fn specs() -> Vec<IpcCommandSpec> {
    vec![
        spec("registry.search", SystemCapsule, &[]),
        spec("registry.getCapsule", SystemCapsule, &[]),
        spec("registry.getFeatured", SystemCapsule, &[]),
        spec(
            "registry.runCapsule",
            SystemCapsule,
            &[Capability::WebviewCreate],
        ),
        spec("registry.installCapsule", SystemCapsule, &[]),
        spec("registry.browseUrl", SystemCapsule, &[]),
    ]
}

//! `capsule.*` commands — minimal public surface available to any WebView.

use super::{spec, IpcCommandSpec};
use crate::ipc::policy::IpcVisibility;

pub fn specs() -> Vec<IpcCommandSpec> {
    use IpcVisibility::PublicCapsule;
    vec![
        spec("capsule.context.get",          PublicCapsule, &[]),
        spec("capsule.permissions.request",  PublicCapsule, &[]),
        spec("capsule.secrets.request",      PublicCapsule, &[]),
        spec("capsule.state.get",            PublicCapsule, &[]),
        spec("capsule.state.set",            PublicCapsule, &[]),
    ]
}

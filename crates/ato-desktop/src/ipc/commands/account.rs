//! `account.*` commands — authentication status and login/logout, system-capsule only.

use super::{IpcCommandSpec, spec};
use crate::ipc::policy::IpcVisibility::SystemCapsule;
use crate::system_capsule::broker::Capability;

pub fn specs() -> Vec<IpcCommandSpec> {
    vec![
        spec("account.authStatus", SystemCapsule, &[]),
        spec("account.login", SystemCapsule, &[Capability::WebviewCreate]),
        spec("account.logout", SystemCapsule, &[]),
    ]
}

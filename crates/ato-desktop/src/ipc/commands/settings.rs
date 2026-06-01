//! `settings.*` commands — desktop settings read/write, system-capsule only.

use super::{IpcCommandSpec, spec};
use crate::ipc::policy::IpcVisibility::SystemCapsule;
use crate::system_capsule::broker::Capability;

pub fn specs() -> Vec<IpcCommandSpec> {
    vec![
        spec("settings.get", SystemCapsule, &[Capability::SettingsRead]),
        spec("settings.set", SystemCapsule, &[Capability::SettingsWrite]),
        spec(
            "settings.navigateTab",
            SystemCapsule,
            &[Capability::SettingsRead],
        ),
        spec(
            "settings.globalAction",
            SystemCapsule,
            &[Capability::SettingsWrite],
        ),
        spec(
            "settings.loadSecretsSnapshot",
            SystemCapsule,
            &[Capability::SettingsRead],
        ),
        spec(
            "settings.putSecret",
            SystemCapsule,
            &[Capability::SettingsWrite],
        ),
        spec(
            "settings.deleteSecret",
            SystemCapsule,
            &[Capability::SettingsWrite],
        ),
        spec(
            "settings.grantSecret",
            SystemCapsule,
            &[Capability::SettingsWrite],
        ),
        spec(
            "settings.revokeSecret",
            SystemCapsule,
            &[Capability::SettingsWrite],
        ),
    ]
}

//! `ato-settings` system capsule — Settings UI.
//!
//! Stage A: stubs only. Stage C will port the GPUI
//! `launcher::settings_content` tree into HTML served at
//! `capsule://system/ato-settings/index.html`, and this dispatch
//! module will gain real handlers.
//!
//! Phase 1 deliberately gates `SetToggle` as `Forbidden` — Phase 2
//! adds a consent prompt before granting `SettingsWrite`.

use gpui::{AnyWindowHandle, App};

use crate::system_capsule::broker::{BrokerError, Capability};

#[derive(Debug)]
pub enum SettingsCommand {
    Close,
    /// Reserved for Stage C. Read-only navigation between settings
    /// tabs (一般 / セキュリティ / ターミナル / …). Carries the
    /// tab id as a string in Phase 1.
    NavigateTab(String),
    /// Reserved for Stage C + Phase 2. Mutating a settings toggle
    /// requires `SettingsWrite`, which is not in `ato-settings`'s
    /// allowlist yet — every call returns `Forbidden` today.
    SetToggle { key: String, value: bool },
}

impl SettingsCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            SettingsCommand::Close => Capability::WindowsClose,
            SettingsCommand::NavigateTab(_) => Capability::SettingsRead,
            SettingsCommand::SetToggle { .. } => Capability::SettingsWrite,
        }
    }
}

pub fn dispatch(
    cx: &mut App,
    host: AnyWindowHandle,
    command: SettingsCommand,
) -> Result<(), BrokerError> {
    match command {
        SettingsCommand::Close => {
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        SettingsCommand::NavigateTab(tab) => {
            // Phase 1 stub. Stage C lands real tab routing via the
            // ato-settings HTML + LauncherViewState integration.
            tracing::info!(?tab, "ato_settings: NavigateTab (stub)");
            let _ = cx;
        }
        SettingsCommand::SetToggle { key, value } => {
            // This arm is unreachable in Phase 1 because the broker
            // already rejected the call with `Forbidden` (no
            // `SettingsWrite` in the manifest's allowlist). Kept
            // for exhaustiveness so the surface compiles cleanly
            // when Phase 2 adds the capability.
            tracing::error!(
                ?key,
                ?value,
                "ato_settings::SetToggle dispatched despite Forbidden — broker bug?"
            );
            let _ = (cx, host);
        }
    }
    Ok(())
}

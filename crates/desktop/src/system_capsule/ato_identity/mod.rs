//! `ato-identity` system capsule — Account / Identity popover.
//!
//! Triggered when the user clicks the avatar button at the right end
//! of the Control Bar. Renders `assets/system/ato-identity/index.html`
//! in a small Wry window. Phase 1 surface is intentionally honest:
//!
//!   - A user-identity header (avatar + display name + email).
//!   - A list of menu items (Profile / Account / Workspace / Trust /
//!     Preferences) labelled "近日公開" — these are placeholders for
//!     Phase 2 panels and cannot be clicked.
//!   - Items that DO work today (Store / Settings) hand off to the
//!     corresponding existing system capsules.
//!   - A 閉じる button.
//!
//! That keeps the popover from being a "lying UI" while still giving
//! the user something concrete to click instead of a no-op log.

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use crate::system_capsule::broker::{BrokerError, Capability};

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IdentityCommand {
    /// User clicked 閉じる / pressed Escape — close the popover.
    Close,
    /// User clicked the "ストアを開く" row — close the popover and
    /// dispatch the existing OpenStoreWindow action.
    OpenStore,
    /// User clicked the "設定を開く" row — close the popover and
    /// dispatch the existing ShowSettings action.
    OpenSettings,
}

impl IdentityCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            IdentityCommand::Close => Capability::WindowsClose,
            IdentityCommand::OpenStore | IdentityCommand::OpenSettings => {
                Capability::LaunchSystemCapsule
            }
        }
    }
}

pub fn dispatch(
    cx: &mut App,
    host: AnyWindowHandle,
    command: IdentityCommand,
) -> Result<(), BrokerError> {
    match command {
        IdentityCommand::Close => {
            tracing::info!("ato_identity: closed by user");
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        IdentityCommand::OpenStore => {
            tracing::info!("ato_identity: opening Store");
            let _ = host.update(cx, |_, window, _| window.remove_window());
            if let Err(err) = crate::window::store::open_store_window(cx) {
                tracing::error!(?err, "ato_identity: open_store_window failed");
            }
        }
        IdentityCommand::OpenSettings => {
            tracing::info!("ato_identity: opening Settings");
            let _ = host.update(cx, |_, window, _| window.remove_window());
            if let Err(err) = crate::window::settings_window::open_settings_window(cx) {
                tracing::error!(?err, "ato_identity: open_settings_window failed");
            }
        }
    }
    Ok(())
}

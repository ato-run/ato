//! `ato-store` system capsule — Store / Registry frontend.
//!
//! Stage A: thin wrapper that delegates to the existing
//! `crate::window::store::open_store_window`. Stage C will swap the
//! actual rendering to HTML served at `capsule://system/ato-store/`,
//! but the command surface here stays stable.

use gpui::{AnyWindowHandle, App};

use crate::system_capsule::broker::{BrokerError, Capability};

#[derive(Debug)]
pub enum StoreCommand {
    /// Open / focus the Store window. Mirrors what the
    /// StartWindow's `OpenStore` IPC action did pre-refactor.
    Open,
    /// Close the Store window that issued this command.
    Close,
}

impl StoreCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            StoreCommand::Open => Capability::LaunchSystemCapsule,
            StoreCommand::Close => Capability::WebviewCreate,
        }
    }
}

pub fn dispatch(
    cx: &mut App,
    host: AnyWindowHandle,
    command: StoreCommand,
) -> Result<(), BrokerError> {
    match command {
        StoreCommand::Open => {
            if let Err(err) = crate::window::store::open_store_window(cx) {
                tracing::error!(error = %err, "ato_store: open_store_window failed");
            }
            // The caller (e.g. a StartWindow) typically wants its
            // own window dismissed after handing off to the Store.
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        StoreCommand::Close => {
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
    }
    Ok(())
}

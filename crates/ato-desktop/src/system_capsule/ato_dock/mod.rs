//! Dock system capsule IPC handler.
//!
//! Handles commands sent from the `ato-dock` WebView page.
//! The `Login` command runs `ato login` in a background thread,
//! then re-opens the Dock window so it picks up the fresh identity.

use anyhow::Result;
use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use super::broker::Capability;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DockCommand {
    Login,
}

impl DockCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            DockCommand::Login => Capability::WebviewCreate,
        }
    }
}

pub fn dispatch(cx: &mut App, _host: AnyWindowHandle, command: DockCommand) -> Result<()> {
    match command {
        DockCommand::Login => {
            let ato_bin = crate::orchestrator::resolve_ato_binary().ok();
            let async_app = cx.to_async();
            cx.foreground_executor()
                .spawn(async move {
                    if let Some(bin) = ato_bin {
                        // Run `ato login` — it opens browser for OAuth and blocks until done.
                        let _ = std::process::Command::new(&bin)
                            .arg("login")
                            .stdin(std::process::Stdio::null())
                            .status();
                    }
                    // After login attempt, reopen the dock so it re-fetches identity.
                    let _ = async_app.update(|cx| {
                        use crate::window::dock::DockWindowSlot;
                        // Close the existing slot so open_dock_window sees no existing window.
                        cx.set_global(DockWindowSlot(None));
                        let _ = crate::window::dock::open_dock_window(cx);
                    });
                })
                .detach();
            Ok(())
        }
    }
}

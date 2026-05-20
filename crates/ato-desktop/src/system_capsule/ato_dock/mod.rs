//! Dock system capsule IPC handler.
//!
//! Handles commands sent from the `ato-dock` WebView page.
//! The `Login` command opens the in-Desktop OAuth WebView window
//! (`AuthLoginWindow`) instead of launching an external browser.

use anyhow::Result;
use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use super::broker::Capability;

/// Source-of-truth shape for a developer-imported capsule project.
/// Drives both the cloning/validation step and how the inferred
/// manifest's `name` slug seed is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockSourceKind {
    GithubRepo,
    LocalPath,
}

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
            crate::window::auth_login_window::open_auth_login_window(cx)?;
            Ok(())
        }
    }
}

//! `ato-web-viewer` system capsule — Web link viewer chrome.
//!
//! Stage A: stubs only. Stage D will port `web_link_view`'s GPUI
//! tab strip + back/forward/reload chrome to HTML served at
//! `capsule://system/ato-web-viewer/index.html`. The Wry tab
//! WebViews themselves stay native (they navigate the open web).
//!
//! The command surface is declared now so the broker's match
//! exhaustively covers all four system capsules from Stage A.

use gpui::{AnyWindowHandle, App};

use crate::system_capsule::broker::{BrokerError, Capability};

#[derive(Debug)]
pub enum WebViewerCommand {
    Back,
    Forward,
    Reload,
    NewTab,
    CloseTab { tab_id: usize },
    SelectTab { tab_id: usize },
    Navigate { url: String },
}

impl WebViewerCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            WebViewerCommand::NewTab => Capability::TabsCreate,
            WebViewerCommand::Back
            | WebViewerCommand::Forward
            | WebViewerCommand::Reload
            | WebViewerCommand::CloseTab { .. }
            | WebViewerCommand::SelectTab { .. } => Capability::WebviewCreate,
            WebViewerCommand::Navigate { .. } => Capability::WebviewCreate,
        }
    }
}

pub fn dispatch(
    _cx: &mut App,
    _host: AnyWindowHandle,
    command: WebViewerCommand,
) -> Result<(), BrokerError> {
    // Stage A stub. Stage D rewires `web_link_view.rs` to issue
    // these commands instead of its current GPUI-direct
    // `webview.evaluate_script("history.back();")` calls.
    tracing::debug!(?command, "ato_web_viewer: stub dispatch");
    Ok(())
}

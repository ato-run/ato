//! `ato-store` system capsule — Store / Registry frontend.
//!
//! Stage B+: the store window loads `assets/system/ato-store/index.html`
//! via a `capsule-store://` custom protocol (Wry `with_asynchronous_custom_protocol`).
//! This module adds the `OpenCapsule` and `BrowseUrl` IPC commands so the
//! HTML can trigger capsule launches and external-URL navigation.

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use crate::state::GuestRoute;
use crate::system_capsule::broker::{BrokerError, Capability};

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoreCommand {
    /// Open / focus the Store window.
    Open,
    /// Close the Store window that issued this command.
    Close,
    /// Launch a capsule from the store catalog. Triggers the standard
    /// consent → boot wizard flow via `open_consent_window_for_route`.
    OpenCapsule { handle: String },
    /// Run a capsule immediately (temporary use). Internally resolves,
    /// installs if missing, then launches.
    RunCapsule { handle: String },
    /// Install a capsule into the local store for persistent access.
    /// Installs but does not necessarily launch.
    InstallCapsule { handle: String },
    /// Open an external HTTPS URL in a new WebLinkView window.
    BrowseUrl { url: String },
    /// Request current session status. Desktop responds via
    /// evaluate_script with a CustomEvent.
    GetSessionStatus,
    /// Trigger the desktop auth flow (ato login --desktop-webview).
    Login,
}

impl StoreCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            StoreCommand::Open => Capability::LaunchSystemCapsule,
            StoreCommand::Close => Capability::WebviewCreate,
            StoreCommand::OpenCapsule { .. } => Capability::WebviewCreate,
            StoreCommand::RunCapsule { .. } => Capability::WebviewCreate,
            StoreCommand::InstallCapsule { .. } => Capability::LaunchSystemCapsule,
            StoreCommand::BrowseUrl { .. } => Capability::WebviewCreate,
            StoreCommand::GetSessionStatus => Capability::WebviewCreate,
            StoreCommand::Login => Capability::LaunchSystemCapsule,
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
        StoreCommand::OpenCapsule { handle }
        | StoreCommand::RunCapsule { handle }
        | StoreCommand::InstallCapsule { handle } => {
            let label = handle
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(&handle)
                .to_string();
            let route = GuestRoute::CapsuleHandle {
                handle: handle.clone(),
                label,
            };
            tracing::info!(handle = %handle, "ato_store: opening/installing/running capsule from catalog");
            if let Err(err) = crate::window::launch_window::open_consent_window_for_route(cx, route)
            {
                tracing::error!(error = %err, handle = %handle, "ato_store: open_consent_window_for_route failed");
            }
        }
        StoreCommand::GetSessionStatus => {
            tracing::debug!(
                "ato_store: GetSessionStatus — stub, responds via init_script in future"
            );
            // TODO: In a follow-up PR, dispatch ato desktop-auth-handoff,
            // evaluate_script a CustomEvent('ato:auth-state-changed', {authenticated, publisher}).
        }
        StoreCommand::Login => {
            tracing::info!("ato_store: Login triggered — desktop auth flow");
            // TODO: spawn ato login --desktop-webview as child process,
            // open a second WebView for OAuth, on completion inject auth token
            // and dispatch a CustomEvent to the store WebView.
        }
        StoreCommand::BrowseUrl { url } => {
            if let Ok(parsed) = url::Url::parse(&url) {
                if matches!(parsed.scheme(), "http" | "https") {
                    let route = GuestRoute::ExternalUrl(parsed);
                    if let Err(err) = crate::window::open_app_window(cx, route) {
                        tracing::error!(error = %err, url = %url, "ato_store: browse_url open_app_window failed");
                    }
                } else {
                    tracing::warn!(url = %url, "ato_store: BrowseUrl rejected non-http scheme");
                }
            } else {
                tracing::warn!(url = %url, "ato_store: BrowseUrl parse failed");
            }
        }
    }
    Ok(())
}

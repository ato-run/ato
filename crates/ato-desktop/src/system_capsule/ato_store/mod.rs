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
    /// Open / focus the Store window. Mirrors what the
    /// StartWindow's `OpenStore` IPC action did pre-refactor.
    Open,
    /// Close the Store window that issued this command.
    Close,
    /// Launch a capsule from the store catalog. Triggers the standard
    /// consent → boot wizard flow via `open_consent_window_for_route`.
    OpenCapsule { handle: String },
    /// Open an external HTTPS URL in a new WebLinkView window.
    BrowseUrl { url: String },
}

impl StoreCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            StoreCommand::Open => Capability::LaunchSystemCapsule,
            StoreCommand::Close => Capability::WebviewCreate,
            StoreCommand::OpenCapsule { .. } => Capability::WebviewCreate,
            StoreCommand::BrowseUrl { .. } => Capability::WebviewCreate,
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
        StoreCommand::OpenCapsule { handle } => {
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
            tracing::info!(handle = %handle, "ato_store: opening capsule from catalog");
            if let Err(err) =
                crate::window::launch_window::open_consent_window_for_route(cx, route)
            {
                tracing::error!(error = %err, handle = %handle, "ato_store: open_consent_window_for_route failed");
            }
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

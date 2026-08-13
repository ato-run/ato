//! `ato-windows` system capsule — owns the Card Switcher overlay
//! and the StartWindow ("new window" picker) surfaces.
//!
//! Stage A: the dispatch logic that used to live in
//! `crate::window::card_switcher::dispatch` and
//! `crate::window::start_window::dispatch` is consolidated here, so
//! the broker has a single per-capsule entry point. The two callers
//! still receive their own `BridgeAction` from
//! `crate::window::web_bridge` — they translate it into
//! `WindowsCommand` and run it through `CapabilityBroker::dispatch`,
//! which routes back here.
//!
//! Stage B will switch the WebView's IPC envelope to
//! `{capsule, command}` directly, removing the `BridgeAction`
//! translation step.

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use crate::state::session::{
    SessionClient, SessionClientId, SessionClientKind, SessionClientState, SessionRegistry,
};
use crate::system_capsule::broker::{BrokerError, Capability};
use crate::window::card_switcher::CardSwitcherWindowSlot;
use crate::window::content_windows::OpenContentWindows;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WindowsCommand {
    /// Close the Card Switcher overlay (called from the switcher's
    /// own page; clears the slot global so the next bar click opens
    /// a fresh one).
    CloseSwitcher,
    /// Close the StartWindow that issued this command. The
    /// StartWindow has no slot — closing is purely
    /// `host.remove_window()`.
    CloseStartWindow,
    /// Raise the target content window. The `host` is the switcher
    /// that issued the request — it dismisses itself after.
    ActivateWindow {
        #[serde(rename = "windowId")]
        window_id: u64,
    },
    /// Close the target content window. The Card Switcher stays open;
    /// the frontend removes the card immediately from the DOM and
    /// `on_window_closed` handles registry cleanup asynchronously.
    /// No-op if the target window is already closed.
    CloseWindow {
        #[serde(rename = "windowId")]
        window_id: u64,
    },
    /// Open a fresh StartWindow + dismiss the calling switcher.
    OpenStart,
    /// Stop a capsule session by session_id.
    StopSession {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    /// Open an OCI session endpoint through Desktop's normal URL surface.
    OpenEndpoint {
        url: String,
        #[serde(rename = "sessionId")]
        session_id: String,
    },
}

impl WindowsCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            WindowsCommand::CloseSwitcher | WindowsCommand::CloseStartWindow => {
                Capability::WindowsClose
            }
            WindowsCommand::ActivateWindow { .. } | WindowsCommand::OpenEndpoint { .. } => {
                Capability::WindowsActivate
            }
            WindowsCommand::CloseWindow { .. } => Capability::WindowsCloseTarget,
            WindowsCommand::OpenStart => Capability::LaunchSystemCapsule,
            WindowsCommand::StopSession { .. } => Capability::WindowsCloseTarget,
        }
    }
}

pub fn dispatch(
    cx: &mut App,
    host: AnyWindowHandle,
    command: WindowsCommand,
) -> Result<(), BrokerError> {
    match command {
        WindowsCommand::CloseSwitcher => {
            cx.set_global(CardSwitcherWindowSlot(None));
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        WindowsCommand::CloseStartWindow => {
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        WindowsCommand::ActivateWindow { window_id } => {
            // Look up the target handle in the cross-window registry.
            // Missing IDs (a window closed between snapshot injection
            // and click) are no-ops; we still dismiss the switcher.
            let target = cx
                .global::<OpenContentWindows>()
                .get(window_id)
                .map(|e| e.handle);
            if let Some(target) = target {
                // Bump MRU so the Control Bar's omnibar reflects the
                // new front window's URL. `focus()` only stamps
                // last_focused_at; `activate_window` does the actual
                // `makeKeyAndOrderFront:`.
                cx.global_mut::<OpenContentWindows>().focus(window_id);
                let _ = target.update(cx, |_, window, _| window.activate_window());
            }
            cx.set_global(CardSwitcherWindowSlot(None));
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        WindowsCommand::OpenStart => {
            crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                cx.set_global(CardSwitcherWindowSlot(None));
                cx.set_global(crate::window::card_switcher::CardSwitcherEntitySlot(None));
                let _ = host.update(cx, |_, window, _| window.remove_window());
                if let Err(err) = crate::window::home::show_ato_home(cx) {
                    tracing::error!(error = %err, "ato_windows: show_ato_home failed");
                }
            });
        }
        WindowsCommand::CloseWindow { window_id } => {
            // Look up the target handle. If the window was already closed
            // between the snapshot and the click, treat as no-op.
            tracing::info!(
                window_id,
                "ato_windows: close button requested target window close"
            );
            let target = cx
                .global::<OpenContentWindows>()
                .get(window_id)
                .map(|e| e.handle);
            if let Some(target) = target {
                let _ = target.update(cx, |_, window, _| window.remove_window());
                tracing::info!(window_id, "ato_windows: close_window dispatched");
            } else {
                tracing::debug!(
                    window_id,
                    "ato_windows: close_window — window already closed (no-op)"
                );
            }
            // The Card Switcher stays open; the frontend already removed
            // the card from the DOM. `on_window_closed` in app.rs handles
            // registry cleanup when the OS close event fires.
        }
        WindowsCommand::StopSession { session_id } => {
            // Use the non-blocking stop path for all session kinds.
            // `stop_session_once_with_ui_completion` sets process_state to Stopping immediately
            // and then dispatches the actual stop (ato stop --id / ato stop
            // --session) on a background executor, then posts completion back
            // to the UI thread so the row does not stay in teardown/loading
            // state after the child process has stopped.
            crate::window::stop_session_once_with_ui_completion(cx, &session_id);
            crate::window::card_switcher::refresh_session_snapshot(cx);
            tracing::info!(
                session_id = %session_id,
                "ato_windows: StopSession dispatched (non-blocking)"
            );
        }
        WindowsCommand::OpenEndpoint { url, session_id } => {
            match crate::window::dock::open_external_url(cx, &url) {
                Ok(handle) => {
                    let window_id = handle.window_id().as_u64();
                    let registry = cx.global_mut::<SessionRegistry>();
                    if registry.get_session(&session_id).is_some() {
                        registry.attach_client(SessionClient {
                            client_id: SessionClientId::next(),
                            session_id: session_id.clone(),
                            client_kind: SessionClientKind::AtoWindow,
                            window_id: Some(window_id),
                            pane_id: None,
                            state: SessionClientState::Attached,
                            attached_at: std::time::SystemTime::now(),
                            last_seen_at: std::time::SystemTime::now(),
                        });
                    } else {
                        tracing::warn!(
                            %url,
                            %session_id,
                            window_id,
                            "ato_windows: endpoint opened without matching session"
                        );
                    }
                    crate::window::card_switcher::refresh_session_snapshot(cx);
                    tracing::info!(
                        %url,
                        %session_id,
                        window_id,
                        "ato_windows: endpoint opened and attached to session"
                    );
                }
                Err(error) => {
                    tracing::error!(%url, %session_id, %error, "ato_windows: endpoint open failed");
                }
            }
        }
    }
    Ok(())
}

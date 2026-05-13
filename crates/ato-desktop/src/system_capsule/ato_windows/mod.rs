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

use crate::system_capsule::broker::{BrokerError, Capability};
use crate::window::card_switcher::CardSwitcherWindowSlot;
use crate::window::content_windows::OpenContentWindows;

#[derive(Debug)]
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
    ActivateWindow { window_id: u64 },
    /// Open a fresh StartWindow + dismiss the calling switcher.
    OpenStart,
    /// Open the demo AppWindow with the WasedaP2P route — invoked
    /// from a StartWindow quick action. Closes the StartWindow.
    OpenAppWindow,
}

impl WindowsCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            WindowsCommand::CloseSwitcher | WindowsCommand::CloseStartWindow => {
                Capability::WindowsClose
            }
            WindowsCommand::ActivateWindow { .. } => Capability::WindowsActivate,
            WindowsCommand::OpenStart => Capability::LaunchSystemCapsule,
            WindowsCommand::OpenAppWindow => Capability::WebviewCreate,
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
            if let Err(err) = crate::window::start_window::open_start_window(cx) {
                tracing::error!(error = %err, "ato_windows: open_start_window failed");
            }
            cx.set_global(CardSwitcherWindowSlot(None));
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        WindowsCommand::OpenAppWindow => {
            // Mirrors `app::on_action(OpenAppWindowExperiment, ...)`
            // — fixed WasedaP2P route for Phase 1. Once the broker
            // is the only path, this can accept a `route` parameter
            // for the StartWindow's typed-URL quick action.
            let route = crate::state::GuestRoute::CapsuleHandle {
                handle: "github.com/Koh0920/WasedaP2P".to_string(),
                label: "WasedaP2P".to_string(),
            };
            if let Err(err) = crate::window::open_app_window(cx, route) {
                tracing::error!(error = %err, "ato_windows: open_app_window failed");
            }
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
    }
    Ok(())
}

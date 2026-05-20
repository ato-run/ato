//! **Deprecated (Stage B):** legacy `BridgeAction`-based IPC bridge.
//! The Card Switcher and StartWindow now route through
//! `crate::system_capsule::ipc`, which uses a typed
//! `{capsule, command}` envelope and the `CapabilityBroker`. This
//! module is retained for one stage so any out-of-tree experiments
//! using the old shape don't break; Stage C/D will delete it.
//!
//! Original design notes:
//!
//! IPC bridge between the Wry-hosted launcher HTML pages (Card
//! Switcher, StartWindow) and the rust-side GPUI actions. Pattern is
//! borrowed from `automation/transport.rs` + `focus_dispatcher.rs`:
//!
//!   - Wry's `with_ipc_handler` callback runs on whatever thread Wry
//!     chooses (typically the main thread, but we treat it as
//!     untrusted from a threading POV).
//!   - The handler does almost nothing — it parses the JSON message
//!     and pushes it onto a shared `Arc<Mutex<Vec<_>>>`.
//!   - A `foreground_executor` polling task drains that queue every
//!     50ms and dispatches on the GPUI main thread, where it has
//!     `&mut App` and can mutate globals / open / close windows.
//!
//! Keeping the IPC handler thread-free of GPUI mutation means we
//! don't have to reason about whether wry called us from the right
//! thread for any particular gpui call.
//!
//! Action vocabulary is intentionally small — every entry maps to a
//! concrete `&mut App` operation. New buttons inside the HTML add
//! new variants here.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action")]
pub enum BridgeAction {
    /// Backdrop click / Escape inside the Card Switcher.
    CloseSwitcher,
    /// Escape inside the Start window.
    CloseStartWindow,
    /// Click on a card or dock tile in the Card Switcher — focus the
    /// target window (raises to front) and dismiss the switcher.
    ActivateWindow {
        #[serde(rename = "windowId")]
        window_id: u64,
    },
    /// "+ 新しいウィンドウ" tile in the Card Switcher — open a fresh
    /// StartWindow (separate concept from Launcher; see
    /// `window/start_window.rs`).
    OpenStartWindow,
    /// Quick action in the Start window — spawn an AppWindow.
    OpenAppWindow,
    /// Quick action in the Start window — open / focus the Store.
    OpenStore,
}

pub type BridgeQueue = Arc<Mutex<Vec<BridgeAction>>>;

pub fn new_queue() -> BridgeQueue {
    Arc::new(Mutex::new(Vec::new()))
}

/// Build the closure handed to `WebViewBuilder::with_ipc_handler`.
/// It parses each JSON message into a `BridgeAction` and pushes it
/// onto the shared queue. Unparseable messages are logged at WARN
/// and dropped — never propagate beyond the bridge surface.
pub fn make_ipc_handler(queue: BridgeQueue) -> impl Fn(wry::http::Request<String>) + 'static {
    move |request: wry::http::Request<String>| {
        let body = request.body();
        match serde_json::from_str::<BridgeAction>(body) {
            Ok(action) => {
                if let Ok(mut q) = queue.lock() {
                    q.push(action);
                }
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    body = %body,
                    "web_bridge: ignoring unparseable IPC message"
                );
            }
        }
    }
}

/// Spawn the foreground drain loop that processes queued
/// `BridgeAction`s. The `dispatcher` closure receives the GPUI `App`
/// context, the originating window handle (so `CloseSwitcher` knows
/// which window to remove), and the action. It runs on the GPUI
/// main thread and may call any `&mut App` API.
///
/// Terminates when the host window closes (the dispatcher gets an
/// `Err(_)` back from `host.update`, which we treat as a signal to
/// exit the loop and stop polling — otherwise we'd spin forever
/// after the page is gone).
pub fn spawn_drain_loop<F>(
    cx: &mut App,
    queue: BridgeQueue,
    host: AnyWindowHandle,
    mut dispatcher: F,
) where
    F: FnMut(&mut App, AnyWindowHandle, BridgeAction) + 'static,
{
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        loop {
            be.timer(Duration::from_millis(50)).await;
            let drained: Vec<BridgeAction> = match queue.lock() {
                Ok(mut q) => std::mem::take(&mut *q),
                Err(_) => continue,
            };
            if drained.is_empty() {
                // Cheap probe: try to touch the host window. If it
                // has been removed (red traffic light), bail out of
                // the loop so we stop polling forever. `AsyncApp::
                // update` returns the closure's value directly;
                // `AnyWindowHandle::update` returns `Result` which is
                // `Err` when the window is closed.
                let host_alive: bool = aa.update(|cx| host.update(cx, |_, _, _| ()).is_ok());
                if !host_alive {
                    return;
                }
                continue;
            }
            for action in drained {
                aa.update(|cx| dispatcher(cx, host, action));
            }
        }
    })
    .detach();
}

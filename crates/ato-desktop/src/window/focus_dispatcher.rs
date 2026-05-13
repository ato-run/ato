//! Thin automation dispatcher used when `ATO_DESKTOP_MULTI_WINDOW=1`
//! takes the legacy `DesktopShell` out of the boot path. Without
//! `WebViewManager` the automation socket would never start, so MCP
//! clients (and AODD scripts) would have nowhere to land their
//! requests.
//!
//! This module owns its own `AutomationHost` and a background poller
//! that drains socket-delivered requests every 50ms. Only the
//! Focus-mode-relevant variant — `HostDispatchAction { action }` — is
//! processed; the others surface an explicit
//! `not supported in Focus mode` error so the caller does not block.

use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui::{AnyWindowHandle, App};

use crate::app::{
    NavigateToUrl, OpenAppWindowExperiment, OpenCardSwitcher,
    OpenStartWindow, OpenStoreWindow, ShowSettings,
};
use crate::automation::command::AutomationCommand;
use crate::automation::AutomationHost;

/// Start the Focus-mode automation dispatcher. Spawns the socket
/// listener (`AutomationHost::start`) plus a foreground polling task
/// that processes pending requests via the supplied AppWindow handle.
///
/// Called exactly once from `app::run` after the AppWindow is open.
pub fn start(cx: &mut App, app_handle: AnyWindowHandle) {
    let host = AutomationHost::new();
    if host.start().is_none() {
        tracing::warn!(
            "Focus-mode automation socket failed to start; MCP host_dispatch_action will not work"
        );
        return;
    }

    let async_app = cx.to_async();
    let pending = host.pending.clone();
    let has_pending = host.has_pending.clone();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let async_app_for_loop = async_app.clone();

    fe.spawn(async move {
        loop {
            be.timer(Duration::from_millis(50)).await;
            // Drain only when the socket flagged work OR something
            // slipped into the queue without flagging (defensive
            // against missed wakeups on the polling boundary).
            let queued = pending.lock().map(|q| !q.is_empty()).unwrap_or(false);
            if !has_pending.swap(false, Ordering::Relaxed) && !queued {
                continue;
            }
            let drained: Vec<_> = match pending.lock() {
                Ok(mut q) => std::mem::take(&mut *q),
                Err(_) => continue,
            };
            for req in drained {
                if req.is_expired() {
                    req.send(Err("automation command timed out".into()));
                    continue;
                }
                match &req.command {
                    AutomationCommand::HostDispatchAction { action } => {
                        let action_name = action.clone();
                        let dispatch_result: Result<(), String> = async_app_for_loop
                            .update(|cx| {
                                app_handle
                                    .update(cx, |_view, window, cx| {
                                        let name = action_name.as_str();
                                        tracing::info!(action = %name, "focus dispatcher routes action");
                                        match name {
                                            "OpenAppWindowExperiment" => {
                                                window.dispatch_action(
                                                    Box::new(OpenAppWindowExperiment),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            "OpenCardSwitcher" => {
                                                window.dispatch_action(
                                                    Box::new(OpenCardSwitcher),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            // "OpenLauncherWindow" was retired in Stage D
                                            // along with the Launcher window. Use
                                            // `ShowSettings` to reach ato-settings instead.
                                            "OpenStoreWindow" => {
                                                window.dispatch_action(
                                                    Box::new(OpenStoreWindow),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            "OpenStartWindow" => {
                                                window.dispatch_action(
                                                    Box::new(OpenStartWindow),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            "ShowSettings" => {
                                                window.dispatch_action(
                                                    Box::new(ShowSettings),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            "CloseAppWindow" => {
                                                // Programmatic close used by
                                                // AODD verification of the
                                                // on_window_closed → Launcher
                                                // recovery path. Equivalent to
                                                // the user clicking the red
                                                // traffic light on the
                                                // AppWindow.
                                                let _ = cx;
                                                window.remove_window();
                                                Ok(())
                                            }
                                            // AODD test path for the Control
                                            // Bar URL pill's NavigateToUrl
                                            // dispatch. The MCP envelope has
                                            // no payload, so this branch
                                            // hard-codes the well-known
                                            // capsule:// URL the user wants
                                            // investigated. A real Enter
                                            // press on the bar input
                                            // dispatches the same action
                                            // with the typed value.
                                            "NavigateToTestCapsule" => {
                                                window.dispatch_action(
                                                    Box::new(NavigateToUrl {
                                                        url:
                                                            "capsule://github.com/Koh0920/WasedaP2P"
                                                                .to_string(),
                                                    }),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            "NavigateToTestHttp" => {
                                                window.dispatch_action(
                                                    Box::new(NavigateToUrl {
                                                        url: "https://ato.run/".to_string(),
                                                    }),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            // Stage B AODD negative test:
                                            // ato-windows requests SettingsWrite.
                                            // Per the inline manifest, ato-windows
                                            // does NOT have SettingsWrite — the
                                            // broker MUST reject with Forbidden
                                            // and the desktop state MUST NOT
                                            // mutate. Asserted via the receipt by
                                            // grepping for `Forbidden` in the
                                            // log.
                                            "BrokerNegativeTest" => {
                                                use crate::system_capsule::ato_settings::SettingsCommand;
                                                use crate::system_capsule::{
                                                    CapabilityBroker, SystemCapsuleId,
                                                    SystemCommand,
                                                };
                                                let result = CapabilityBroker::dispatch(
                                                    cx,
                                                    app_handle,
                                                    SystemCapsuleId::AtoWindows,
                                                    SystemCommand::AtoSettings(
                                                        SettingsCommand::SetToggle {
                                                            key: "test".to_string(),
                                                            value: true,
                                                        },
                                                    ),
                                                );
                                                match result {
                                                    Ok(()) => tracing::error!(
                                                        "BrokerNegativeTest: expected Forbidden, got Ok — broker bound BROKEN"
                                                    ),
                                                    Err(err) => tracing::info!(
                                                        ?err,
                                                        "BrokerNegativeTest: broker rejected as expected"
                                                    ),
                                                }
                                                Ok(())
                                            }
                                            other => Err(format!(
                                                "unknown action '{other}' — add it to focus_dispatcher::start"
                                            )),
                                        }
                                    })
                                    .map_err(|e| format!("AppWindow update failed: {e}"))
                                    .and_then(std::convert::identity)
                            });
                        match dispatch_result {
                            Ok(()) => {
                                req.send(Ok(serde_json::json!({
                                    "ok": true,
                                    "queued_action": action,
                                })));
                            }
                            Err(msg) => {
                                req.send(Err(msg));
                            }
                        }
                    }
                    other => {
                        // browser_* and other WebView-bound commands
                        // have no consumer in Focus mode. Returning an
                        // explicit error is honest: receipt R3-style
                        // "lying UI" would be claiming success.
                        req.send(Err(format!(
                            "automation command {:?} is not supported in Focus mode (no WebView pane)",
                            std::mem::discriminant(other)
                        )));
                    }
                }
            }
        }
    })
    .detach();

    tracing::info!("Focus-mode automation dispatcher started");
}

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
    NavigateToUrl, OpenAppWindowExperiment, OpenCardSwitcher, OpenIdentityMenu,
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
                                            "OpenIdentityMenu" => {
                                                window.dispatch_action(
                                                    Box::new(OpenIdentityMenu),
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
                                            // AODD entrypoints for the
                                            // `ato-launch` system-capsule
                                            // wizards. Phase 1 — these are
                                            // not yet wired into the real
                                            // capsule launch pipeline; MCP
                                            // is the only caller, used for
                                            // receipt-generating screenshots.
                                            "OpenLaunchConsent" => {
                                                if let Err(err) =
                                                    crate::window::launch_window::open_consent_window(cx)
                                                {
                                                    tracing::error!(?err, "open_consent_window failed");
                                                }
                                                Ok(())
                                            }
                                            "OpenLaunchBoot" => {
                                                if let Err(err) =
                                                    crate::window::launch_window::open_boot_window(cx, None)
                                                {
                                                    tracing::error!(?err, "open_boot_window failed");
                                                }
                                                Ok(())
                                            }
                                            // AODD verification of the
                                            // consent → AppWindow + boot
                                            // chain. Mirrors what the broker
                                            // does on AtoLaunch::Approve,
                                            // but driven from MCP because
                                            // clicking the in-WebView
                                            // Approve button requires
                                            // macOS Accessibility. Reads
                                            // the PendingLaunchTarget set
                                            // by NavigateToUrl(capsule://),
                                            // spawns the AppWindow, opens
                                            // the boot wizard.
                                            "ForceApprovePending" => {
                                                let pending = cx
                                                    .try_global::<crate::window::launch_window::PendingLaunchTarget>()
                                                    .and_then(|g| g.0.clone());
                                                cx.set_global(
                                                    crate::window::launch_window::PendingLaunchTarget(None),
                                                );
                                                match pending {
                                                    Some(route) => {
                                                        tracing::info!(
                                                            ?route,
                                                            "ForceApprovePending: consuming pending target"
                                                        );
                                                        // Open boot window FIRST so PendingBootShell
                                                        // is set before AppCapsuleShell::new reads it.
                                                        if let Err(err) =
                                                            crate::window::launch_window::open_boot_window(cx, Some(&route))
                                                        {
                                                            tracing::error!(?err, "open_boot_window failed");
                                                        }
                                                        if let Err(err) =
                                                            crate::window::open_app_window(cx, route.clone())
                                                        {
                                                            tracing::error!(?err, "open_app_window failed");
                                                        }
                                                    }
                                                    None => tracing::warn!(
                                                        "ForceApprovePending: no pending target — did NavigateToUrl run first?"
                                                    ),
                                                }
                                                Ok(())
                                            }
                                            "BrokerNegativeTest" => {
                                                use crate::system_capsule::ato_settings::SettingsCommand;
                                                use crate::system_capsule::{
                                                    CapabilityBroker, SystemCapsuleId,
                                                    SystemCommand,
                                                };
                                                // Test that AtoWindows cannot invoke SettingsWrite commands
                                                // (it only has WindowsCreate/Close in its manifest).
                                                let result = CapabilityBroker::dispatch(
                                                    cx,
                                                    app_handle,
                                                    SystemCapsuleId::AtoWindows,
                                                    SystemCommand::AtoSettings(
                                                        SettingsCommand::PatchGlobalSettings {
                                                            request_id: None,
                                                            patch: serde_json::json!({"theme": "dark"}),
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

//! Thin automation dispatcher used when Focus View mode takes the
//! legacy `DesktopShell` out of the boot path. Without `WebViewManager`
//! the automation socket would never start, so MCP clients (and AODD
//! scripts) would have nowhere to land their requests.
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
    FocusControlBarInput, HideControlBar, NavigateToUrl, OpenAppWindowExperiment, OpenCardSwitcher,
    OpenDockWindow, OpenGithubRunWindow, OpenStartWindow, OpenStoreWindow, ShowControlBar,
    ShowSettings, ToggleControlBar,
};
use crate::automation::command::AutomationCommand;
use crate::automation::AutomationHost;
use crate::state::session::SessionRegistry;
use crate::state::GuestRoute;
use crate::system_capsule::ato_onboarding::{OnboardingCommand, ONBOARDING_VERSION};
use crate::webview::{dispatch_automation_command, DOCK_AUTOMATION_PANE_ID};
use crate::window::content_windows::{ContentWindowKind, OpenContentWindows};
use crate::window::dock::DockEntitySlot;

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

    // Register as GPUI global so the dock's page-load handler can find
    // it via `cx.try_global::<AutomationHost>()` and call
    // `mark_page_loaded(DOCK_AUTOMATION_PANE_ID)`.
    cx.set_global(host.clone());

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

                // Dock-pane commands: route browser_* to the DockWebView.
                if req.pane_id == DOCK_AUTOMATION_PANE_ID {
                    // OpenUrl is an app-level navigation command that
                    // dispatch_automation_command cannot handle (it is
                    // intercepted upstream in WebViewManager mode).  In
                    // Focus mode we treat it as a Navigate to the same URL
                    // so the caller gets consistent behaviour.
                    if let AutomationCommand::OpenUrl { url } = &req.command {
                        let url = url.clone();
                        let _ = async_app_for_loop.update(|cx| {
                            let entity_opt = cx
                                .try_global::<DockEntitySlot>()
                                .and_then(|s| s.0.clone());
                            if let Some(entity) = entity_opt {
                                let dock = entity.read(cx);
                                match dock.webview.load_url(&url) {
                                    Ok(()) => {
                                        req.send(Ok(serde_json::json!({ "ok": true })));
                                    }
                                    Err(e) => {
                                        req.send(Err(e.to_string()));
                                    }
                                }
                            } else {
                                req.send(Err("dock is not open".into()));
                            }
                        });
                        continue;
                    }
                    // Page-load guard: most JS commands require the page to
                    // be ready.  Navigate/Screenshot are exempt.
                    let needs_loaded = !matches!(
                        &req.command,
                        AutomationCommand::Navigate { .. }
                            | AutomationCommand::NavigateBack
                            | AutomationCommand::NavigateForward
                            | AutomationCommand::Screenshot
                    );
                    if needs_loaded && !host.is_page_loaded(DOCK_AUTOMATION_PANE_ID) {
                        if req.is_expired() {
                            req.send(Err("dock page not loaded; timed out".into()));
                        } else {
                            // Re-enqueue for the next 50 ms tick.
                            if let Ok(mut q) = pending.lock() {
                                q.push(req);
                                has_pending.store(true, Ordering::Relaxed);
                            }
                        }
                        continue;
                    }
                    let host_clone = host.clone();
                    let _ = async_app_for_loop.update(|cx| {
                        let entity_opt = cx
                            .try_global::<DockEntitySlot>()
                            .and_then(|s| s.0.clone());
                        if let Some(entity) = entity_opt {
                            let dock = entity.read(cx);
                            dispatch_automation_command(
                                req,
                                &dock.webview,
                                DOCK_AUTOMATION_PANE_ID,
                                &host_clone,
                            );
                        } else {
                            req.send(Err("dock is not open".into()));
                        }
                    });
                    continue;
                }

                match &req.command {
                    AutomationCommand::ListPanes => {
                        // In Focus mode the only WebView pane is the dock
                        // (when open). Report it if `DockEntitySlot` is set.
                        let dock_open = async_app_for_loop
                            .update(|cx| {
                                cx.try_global::<DockEntitySlot>()
                                    .and_then(|s| s.0.as_ref())
                                    .is_some()
                            });
                        let panes = if dock_open {
                            serde_json::json!([{
                                "pane_id": DOCK_AUTOMATION_PANE_ID,
                                "kind": "dock",
                                "url": "ato://dock",
                            }])
                        } else {
                            serde_json::json!([])
                        };
                        req.send(Ok(serde_json::json!({ "panes": panes })));
                    }
                    AutomationCommand::HostDispatchAction { action, url } => {
                        let action_name = action.clone();
                        let action_url = url.clone();
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
                                            "OpenGithubRunWindow" => {
                                                window.dispatch_action(
                                                    Box::new(OpenGithubRunWindow),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            "GithubRunFindCandidates" => {
                                                let repo = action_url.clone()
                                                    .ok_or_else(|| "GithubRunFindCandidates requires a `url` (repo) parameter".to_string())?;
                                                let shell_weak = cx
                                                    .try_global::<crate::window::launch_window::ActiveGithubRunShell>()
                                                    .and_then(|s| s.0.clone());
                                                let Some(shell_weak) = shell_weak else {
                                                    return Err("GithubRunFindCandidates: no ActiveGithubRunShell — open the window first".into());
                                                };
                                                if let Some(shell) = shell_weak.upgrade() {
                                                    let mock = serde_json::json!({
                                                        "ok": true,
                                                        "candidates": [{
                                                            "title": repo,
                                                            "version": "0.1.0",
                                                            "description": "AODD mock candidate",
                                                            "author": repo.split('/').next().unwrap_or(""),
                                                            "status": "community",
                                                            "source": "github",
                                                            "toml": format!("[capsule]\nname = \"{repo}\"\nversion = \"0.1.0\"\n"),
                                                            "repo": repo,
                                                        }]
                                                    });
                                                    shell.read(cx).inject_github_candidates(&mock);
                                                }
                                                Ok(())
                                            }
                                            "GithubRunProceedToConsent" => {
                                                let repo = action_url.clone()
                                                    .ok_or_else(|| "GithubRunProceedToConsent requires a `url` (repo) parameter".to_string())?;
                                                let handle = crate::window::launch_window::normalize_github_handle(&repo);
                                                let route = crate::state::GuestRoute::CapsuleHandle {
                                                    handle: handle.clone(),
                                                    label: repo.clone(),
                                                };
                                                // Close the GitHub Run window if still open.
                                                if let Some(shell_weak) = cx
                                                    .try_global::<crate::window::launch_window::ActiveGithubRunShell>()
                                                    .and_then(|s| s.0.clone())
                                                {
                                                    let _ = shell_weak;
                                                }
                                                cx.set_global(crate::window::launch_window::ActiveGithubRunShell(None));
                                                if let Err(err) = crate::window::launch_window::open_consent_window_for_route(cx, route) {
                                                    return Err(format!("GithubRunProceedToConsent: {err}"));
                                                }
                                                Ok(())
                                            }
                                            "ShowSettings" => {
                                                window.dispatch_action(
                                                    Box::new(ShowSettings),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            "OpenIdentityMenu" | "OpenDockWindow" => {
                                                window.dispatch_action(
                                                    Box::new(OpenDockWindow),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            "ShowControlBar" => {
                                                window.dispatch_action(
                                                    Box::new(ShowControlBar),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            "HideControlBar" => {
                                                window.dispatch_action(
                                                    Box::new(HideControlBar),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            "ToggleControlBar" => {
                                                window.dispatch_action(
                                                    Box::new(ToggleControlBar),
                                                    cx,
                                                );
                                                Ok(())
                                            }
                                            "FocusControlBarInput" => {
                                                window.dispatch_action(
                                                    Box::new(FocusControlBarInput),
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
                                            // Generic NavigateToUrl action — MCP callers pass `url` parameter.
                                            // Legacy hardcoded test aliases below remain for backwards compat.
                                            "NavigateToUrl" => {
                                                let target = action_url.clone()
                                                    .ok_or_else(|| "NavigateToUrl requires a `url` parameter".to_string())?;
                                                window.dispatch_action(
                                                    Box::new(NavigateToUrl { url: target }),
                                                    cx,
                                                );
                                                Ok(())
                                            }
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
                                            "OpenImportBlinko" => {
                                                let url = "https://github.com/blinkospace/blinko".to_string();
                                                if let Err(err) = crate::window::import_window::open_with_url(
                                                    cx,
                                                    url,
                                                ) {
                                                    tracing::error!(?err, "OpenImportBlinko: open_with_url failed");
                                                }
                                                Ok(())
                                            }
                                            "RunImportBlinko" => {
                                                crate::system_capsule::ato_import::handle_confirm_unsafe(cx);
                                                Ok(())
                                            }
                                            "CheckImportState" => {
                                                let session_arc = crate::window::import_window::session_arc(cx);
                                                match session_arc.lock() {
                                                    Ok(session) => {
                                                        let snapshot = session.snapshot();
                                                        tracing::info!(
                                                            state = ?snapshot.state,
                                                            recipe_origin = ?snapshot.recipe.as_ref().map(|r| &r.origin),
                                                            recipe_hash = ?snapshot.recipe.as_ref().map(|r| &r.recipe_hash),
                                                            recipe_source = ?snapshot.recipe_resolution.as_ref().map(|r| &r.source),
                                                            signed_in = snapshot.signed_in,
                                                            "ImportSession state check"
                                                        );
                                                    }
                                                    Err(_) => {
                                                        tracing::warn!("CheckImportState: session mutex poisoned");
                                                    }
                                                }
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
                                            "OpenLaunchConsentConfigPanel" => {
                                                if let Err(err) = crate::window::launch_window::open_active_consent_config_panel(cx) {
                                                    tracing::error!(?err, "open_active_consent_config_panel failed");
                                                }
                                                Ok(())
                                            }
                                            "CompleteOnboarding" | "SkipOnboarding" => {
                                                let skipped = name == "SkipOnboarding";
                                                let onboarding_handle = cx
                                                    .global::<OpenContentWindows>()
                                                    .mru_order()
                                                    .into_iter()
                                                    .find(|entry| {
                                                        matches!(
                                                            entry.kind,
                                                            ContentWindowKind::Onboarding
                                                        )
                                                    })
                                                    .map(|entry| entry.handle);
                                                let Some(host) = onboarding_handle else {
                                                    return Err("onboarding window is not open".into());
                                                };
                                                crate::system_capsule::ato_onboarding::dispatch(
                                                    cx,
                                                    host,
                                                    OnboardingCommand::Complete {
                                                        version: ONBOARDING_VERSION,
                                                        skipped,
                                                    },
                                                )
                                                .map_err(|err| err.to_string())
                                            }
                                            "ScrollLaunchConsentConfigPanelBottom" => {
                                                if let Err(err) = crate::window::launch_window::scroll_active_consent_config_panel_to_bottom(cx) {
                                                    tracing::error!(?err, "scroll_active_consent_config_panel_to_bottom failed");
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
                                            "OpenCapsuleSettingsDemo" => {
                                                if let Err(err) =
                                                    crate::window::capsule_panel::open_demo_capsule_settings_window(cx)
                                                {
                                                    tracing::error!(?err, "open_demo_capsule_settings_window failed");
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
                                                let stashed: Option<crate::state::GuestRoute> = cx
                                                    .global_mut::<crate::window::launch_window::PendingLaunches>()
                                                    .0
                                                    .drain()
                                                    .next()
                                                    .map(|(_, s)| s.route);
                                                match stashed {
                                                    Some(route) => {
                                                        tracing::info!(
                                                            ?route,
                                                            "ForceApprovePending: consuming pending target"
                                                        );
                                                        match crate::window::launch_window::open_boot_window(cx, Some(&route)) {
                                                            Ok(boot_handle) => {
                                                                 crate::window::launch_window::start_boot_launch(
                                                                     cx,
                                                                     route.clone(),
                                                                     Vec::new(),
                                                                     boot_handle,
                                                                     crate::state::session::SessionClientKind::AtoWindow,
                                                                 );
                                                            }
                                                            Err(err) => {
                                                                tracing::error!(?err, "open_boot_window failed");
                                                            }
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
                    AutomationCommand::ListSessions => {
                        let entries = async_app_for_loop.update(|cx| {
                            cx.global::<SessionRegistry>().view_entries()
                        });
                        let sessions_json = match serde_json::to_value(&entries) {
                            Ok(v) => v,
                            Err(e) => {
                                req.send(Err(format!("serialize sessions failed: {e}")));
                                continue;
                            }
                        };
                        req.send(Ok(serde_json::json!({ "sessions": sessions_json })));
                    }
                    AutomationCommand::AuthStatus => {
                        let status = crate::webview::signed_out_auth_status();
                        match serde_json::to_value(status) {
                            Ok(json) => req.send(Ok(json)),
                            Err(_) => req.send(Ok(serde_json::json!({
                                "signed_in": false,
                                "api_base_url": "https://api.ato.run",
                                "account_hint": serde_json::Value::Null,
                            }))),
                        };
                    }
                    AutomationCommand::StopActiveSession => {
                        // Snapshot session metadata first; stop_guest_session
                        // is fire-and-forget (non-blocking) so the UI thread
                        // is never parked. on_window_closed will call
                        // stop_session_once for the same session when the
                        // content window closes — that call is idempotent.
                        let stop_result: Result<serde_json::Value, String> =
                            async_app_for_loop.update(|cx| {
                                let active = cx
                                    .global::<OpenContentWindows>()
                                    .mru_order()
                                    .into_iter()
                                    .find(|e| {
                                        matches!(
                                            &e.kind,
                                            ContentWindowKind::AppWindow {
                                                route: GuestRoute::CapsuleHandle { .. }
                                                    | GuestRoute::CapsuleUrl { .. }
                                                    | GuestRoute::Capsule { .. }
                                                    | GuestRoute::Terminal { .. }
                                            }
                                        )
                                    });

                                let Some(entry) = active else {
                                    tracing::info!(
                                        "Focus StopActiveSession: no active capsule window"
                                    );
                                    return Ok(serde_json::json!({
                                        "ok": true,
                                        "stopped": false,
                                        "had_active_session": false,
                                        "session_id": serde_json::Value::Null,
                                        "handle": serde_json::Value::Null,
                                    }));
                                };

                                let session_id =
                                    entry.capsule.as_ref().and_then(|c| c.session_id.clone());
                                let handle_str = entry
                                    .capsule
                                    .as_ref()
                                    .map(|c| c.active_handle().to_string());

                                let stopped = if let Some(ref sid) = session_id {
                                    match crate::orchestrator::stop_guest_session(sid) {
                                        Ok(true) => {
                                            tracing::info!(
                                                session_id = %sid,
                                                "Focus StopActiveSession: stop dispatched"
                                            );
                                            true
                                        }
                                        Ok(false) => {
                                            tracing::warn!(
                                                session_id = %sid,
                                                "Focus StopActiveSession: stop_guest_session returned false (not running?)"
                                            );
                                            false
                                        }
                                        Err(err) => {
                                            tracing::warn!(
                                                error = %err,
                                                session_id = %sid,
                                                "Focus StopActiveSession: stop_guest_session failed"
                                            );
                                            false
                                        }
                                    }
                                } else {
                                    false
                                };

                                // Close the content window regardless of the
                                // stop outcome so the Focus View can be
                                // re-used for a fresh launch.
                                let _ = entry
                                    .handle
                                    .update(cx, |_, window, _| window.remove_window());

                                Ok(serde_json::json!({
                                    "ok": true,
                                    "stopped": stopped,
                                    "had_active_session": session_id.is_some(),
                                    "session_id": session_id,
                                    "handle": handle_str,
                                }))
                            });

                        match stop_result {
                            Ok(json) => req.send(Ok(json)),
                            Err(msg) => req.send(Err(msg)),
                        };
                    }
                    AutomationCommand::RestartActiveSession => {
                        let result: Result<serde_json::Value, String> =
                            async_app_for_loop.update(|cx| {
                                let active = cx
                                    .global::<OpenContentWindows>()
                                    .mru_order()
                                    .into_iter()
                                    .find(|e| {
                                        matches!(
                                            &e.kind,
                                            ContentWindowKind::AppWindow {
                                                route: GuestRoute::CapsuleHandle { .. }
                                                    | GuestRoute::CapsuleUrl { .. }
                                            }
                                        )
                                    });

                                let Some(entry) = active else {
                                    tracing::info!(
                                        "Focus RestartActiveSession: no restartable capsule window"
                                    );
                                    return Ok(serde_json::json!({
                                        "ok": true,
                                        "restarted": false,
                                        "had_active_session": false,
                                        "session_id": serde_json::Value::Null,
                                        "handle": serde_json::Value::Null,
                                    }));
                                };

                                let session_id =
                                    entry.capsule.as_ref().and_then(|c| c.session_id.clone());
                                let handle_str = entry
                                    .capsule
                                    .as_ref()
                                    .map(|c| c.active_handle().to_string());
                                let ContentWindowKind::AppWindow { route } = entry.kind.clone()
                                else {
                                    return Ok(serde_json::json!({
                                        "ok": true,
                                        "restarted": false,
                                        "had_active_session": false,
                                        "session_id": serde_json::Value::Null,
                                        "handle": serde_json::Value::Null,
                                    }));
                                };

                                let launch_configs = session_id
                                    .as_deref()
                                    .and_then(|sid| {
                                        cx.global::<crate::state::session::SessionRegistry>()
                                            .get_session(sid)
                                            .map(|s| s.launch_context.launch_configs.clone())
                                    })
                                    .unwrap_or_default();

                                let materialized_record_path =
                                    session_id.as_deref().and_then(|sid| {
                                        match &route {
                                            crate::state::GuestRoute::CapsuleHandle { .. }
                                            | crate::state::GuestRoute::CapsuleUrl { .. } => {
                                                crate::orchestrator::materialized_record_path_for_session(sid).ok()
                                            }
                                            _ => None,
                                        }
                                    });

                                if let Some(ref sid) = session_id {
                                    if let Err(err) =
                                        crate::orchestrator::stop_guest_session_and_wait(
                                            sid,
                                            std::time::Duration::from_secs(3),
                                        )
                                    {
                                        return Err(format!(
                                            "Focus RestartActiveSession: stop failed: {err}"
                                        ));
                                    }
                                }

                                let _ = entry
                                    .handle
                                    .update(cx, |_, window, _| window.remove_window());

                                let open_result =
                                    if let Some(record_path) = materialized_record_path {
                                        crate::window::orchestrator::open_app_window_from_materialized_record_with_configs(
                                            cx,
                                            route.clone(),
                                            record_path,
                                            launch_configs,
                                        )
                                    } else {
                                        crate::window::orchestrator::open_app_window_with_configs(
                                            cx,
                                            route.clone(),
                                            launch_configs,
                                        )
                                    };
                                if let Err(err) = open_result {
                                    return Err(format!(
                                        "Focus RestartActiveSession: reopen failed: {err}"
                                    ));
                                }

                                Ok(serde_json::json!({
                                    "ok": true,
                                    "restarted": true,
                                    "had_active_session": session_id.is_some(),
                                    "session_id": session_id,
                                    "handle": handle_str,
                                }))
                            });
                        match result {
                            Ok(json) => req.send(Ok(json)),
                            Err(msg) => req.send(Err(msg)),
                        };
                    }
                    other => {
                        // Non-dock browser_* and other commands with no
                        // consumer in Focus mode. Returning an explicit
                        // error is honest: lying UI would claim success.
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

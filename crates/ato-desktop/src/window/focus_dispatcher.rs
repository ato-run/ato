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
use crate::automation::AutomationHost;
use crate::automation::command::AutomationCommand;
use crate::state::GuestRoute;
use crate::state::session::SessionRegistry;
use crate::system_capsule::ato_onboarding::{ONBOARDING_VERSION, OnboardingCommand};
use crate::webview::{DOCK_AUTOMATION_PANE_ID, dispatch_automation_command};
use crate::window::content_windows::{ContentWindowKind, OpenContentWindows};
use crate::window::dock::DockEntitySlot;
use crate::window::focus_guest_panes::{
    FocusGuestPaneEntry, FocusGuestPaneRegistry, is_focus_guest_pane_id,
};

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
            // While a Wry `build_as_child` is pumping the Win32 message loop
            // on the main thread (Windows), an outer GPUI `App` borrow may be
            // held. Resuming here and calling `update` would double-borrow and
            // panic, so defer this drain until the guard clears.
            if crate::webview_init_guard::WebviewInitGuard::is_active() {
                continue;
            }
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
                        async_app_for_loop.update(|cx| {
                            let entity_opt = cx
                                .try_global::<DockEntitySlot>()
                                .and_then(|s| s.0.clone());
                            if let Some(entity) = entity_opt {
                                let dock = entity.read(cx);
                                match dock.webview.as_ref() {
                                    Some(webview) => match webview.load_url(&url) {
                                        Ok(()) => {
                                            req.send(Ok(serde_json::json!({ "ok": true })));
                                        }
                                        Err(e) => {
                                            req.send(Err(e.to_string()));
                                        }
                                    },
                                    None => {
                                        req.send(Err("dock webview unavailable".into()));
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
                    async_app_for_loop.update(|cx| {
                        let entity_opt = cx
                            .try_global::<DockEntitySlot>()
                            .and_then(|s| s.0.clone());
                        if let Some(entity) = entity_opt {
                            let dock = entity.read(cx);
                            if let Some(webview) = dock.webview.as_ref() {
                                dispatch_automation_command(
                                    req,
                                    webview,
                                    DOCK_AUTOMATION_PANE_ID,
                                    &host_clone,
                                );
                            } else {
                                req.send(Err("dock webview unavailable".into()));
                            }
                        } else {
                            req.send(Err("dock is not open".into()));
                        }
                    });
                    continue;
                }

                // Guest capsule panes (#370): route MCP browser_* commands to
                // the private WebView owned by an `AppCapsuleShell`. Explicit
                // dock-pane commands were already handled above; everything
                // here targets a guest pane, defaulting `pane_id = 0` to the
                // frontmost guest (or the dock when no guest is open).
                if is_browser_pane_command(&req.command) {
                    let host_clone = host.clone();
                    let pending_ref = &pending;
                    let has_pending_ref = &has_pending;
                    async_app_for_loop.update(|cx| {
                        // Resolve the target pane.
                        let pane_id = if req.pane_id == 0 {
                            frontmost_guest_pane_id(cx)
                        } else if is_focus_guest_pane_id(req.pane_id) {
                            Some(req.pane_id)
                        } else {
                            None
                        };
                        let pane_id = match pane_id {
                            Some(p) => p,
                            None => {
                                // No guest pane resolved. For an unspecified
                                // pane, fall back to the dock if it is open;
                                // otherwise surface the honest "no pane" error.
                                if req.pane_id == 0 {
                                    let dock_open = cx
                                        .try_global::<DockEntitySlot>()
                                        .and_then(|s| s.0.as_ref())
                                        .is_some();
                                    if dock_open {
                                        let mut req = req;
                                        req.pane_id = DOCK_AUTOMATION_PANE_ID;
                                        if let Ok(mut q) = pending_ref.lock() {
                                            q.push(req);
                                            has_pending_ref.store(true, Ordering::Relaxed);
                                        }
                                    } else {
                                        req.send(Err("no WebView pane".into()));
                                    }
                                } else {
                                    req.send(Err(format!(
                                        "unknown pane {} (no such guest capsule)",
                                        req.pane_id
                                    )));
                                }
                                return;
                            }
                        };

                        // Page-load guard, mirroring the dock path: JS-bearing
                        // commands wait until the page is ready; navigation and
                        // screenshots are exempt.
                        let needs_loaded = !matches!(
                            &req.command,
                            AutomationCommand::Navigate { .. }
                                | AutomationCommand::NavigateBack
                                | AutomationCommand::NavigateForward
                                | AutomationCommand::Screenshot
                        );
                        if needs_loaded && !host_clone.is_page_loaded(pane_id) {
                            if req.is_expired() {
                                req.send(Err("guest capsule page not loaded; timed out".into()));
                            } else if let Ok(mut q) = pending_ref.lock() {
                                q.push(req);
                                has_pending_ref.store(true, Ordering::Relaxed);
                            }
                            return;
                        }

                        // Upgrade the shell and dispatch through its private
                        // WebView. A dead weak means the window closed.
                        let entry = cx.global::<FocusGuestPaneRegistry>().get(pane_id).cloned();
                        match entry.and_then(|e| e.shell.upgrade()) {
                            Some(shell) => {
                                shell.update(cx, |shell, _cx| {
                                    shell.dispatch_automation_request(req, pane_id, &host_clone);
                                });
                            }
                            None => {
                                req.send(Err("guest capsule pane is not available".into()));
                            }
                        }
                    });
                    continue;
                }

                match &req.command {
                    AutomationCommand::ListPanes => {
                        // Report every live guest capsule pane (#370), plus
                        // the dock pane when it is open. This is the fix for
                        // `browser_tabs -> []`: guest WebViews owned by
                        // `AppCapsuleShell` are now first-class automation
                        // panes.
                        let panes = async_app_for_loop.update(|cx| {
                            let dock_open = cx
                                .try_global::<DockEntitySlot>()
                                .and_then(|s| s.0.as_ref())
                                .is_some();
                            let entries = cx.global::<FocusGuestPaneRegistry>().list();
                            let mut guests = Vec::with_capacity(entries.len());
                            for entry in entries {
                                // Skip panes whose window has gone away.
                                let Some(shell) = entry.shell.upgrade() else {
                                    continue;
                                };
                                let shell_ref = shell.read(cx);
                                let url = shell_ref.current_url_for_automation();
                                let session_id = shell_ref.current_session_id();
                                let has_webview = shell_ref.has_webview();
                                // `OpenContentWindows` is the metadata source of
                                // truth for the user-facing title.
                                let title = cx
                                    .global::<OpenContentWindows>()
                                    .get(entry.window_id)
                                    .map(|e| e.title.to_string())
                                    .unwrap_or_else(|| short_handle_title(&entry.handle));
                                let status = if has_webview { "Ready" } else { "Starting" };
                                guests.push(GuestPaneMeta {
                                    pane_id: entry.pane_id,
                                    window_id: entry.window_id,
                                    url,
                                    title,
                                    handle: entry.handle.clone(),
                                    session_id,
                                    status: status.to_string(),
                                });
                            }
                            build_pane_list(&guests, dock_open)
                        });
                        req.send(Ok(serde_json::json!({ "panes": panes })));
                    }
                    AutomationCommand::FocusPane { pane_id } => {
                        let pane_id = *pane_id;
                        let result: Result<serde_json::Value, String> =
                            async_app_for_loop.update(|cx| {
                                if is_focus_guest_pane_id(pane_id) {
                                    let entry = cx
                                        .global::<FocusGuestPaneRegistry>()
                                        .get(pane_id)
                                        .cloned();
                                    let Some(entry) = entry else {
                                        return Err(format!(
                                            "unknown guest pane {pane_id} (no such guest capsule)"
                                        ));
                                    };
                                    // Bump MRU so a later `pane_id = 0` browser
                                    // command defaults to this pane, and raise
                                    // its window so screenshots capture it.
                                    cx.global_mut::<OpenContentWindows>().focus(entry.window_id);
                                    let handle = cx
                                        .global::<OpenContentWindows>()
                                        .get(entry.window_id)
                                        .map(|e| e.handle);
                                    if let Some(handle) = handle {
                                        let _ = handle
                                            .update(cx, |_, window, _| window.activate_window());
                                    }
                                    Ok(serde_json::json!({ "ok": true, "pane_id": pane_id }))
                                } else if pane_id == DOCK_AUTOMATION_PANE_ID {
                                    Ok(serde_json::json!({ "ok": true, "pane_id": pane_id }))
                                } else {
                                    Err(format!("unknown pane {pane_id}"))
                                }
                            });
                        match result {
                            Ok(json) => req.send(Ok(json)),
                            Err(msg) => req.send(Err(msg)),
                        };
                    }
                    AutomationCommand::HostDispatchAction { action, url } => {
                        if let Some(response) =
                            crate::app::navigate_to_url_mcp_preflight(&action, url.as_deref())
                        {
                            req.send(Ok(response));
                            continue;
                        }
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
                                                    community_toml_id: None,
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
                                                        launch_handle: None,
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
                                // #568: installed app-capsule windows are tracked
                                // in the FocusGuestPaneRegistry (the route-agnostic
                                // source of truth that backs browser_tabs). When
                                // the frontmost content window is such a pane,
                                // resolve the stop through it: read the *live*
                                // session id from the shell and close the right
                                // window. The OpenContentWindows-only lookup below
                                // returned had_active_session:false for these
                                // windows because the cached capsule context can
                                // lack a session id (e.g. while the page is still
                                // settling), and it does not cover every installed
                                // route variant.
                                if let Some(front) = cx
                                    .global::<OpenContentWindows>()
                                    .mru_order()
                                    .into_iter()
                                    .next()
                                {
                                    let window_id = front.handle.window_id().as_u64();
                                    if let Some(pane_id) = cx
                                        .global::<FocusGuestPaneRegistry>()
                                        .pane_id_for_window(window_id)
                                        && let Some(entry) = cx
                                            .global::<FocusGuestPaneRegistry>()
                                            .get(pane_id)
                                            .cloned()
                                    {
                                        let session_id = guest_pane_session_id(cx, &entry);
                                        let stopped = match &session_id {
                                            Some(sid) => matches!(
                                                crate::orchestrator::stop_guest_session(sid),
                                                Ok(true)
                                            ),
                                            None => false,
                                        };
                                        // Close the window regardless of stop
                                        // outcome so the Focus View can be reused.
                                        let _ = close_content_window(cx, window_id);
                                        return Ok(serde_json::json!({
                                            "ok": true,
                                            "stopped": stopped,
                                            "had_active_session": session_id.is_some(),
                                            "session_id": session_id,
                                            "handle": entry.handle,
                                        }));
                                    }
                                }

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
                                    // No windowed capsule session — but a Desktop
                                    // installed relaunch may have spawned a runtime
                                    // that has not yet been handed off to a window
                                    // (readiness still in flight). Stop it so it
                                    // cannot orphan on its resolved port, and report
                                    // had_active_session honestly even pre-readiness.
                                    let inflight =
                                        crate::window::launch_window::stop_inflight_installed_launches();
                                    if inflight > 0 {
                                        tracing::info!(
                                            inflight,
                                            "Focus StopActiveSession: stopped in-flight installed launch(es)"
                                        );
                                        return Ok(serde_json::json!({
                                            "ok": true,
                                            "stopped": true,
                                            "had_active_session": true,
                                            "session_id": serde_json::Value::Null,
                                            "handle": serde_json::Value::Null,
                                        }));
                                    }
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

                                if let Some(ref sid) = session_id
                                    && let Err(err) =
                                        crate::orchestrator::stop_guest_session_and_wait(
                                            sid,
                                            std::time::Duration::from_secs(3),
                                        )
                                    {
                                        return Err(format!(
                                            "Focus RestartActiveSession: stop failed: {err}"
                                        ));
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
                    AutomationCommand::ClosePane { pane_id } => {
                        // Close an installed app-capsule window/pane (#568).
                        // Previously fell through to the catch-all below and
                        // returned "Discriminant(18) is not supported in Focus
                        // mode", so MCP `browser_close_tab` could not close a
                        // guest capsule window. Resolve the pane via the
                        // FocusGuestPaneRegistry (route-agnostic source of truth
                        // for live guest panes) and close its GPUI window.
                        let pane_id = *pane_id;
                        let result: Result<serde_json::Value, String> =
                            async_app_for_loop.update(|cx| {
                                let Some(entry) = resolve_guest_pane(cx, pane_id) else {
                                    return Err(if pane_id == 0 {
                                        "no WebView pane to close".to_string()
                                    } else {
                                        format!("unknown pane {pane_id} (no such guest capsule)")
                                    });
                                };
                                // Read the live session id for a truthful
                                // response, then close the window. on_window_closed
                                // applies windowCloseBehavior: it detaches the
                                // client and stops the session only when policy =
                                // stop-session (keep-session-running leaves the
                                // session discoverable for relaunch).
                                let session_id = guest_pane_session_id(cx, &entry);
                                let closed = close_content_window(cx, entry.window_id);
                                Ok(serde_json::json!({
                                    "ok": true,
                                    "closed": closed,
                                    "pane_id": entry.pane_id,
                                    "window_id": entry.window_id,
                                    "handle": entry.handle,
                                    "session_id": session_id,
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

/// Metadata for one pane reported by `browser_tabs` (ListPanes).
#[derive(Debug, Clone)]
pub(crate) struct GuestPaneMeta {
    pub pane_id: usize,
    pub window_id: u64,
    pub url: String,
    pub title: String,
    pub handle: String,
    pub session_id: Option<String>,
    pub status: String,
}

/// Build the `browser_tabs` pane array: every live guest capsule pane,
/// followed by the dock pane when it is open. Kept pure (no GPUI globals) so
/// the JSON shape is unit-testable (#370).
pub(crate) fn build_pane_list(guests: &[GuestPaneMeta], dock_open: bool) -> Vec<serde_json::Value> {
    let mut panes: Vec<serde_json::Value> = guests
        .iter()
        .map(|g| {
            serde_json::json!({
                "pane_id": g.pane_id,
                "kind": "guest-capsule",
                "window_id": g.window_id,
                "url": g.url,
                "title": g.title,
                "handle": g.handle,
                "session_id": g.session_id,
                "status": g.status,
            })
        })
        .collect();
    if dock_open {
        panes.push(serde_json::json!({
            "pane_id": DOCK_AUTOMATION_PANE_ID,
            "kind": "dock",
            "url": "ato://dock",
        }));
    }
    panes
}

/// True for MCP browser commands that operate on a specific WebView pane and
/// are dispatched via `webview::dispatch_automation_command`. These are the
/// commands the Focus dispatcher routes to a guest capsule pane (#370).
fn is_browser_pane_command(cmd: &AutomationCommand) -> bool {
    use AutomationCommand::*;
    matches!(
        cmd,
        Snapshot
            | Screenshot
            | Click { .. }
            | ClickAt { .. }
            | Fill { .. }
            | Type { .. }
            | SelectOption { .. }
            | Check { .. }
            | PressKey { .. }
            | Evaluate { .. }
            | VerifyTextVisible { .. }
            | VerifyElementVisible { .. }
            | WaitFor { .. }
            | Navigate { .. }
            | NavigateBack
            | NavigateForward
            | ConsoleMessages
    )
}

/// Most-recently-focused live guest capsule pane, used to default
/// `pane_id = 0` browser commands to the frontmost guest.
fn frontmost_guest_pane_id(cx: &App) -> Option<usize> {
    let registry = cx.global::<FocusGuestPaneRegistry>();
    cx.global::<OpenContentWindows>()
        .mru_order()
        .into_iter()
        .find_map(|entry| {
            let window_id = entry.handle.window_id().as_u64();
            let pane_id = registry.pane_id_for_window(window_id)?;
            registry
                .get(pane_id)
                .filter(|p| p.shell.upgrade().is_some())
                .map(|_| pane_id)
        })
}

/// Resolve the guest capsule pane targeted by a close/stop request (#568).
/// `pane_id == 0` selects the frontmost live guest pane; an explicit Focus
/// guest pane id selects that pane. Returns `None` for the dock id, an unknown
/// id, or when no guest pane is open.
fn resolve_guest_pane(cx: &App, pane_id: usize) -> Option<FocusGuestPaneEntry> {
    let target = if pane_id == 0 {
        frontmost_guest_pane_id(cx)?
    } else if is_focus_guest_pane_id(pane_id) {
        pane_id
    } else {
        return None;
    };
    cx.global::<FocusGuestPaneRegistry>().get(target).cloned()
}

/// Live session id of a guest pane, read from its shell (`None` while the
/// capsule is still booting). Preferred over the cached
/// `OpenContentWindows` capsule context, which can lag the shell state.
fn guest_pane_session_id(cx: &App, entry: &FocusGuestPaneEntry) -> Option<String> {
    entry
        .shell
        .upgrade()
        .and_then(|shell| shell.read(cx).current_session_id())
}

/// Remove the GPUI content window for `window_id`. Returns true if a window
/// handle was found and removal was dispatched. Closing the window runs
/// `app::on_window_closed`, which detaches the session client and (when
/// `windowCloseBehavior = stop-session`) stops the underlying session.
fn close_content_window(cx: &mut App, window_id: u64) -> bool {
    let handle = cx
        .global::<OpenContentWindows>()
        .get(window_id)
        .map(|e| e.handle);
    match handle {
        Some(handle) => {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
            true
        }
        None => false,
    }
}

/// Short, user-facing title derived from a capsule handle string. Fallback
/// used when `OpenContentWindows` has no entry for the pane's window.
fn short_handle_title(handle: &str) -> String {
    handle
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(handle)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::focus_guest_panes::focus_guest_pane_id;

    fn meta(pane_id: usize, window_id: u64, title: &str) -> GuestPaneMeta {
        GuestPaneMeta {
            pane_id,
            window_id,
            url: format!("http://127.0.0.1:5000/{title}"),
            title: title.to_string(),
            handle: format!("capsule://example/{title}"),
            session_id: Some(format!("sess-{window_id}")),
            status: "Ready".to_string(),
        }
    }

    #[test]
    fn focus_list_panes_includes_guest_capsule_panes() {
        let guests = vec![meta(focus_guest_pane_id(12), 12, "memos")];
        let panes = build_pane_list(&guests, false);
        assert_eq!(panes.len(), 1, "dock closed → only the guest pane");
        let p = &panes[0];
        assert_eq!(p["kind"], "guest-capsule");
        assert_eq!(p["pane_id"], focus_guest_pane_id(12));
        assert_eq!(p["window_id"], 12);
        assert_eq!(p["title"], "memos");
        assert_eq!(p["handle"], "capsule://example/memos");
        assert_eq!(p["session_id"], "sess-12");
        assert_eq!(p["status"], "Ready");
    }

    #[test]
    fn focus_list_panes_includes_dock_and_guest_when_both_open() {
        let guests = vec![
            meta(focus_guest_pane_id(1), 1, "memos"),
            meta(focus_guest_pane_id(2), 2, "blinko"),
        ];
        let panes = build_pane_list(&guests, true);
        assert_eq!(panes.len(), 3, "two guests + dock");
        // Guests come first, dock appended last.
        assert_eq!(panes[0]["kind"], "guest-capsule");
        assert_eq!(panes[1]["kind"], "guest-capsule");
        assert_eq!(panes[2]["kind"], "dock");
        assert_eq!(panes[2]["pane_id"], DOCK_AUTOMATION_PANE_ID);
    }

    #[test]
    fn empty_registry_with_closed_dock_lists_nothing() {
        assert!(build_pane_list(&[], false).is_empty());
    }

    #[test]
    fn browser_commands_are_classified_for_guest_routing() {
        use AutomationCommand::*;
        assert!(is_browser_pane_command(&Snapshot));
        assert!(is_browser_pane_command(&Screenshot));
        assert!(is_browser_pane_command(&Navigate {
            url: "http://x".into()
        }));
        assert!(is_browser_pane_command(&Click {
            ref_id: "e1".into()
        }));
        // Non-pane / app-level commands must NOT be routed to a guest pane.
        assert!(!is_browser_pane_command(&ListPanes));
        assert!(!is_browser_pane_command(&FocusPane { pane_id: 1 }));
        assert!(!is_browser_pane_command(&ListSessions));
        assert!(!is_browser_pane_command(&StopActiveSession));
        // ClosePane is handled by the app-level match arm (#568), not the
        // guest-pane browser dispatch.
        assert!(!is_browser_pane_command(&ClosePane { pane_id: 1 }));
        assert!(!is_browser_pane_command(&HostDispatchAction {
            action: "X".into(),
            url: None
        }));
    }
}

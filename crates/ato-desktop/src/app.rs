use capsule_core::common::paths::ato_path;
use gpui::{
    actions, px, size, Action, App, AppContext, AssetSource, Bounds, KeyBinding, SharedString,
    WindowBounds, WindowDecorations, WindowOptions,
};
#[cfg(target_os = "macos")]
use gpui::{Menu, MenuItem, OsAction, SystemMenuType};
#[cfg(target_os = "macos")]
use gpui_component::input;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::config::ControlBarMode;
use crate::ui::DesktopShell;
use gpui::AsyncApp;
use gpui_component::TitleBar;

actions!(
    ato_desktop,
    [
        FocusCommandBar,
        ShowSettings,
        NextWorkspace,
        PreviousWorkspace,
        NextTask,
        PreviousTask,
        SplitPane,
        ExpandSplit,
        ShrinkSplit,
        DismissTransient,
        CycleHandle,
        BrowserBack,
        BrowserForward,
        BrowserReload,
        NewTab,
        NativeUndo,
        NativeRedo,
        NativeCut,
        NativeCopy,
        NativePaste,
        NativeSelectAll,
        ToggleTheme,
        OpenLocalRegistry,
        OpenCloudDock,
        SignInToAtoRun,
        SignOut,
        OpenAuthInBrowser,
        CancelAuthHandoff,
        ResumeAfterAuth,
        AllowPermissionOnce,
        AllowPermissionForSession,
        DenyPermissionPrompt,
        SaveConfigForm,
        CancelConfigForm,
        ApproveConsentForm,
        CancelConsentForm,
        // #117 — unified pre-launch resolution modal that combines
        // E103 secret entry with E302 consent approval into one
        // overlay. The legacy SaveConfigForm / ApproveConsentForm
        // actions stay for the (now fallback-only) single-slot modals.
        SubmitResolutionForm,
        CancelResolutionForm,
        // #117 step navigation — consent step (review-only) →
        // secrets step (form input). Skipped if either side is empty.
        ResolutionFormNext,
        ResolutionFormBack,
        ToggleRouteMetadataPopover,
        ToggleDock,
        ToggleAutoDevtools,
        ToggleDevConsole,
        CheckForUpdates,
        OpenLatestReleasePage,
        Quit,
        ConfirmQuitKeep,
        ConfirmQuitClear,
        CancelQuit,
        // RFC: SURFACE_CLOSE_SEMANTICS §6 — explicit Stop UI. The
        // shortcut on `StopActiveSession` is provisional; if a
        // platform / keymap conflict surfaces we re-bind without
        // changing the action name.
        StopActiveSession,
        StopAllRetainedSessions,
        // #169 — Opens an additional top-level GPUI window rendering the
        // placeholder `AppWindowShell` so the multi-window orchestrator
        // can be exercised end-to-end before later layers (#171–#174)
        // plug in real content. The action is wired unconditionally,
        // but the handler is a no-op when the flag is off.
        OpenAppWindowExperiment,
        // #173 — opens the Card Switcher overlay window.
        OpenCardSwitcher,
        // Opens the Store window — a Wry WebView pointed at
        // https://ato.run/. Re-clicks focus the existing window
        // rather than stacking duplicates. Gated on the multi-window
        // flag.
        OpenStoreWindow,
        OpenCapsulePanel,
        ShowControlBar,
        HideControlBar,
        ToggleControlBar,
        FocusControlBarInput,
        // Opens a fresh StartWindow — the standalone "compose a new
        // window" surface that the Card Switcher's new-window tile
        // routes to. Always spawns a new window (no slot reuse).
        OpenStartWindow,
        // Identity / Account menu trigger — fired from the Control
        // Bar's right-end Identity button. Phase 1 logs the click;
        // Phase 2 will open a real popover (Profile / Account /
        // Workspace / Trust / Preferences / Help / About).
        OpenIdentityMenu,
        // Opens the Dock window — the publisher tool for managing
        // capsules, setting up a Dock, and monitoring publish status.
        // URL: capsule://run.ato.desktop/dock
        OpenDockWindow
    ]
);

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct NavigateToUrl {
    pub url: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct SetControlBarMode {
    pub mode: ControlBarMode,
}

/// Hand a URL to the OS so it opens in the user's default browser
/// (or whatever app is registered for the scheme). Used by the
/// route-metadata popover to make local_url / healthcheck_url /
/// invoke_url click-through to the same dev server the WebView is
/// rendering, but in a real browser for inspection / DevTools.
#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct OpenExternalLink {
    pub url: String,
}

/// Trigger the active pane to navigate to a registry handle pinned to a
/// newer version (e.g. `capsule://ato.run/foo/bar@1.2.3`). Dispatched by
/// the Install-update button in the route-metadata popover. The desktop
/// reuses the existing NavigateToUrl flow, so there's no extra install
/// plumbing — `ato app session start` lazily fetches & installs whatever
/// version isn't cached yet.
#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct InstallCapsuleUpdate {
    pub url: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct RestartContentWindow {
    pub window_id: u64,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct StopContentWindow {
    pub window_id: u64,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct OpenContentWindowLogs {
    pub window_id: u64,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct OpenContentWindowSettings {
    pub window_id: u64,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct SelectTask {
    pub task_id: usize,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct SelectSettingsTab {
    pub tab: crate::state::SettingsTab,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct SelectRouteMetadataTab {
    pub tab: crate::state::CapsuleDetailTab,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct CloseTask {
    pub task_id: usize,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct MoveTask {
    pub task_id: usize,
    pub to_index: usize,
}

struct LocalAssetSource(std::path::PathBuf);

#[derive(Default)]
pub struct OpenUrlBridge {
    pending: Mutex<VecDeque<String>>,
    async_app: Mutex<Option<AsyncApp>>,
    refresh_scheduled: Arc<AtomicBool>,
}

impl OpenUrlBridge {
    pub fn push_urls(&self, urls: Vec<String>) {
        if urls.is_empty() {
            return;
        }

        if let Ok(mut pending) = self.pending.lock() {
            pending.extend(urls);
        }

        self.schedule_refresh();
    }

    pub fn install_async_app(&self, async_app: AsyncApp) {
        if let Ok(mut slot) = self.async_app.lock() {
            *slot = Some(async_app.clone());
        }
        self.schedule_refresh();
    }

    pub fn drain_urls(&self) -> Vec<String> {
        let Ok(mut pending) = self.pending.lock() else {
            return Vec::new();
        };
        pending.drain(..).collect()
    }

    fn schedule_refresh(&self) {
        let async_app = self
            .async_app
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().cloned());

        let Some(async_app) = async_app else {
            return;
        };

        if self.refresh_scheduled.swap(true, Ordering::AcqRel) {
            return;
        }

        let refresh_app = async_app.clone();
        let bg = async_app.background_executor().clone();
        let refresh_scheduled = self.refresh_scheduled.clone();
        async_app
            .foreground_executor()
            .spawn(async move {
                // Defer to a future tick. Without this, install_async_app
                // and the macOS first-launch on_open_urls callback both
                // run while GPUI's App RefCell is already mut-borrowed
                // (we are inside application.run() / an AppKit selector
                // when they fire). Calling refresh() right away then
                // double-borrows and panics with
                // "RefCell already borrowed" at gpui async_context.rs.
                // A 16 ms timer (≈ one render frame) yields control back
                // to the GPUI event loop so the original borrow drops
                // before refresh() runs.
                bg.timer(std::time::Duration::from_millis(16)).await;
                refresh_app.refresh();
                refresh_scheduled.store(false, Ordering::Release);
            })
            .detach();
    }
}

impl AssetSource for LocalAssetSource {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        // Local override first — lets us ship our own bg images,
        // automation/, preload/, etc. under crates/ato-desktop/assets/.
        let full_path = self.0.join(path);
        if let Ok(data) = std::fs::read(&full_path) {
            return Ok(Some(Cow::Owned(data)));
        }
        // Fall back to the gpui-component bundle for icons/*.svg etc.
        // gpui_component widgets (Icon, Close button) reference paths
        // like "icons/close.svg" that live inside gpui-component's
        // RustEmbed bundle, not under our local assets/ tree.
        match gpui_component_assets::Assets.load(path) {
            Ok(Some(data)) => Ok(Some(data)),
            _ => {
                println!("Debug: Failed to load asset: {}", full_path.display());
                Ok(None)
            }
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        // Delegate to gpui-component-assets so widgets that enumerate
        // (e.g. icon pickers) see the bundled SVGs.
        gpui_component_assets::Assets.list(path)
    }
}

pub fn run() {
    let assets_dir = resolve_assets_dir().expect("failed to resolve ato-desktop assets directory");
    let open_url_bridge = Arc::new(OpenUrlBridge::default());
    let application = gpui_platform::application().with_assets(LocalAssetSource(assets_dir));
    application.on_open_urls({
        let open_url_bridge = open_url_bridge.clone();
        move |urls| {
            open_url_bridge.push_urls(urls);
        }
    });
    application.run(move |cx: &mut App| {
        gpui_component::init(cx);
        open_url_bridge.install_async_app(cx.to_async());
        // Cross-window MRU registry — populated as AppWindows spawn,
        // read by Card Switcher (#173) to render real entries instead
        // of hardcoded placeholders.
        cx.set_global(crate::state::AppWindowRegistry::default());
        cx.set_global(crate::window::content_windows::OpenContentWindows::default());
        crate::window::install_control_bar_controller(cx);
        // Slot tracking the currently-open Card Switcher window so
        // the Control Bar's switcher button can toggle (open → close)
        // rather than stack overlays.
        cx.set_global(crate::window::card_switcher::CardSwitcherWindowSlot::default());
        // Slot tracking the currently-open Launcher window so the
        // Stage D retired the Launcher window — the focused
        // settings cog now opens an `ato-settings` system capsule
        // window directly.
        cx.set_global(crate::window::settings_window::SettingsWindowSlot(None));
        // Slot tracking the currently-open Store window (Wry WebView
        // on ato.run).
        cx.set_global(crate::window::store::StoreWindowSlot::default());
        // Slot tracking the currently-open Developer Console window.
        cx.set_global(crate::window::dock::DockWindowSlot::default());
        cx.set_global(crate::window::dock::DockEntitySlot::default());
        cx.set_global(crate::window::capsule_panel::CapsulePanelWindowSlot::default());
        cx.set_global(crate::window::capsule_panel::CapsuleSettingsWindowSlot::default());
        // Slot tracking the in-Desktop OAuth login window.
        cx.set_global(crate::window::auth_login_window::AuthLoginWindowSlot::default());

        // Scope the shell shortcuts so guest webviews do not inherit host commands.
        cx.bind_keys([
            KeyBinding::new("cmd-k", FocusCommandBar, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-,", ShowSettings, Some("AtoDesktopShell")),
            KeyBinding::new("ctrl-tab", NextWorkspace, Some("AtoDesktopShell")),
            KeyBinding::new("ctrl-shift-tab", PreviousWorkspace, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-]", NextTask, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-[", PreviousTask, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-\\", SplitPane, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-alt-right", ExpandSplit, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-alt-left", ShrinkSplit, Some("AtoDesktopShell")),
            KeyBinding::new("tab", CycleHandle, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-t", NewTab, Some("AtoDesktopShell")),
            KeyBinding::new("escape", DismissTransient, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-z", NativeUndo, Some("Pane")),
            KeyBinding::new("cmd-shift-z", NativeRedo, Some("Pane")),
            KeyBinding::new("cmd-x", NativeCut, Some("Pane")),
            KeyBinding::new("cmd-c", NativeCopy, Some("Pane")),
            KeyBinding::new("cmd-v", NativePaste, Some("Pane")),
            KeyBinding::new("cmd-a", NativeSelectAll, Some("Pane")),
            // WebView-hosting shell bindings — delegate keyboard copy/paste to
            // the Wry WKWebView (macOS native child view, not GPUI first-responder).
            // A single "WebViewShell" context covers all windows that use the
            // WebViewPasteShell trait (dock, launch wizard, store, settings, etc.).
            KeyBinding::new("cmd-x", NativeCut, Some("WebViewShell")),
            KeyBinding::new("cmd-c", NativeCopy, Some("WebViewShell")),
            KeyBinding::new("cmd-v", NativePaste, Some("WebViewShell")),
            KeyBinding::new("cmd-a", NativeSelectAll, Some("WebViewShell")),
            KeyBinding::new("cmd-alt-i", ToggleDock, None),
            KeyBinding::new("cmd-shift-b", ToggleControlBar, None),
            KeyBinding::new("ctrl-shift-b", ToggleControlBar, None),
            KeyBinding::new("cmd-l", FocusControlBarInput, None),
            KeyBinding::new("ctrl-l", FocusControlBarInput, None),
            KeyBinding::new("cmd-r", BrowserReload, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-left", BrowserBack, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-right", BrowserForward, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-q", Quit, None),
            // RFC: SURFACE_CLOSE_SEMANTICS §6.3 — provisional Stop
            // shortcut. Cmd+W remains "close pane" (now retains the
            // session); Cmd+Shift+W is the explicit "stop session"
            // action that actively kills the process.
            KeyBinding::new("cmd-shift-w", StopActiveSession, Some("AtoDesktopShell")),
            // #169 / #170 / #173 — Focus View companion windows.
            // Keystroke bindings are intentionally limited to
            // in-Focus navigation (Launcher, Card Switcher). The
            // legacy ↔ Focus mode itself is chosen via config key
            // `desktop.focus_view_enabled` at startup; there is no
            // in-session toggle. `OpenAppWindowExperiment` survives as an
            // action handler (reachable via the automation socket
            // `host_dispatch_action` for AODD scripts that need to
            // spawn an additional Focus AppWindow), but has no key
            // binding.
            // Stage D: cmd-shift-k previously opened the Launcher.
            // The Launcher window has been retired. ShowSettings
            // (cmd-,) now reaches the ato-settings system capsule
            // directly; the StartWindow is reached via the Card
            // Switcher's "+ 新しいウィンドウ" tile.
            // #173 — open the Card Switcher overlay window.
            // Provisional binding; will be augmented by gesture
            // invocation from the Control Bar (#174).
            KeyBinding::new(
                "cmd-shift-p",
                OpenCardSwitcher,
                Some("AtoDesktopShell"),
            ),
        ]);

        #[cfg(target_os = "macos")]
        install_app_menus(cx);

        cx.on_action(|_: &NativeUndo, _: &mut App| {});
        cx.on_action(|_: &NativeRedo, _: &mut App| {});
        cx.on_action(|_: &NativeCut, _: &mut App| {});
        cx.on_action(|_: &NativeCopy, _: &mut App| {});
        cx.on_action(|_: &NativePaste, _: &mut App| {});
        cx.on_action(|_: &NativeSelectAll, _: &mut App| {});
        // Quit is intercepted by DesktopShell so it can prompt the
        // user to keep or clear persisted tabs. ConfirmQuitKeep /
        // ConfirmQuitClear / CancelQuit are the resolution actions.
        cx.on_action(|_: &ConfirmQuitKeep, cx| cx.quit());
        cx.on_action(|_: &ConfirmQuitClear, cx| {
            if let Ok(path) = ato_path("desktop-tabs.json") {
                let _ = std::fs::remove_file(&path);
            }
            cx.quit();
        });
        cx.on_window_closed(|cx, window_id| {
            // Evict the closed window from the AppWindow registry so
            // Card Switcher / MRU stay accurate. The registry uses
            // the GPUI WindowId u64 it stamped at open time.
            let closed_id = window_id.as_u64();
            let removed_id = cx
                .global_mut::<crate::state::AppWindowRegistry>()
                .find_by_gpui_window_id(closed_id);
            if let Some(id) = removed_id {
                cx.global_mut::<crate::state::AppWindowRegistry>()
                    .close(id);
                tracing::info!(
                    app_window_id = id,
                    gpui_window_id = closed_id,
                    "AppWindow evicted from registry on close"
                );
            }

            // Evict from the cross-window content registry so the
            // Card Switcher badge decrements and the corresponding
            // card disappears. No-op for chrome windows (Control Bar,
            // Card Switcher overlay) since they never registered.
            if cx
                .global_mut::<crate::window::content_windows::OpenContentWindows>()
                .remove(closed_id)
            {
                tracing::info!(
                    gpui_window_id = closed_id,
                    "content window evicted from registry on close"
                );
            }
            if let Some(handle) = cx.global::<crate::window::ControlBarController>().handle {
                if handle.window_id() == window_id {
                    cx.global_mut::<crate::window::ControlBarController>()
                        .clear_window(handle);
                    tracing::info!("Control Bar window closed; controller cleared");
                }
            }

            // Clear singleton slots when their tracked window closes
            // so the next Settings / Store / switcher click opens a
            // fresh one cleanly. (The Launcher window was retired
            // in Stage D of the system-capsule refactor; ato-settings
            // is slot-free.)
            let switcher_slot = cx
                .global::<crate::window::card_switcher::CardSwitcherWindowSlot>()
                .0;
            if switcher_slot.map(|h| h.window_id() == window_id).unwrap_or(false) {
                cx.set_global(
                    crate::window::card_switcher::CardSwitcherWindowSlot(None),
                );
                tracing::info!("Card Switcher window closed; slot cleared");
            }
            let settings_slot = cx
                .global::<crate::window::settings_window::SettingsWindowSlot>()
                .0;
            if settings_slot
                .map(|h| h.window_id() == window_id)
                .unwrap_or(false)
            {
                cx.set_global(crate::window::settings_window::SettingsWindowSlot(None));
                tracing::info!("Settings window closed; slot cleared");
            }
            let capsule_panel_slot = cx
                .global::<crate::window::capsule_panel::CapsulePanelWindowSlot>()
                .0;
            if capsule_panel_slot
                .map(|h| h.window_id() == window_id)
                .unwrap_or(false)
            {
                cx.set_global(crate::window::capsule_panel::CapsulePanelWindowSlot(None));
                tracing::info!("Capsule panel window closed; slot cleared");
            }
            let capsule_settings_slot = cx
                .global::<crate::window::capsule_panel::CapsuleSettingsWindowSlot>()
                .0;
            if capsule_settings_slot
                .map(|h| h.window_id() == window_id)
                .unwrap_or(false)
            {
                cx.set_global(crate::window::capsule_panel::CapsuleSettingsWindowSlot(None));
                tracing::info!("Capsule settings window closed; slot cleared");
            }
            let store_slot = cx
                .global::<crate::window::store::StoreWindowSlot>()
                .0;
            if store_slot.map(|h| h.window_id() == window_id).unwrap_or(false) {
                cx.set_global(crate::window::store::StoreWindowSlot(None));
                tracing::info!("Store window closed; slot cleared");
            }
            let dock_slot = cx
                .global::<crate::window::dock::DockWindowSlot>()
                .0;
            if dock_slot
                .map(|h| h.window_id() == window_id)
                .unwrap_or(false)
            {
                crate::window::dock::cleanup_dock_window(cx);
                tracing::info!("Dock window closed; slot cleared");
            }

            // In Focus mode the Control Bar is a process-lifetime
            // singleton with its own lifecycle, decoupled from any
            // AppWindow. Closing the last AppWindow therefore should
            // NOT auto-open a Launcher — the bar is already there as
            // the user's landing surface. We quit only when every
            // remaining window (including the Control Bar) is gone.
            if cx.windows().is_empty() && !crate::window::is_multi_window_enabled() {
                cx.quit();
            }
        })
        .detach();

        cx.on_action(|_: &ShowControlBar, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            if let Err(err) = crate::window::show_control_bar(cx) {
                tracing::error!(error = %err, "ShowControlBar failed");
            }
        });

        cx.on_action(|_: &HideControlBar, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            crate::window::hide_control_bar(cx);
        });

        cx.on_action(|_: &ToggleControlBar, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            if let Err(err) = crate::window::toggle_control_bar(cx) {
                tracing::error!(error = %err, "ToggleControlBar failed");
            }
        });

        cx.on_action(|_: &FocusControlBarInput, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            if let Err(err) = crate::window::focus_control_bar_input(cx) {
                tracing::error!(error = %err, "FocusControlBarInput failed");
            }
        });

        cx.on_action(|_action: &SetControlBarMode, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            // Temporary safety gate: keep control bar in floating mode only.
            let mode = ControlBarMode::Floating;
            let mut config = crate::config::load_config();
            config.desktop.control_bar.mode = mode;
            config.desktop.control_bar.visible_on_startup = true;
            config.desktop.control_bar.auto_hide = false;
            crate::config::save_config(&config);
            if let Err(err) = crate::window::set_control_bar_mode(cx, mode) {
                tracing::error!(error = %err, "SetControlBarMode failed");
            }
        });

        cx.on_action(|_: &StopActiveSession, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            stop_active_focus_capsule(cx);
        });
        cx.on_action(|action: &StopContentWindow, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            stop_focus_content_window(cx, action.window_id);
        });
        cx.on_action(|action: &RestartContentWindow, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            restart_focus_content_window(cx, action.window_id);
        });
        cx.on_action(|action: &OpenContentWindowLogs, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            open_focus_content_window_logs(cx, action.window_id);
        });
        cx.on_action(|action: &OpenContentWindowSettings, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            if let Err(err) =
                crate::window::capsule_panel::open_capsule_settings_window(cx, action.window_id)
            {
                tracing::error!(error = %err, "OpenContentWindowSettings failed");
            }
        });

        cx.on_action(|_: &StopAllRetainedSessions, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            stop_all_focus_capsules(cx);
        });

        // #169 — multi-window experiment action. Opens a placeholder
        // `AppWindowShell` window (Focus View is now the default;
        // opt out via config `desktop.focus_view_enabled = false`).
        cx.on_action(|_: &OpenAppWindowExperiment, cx: &mut App| {
            tracing::info!("OpenAppWindowExperiment handler entered");
            if !crate::window::is_multi_window_enabled() {
                tracing::warn!(
                    "OpenAppWindowExperiment dispatched but multi-window flag is off"
                );
                return;
            }
            // Go through the consent wizard so the full boot flow is
            // exercised end-to-end from the keyboard shortcut.
            let route = crate::state::GuestRoute::CapsuleHandle {
                handle: "github.com/Koh0920/WasedaP2P".to_string(),
                label: "WasedaP2P".to_string(),
            };
            tracing::info!("calling open_consent_window_for_route");
            match crate::window::launch_window::open_consent_window_for_route(cx, route) {
                Ok(()) => tracing::info!("open_consent_window_for_route returned Ok"),
                Err(err) => {
                    tracing::error!(error = %err, "open_consent_window_for_route failed")
                }
            }
        });

        // OpenLauncherWindow / open_launcher_window were retired in
        // Stage D. The Settings cog now dispatches `ShowSettings`
        // directly, which opens the `ato-settings` system capsule
        // in its own window.

        // Identity / Account menu trigger from the Control Bar's
        // right-end avatar button. Opens the `ato-identity` system
        // capsule. The popover renders an honest Phase-1 surface:
        // Store / Settings rows are live (hand off to the existing
        // system capsules), while Profile / Account / Workspace /
        // Trust rows are visibly disabled with "近日公開" pills.
        cx.on_action(|_: &OpenIdentityMenu, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            if let Err(err) = crate::window::identity_window::open_identity_window(cx) {
                tracing::error!(error = %err, "OpenIdentityMenu: open_identity_window failed");
            }
        });

        // Settings cog routing in Focus mode — Stages C+D:
        // ShowSettings opens a standalone Wry-hosted Settings
        // window (the `ato-settings` system capsule). The legacy
        // Launcher window was retired in Stage D so the Control
        // Bar dispatches ShowSettings as the sole action for the
        // settings cog click.
        cx.on_action(|_: &ShowSettings, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            if let Err(err) = crate::window::settings_window::open_settings_window(cx) {
                tracing::error!(error = %err, "ShowSettings: open_settings_window failed");
            }
        });

        // Focus-mode handler for the Control Bar URL pill's
        // PressEnter. Parses the typed URL and spawns an AppWindow
        // with the matching GuestRoute. The legacy DesktopShell has
        // its own `on_navigate_to_url` for the single-window mode;
        // it never runs here because DesktopShell isn't instantiated
        // when the multi-window flag is on.
        //
        // Supported schemes:
        //   - capsule://<handle...>  → CapsuleHandle route (spawns an
        //     AppWindow whose registry entry tracks the capsule
        //     identity). NOTE: full capsule SESSION orchestration
        //     (running `ato app session start`, mounting the
        //     WebView) is NOT wired into AppWindow yet — that path
        //     waits on the per-window WebViewManager migration.
        //   - http(s)://...          → ExternalUrl route.
        //   - anything else          → log + ignore.
        cx.on_action(|action: &NavigateToUrl, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            let raw = action.url.trim();
            if raw.is_empty() {
                return;
            }
            tracing::info!(url = %raw, "Focus-mode NavigateToUrl");
            if let Some(rest) = raw.strip_prefix("capsule://") {
                let handle = rest.trim_end_matches('/').to_string();
                if handle.is_empty() {
                    tracing::warn!("capsule:// with empty handle — ignored");
                    return;
                }
                // Label = last path segment of the handle. Falls
                // back to the whole handle when there is no slash.
                let label = handle
                    .rsplit('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&handle)
                    .to_string();

                let open_mode = crate::config::load_config().desktop.capsule_open_mode;
                match open_mode {
                    crate::config::CapsuleOpenMode::OsBrowser => {
                        // Launch capsule in background, then open local URL in OS browser.
                        let (tx, rx) = std::sync::mpsc::channel();
                        let handle_clone = handle.clone();
                        std::thread::spawn(move || {
                            let result = crate::orchestrator::resolve_and_start_guest(
                                &handle_clone,
                                &[],
                                &[],
                                None,
                            );
                            let _ = tx.send((handle_clone, result));
                        });
                        let async_cx = cx.to_async();
                        async_cx
                            .foreground_executor()
                            .spawn({
                                let be = async_cx.background_executor().clone();
                                let aa = async_cx.clone();
                                async move {
                                    loop {
                                        match rx.try_recv() {
                                            Ok((handle, result)) => {
                                                aa.update(|_cx| {
                                                    match result {
                                                        Ok(session) => {
                                                            let url = session.local_url.unwrap_or_else(|| {
                                                                format!("https://ato.run/{}", handle)
                                                            });
                                                            if let Err(e) =
                                                                crate::ui::open_external_url(&url)
                                                            {
                                                                tracing::error!(
                                                                    error = %e,
                                                                    url = %url,
                                                                    "os-browser: open_external_url failed"
                                                                );
                                                            }
                                                        }
                                                        Err(launch_err) => {
                                                            tracing::error!(
                                                                error = %launch_err,
                                                                handle = %handle,
                                                                "os-browser: resolve_and_start_guest failed"
                                                            );
                                                        }
                                                    }
                                                });
                                                break;
                                            }
                                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                                tracing::error!(
                                                    "os-browser: background thread disconnected"
                                                );
                                                break;
                                            }
                                            Err(std::sync::mpsc::TryRecvError::Empty) => {
                                                be.timer(std::time::Duration::from_millis(100))
                                                    .await;
                                            }
                                        }
                                    }
                                }
                            })
                            .detach();
                    }
                    crate::config::CapsuleOpenMode::Webviewer => {
                        tracing::warn!(
                            "capsule_open_mode=webviewer: not yet implemented, falling back to window"
                        );
                        // fall through to window behaviour
                        let route =
                            crate::state::GuestRoute::CapsuleHandle { handle, label };
                        if let Err(err) =
                            crate::window::launch_window::open_consent_window_for_route(
                                cx, route,
                            )
                        {
                            tracing::error!(
                                error = %err,
                                "NavigateToUrl(capsule) open_consent_window_for_route failed"
                            );
                        }
                    }
                    crate::config::CapsuleOpenMode::Window => {
                        let route =
                            crate::state::GuestRoute::CapsuleHandle { handle, label };
                        // Gate every capsule launch on a pre-flight consent
                        // wizard. On Approve the broker spawns the real
                        // AppWindow + boot wizard; on Cancel nothing happens.
                        if let Err(err) =
                            crate::window::launch_window::open_consent_window_for_route(
                                cx, route,
                            )
                        {
                            tracing::error!(
                                error = %err,
                                "NavigateToUrl(capsule) open_consent_window_for_route failed"
                            );
                        }
                    }
                }
                return;
            }
            match url::Url::parse(raw) {
                Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => {
                    let open_mode = crate::config::load_config().desktop.capsule_open_mode;
                    match open_mode {
                        crate::config::CapsuleOpenMode::OsBrowser => {
                            let url_str = parsed.to_string();
                            if let Err(e) = crate::ui::open_external_url(&url_str) {
                                tracing::error!(
                                    error = %e,
                                    url = %url_str,
                                    "os-browser: open_external_url for http URL failed"
                                );
                            }
                        }
                        crate::config::CapsuleOpenMode::Webviewer => {
                            tracing::warn!(
                                "capsule_open_mode=webviewer: not yet implemented for http URLs, falling back to window"
                            );
                            let route = crate::state::GuestRoute::ExternalUrl(parsed);
                            if let Err(err) = crate::window::open_app_window(cx, route) {
                                tracing::error!(
                                    error = %err,
                                    "NavigateToUrl(http) open_app_window failed"
                                );
                            }
                        }
                        crate::config::CapsuleOpenMode::Window => {
                            let route = crate::state::GuestRoute::ExternalUrl(parsed);
                            if let Err(err) = crate::window::open_app_window(cx, route) {
                                tracing::error!(
                                    error = %err,
                                    "NavigateToUrl(http) open_app_window failed"
                                );
                            }
                        }
                    }
                }
                Ok(parsed) => {
                    tracing::warn!(
                        scheme = parsed.scheme(),
                        "NavigateToUrl: unsupported scheme — ignored"
                    );
                }
                Err(err) => {
                    tracing::warn!(error = %err, url = %raw, "NavigateToUrl: parse failed");
                }
            }
        });

        // #173 — open Card Switcher overlay. No-op when multi-window
        // flag is off. The overlay snapshots open `AppWindow`s and
        // renders them as MRU-ordered cards; until the per-window
        // WebViewManager migration lands the snapshot/dismissal logic
        // is placeholder.
        cx.on_action(|_: &OpenCardSwitcher, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                tracing::debug!(
                    "OpenCardSwitcher dispatched but multi-window flag is off"
                );
                return;
            }
            if let Err(err) = crate::window::open_card_switcher_window(cx) {
                tracing::error!(error = %err, "failed to open card switcher window");
            }
        });

        // Open / focus the Store window (Wry WebView → ato.run).
        cx.on_action(|_: &OpenStoreWindow, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                tracing::debug!(
                    "OpenStoreWindow dispatched but multi-window flag is off"
                );
                return;
            }
            if let Err(err) = crate::window::store::open_store_window(cx) {
                tracing::error!(error = %err, "failed to open store window");
            }
        });

        // Open / focus the Dock window.
        cx.on_action(|_: &OpenDockWindow, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                tracing::debug!(
                    "OpenDockWindow dispatched but multi-window flag is off"
                );
                return;
            }
            if let Err(err) = crate::window::dock::open_dock_window(cx) {
                tracing::error!(error = %err, "failed to open dock window");
            }
        });
        cx.on_action(|_: &OpenCapsulePanel, cx: &mut App| {
            if !crate::window::is_multi_window_enabled() {
                return;
            }
            if let Err(err) = crate::window::capsule_panel::open_capsule_panel_window(cx) {
                tracing::error!(error = %err, "failed to open capsule panel window");
            }
        });

        // Spawn a fresh StartWindow. Unlike the Launcher / Store
        // handlers, there is no slot — every dispatch produces a new
        // window. The Card Switcher's new-window tile invokes the
        // underlying function directly (not through this action) to
        // avoid the dispatch-queue-vs-window-removal race, but the
        // action is still registered so MCP / keybind paths reach
        // the same target.
        cx.on_action(|_: &OpenStartWindow, cx: &mut App| {
            if let Err(err) = crate::window::start_window::open_start_window(cx) {
                tracing::error!(error = %err, "failed to open start window");
            }
        });

        // The two startup modes are mutually exclusive — there is no
        // in-session toggle, only a process-lifetime choice. Focus View
        // (AppWindow + Control Bar) is the default; config key
        // `desktop.focus_view_enabled = false` selects legacy DesktopShell.
        if crate::window::is_multi_window_enabled() {
            tracing::info!("Focus View mode — selected by desktop.focus_view_enabled config");
            // Spawn the Control Bar FIRST as a Focus-mode singleton.
            // Its lifecycle is independent of any AppWindow: closing
            // the active AppWindow does not close the bar; opening a
            // new AppWindow re-uses the existing bar. The bar stays
            // until the user explicitly closes it or the process
            // exits.
            let control_bar_handle = if matches!(
                crate::window::control_bar_mode(cx),
                ControlBarMode::Hidden
            ) {
                tracing::info!("Focus View Control Bar starts hidden");
                None
            } else {
                match crate::window::open_focus_control_bar(cx) {
                    Ok(h) => Some(h),
                    Err(err) => {
                        tracing::error!(error = %err, "Focus View Control Bar startup failed; quitting");
                        cx.quit();
                        return;
                    }
                }
            };
            tracing::info!("Focus View Control Bar opened at startup");

            // Opening a Wry WebView synchronously during GPUI startup
            // (before the macOS RunLoop has completed its first pass)
            // causes WKWebView to initialize in a broken state where
            // inline JavaScript is silently blocked. Defer store window
            // creation by one event-loop tick so the RunLoop is fully
            // live before WKWebView initializes.
            let startup_config = crate::config::load_config();
            let startup_surface = startup_config.desktop.startup_surface;
            let show_onboarding =
                crate::system_capsule::ato_onboarding::should_show_onboarding(&startup_config);
            let async_cx = cx.to_async();
            cx.foreground_executor()
                .spawn(async move {
                    // One frame is enough for the macOS RunLoop to complete
                    // its first pass and for WKWebView to initialize normally.
                    async_cx
                        .background_executor()
                        .timer(std::time::Duration::from_millis(32))
                        .await;
                    let _ = async_cx.update(|cx| {
                        if show_onboarding {
                            match crate::window::onboarding_window::open_onboarding_window(cx) {
                                Ok(_) => tracing::info!("Onboarding window opened at startup"),
                                Err(err) => {
                                    tracing::error!(error = %err, "Onboarding window failed at startup")
                                }
                            }
                            return;
                        }
                        match crate::window::open_configured_startup_surface(cx, startup_surface) {
                            Ok(_) => tracing::info!(?startup_surface, "Startup surface opened"),
                            Err(err) => {
                                tracing::error!(error = %err, ?startup_surface, "Startup surface failed")
                            }
                        }
                    });
                })
                .detach();

            // Focus mode has no DesktopShell / WebViewManager, so the
            // automation socket would never start and host_dispatch_action
            // would have nowhere to land. Start a thin dispatcher that
            // owns its own `AutomationHost`, drains socket-delivered
            // requests, and routes `HostDispatchAction { action }` to
            // the initial window as a real GPUI action dispatch.
            // Actions are App-level so dispatching via any window handle
            // reaches the registered handler — the Control Bar handle
            // is used here since the Store window is deferred.
            if let Some(control_bar_handle) = control_bar_handle {
                crate::window::focus_dispatcher::start(cx, control_bar_handle);
            } else {
                tracing::warn!(
                    "Focus dispatcher not started because the Control Bar is hidden at startup"
                );
            }
        } else {
            tracing::info!("Legacy DesktopShell mode — desktop.focus_view_enabled is false");
            let bounds = Bounds::centered(None, size(px(1440.0), px(920.0)), cx);
            cx.open_window(
                WindowOptions {
                    titlebar: Some(TitleBar::title_bar_options()),
                    focus: true,
                    show: true,
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_decorations: Some(WindowDecorations::Client),
                    ..Default::default()
                },
                {
                    let open_url_bridge = open_url_bridge.clone();
                    move |window, cx| {
                        let shell =
                            cx.new(|cx| DesktopShell::new(window, cx, open_url_bridge.clone()));
                        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
                    }
                },
            )
            .expect("failed to open ato-desktop window");
        }

        cx.activate(true);
    });
}

fn stop_active_focus_capsule(cx: &mut App) {
    use crate::state::GuestRoute;
    use crate::window::content_windows::{ContentWindowKind, OpenContentWindows};

    let active = cx
        .global::<OpenContentWindows>()
        .mru_order()
        .into_iter()
        .find(|entry| {
            matches!(
                &entry.kind,
                ContentWindowKind::AppWindow {
                    route: GuestRoute::CapsuleHandle { .. }
                        | GuestRoute::CapsuleUrl { .. }
                        | GuestRoute::Capsule { .. }
                        | GuestRoute::Terminal { .. }
                }
            )
        });

    if let Some(entry) = active {
        let _ = entry
            .handle
            .update(cx, |_, window, _| window.remove_window());
        tracing::info!(title = %entry.title, "Focus View active capsule stopped by closing its AppWindow");
    } else {
        tracing::info!("StopActiveSession ignored: no active Focus View capsule window");
    }
}

fn stop_focus_content_window(cx: &mut App, window_id: u64) {
    let target = cx
        .global::<crate::window::content_windows::OpenContentWindows>()
        .get(window_id)
        .map(|entry| entry.handle);
    if let Some(target) = target {
        let _ = target.update(cx, |_, window, _| window.remove_window());
    }
}

fn restart_focus_content_window(cx: &mut App, window_id: u64) {
    use crate::window::content_windows::{ContentWindowKind, OpenContentWindows};

    let Some(entry) = cx.global::<OpenContentWindows>().get(window_id).cloned() else {
        return;
    };
    let ContentWindowKind::AppWindow { route } = entry.kind else {
        return;
    };
    let _ = entry
        .handle
        .update(cx, |_, window, _| window.remove_window());
    if let Err(err) = crate::window::open_app_window(cx, route) {
        tracing::error!(error = %err, window_id, "RestartContentWindow failed");
    }
}

fn open_focus_content_window_logs(cx: &mut App, window_id: u64) {
    let path = cx
        .global::<crate::window::content_windows::OpenContentWindows>()
        .get(window_id)
        .and_then(|entry| entry.capsule.as_ref())
        .and_then(|capsule| capsule.log_path.clone());
    let Some(path) = path else {
        return;
    };
    if let Err(err) = Command::new("open").arg(&path).spawn() {
        tracing::error!(error = %err, log_path = %path, "OpenContentWindowLogs failed");
    }
}

fn stop_all_focus_capsules(cx: &mut App) {
    use crate::state::GuestRoute;
    use crate::window::content_windows::{ContentWindowKind, OpenContentWindows};

    let targets: Vec<_> = cx
        .global::<OpenContentWindows>()
        .mru_order()
        .into_iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                ContentWindowKind::AppWindow {
                    route: GuestRoute::CapsuleHandle { .. }
                        | GuestRoute::CapsuleUrl { .. }
                        | GuestRoute::Capsule { .. }
                        | GuestRoute::Terminal { .. }
                }
            )
        })
        .collect();
    let count = targets.len();
    for entry in targets {
        let _ = entry
            .handle
            .update(cx, |_, window, _| window.remove_window());
    }
    tracing::info!(count, "Focus View capsule windows stopped");
}

#[cfg(target_os = "macos")]
fn install_app_menus(cx: &mut App) {
    let mode = crate::window::control_bar_mode(cx);
    cx.set_menus(vec![
        Menu {
            name: "ato-desktop".into(),
            items: vec![
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Show Control Bar", ShowControlBar),
                MenuItem::action("Hide Control Bar", HideControlBar),
                MenuItem::submenu(
                    Menu::new("Control Bar Mode").items([MenuItem::action(
                        "Floating",
                        SetControlBarMode {
                            mode: ControlBarMode::Floating,
                        },
                    )
                    .checked(mode == ControlBarMode::Floating)]),
                ),
                MenuItem::separator(),
                MenuItem::action("Open Store", OpenStoreWindow),
                MenuItem::action("Open Settings", ShowSettings),
                MenuItem::separator(),
                MenuItem::action("Stop Active Capsule", StopActiveSession),
                MenuItem::action("Stop All Capsules", StopAllRetainedSessions),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Undo", NativeUndo, OsAction::Undo),
                MenuItem::os_action("Redo", NativeRedo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::action("Cut", NativeCut),
                MenuItem::action("Copy", NativeCopy),
                MenuItem::action("Paste", NativePaste),
                MenuItem::separator(),
                MenuItem::action("Delete", input::Delete),
                MenuItem::action("Delete Previous Word", input::DeleteToPreviousWordStart),
                MenuItem::action("Delete Next Word", input::DeleteToNextWordEnd),
                MenuItem::separator(),
                MenuItem::action("Find", input::Search),
                MenuItem::separator(),
                MenuItem::action("Select All", NativeSelectAll),
            ],
            disabled: false,
        },
    ]);
}

fn resolve_assets_dir() -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("ATO_DESKTOP_ASSETS_DIR") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return Ok(path);
        }
    }

    // current_dir/current_exe failures must not crash launch — when the
    // shell's cwd inode is stale (bundle replaced under an open shell),
    // getcwd(2) returns ENOENT. Fall through to the next strategy instead.
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_assets = cwd.join("assets");
        if cwd_assets.is_dir() {
            return Ok(cwd_assets);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(macos_dir) = exe.parent() {
            if let Some(contents) = macos_dir.parent() {
                let bundled = contents.join("Resources").join("assets");
                if bundled.is_dir() {
                    return Ok(bundled);
                }
            }
            let sibling = macos_dir.join("assets");
            if sibling.is_dir() {
                return Ok(sibling);
            }
        }
    }

    Err(anyhow::anyhow!(
        "ato-desktop assets directory was not found; set ATO_DESKTOP_ASSETS_DIR or run from the app root"
    ))
}

#[cfg(test)]
mod tests {
    use super::resolve_assets_dir;

    #[test]
    fn resolve_assets_dir_finds_workspace_assets() {
        let path = resolve_assets_dir().expect("workspace assets should resolve");
        assert!(path.ends_with("assets"));
        assert!(path.is_dir());
    }
}

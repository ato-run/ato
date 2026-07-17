use capsule::common::paths::ato_path;
use gpui::{Action, App, AssetSource, KeyBinding, SharedString, actions};
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

use crate::bundle_paths::DesktopBundlePaths;
use crate::config::ControlBarMode;
use gpui::AsyncApp;

actions!(
    desktop,
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
        ConfirmQuitWithCleanup,
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
        // #174 — focus the previous / next app window in MRU order via
        // a two-finger horizontal trackpad swipe on the Control Bar.
        FocusPrevAppWindow,
        FocusNextAppWindow,
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
        // Opens the GitHub repository execution wizard — accepts a
        // GitHub URL or owner/repo shorthand, looks up capsule.toml
        // candidates (metadata-only, no clone), and walks the user
        // through candidate review and consent before launching.
        OpenGithubRunWindow,
        // Identity / Account menu trigger — fired from the Control
        // Bar's right-end Identity button. Phase 1 logs the click;
        // Phase 2 will open a real popover (Profile / Account /
        // Workspace / Trust / Preferences / Help / About).
        OpenIdentityMenu,
        // Opens the Dock window — the publisher tool for managing
        // capsules, setting up a Dock, and monitoring publish status.
        // URL: capsule://run.ato.desktop/dock
        OpenDockWindow,
        // Toggle the Control Bar info popup (anchor below URL bar)
        ToggleControlBarInfoPopup,
        // Toggle star/pin state for the current capsule URL
        ToggleStarCapsule,
        // Shell Icon Bar: open/raise the Ato PWA Home — the fixed
        // leading Ato icon in the top pill. The Home window is the
        // control surface (login, Discover, Run, runner settings).
        ShowAtoHome
    ]
);

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct NavigateToUrl {
    pub url: String,
}

/// Shell Icon Bar: raise the content window backing a capsule tab.
#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct FocusContentWindow {
    pub window_id: u64,
}

/// Shell Icon Bar: a blocked launch tab was clicked — show the
/// diagnostic placeholder (blocker kind + capsule). A real consent /
/// billing resolution UI is a later phase.
#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct ShowLaunchBlockedInfo {
    pub title: String,
    pub reason: String,
}

/// Parse an `ato://app/<install_profile_key>` URL into its install profile key.
///
/// Returns `None` for URLs that are not `ato://app` (so callers fall through to
/// other routing). Returns `Some(Err(_))` for malformed `ato://app` URLs so the
/// MCP surface can report a structured `invalid_ato_app_url` failure instead of
/// silently ignoring a typo'd deep link.
pub(crate) fn ato_app_install_profile_key(raw: &str) -> Option<Result<String, String>> {
    let trimmed = raw.trim();
    let parsed = match url::Url::parse(trimmed) {
        Ok(parsed) => parsed,
        Err(err) => {
            return trimmed
                .starts_with("ato://app")
                .then(|| Err(format!("invalid ato://app URL: {err}")));
        }
    };
    if parsed.scheme() != "ato" || parsed.host_str() != Some("app") {
        return None;
    }

    let segments: Vec<_> = parsed
        .path_segments()
        .map(|segments| segments.filter(|segment| !segment.is_empty()).collect())
        .unwrap_or_default();
    if segments.len() != 1 {
        return Some(Err(
            "ato://app URL must be shaped as ato://app/<install_profile_key>".to_string(),
        ));
    }

    Some(Ok(segments[0].to_string()))
}

/// MCP preflight for `host_dispatch_action`. When an automation client dispatches
/// `NavigateToUrl` with an `ato://app/<ipk>` URL that is malformed, unknown, or
/// points to a degraded installed profile, return a structured
/// `{ok:false, action, url, reason, detail?}` response so the call fails visibly
/// instead of queueing an action that would silently do nothing. Returns `None`
/// for every other action/URL, in which case the dispatcher queues as normal.
pub(crate) fn navigate_to_url_mcp_preflight(
    action: &str,
    url: Option<&str>,
) -> Option<serde_json::Value> {
    if action != "NavigateToUrl" {
        return None;
    }
    let raw = url?;
    let install_profile_key = match ato_app_install_profile_key(raw)? {
        Ok(key) => key,
        Err(message) => {
            return Some(serde_json::json!({
                "ok": false,
                "action": action,
                "url": raw,
                "reason": "invalid_ato_app_url",
                "detail": message,
            }));
        }
    };

    match crate::install_lifecycle_dashboard::inspect_launchable_installed_profile(
        &install_profile_key,
    ) {
        Ok(_) => None,
        Err(err) => {
            let mut response = serde_json::json!({
                "ok": false,
                "action": action,
                "url": raw,
                "reason": err.reason(),
            });
            if let Some(detail) = err.detail() {
                response["detail"] = serde_json::Value::String(detail.to_string());
            }
            Some(response)
        }
    }
}

/// Returns true if `input` looks like a GitHub repository URL (with or
/// without scheme/host prefix). Used by the control bar to route GitHub
/// repo inputs to the GitHub Import review surface instead of the
/// capsule:// / external-URL flows. Bare `owner/repo` is intentionally
/// excluded to avoid colliding with other input intents in the URL bar.
fn looks_like_github_repo_input(input: &str) -> bool {
    let lower = input.trim().to_ascii_lowercase();
    const PREFIXES: &[&str] = &[
        "github.com/",
        "www.github.com/",
        "https://github.com/",
        "https://www.github.com/",
        "http://github.com/",
        "http://www.github.com/",
    ];
    PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// Classify a closing GPUI window so the `on_window_closed` log makes it
/// clear whether the boot wizard, an AppWindow, or chrome was closed.
/// This is diagnostic-only (#370 lifecycle investigation).
fn classify_closed_window_kind(cx: &App, window_id: u64) -> &'static str {
    // Check if the window id belongs to the boot wizard slot.
    let boot_matches = cx
        .try_global::<crate::window::launch_window::BootWindowSlot>()
        .and_then(|s| s.boot_window)
        .map(|h| h.window_id().as_u64() == window_id)
        .unwrap_or(false);
    if boot_matches {
        return "boot-wizard";
    }

    // Check if registered as an AppWindow in the registry.
    if cx
        .global::<crate::state::AppWindowRegistry>()
        .find_by_gpui_window_id(window_id)
        .is_some()
    {
        return "app-window";
    }

    // Check singleton chrome windows.
    if let Some(c) = cx.try_global::<crate::window::ControlBarController>()
        && c.handle
            .map(|h| h.window_id().as_u64() == window_id)
            .unwrap_or(false)
    {
        return "control-bar";
    }
    if cx
        .global::<crate::window::card_switcher::CardSwitcherWindowSlot>()
        .0
        .map(|h| h.window_id().as_u64() == window_id)
        .unwrap_or(false)
    {
        return "card-switcher";
    }
    if cx
        .global::<crate::window::settings_window::SettingsWindowSlot>()
        .0
        .map(|h| h.window_id().as_u64() == window_id)
        .unwrap_or(false)
    {
        return "settings";
    }

    // Check content windows (dock, store, onboarding, start, etc.)
    if cx
        .global::<crate::window::content_windows::OpenContentWindows>()
        .get(window_id)
        .is_some()
    {
        return "content-window";
    }

    "unknown"
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct SetControlBarMode {
    pub mode: ControlBarMode,
}

/// Hand a URL to the OS so it opens in the user's default browser
/// (or whatever app is registered for the scheme). Used by the
/// route-metadata popover to make local_url / healthcheck_url /
/// invoke_url click-through to the same dev server the WebView is
/// rendering, but in a real browser for inspection / DevTools.
#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
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
#[action(namespace = desktop, no_json)]
pub struct InstallCapsuleUpdate {
    pub url: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct RestartContentWindow {
    pub window_id: u64,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct StopContentWindow {
    pub window_id: u64,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct OpenContentWindowLogs {
    pub window_id: u64,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct OpenContentWindowSettings {
    pub window_id: u64,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct SelectTask {
    pub task_id: usize,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct SelectSettingsTab {
    pub tab: crate::state::SettingsTab,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct SelectInstalledApp {
    pub installed_app_id: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct SelectInstalledProfile {
    pub installed_app_id: String,
    pub profile_id: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct SelectRouteMetadataTab {
    pub tab: crate::state::CapsuleDetailTab,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
pub struct CloseTask {
    pub task_id: usize,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = desktop, no_json)]
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
                crate::webview_init_guard::wait_until_idle(&bg).await;
                refresh_app.refresh();
                refresh_scheduled.store(false, Ordering::Release);
            })
            .detach();
    }
}

impl AssetSource for LocalAssetSource {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        // Local override first — lets us ship our own bg images,
        // automation/, preload/, etc. under crates/desktop/assets/.
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

pub fn run(skip_onboarding: bool) {
    let assets_dir = match resolve_assets_dir() {
        Ok(path) => path,
        Err(error) => {
            tracing::error!(error = %error, "failed to resolve ato-desktop assets directory");
            eprintln!("Ato Desktop startup error:\n{error}");
            return;
        }
    };
    match crate::system_capsule::materializer::bootstrap_from_assets(&assets_dir) {
        Ok(report) => {
            tracing::info!(
                materialized = report.materialized.len(),
                reused = report.reused.len(),
                degraded = report.degraded.len(),
                "system capsule seeds bootstrapped"
            );
            for degraded in &report.degraded {
                tracing::warn!(
                    capsule = degraded.capsule,
                    error = degraded.error,
                    "system capsule entered degraded state during bootstrap"
                );
            }
        }
        Err(error) => {
            tracing::error!(?error, "system capsule bootstrap failed before startup");
        }
    }
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
        cx.set_global(crate::state::session::SessionRegistry::default());
        cx.set_global(crate::window::launch_window::PendingLaunches::default());
        cx.set_global(crate::window::focus_guest_panes::FocusGuestPaneRegistry::default());
        cx.set_global(crate::state::capsule_state::CapsuleStateStore::default());
        cx.set_global(
            crate::system_capsule::window_registry::SystemCapsuleWindowRegistry::default(),
        );
        crate::window::install_control_bar_controller(cx);
        // Windows system tray (KOH-41): global lifecycle menu (Open Ato /
        // Running Apps / Stop All Running Apps / Quit Ato). The tray is the
        // escape hatch for stopping background-running sessions since closing a
        // window no longer stops its session.
        #[cfg(target_os = "windows")]
        crate::window::tray::install_tray(cx);
        // Windows taskbar Jump List (KOH-41): same lifecycle actions exposed on
        // the taskbar button's right-click menu, forwarded to this instance.
        #[cfg(target_os = "windows")]
        crate::window::taskbar::install_taskbar(cx);
        // Slot tracking the currently-open Card Switcher window so
        // the Control Bar's switcher button can toggle (open → close)
        // rather than stack overlays.
        cx.set_global(crate::window::card_switcher::CardSwitcherWindowSlot::default());
        cx.set_global(crate::window::card_switcher::CardSwitcherEntitySlot::default());
        // Slot tracking the currently-open Launcher window so the
        // Stage D retired the Launcher window — the focused
        // settings cog now opens an `ato-settings` system capsule
        // window directly.
        cx.set_global(crate::window::settings_window::SettingsWindowSlot(None));
        // Slot tracking the currently-open Store window (Wry WebView
        // on ato.run).
        cx.set_global(crate::window::store::StoreWindowSlot::default());
        // Slot tracking the dedicated Ato PWA Home window (the Shell
        // Icon Bar's fixed Ato icon opens/raises it).
        cx.set_global(crate::window::ato_home_shell::AtoHomeWindowSlot::default());
        cx.set_global(crate::window::quit_prompt::QuitPromptWindowSlot::default());
        cx.set_global(crate::window::launch_blocked_popup::LaunchBlockedPopupSlot::default());
        // Account-wide active runs on other runners — polled from the
        // account API so the Shell Icon Bar can show them as tabs.
        crate::remote_runs::start_remote_runs_poller(cx);
        // Launch handoff tracking (IPC bridge / ato://launch): launches
        // the desktop has accepted visible ownership of, polled per
        // launch and surfaced as Shell Icon Bar tabs.
        crate::launch_tracker::init(cx);
        // Slot tracking the currently-open Developer Console window.
        cx.set_global(crate::window::dock::DockWindowSlot::default());
        cx.set_global(crate::window::dock::DockEntitySlot::default());
        cx.set_global(crate::window::dock::DockIdentityCache::default());
        cx.set_global(crate::window::capsule_panel::CapsulePanelWindowSlot::default());
        cx.set_global(crate::window::capsule_panel::CapsuleSettingsWindowSlot::default());
        // Slot tracking the control bar info popup.
        cx.set_global(crate::window::control_bar::InfoPopupWindowSlot::default());
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
            // in-Focus navigation (Launcher, Card Switcher).
            // `OpenAppWindowExperiment` survives as an action handler
            // (reachable via the automation socket `host_dispatch_action`
            // for AODD scripts that need to spawn an additional Focus
            // AppWindow), but has no key binding.
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
        // ConfirmQuitKeep / ConfirmQuitClear / CancelQuit resolve the quit prompt.
        cx.on_action(|_: &ConfirmQuitKeep, cx| {
            crate::system_capsule::ato_import::stop_active_import_preview_blocking(
                cx,
                "desktop_shutdown",
            );
            let count = cx
                .global_mut::<crate::state::session::SessionRegistry>()
                .stop_all_running();
            tracing::info!(count, "app quit: stopped running sessions");
            cx.quit();
        });
        cx.on_action(|_: &ConfirmQuitClear, cx| {
            crate::system_capsule::ato_import::stop_active_import_preview_blocking(
                cx,
                "desktop_shutdown",
            );
            let count = cx
                .global_mut::<crate::state::session::SessionRegistry>()
                .stop_all_running();
            tracing::info!(count, "app quit: stopped running sessions");
            if let Ok(path) = ato_path("desktop-tabs.json") {
                let _ = std::fs::remove_file(&path);
            }
            cx.quit();
        });
        cx.on_action(|_: &ConfirmQuitWithCleanup, cx| {
            crate::system_capsule::ato_import::stop_active_import_preview_blocking(
                cx,
                "desktop_shutdown",
            );
            crate::window::dock::cleanup_dock_window(cx);
            let report = crate::orchestrator::cleanup_host_resources();
            tracing::info!(?report, "Host resource cleanup completed on quit");
            cx.quit();
        });
        cx.on_window_closed(|cx, window_id| {
            // Evict the closed window from the AppWindow registry so
            // Card Switcher / MRU stay accurate. The registry uses
            // the GPUI WindowId u64 it stamped at open time.
            let closed_id = window_id.as_u64();
            // Classify the closed window so the log makes it clear
            // whether the boot wizard, an AppWindow, or chrome closed.
            let closed_kind = classify_closed_window_kind(cx, closed_id);
            tracing::info!(
                closed_id,
                %closed_kind,
                "OS/GPUI window close observed; foreground surface destroyed"
            );
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

            // Evict the Focus guest capsule automation pane (#370) so a
            // stale pane is never reported by `browser_tabs` after its
            // window closes, and fail any in-flight MCP browser requests
            // still queued against it so callers don't hang.
            if let Some(entry) = cx
                .global_mut::<crate::window::focus_guest_panes::FocusGuestPaneRegistry>()
                .unregister_window(closed_id)
            {
                if let Some(host) = cx.try_global::<crate::automation::AutomationHost>() {
                    host.fail_requests_for_pane(entry.pane_id);
                }
                tracing::info!(
                    gpui_window_id = closed_id,
                    pane_id = entry.pane_id,
                    "focus guest capsule pane unregistered on close"
                );
            }
            if let Some(handle) = cx.global::<crate::window::ControlBarController>().handle
                && handle.window_id() == window_id {
                    cx.global_mut::<crate::window::ControlBarController>()
                        .clear_window(handle);
                    tracing::info!("Control Bar window closed; controller cleared");
                    // The bar must never stay gone: whatever closed it
                    // (stray Cmd+W, programmatic remove_window), reopen it
                    // unless the app is quitting. Deferred so the close
                    // finishes unwinding first.
                    if !crate::window::is_shutting_down() {
                        crate::system_capsule::ipc::defer_after_dispatch_for(
                            cx,
                            std::time::Duration::from_millis(150),
                            |cx| {
                                if crate::window::is_shutting_down() {
                                    return;
                                }
                                match crate::window::open_focus_control_bar(cx) {
                                    Ok(_) => tracing::info!("Control Bar reopened after close"),
                                    Err(err) => tracing::warn!(
                                        error = %err,
                                        "Control Bar reopen after close failed"
                                    ),
                                }
                            },
                        );
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
                cx.set_global(
                    crate::window::card_switcher::CardSwitcherEntitySlot(None),
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
            let info_popup_slot = cx
                .global::<crate::window::control_bar::InfoPopupWindowSlot>()
                .0;
            if info_popup_slot
                .map(|h| h.window_id() == window_id)
                .unwrap_or(false)
            {
                cx.set_global(crate::window::control_bar::InfoPopupWindowSlot(None));
                tracing::info!("Info popup window closed; slot cleared");
            }
            let store_slot = cx
                .global::<crate::window::store::StoreWindowSlot>()
                .0;
            if store_slot.map(|h| h.window_id() == window_id).unwrap_or(false) {
                cx.set_global(crate::window::store::StoreWindowSlot(None));
                tracing::info!("Store window closed; slot cleared");
            }
            let import_slot = cx
                .try_global::<crate::window::import_window::ImportWindowSlot>()
                .and_then(|slot| slot.window);
            if import_slot
                .map(|h| h.window_id() == window_id)
                .unwrap_or(false)
            {
                crate::system_capsule::ato_import::stop_active_import_preview(cx, "window_close");
                cx.set_global(crate::window::import_window::ImportWindowSlot::default());
                tracing::info!("Import window closed; slot cleared");
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

            // Unregister system capsule window binding on close.
            cx.global_mut::<crate::system_capsule::window_registry::SystemCapsuleWindowRegistry>()
                .unregister_window(window_id);
            tracing::debug!(?window_id, "SystemCapsuleWindowRegistry: binding removed on window close");

            // Session lifecycle handling based on windowCloseBehavior.
            // The AppCapsuleShell Drop will detach the client; we decide
            // whether to also stop the process here.
            let close_behavior =
                crate::config::load_config().desktop.window_close_behavior;
            let affected_session_ids = {
                let registry =
                    cx.global_mut::<crate::state::session::SessionRegistry>();
                let ids = registry.detach_clients_by_window_id(closed_id);
                if close_behavior == crate::config::WindowCloseBehavior::StopSession {
                    tracing::info!(
                        ?ids,
                        "windowCloseBehavior=stop-session: detached clients; stopping sessions asynchronously"
                    );
                } else {
                    tracing::info!(
                        ?ids,
                        "windowCloseBehavior=keep-session-running: sessions detached"
                    );
                }
                ids
            };
            if close_behavior == crate::config::WindowCloseBehavior::StopSession {
                for sid in &affected_session_ids {
                    crate::window::stop_session_once_with_ui_completion(cx, sid);
                }
            }
            if close_behavior == crate::config::WindowCloseBehavior::StopSession {
                // Clear ephemeral capsule state for stopped sessions.
                for sid in &affected_session_ids {
                    cx.global_mut::<crate::state::capsule_state::CapsuleStateStore>()
                        .clear_session(sid);
                }
            }

            // Window-lifecycle endgame.
            //
            // The Control Bar is a process-lifetime singleton, so
            // `cx.windows()` is effectively never empty while the bar is up —
            // and the bar is a WS_EX_TOOLWINDOW that never appears in the
            // Windows taskbar. A user who closes every content window from the
            // taskbar would otherwise be left with an invisible, unclosable
            // bar and a process that never exits. Instead, when the last
            // *content* window closes we bring back the Start capsule as the
            // landing surface (its quit button is the explicit exit). If the
            // Start page cannot be opened and only the Control Bar remains,
            // that is an unrecoverable state — quit as abnormal.
            // Closing the quit prompt itself means "Reopen": bring the
            // PWA Home back instead of re-showing the prompt.
            let prompt_slot = cx
                .global::<crate::window::quit_prompt::QuitPromptWindowSlot>()
                .0;
            if prompt_slot.map(|h| h.window_id() == window_id).unwrap_or(false) {
                cx.set_global(crate::window::quit_prompt::QuitPromptWindowSlot(None));
                if !crate::window::is_shutting_down() {
                    crate::system_capsule::ipc::defer_after_dispatch_for(
                        cx,
                        std::time::Duration::from_millis(100),
                        |cx| {
                            if let Err(err) = crate::window::home::open_home_window(cx) {
                                tracing::error!(error = %err, "quit prompt closed: reopen Home failed");
                            }
                        },
                    );
                }
            } else if !crate::window::is_shutting_down()
                && cx
                    .global::<crate::window::content_windows::OpenContentWindows>()
                    .is_empty()
            {
                reopen_start_or_quit(cx);
            }
        })
        .detach();

        cx.on_action(|_: &ShowControlBar, cx: &mut App| {
            if let Err(err) = crate::window::show_control_bar(cx) {
                tracing::error!(error = %err, "ShowControlBar failed");
            }
        });

        cx.on_action(|_: &HideControlBar, cx: &mut App| {
            crate::window::hide_control_bar(cx);
        });

        cx.on_action(|_: &ToggleControlBar, cx: &mut App| {
            if let Err(err) = crate::window::toggle_control_bar(cx) {
                tracing::error!(error = %err, "ToggleControlBar failed");
            }
        });

        cx.on_action(|_: &FocusControlBarInput, cx: &mut App| {
            if let Err(err) = crate::window::focus_control_bar_input(cx) {
                tracing::error!(error = %err, "FocusControlBarInput failed");
            }
        });

        cx.on_action(|_action: &SetControlBarMode, cx: &mut App| {
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
            stop_active_focus_capsule(cx);
        });
        cx.on_action(|action: &StopContentWindow, cx: &mut App| {
            stop_focus_content_window(cx, action.window_id);
        });
        cx.on_action(|action: &RestartContentWindow, cx: &mut App| {
            restart_focus_content_window(cx, action.window_id);
        });
        cx.on_action(|action: &OpenContentWindowLogs, cx: &mut App| {
            open_focus_content_window_logs(cx, action.window_id);
        });
        cx.on_action(|action: &OpenContentWindowSettings, cx: &mut App| {
            crate::window::control_bar::dismiss_info_popup(cx);
            if let Err(err) =
                crate::window::capsule_panel::open_capsule_settings_window(cx, action.window_id)
            {
                tracing::error!(error = %err, "OpenContentWindowSettings failed");
            }
        });

        cx.on_action(|_: &StopAllRetainedSessions, cx: &mut App| {
            stop_all_focus_capsules(cx);
        });

        // #169 — multi-window experiment action. Opens a placeholder
        // `AppWindowShell` window via the consent wizard so the full
        // boot flow is exercised from the automation socket.
        cx.on_action(|_: &OpenAppWindowExperiment, cx: &mut App| {
            tracing::info!("OpenAppWindowExperiment handler entered");
            // Go through the consent wizard so the full boot flow is
            // exercised end-to-end from the keyboard shortcut.
            let route = crate::state::GuestRoute::CapsuleHandle {
                handle: "github.com/Koh0920/WasedaP2P".to_string(),
                label: "WasedaP2P".to_string(),
                community_toml_id: None,
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

        // Backward-compatible alias: OpenIdentityMenu now routes to
        // the Dock window. The Control Bar avatar button dispatches
        // OpenDockWindow directly, but external callers may still
        // send OpenIdentityMenu.
        cx.on_action(|_: &OpenIdentityMenu, cx: &mut App| {
            crate::system_capsule::ipc::defer_after_dispatch_for(
                cx,
                std::time::Duration::from_millis(50),
                |cx| {
                    if let Err(err) = crate::window::dock::open_dock_window(cx) {
                        tracing::error!(error = %err, "OpenIdentityMenu: open_dock_window failed");
                    }
                },
            );
        });

        // Settings cog routing in Focus mode — Stages C+D:
        // ShowSettings opens a standalone Wry-hosted Settings
        // window (the `ato-settings` system capsule). The legacy
        // Launcher window was retired in Stage D so the Control
        // Bar dispatches ShowSettings as the sole action for the
        // settings cog click.
        cx.on_action(|_: &ShowSettings, cx: &mut App| {
            crate::system_capsule::ipc::defer_after_dispatch_for(
                cx,
                std::time::Duration::from_millis(50),
                |cx| {
                    crate::window::control_bar::dismiss_info_popup(cx);
                    if let Err(err) = crate::window::settings_window::open_settings_window(cx) {
                        tracing::error!(error = %err, "ShowSettings: open_settings_window failed");
                    }
                },
            );
        });

        // Handler for the Control Bar URL pill's PressEnter. Parses the
        // typed URL and spawns an AppWindow with the matching GuestRoute.
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
            let owned_url;
            let mut raw = action.url.trim();
            if raw.is_empty() {
                return;
            }
            tracing::info!(url = %raw, "Focus-mode NavigateToUrl");

            // ato://launch?launch_id=<id>[&capsule_ref=<publisher/slug>] —
            // the external-browser → Desktop fallback of the launch
            // handoff (launch-unification plan §4; the primary in-Desktop
            // path is the injected `__ATO_DESKTOP__.launch()` IPC bridge).
            // Only the launch_id (plus an optional display ref) rides the
            // intent — app_url / runner URLs / tokens are never accepted;
            // the LaunchTracker re-fetches the launch with owner-verified
            // credentials, so a launch_id alone can never open anything
            // the signed-in user doesn't own.
            if raw.starts_with("ato://launch") {
                let parsed = url::Url::parse(raw).ok();
                let query = |key: &str| {
                    parsed.as_ref().and_then(|url| {
                        url.query_pairs()
                            .find(|(k, _)| k == key)
                            .map(|(_, v)| v.into_owned())
                    })
                };
                match query("launch_id") {
                    Some(id) if crate::launch_tracker::is_valid_launch_id(&id) => {
                        let capsule_ref = query("capsule_ref")
                            .filter(|r| crate::launch_tracker::is_valid_capsule_ref(r))
                            .unwrap_or_default();
                        tracing::info!(
                            launch_id = %id,
                            "NavigateToUrl(ato://launch): tracking launch"
                        );
                        if !crate::launch_tracker::register_launch(cx, id, capsule_ref) {
                            tracing::warn!(
                                "NavigateToUrl(ato://launch): tracker refused (cap reached) — ignored"
                            );
                        }
                    }
                    _ => {
                        tracing::warn!(
                            url = %raw,
                            "NavigateToUrl(ato://launch): missing or invalid launch_id — ignored"
                        );
                    }
                }
                return;
            }

            // ato://open?handle=<url-or-capsule-ref> — the PWA Home's
            // "open this app outside my WebView" intent (mirrors the legacy
            // AppState::handle_host_route deep link). Unwrap the inner
            // target and route it like any other NavigateToUrl input: an
            // https session URL opens an independent app window, a
            // capsule:// ref goes through the native launch flow.
            if raw.starts_with("ato://open") {
                let inner = url::Url::parse(raw).ok().and_then(|url| {
                    url.query_pairs()
                        .find(|(k, _)| k == "handle" || k == "url")
                        .map(|(_, v)| v.into_owned())
                });
                match inner {
                    Some(inner) if !inner.trim().is_empty() => {
                        tracing::info!(target = %inner, "NavigateToUrl(ato://open): unwrapped");
                        owned_url = inner;
                        raw = owned_url.trim();
                    }
                    _ => {
                        tracing::warn!(
                            url = %raw,
                            "NavigateToUrl(ato://open): missing 'handle' query parameter — ignored"
                        );
                        return;
                    }
                }
            }

            // GitHub Import: github.com/owner/repo or https://github.com/...
            // is routed to the GitHub Import review surface rather than the
            // capsule consent or external-URL flows. Bare `owner/repo` is
            // not matched here to avoid colliding with other input intents.
            if looks_like_github_repo_input(raw)
                && let Ok(normalized) =
                    crate::source_import_session::normalize_github_import_input(raw)
                {
                    if let Err(err) = crate::window::import_window::open_with_url(
                        cx,
                        normalized.source_url_normalized.clone(),
                    ) {
                        tracing::error!(
                            error = %err,
                            url = %raw,
                            "NavigateToUrl(github): open_with_url failed"
                        );
                    }
                    return;
                }

            // ato://app/<ipk> — the durable open identity of an installed app
            // (#261). Route it straight to the install-owned, pre-consented
            // launch path keyed by install_profile_key so deep links and the
            // MCP `browser_navigate` / `NavigateToUrl` surface can reopen an
            // installed app without re-showing the first-run consent wizard.
            if raw.starts_with("ato://app/") {
                match crate::system_capsule::ato_start::open_installed_app_by_ipk(cx, raw) {
                    Ok(()) => {}
                    Err(err) => {
                        // Unknown / degraded ipk or unreadable store. Fail
                        // visibly rather than silently degrading to a handle
                        // launch + consent wizard, which would lose the pinned
                        // revision and install identity.
                        tracing::warn!(
                            error = %err,
                            url = %raw,
                            "NavigateToUrl(ato://app): no launchable installed profile — ignored"
                        );
                    }
                }
                return;
            }

            // ato://run?source=<capsule-ref>[&run_id=<id>] — the embedded PWA
            // Home's "run this on my Desktop Runner" intent (PR-D1). Unlike
            // `capsule://` below, this never embeds a guest WebView pane:
            // approval spawns `ato run <source>` on the Desktop Runner
            // cold-OCI substrate directly. Checked via a parsed host
            // comparison (not `starts_with`) so it cannot collide with a
            // future `ato://runner/...` verb, which shares the `ato://run`
            // string prefix.
            if let Ok(parsed_ato) = url::Url::parse(raw)
                && parsed_ato.scheme() == "ato"
                && parsed_ato.host_str() == Some("run")
            {
                match crate::intent::parse_run_query(raw) {
                    Some((source, run_id)) => {
                        // No per-navigation pane origin is available on this
                        // live router (see the `crate::intent` module note);
                        // the Home surface's own configured origin is the
                        // honest stand-in for "who asked for this" in logs.
                        let origin = crate::window::home::home_url(&crate::config::load_config())
                            .origin()
                            .ascii_serialization();
                        crate::system_capsule::ato_start::dispatch_run_intent(
                            cx, source, run_id, origin,
                        );
                    }
                    None => {
                        tracing::warn!(
                            url = %raw,
                            "NavigateToUrl(ato://run): missing 'source' — ignored"
                        );
                    }
                }
                return;
            }

            if let Some(rest) = raw.strip_prefix("capsule://") {
                // Extract optional ?ctoml=<id> query parameter.
                let (rest_path, community_toml_id) = if let Some(q_pos) = rest.find('?') {
                    let path = &rest[..q_pos];
                    let query = &rest[q_pos + 1..];
                    let cid = query
                        .split('&')
                        .find_map(|kv| kv.strip_prefix("ctoml="))
                        .filter(|v| !v.is_empty())
                        .map(str::to_string);
                    (path, cid)
                } else {
                    (rest, None)
                };
                let handle = rest_path.trim_end_matches('/').to_string();
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
                        // Go through the consent wizard — E103/E302 modals
                        // will appear in the Desktop shell before the capsule
                        // is launched and opened in the OS browser.
                        let route = crate::state::GuestRoute::CapsuleHandle {
                            handle,
                            label,
                            community_toml_id,
                        };
                        // #460 PR3b: route through Runtime Setup first if the host
                        // OCI runtime is not ready, then resume this launch.
                        crate::runtime_setup::launch_intent::open_capsule_launch_gated(
                            cx,
                            route,
                            crate::state::session::SessionClientKind::OsBrowser,
                            "launch_flow",
                        );
                    }
                    crate::config::CapsuleOpenMode::Webviewer => {
                        tracing::warn!(
                            "capsule_open_mode=webviewer: not yet implemented, falling back to window"
                        );
                        // fall through to window behaviour
                        let route = crate::state::GuestRoute::CapsuleHandle {
                            handle,
                            label,
                            community_toml_id,
                        };
                        crate::runtime_setup::launch_intent::open_capsule_launch_gated(
                            cx,
                            route,
                            crate::state::session::SessionClientKind::AtoWindow,
                            "launch_flow",
                        );
                    }
                    crate::config::CapsuleOpenMode::Window => {
                        let route = crate::state::GuestRoute::CapsuleHandle {
                            handle,
                            label,
                            community_toml_id,
                        };
                        // Gate every capsule launch on a pre-flight consent
                        // wizard. On Approve the broker spawns the real
                        // AppWindow + boot wizard; on Cancel nothing happens.
                        // #460 PR3b: if the host OCI runtime is not ready, this
                        // first detours through Runtime Setup and resumes the
                        // launch once it is ready instead of stranding the user.
                        crate::runtime_setup::launch_intent::open_capsule_launch_gated(
                            cx,
                            route,
                            crate::state::session::SessionClientKind::AtoWindow,
                            "launch_flow",
                        );
                    }
                }
                return;
            }
            match url::Url::parse(raw) {
                Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => {
                    // Re-opening a URL whose origin is already hosted by an
                    // open app window focuses that window instead of
                    // spawning a duplicate — clicking an icon-bar tab (or
                    // re-launching from the PWA) is a focus gesture.
                    if let Some(existing) = find_open_external_window(cx, parsed.as_str()) {
                        let window_id = existing.window_id().as_u64();
                        tracing::info!(
                            url = %parsed,
                            window_id,
                            "NavigateToUrl(http): focusing existing window"
                        );
                        cx.global_mut::<crate::window::content_windows::OpenContentWindows>()
                            .focus(window_id);
                        crate::window::raise_content_window(cx, existing);
                        return;
                    }
                    let open_mode = crate::config::load_config().desktop.capsule_open_mode;
                    match open_mode {
                        crate::config::CapsuleOpenMode::OsBrowser => {
                            let url_str = parsed.to_string();
                            if let Err(e) = crate::proc_util::open_external_url(&url_str) {
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

        // #173 — open Card Switcher overlay. The overlay snapshots open
        // `AppWindow`s and renders them as MRU-ordered cards.
        cx.on_action(|_: &OpenCardSwitcher, cx: &mut App| {
            crate::system_capsule::ipc::defer_after_dispatch_for(
                cx,
                std::time::Duration::from_millis(50),
                |cx| {
                    crate::window::control_bar::dismiss_info_popup(cx);
                    if let Err(err) = crate::window::open_card_switcher_window(cx) {
                        tracing::error!(error = %err, "failed to open card switcher window");
                    }
                },
            );
        });

        // Shell Icon Bar — the fixed Ato icon opens/raises the Ato PWA
        // Home control surface.
        cx.on_action(|_: &ShowAtoHome, cx: &mut App| {
            tracing::info!("ShowAtoHome action received");
            crate::window::control_bar::dismiss_info_popup(cx);
            if let Err(err) = crate::window::home::show_ato_home(cx) {
                tracing::error!(error = %err, "ShowAtoHome: failed to open Home surface");
            }
        });

        // Shell Icon Bar — clicking a blocked launch tab shows the
        // diagnostic placeholder popup.
        cx.on_action(|action: &ShowLaunchBlockedInfo, cx: &mut App| {
            if let Err(err) = crate::window::launch_blocked_popup::open_launch_blocked_popup(
                cx,
                action.title.clone(),
                action.reason.clone(),
            ) {
                tracing::error!(error = %err, "ShowLaunchBlockedInfo: popup failed");
            }
        });

        // Shell Icon Bar — a capsule tab raises its content window.
        cx.on_action(|action: &FocusContentWindow, cx: &mut App| {
            use crate::window::content_windows::OpenContentWindows;
            let target = cx
                .global::<OpenContentWindows>()
                .get(action.window_id)
                .map(|entry| entry.handle);
            let Some(handle) = target else {
                tracing::warn!(
                    window_id = action.window_id,
                    "FocusContentWindow: window not tracked (already closed?)"
                );
                return;
            };
            cx.global_mut::<OpenContentWindows>().focus(action.window_id);
            tracing::info!(window_id = action.window_id, "FocusContentWindow: raising");
            crate::window::raise_content_window(cx, handle);
        });

        // #174 — cycle through open app windows in MRU order via
        // trackpad swipe gestures on the Control Bar.
        cx.on_action(|_: &FocusPrevAppWindow, cx: &mut App| {
            cycle_app_window(cx, -1);
        });
        cx.on_action(|_: &FocusNextAppWindow, cx: &mut App| {
            cycle_app_window(cx, 1);
        });

        // Open / focus the Store window (Wry WebView → ato.run).
        cx.on_action(|_: &OpenStoreWindow, cx: &mut App| {
            crate::system_capsule::ipc::defer_after_dispatch_for(
                cx,
                std::time::Duration::from_millis(50),
                |cx| {
                    crate::window::control_bar::dismiss_info_popup(cx);
                    if let Err(err) = crate::window::store::open_store_window(cx) {
                        tracing::error!(error = %err, "failed to open store window");
                    }
                },
            );
        });

        // Toggle Dock visibility. If the dock window exists, close it.
        // If not, open it. The identity cache makes re-opening fast.
        cx.on_action(|_: &ToggleDock, cx: &mut App| {
            let slot = cx.global::<crate::window::dock::DockWindowSlot>();
            if let Some(handle) = slot.0 {
                crate::system_capsule::ipc::defer_after_dispatch_for(
                    cx,
                    std::time::Duration::from_millis(50),
                    move |cx| {
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                    },
                );
            } else {
                crate::system_capsule::ipc::defer_after_dispatch_for(
                    cx,
                    std::time::Duration::from_millis(50),
                    |cx| {
                        if let Err(err) = crate::window::dock::open_dock_window(cx) {
                            tracing::error!(error = %err, "ToggleDock: open_dock_window failed");
                        }
                    },
                );
            }
        });

        // Open / focus the Dock window.
        cx.on_action(|_: &OpenDockWindow, cx: &mut App| {
            crate::system_capsule::ipc::defer_after_dispatch_for(
                cx,
                std::time::Duration::from_millis(50),
                |cx| {
                    crate::window::control_bar::dismiss_info_popup(cx);
                    if let Err(err) = crate::window::dock::open_dock_window(cx) {
                        tracing::error!(error = %err, "failed to open dock window");
                    }
                },
            );
        });

        // Toggle the Control Bar info popup. Dispatched from the info icon
        // button in the URL pill — safe because it runs outside render.
        cx.on_action(|_: &ToggleControlBarInfoPopup, cx: &mut App| {
            if let Some(shell) = cx.global::<crate::window::ControlBarController>().shell.clone() {
                shell.update(cx, |shell, cx| shell.toggle_info_popup(cx));
            }
        });

        // Toggle star/pin state for the current capsule URL. Dispatched from
        // the star icon button in the URL pill.
        cx.on_action(|_: &ToggleStarCapsule, cx: &mut App| {
            if let Some(shell) = cx.global::<crate::window::ControlBarController>().shell.clone() {
                shell.update(cx, |shell, cx| shell.toggle_star(cx));
            }
        });

        cx.on_action(|_: &OpenCapsulePanel, cx: &mut App| {
            crate::system_capsule::ipc::defer_after_dispatch_for(
                cx,
                std::time::Duration::from_millis(50),
                |cx| {
                    crate::window::control_bar::dismiss_info_popup(cx);
                    if let Err(err) = crate::window::capsule_panel::open_capsule_panel_window(cx) {
                        tracing::error!(error = %err, "failed to open capsule panel window");
                    }
                },
            );
        });

        // Spawn a fresh StartWindow. Unlike the Launcher / Store
        // handlers, there is no slot — every dispatch produces a new
        // window. The Card Switcher's new-window tile invokes the
        // underlying function directly (not through this action) to
        // avoid the dispatch-queue-vs-window-removal race, but the
        // action is still registered so MCP / keybind paths reach
        // the same target.
        cx.on_action(|_: &OpenStartWindow, cx: &mut App| {
            // ato-start is retired: the "new window" gesture opens (or
            // raises) the PWA Home instead.
            crate::system_capsule::ipc::defer_after_dispatch_for(
                cx,
                std::time::Duration::from_millis(50),
                |cx| {
                    if let Err(err) = crate::window::home::show_ato_home(cx) {
                        tracing::error!(error = %err, "OpenStartWindow: show_ato_home failed");
                    }
                },
            );
        });

        cx.on_action(|_: &OpenGithubRunWindow, cx: &mut App| {
            crate::system_capsule::ipc::defer_after_dispatch_for(
                cx,
                std::time::Duration::from_millis(50),
                |cx| {
                    if let Err(err) = crate::window::launch_window::open_github_run_window(cx) {
                        tracing::error!(error = %err, "failed to open github run window");
                    }
                },
            );
        });

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
            crate::system_capsule::ato_onboarding::should_show_onboarding(&startup_config)
                && !skip_onboarding;
        let async_cx = cx.to_async();
        cx.foreground_executor()
            .spawn(async move {
                // One frame is enough for the macOS RunLoop to complete
                // its first pass and for WKWebView to initialize normally.
                let bg_exec = async_cx.background_executor();
                async_cx
                    .background_executor()
                    .timer(std::time::Duration::from_millis(32))
                    .await;
                crate::webview_init_guard::wait_until_idle(bg_exec).await;
                async_cx.update(|cx| {
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
                        Ok(_) => {
                            tracing::info!(?startup_surface, "Startup surface opened");
                        }
                        Err(err) => {
                            tracing::error!(error = %err, ?startup_surface, "Startup surface failed")
                        }
                    }
                    // Background-refresh the installed-apps cache after the
                    // surface is open so the launcher has data on first paint.
                    cx.background_executor()
                        .spawn(async move {
                            let _ = crate::install_lifecycle_dashboard::DashboardCache::refresh();
                        })
                        .detach();
                });
            })
            .detach();

        // Start the focus dispatcher, which owns its own `AutomationHost`,
        // drains socket-delivered requests, and routes `HostDispatchAction`
        // to a real GPUI action dispatch. Actions are App-level so
        // dispatching via any window handle reaches the registered handler —
        // the Control Bar handle is used here since the Store window is deferred.
        if let Some(control_bar_handle) = control_bar_handle {
            crate::window::focus_dispatcher::start(cx, control_bar_handle);
        } else {
            tracing::warn!(
                "Focus dispatcher not started because the Control Bar is hidden at startup"
            );
        }

        cx.activate(true);
    });
}

/// Focus View: after the last content window closes, bring the Start
/// capsule back as the landing surface. If it cannot be opened and only the
/// Control Bar is left, the shell has nothing usable to show — treat that as
/// abnormal and quit. Deferred a tick so we never open a Wry WebView while
/// GPUI is still unwinding the `on_window_closed` callback (synchronous
/// `build_as_child` re-entrancy panics on Windows).
fn reopen_start_or_quit(cx: &mut App) {
    crate::system_capsule::ipc::defer_after_dispatch(cx, |cx| {
        if crate::window::is_shutting_down() {
            return;
        }
        // A content window may have opened in the meantime (e.g. the user
        // launched something from the Control Bar) — nothing to do then.
        if !cx
            .global::<crate::window::content_windows::OpenContentWindows>()
            .is_empty()
        {
            return;
        }
        // macOS: the Shell Icon Bar IS the landing surface. It stays
        // visible (Dock icon + menu bar keep the app reachable and
        // quittable), and its Ato icon reopens the Home window on
        // demand. Windows: the bar is a taskbar-invisible toolwindow,
        // so ask "Quit Ato?" instead — Quit exits, Reopen (or closing
        // the prompt) brings the PWA Home back. ato-start is retired
        // as a landing surface on both platforms.
        if cfg!(target_os = "macos") {
            tracing::info!(
                "last content window closed — Shell Icon Bar remains as the landing surface"
            );
            return;
        }
        match crate::window::quit_prompt::open_quit_prompt_window(cx) {
            Ok(_) => {
                tracing::info!("last content window closed — quit prompt shown");
            }
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "failed to open the quit prompt after last window closed — quitting as abnormal"
                );
                crate::window::begin_shutdown();
                cx.quit();
            }
        }
    });
}

/// Find an open ExternalUrl app window hosting the same web origin as
/// `url` (session URLs are origin-unique). Used by NavigateToUrl(http)
/// to focus instead of duplicating.
fn find_open_external_window(cx: &App, url: &str) -> Option<gpui::AnyWindowHandle> {
    use crate::state::GuestRoute;
    use crate::window::content_windows::{ContentWindowKind, OpenContentWindows};

    let target = url::Url::parse(url).ok()?;
    cx.global::<OpenContentWindows>()
        .mru_order()
        .into_iter()
        .find(|entry| match &entry.kind {
            ContentWindowKind::AppWindow {
                route: GuestRoute::ExternalUrl(open),
            } => open.origin() == target.origin(),
            _ => false,
        })
        .map(|entry| entry.handle)
}

/// Cycle focus through open app windows in MRU order.
/// `direction` is `+1` for next, `-1` for previous.
fn cycle_app_window(cx: &mut App, direction: i32) {
    use crate::window::content_windows::OpenContentWindows;

    let windows = cx.global::<OpenContentWindows>().mru_order();
    if windows.len() < 2 {
        return;
    }
    // The frontmost window is windows[0] (MRU order).
    // Wrap around to the next/previous entry.
    let len = windows.len() as i32;
    let idx = ((direction % len) + len) as usize % windows.len();
    let target = windows[idx].handle;
    let _ = target.update(cx, |_, window, _| window.activate_window());
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
    let capsule_session_id = entry
        .capsule
        .as_ref()
        .and_then(|capsule| capsule.session_id.clone());
    let ContentWindowKind::AppWindow { route } = entry.kind else {
        return;
    };
    let launch_configs = capsule_session_id
        .as_deref()
        .and_then(|session_id| {
            cx.global::<crate::state::session::SessionRegistry>()
                .get_session(session_id)
                .map(|session| session.launch_context.launch_configs.clone())
        })
        .unwrap_or_default();
    let materialized_record_path =
        capsule_session_id
            .as_deref()
            .and_then(|session_id| match &route {
                crate::state::GuestRoute::CapsuleHandle { .. }
                | crate::state::GuestRoute::CapsuleUrl { .. } => {
                    crate::orchestrator::materialized_record_path_for_session(session_id).ok()
                }
                _ => None,
            });
    if let Some(session_id) = capsule_session_id.as_deref()
        && let Err(err) = crate::orchestrator::stop_guest_session_and_wait(
            session_id,
            std::time::Duration::from_secs(3),
        )
    {
        tracing::error!(error = %err, window_id, "RestartContentWindow stop failed");
        return;
    }
    let _ = entry
        .handle
        .update(cx, |_, window, _| window.remove_window());
    let restart_result = if let Some(record_path) = materialized_record_path {
        crate::window::orchestrator::open_app_window_from_materialized_record_with_configs(
            cx,
            route.clone(),
            record_path,
            launch_configs,
        )
    } else {
        crate::window::orchestrator::open_app_window_with_configs(cx, route.clone(), launch_configs)
    };
    if let Err(err) = restart_result {
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
    DesktopBundlePaths::from_env()
        .resolve_assets_dir()
        .map_err(anyhow::Error::new)
}

#[cfg(test)]
mod tests {
    use super::{ato_app_install_profile_key, navigate_to_url_mcp_preflight, resolve_assets_dir};
    use serial_test::serial;

    #[test]
    fn resolve_assets_dir_finds_workspace_assets() {
        let path = resolve_assets_dir().expect("workspace assets should resolve");
        assert!(path.ends_with("assets"));
        assert!(path.is_dir());
    }

    #[test]
    fn ato_app_install_profile_key_extracts_single_key() {
        let key = ato_app_install_profile_key("ato://app/ipk_abc123?utm=ignored")
            .expect("ato app URL should be recognized")
            .expect("key should parse");

        assert_eq!(key, "ipk_abc123");
    }

    #[test]
    fn ato_app_install_profile_key_ignores_other_urls() {
        assert!(ato_app_install_profile_key("capsule://github.com/ato-run/demo").is_none());
        assert!(ato_app_install_profile_key("https://ato.run/").is_none());
    }

    #[test]
    fn ato_app_install_profile_key_rejects_missing_or_nested_key() {
        let missing = ato_app_install_profile_key("ato://app/")
            .expect("ato app URL should be recognized")
            .unwrap_err();
        let nested = ato_app_install_profile_key("ato://app/ipk_a/extra")
            .expect("ato app URL should be recognized")
            .unwrap_err();

        assert!(missing.contains("ato://app/<install_profile_key>"));
        assert!(nested.contains("ato://app/<install_profile_key>"));
    }

    #[test]
    #[serial]
    fn navigate_to_url_mcp_preflight_returns_structured_failure_for_unknown_ato_app() {
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("ATO_HOME", dir.path());
        }

        let response =
            navigate_to_url_mcp_preflight("NavigateToUrl", Some("ato://app/ipk_does_not_exist"))
                .expect("ato app URL should produce a preflight response");

        unsafe {
            std::env::remove_var("ATO_HOME");
        }

        assert_eq!(response["ok"], false);
        assert_eq!(response["action"], "NavigateToUrl");
        assert_eq!(response["url"], "ato://app/ipk_does_not_exist");
        assert_eq!(response["reason"], "installed_profile_not_found");
    }
}

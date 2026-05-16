mod chrome;
mod modals;
mod panels;
pub(crate) mod share;
mod sidebar;
mod theme;

use theme::Theme;

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, hsla, linear_color_stop, linear_gradient, point, px, AnyElement, AsyncWindowContext,
    BoxShadow, Context, Div, Entity, ExternalPaths, FocusHandle, Focusable, FontWeight, Image,
    ImageFormat, IntoElement, MouseButton, Render, WeakEntity, Window,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;

use self::chrome::render_command_chrome;
use self::panels::{render_settings_overlay, render_stage};
use self::sidebar::{
    favicon_candidate_urls, parse_link_icon_candidates, render_task_rail, FaviconState,
};
use crate::logging::TARGET_FAVICON;

use crate::app::{
    AllowPermissionForSession, AllowPermissionOnce, ApproveConsentForm, BrowserBack,
    BrowserForward, BrowserReload, CancelAuthHandoff, CancelConfigForm, CancelConsentForm,
    CancelQuit, CancelResolutionForm, CheckForUpdates, CloseTask, ConfirmQuitClear,
    ConfirmQuitKeep, CycleHandle, DenyPermissionPrompt, DismissTransient, ExpandSplit,
    FocusCommandBar, InstallCapsuleUpdate, MoveTask, NativeCopy, NativeCut, NativePaste,
    NativeRedo, NativeSelectAll, NativeUndo, NavigateToUrl, NewTab, NextTask, NextWorkspace,
    OpenAuthInBrowser, OpenCloudDock, OpenExternalLink, OpenLatestReleasePage, OpenLocalRegistry,
    OpenUrlBridge, PreviousTask, PreviousWorkspace, Quit, ResolutionFormBack, ResolutionFormNext,
    ResumeAfterAuth, SaveConfigForm, SelectRouteMetadataTab, SelectSettingsTab, SelectTask,
    ShowSettings, ShrinkSplit, SignInToAtoRun, SignOut, SplitPane, SubmitResolutionForm,
    ToggleAutoDevtools, ToggleDevConsole, ToggleRouteMetadataPopover, ToggleTheme,
};
use crate::orchestrator::cleanup_stale_capsule_sessions;
use crate::state::{
    ActivityTone, AppState, AuthSessionStatus, CapsuleDetailTab, PaneBounds, PaneId, PaneSurface,
    ShellMode, SidebarTaskIconSpec,
};
use crate::terminal::TerminalSessionManager;
use crate::webview::WebViewManager;
use capsule_wire::config::ConfigKind;

pub(super) const CHROME_HEIGHT: f32 = 48.0;
pub(super) const RAIL_WIDTH: f32 = 52.0;
pub(super) const STAGE_PADDING: f32 = 0.0;

const DEVTOOLS_DEBUG_ENV: &str = "ATO_DESKTOP_DEVTOOLS_DEBUG";
const DEVTOOLS_RESYNC_DELAYS_MS: &[u64] = &[32, 96, 192];
const FAVICON_RETRY_DELAY: Duration = Duration::from_secs(10);

fn devtools_debug_enabled() -> bool {
    std::env::var_os(DEVTOOLS_DEBUG_ENV)
        .map(|value| {
            let value = value.to_string_lossy();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

fn log_devtools(message: impl AsRef<str>) {
    if devtools_debug_enabled() {
        eprintln!("[ato-desktop][devtools-ui] {}", message.as_ref());
    }
}

fn format_bounds(bounds: PaneBounds) -> String {
    format!(
        "x={:.1} y={:.1} w={:.1} h={:.1}",
        bounds.x, bounds.y, bounds.width, bounds.height
    )
}

/// Query the local capsule registry for matching capsules.
/// Runs synchronously (designed to be called from a background thread).
fn search_local_registry(query: &str) -> Vec<crate::state::CapsuleSearchResult> {
    let encoded: String = query
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{:02X}", b),
        })
        .collect();
    let url = format!("http://127.0.0.1:8787/v1/capsules?q={encoded}");

    let response = match ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .get(&url)
        .call()
    {
        Ok(r) => r,
        Err(_) => return Vec::new(), // Registry not running
    };

    let body_str = match response.into_string() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let body: serde_json::Value = match serde_json::from_str(&body_str) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let Some(items) = body
        .as_array()
        .or_else(|| body.get("capsules").and_then(|v| v.as_array()))
    else {
        return Vec::new();
    };

    items
        .iter()
        .take(5)
        .filter_map(|item| {
            // CLI registry (`/v1/capsules`) returns `scoped_id` ("publisher/slug")
            // as the canonical handle; legacy mock catalogs used `handle`.
            // Display name comes from `name` (canonical) or `display_name` (legacy).
            let handle = item
                .get("scoped_id")
                .or_else(|| item.get("scopedId"))
                .or_else(|| item.get("handle"))
                .and_then(|v| v.as_str())?;
            let display_name = item
                .get("name")
                .or_else(|| item.get("display_name"))
                .and_then(|v| v.as_str())
                .unwrap_or(handle);
            Some(crate::state::CapsuleSearchResult {
                handle: handle.to_string(),
                display_name: display_name.to_string(),
                description: item
                    .get("description")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
            })
        })
        .collect()
}

/// Hit GitHub's `releases/latest` and compare its tag to the local
/// `CARGO_PKG_VERSION`. Returns the resulting [`UpdateCheck`] state
/// to be assigned to AppState by the render-loop poller. Runs
/// synchronously (call from a worker thread).
///
/// We do not implement semver-aware comparison — the `tag_name`
/// already mirrors `Cargo.toml`'s version verbatim, and a simple
/// string inequality is enough to detect "newer release exists".
/// This keeps us free of a `semver` crate dependency and matches
/// how cargo-dist labels its releases (`v0.4.97` ↔ `0.4.97`).
fn fetch_latest_release(current: &str) -> crate::state::UpdateCheck {
    const ENDPOINT: &str = "https://api.github.com/repos/ato-run/ato/releases/latest";
    let response = match ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .get(ENDPOINT)
        // GitHub's API requires a User-Agent and recommends an
        // explicit Accept header for stability across versions.
        .set("User-Agent", "ato-desktop-updater")
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(r) => r,
        Err(error) => {
            return crate::state::UpdateCheck::Failed {
                message: format!("network error: {error}"),
            }
        }
    };
    let body = match response.into_string() {
        Ok(b) => b,
        Err(error) => {
            return crate::state::UpdateCheck::Failed {
                message: format!("read error: {error}"),
            }
        }
    };
    let json: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(error) => {
            return crate::state::UpdateCheck::Failed {
                message: format!("parse error: {error}"),
            }
        }
    };
    let tag = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if tag.is_empty() {
        return crate::state::UpdateCheck::Failed {
            message: "release JSON missing tag_name".to_string(),
        };
    }
    let latest = tag.trim_start_matches('v').to_string();
    if latest == current {
        return crate::state::UpdateCheck::UpToDate {
            version: current.to_string(),
        };
    }
    let html_url = json
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("https://github.com/ato-run/ato/releases/tag/{tag}"));
    crate::state::UpdateCheck::Available { latest, html_url }
}

/// Hand a URL to the OS so the user's default browser opens it.
/// macOS/Linux/Windows fan-out — we only ship to those three so
/// that's the entire matrix. Errors bubble up so the caller can
/// surface them in the activity rail.
fn open_external_url(url: &str) -> std::io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    let status = command.arg(url).status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "browser-open exited with status {status}"
        )));
    }
    Ok(())
}

pub struct DesktopShell {
    state: AppState,
    omnibar: Entity<InputState>,
    /// Independent input owned by the new-tab Launcher panel — kept
    /// separate from the chrome omnibar so clicking the launcher
    /// search bar does NOT punt focus back up to the top of the
    /// window.
    launcher_search: Entity<InputState>,
    focus_handle: FocusHandle,
    favicon_cache: HashMap<String, FaviconState>,
    webviews: WebViewManager,
    terminal_sessions: TerminalSessionManager,
    open_url_bridge: Arc<OpenUrlBridge>,
    capsule_search_rx: Option<std::sync::mpsc::Receiver<Vec<crate::state::CapsuleSearchResult>>>,
    /// `ato login` child-process exit signal. Non-None while the
    /// CLI bridge auth flow is in progress; the inner bool is true
    /// on successful exit (CLI wrote credentials), false otherwise.
    cli_login_rx: Option<std::sync::mpsc::Receiver<bool>>,
    /// In-flight GitHub releases/latest fetch. The render loop polls
    /// this and writes the resulting UpdateCheck onto AppState.
    update_check_rx: Option<std::sync::mpsc::Receiver<crate::state::UpdateCheck>>,
    /// Per-capsule registry update results. The corresponding Sender lives
    /// inside `WebViewManager` (installed at startup) and a fresh worker
    /// thread is spawned every time a capsule pane successfully launches —
    /// see `WebViewManager::spawn_capsule_update_check`. The render loop
    /// drains this Receiver and writes the (PaneId, CapsuleUpdate) pairs
    /// into `AppState::capsule_updates`.
    capsule_update_rx: std::sync::mpsc::Receiver<(usize, crate::state::CapsuleUpdate)>,
    /// Lazy-allocated by `render` whenever `state.pending_config`
    /// flips from `None → Some` (or to a different request). Owns
    /// the per-field `InputState` entities so keystroke/cursor state
    /// survives across re-renders. Dropped when `pending_config`
    /// returns to `None`.
    config_modal: Option<modals::config_form::ConfigModal>,
    /// E302 consent modal. Read-only snapshot of
    /// `state.pending_consent`; rebuilt when the underlying request's
    /// identity tuple changes. Dropped when `pending_consent` returns
    /// to `None`.
    consent_modal: Option<modals::consent_form::ConsentModal>,
    /// #117 — unified pre-launch resolution modal. Reconciled against
    /// `state.pending_resolution` on every render: created on the
    /// `None → Some` transition, patched in place when new
    /// requirements are merged into the same `PendingResolutionRequest`,
    /// rebuilt only when the handle changes or a previously-input
    /// field disappears. Dropped when `pending_resolution` returns
    /// to `None`. Takes precedence over `config_modal` /
    /// `consent_modal` in the render gate so we never render the
    /// legacy single-slot overlays alongside the unified one.
    resolution_modal: Option<modals::resolution_form::ResolutionModal>,
}

impl DesktopShell {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        open_url_bridge: Arc<OpenUrlBridge>,
    ) -> Self {
        let mut state = crate::state::persistence::load_tabs().unwrap_or_else(AppState::initial);
        let focus_handle = cx.focus_handle();
        let omnibar = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("")
                .default_value(state.command_bar_text.clone())
        });
        let launcher_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search, command, or ask AI…"));
        ato_session_core::sweep::sweep_startup_runtime_artifacts_best_effort();
        match cleanup_stale_capsule_sessions() {
            Ok(notes) => {
                for note in notes {
                    state.push_activity(ActivityTone::Info, note);
                }
            }
            Err(error) => {
                state.push_activity(
                    ActivityTone::Warning,
                    format!("Failed to cleanup stale guest sessions: {error}"),
                );
            }
        }

        let mut webviews = WebViewManager::new(window.window_handle(), cx.to_async());
        // Expose the AutomationHost as a GPUI global so that other windows
        // (e.g. the dock) can clone it to register page-load handlers.
        cx.set_global(webviews.automation_host());
        // Channel for the per-pane capsule update check. The Sender goes
        // into the webview manager (cloned per worker thread); the
        // Receiver lives on this shell and is drained by
        // poll_capsule_updates on every render.
        let (capsule_update_tx, capsule_update_rx) = std::sync::mpsc::channel();
        webviews.install_capsule_update_channel(capsule_update_tx);
        let size = window.bounds().size;
        let stage = compute_stage_bounds(&state, f32::from(size.width), f32::from(size.height));
        state.set_active_bounds(stage);
        webviews.sync_from_state(window, &mut state);
        // Always start with the host focus_handle in the action
        // dispatch chain so rail clicks (NewTab / SelectTask /
        // CloseTask / MoveTask) reach DesktopShell on the very first
        // click. Without this, the active WebView owns the macOS
        // first responder and the inaugural button click is consumed
        // just to transfer focus, not run its action — users see
        // their first click do nothing, second click finally fire.
        // sync_focus_target will re-route the responder to the
        // active WebView the next time the user actually clicks
        // inside it.
        window.focus(&focus_handle, cx);
        let _ = webviews.wants_host_focus(&state); // kept for side effects

        cx.subscribe_in(
            &omnibar,
            window,
            |this: &mut Self, omnibar, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let url = omnibar.read(cx).value().to_string();
                    window.dispatch_action(Box::new(NavigateToUrl { url }), cx);
                }
                InputEvent::Change | InputEvent::Focus => {
                    this.sync_omnibar_with_state(window, cx, false);
                    cx.notify();
                }
                InputEvent::Blur => {
                    if matches!(this.state.shell_mode, ShellMode::CommandBar) {
                        this.state.dismiss_transient();
                        this.sync_focus_target(window, cx);
                    }
                    this.sync_omnibar_with_state(window, cx, false);
                    cx.notify();
                }
            },
        )
        .detach();

        cx.subscribe_in(
            &launcher_search,
            window,
            |_this: &mut Self, search, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let url = search.read(cx).value().to_string();
                    if !url.is_empty() {
                        window.dispatch_action(Box::new(NavigateToUrl { url }), cx);
                    }
                }
            },
        )
        .detach();

        cx.observe_window_bounds(window, |this, window, cx| {
            let size = window.bounds().size;
            let stage =
                compute_stage_bounds(&this.state, f32::from(size.width), f32::from(size.height));
            log_devtools(format!(
                "window_bounds_changed size=({:.1},{:.1}) stage={} shell_mode={:?}",
                f32::from(size.width),
                f32::from(size.height),
                format_bounds(stage),
                this.state.shell_mode
            ));
            this.state.set_active_bounds(stage);
            this.webviews.sync_from_state(window, &mut this.state);
            this.sync_omnibar_with_state(window, cx, false);
            cx.notify();
        })
        .detach();

        Self {
            state,
            omnibar,
            launcher_search,
            focus_handle,
            favicon_cache: HashMap::new(),
            webviews,
            terminal_sessions: TerminalSessionManager::new(),
            open_url_bridge,
            capsule_search_rx: None,
            cli_login_rx: None,
            update_check_rx: None,
            capsule_update_rx,
            config_modal: None,
            consent_modal: None,
            resolution_modal: None,
        }
    }

    /// Trigger an async capsule search if the omnibar text changed and is non-empty.
    fn maybe_trigger_capsule_search(&mut self, query: &str) {
        let trimmed = query.trim();
        if trimmed == self.state.capsule_search_query {
            return;
        }
        self.state.capsule_search_query = trimmed.to_string();

        if trimmed.is_empty() || trimmed.len() < 2 {
            self.state.capsule_search_results.clear();
            self.capsule_search_rx = None;
            return;
        }

        // Skip if it looks like a URL
        if trimmed.starts_with("http://")
            || trimmed.starts_with("https://")
            || trimmed.contains("://")
        {
            self.state.capsule_search_results.clear();
            self.capsule_search_rx = None;
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel();
        self.capsule_search_rx = Some(rx);

        let query_str = trimmed.to_string();
        std::thread::spawn(move || {
            let results = search_local_registry(&query_str);
            let _ = tx.send(results);
        });
    }

    /// Poll for capsule search results from the background thread.
    fn poll_capsule_search(&mut self) {
        if let Some(ref rx) = self.capsule_search_rx {
            if let Ok(results) = rx.try_recv() {
                self.state.capsule_search_results = results;
                self.capsule_search_rx = None;
            }
        }
    }

    /// Drain the `ato login` child-process exit signal. On successful
    /// exit the CLI credential store now holds a session token; we
    /// trigger handle_host_route with a synthetic cloud-dock callback
    /// so the existing verification + cookie-injection path runs.
    fn poll_cli_login(&mut self) {
        let ok = match self.cli_login_rx.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(ok)) => ok,
            _ => return,
        };
        self.cli_login_rx = None;
        if ok {
            self.state
                .handle_host_route("ato://auth/callback/cloud-dock");
        } else {
            self.state.push_activity(
                crate::state::ActivityTone::Warning,
                "ato login exited without completing sign-in.",
            );
        }
    }

    /// Drain the GitHub releases/latest fetch result and write it to
    /// AppState so the Updates card re-renders with the new status.
    fn poll_update_check(&mut self) {
        let Some(rx) = self.update_check_rx.as_ref() else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        self.update_check_rx = None;
        self.state.update_check = result;
    }

    /// Drain pending per-pane capsule update results posted by the worker
    /// threads spawned in `WebViewManager::spawn_capsule_update_check`.
    /// Multiple results can land in a single render frame (one per pane that
    /// just settled), so we loop until the channel is empty.
    fn poll_capsule_updates(&mut self) {
        while let Ok((pane_id, update)) = self.capsule_update_rx.try_recv() {
            self.state.capsule_updates.insert(pane_id, update);
        }
    }

    fn on_check_for_updates(
        &mut self,
        _: &CheckForUpdates,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Idempotent: ignore re-entry while a fetch is already in flight.
        if self.update_check_rx.is_some()
            || matches!(self.state.update_check, crate::state::UpdateCheck::Checking)
        {
            return;
        }
        self.state.update_check = crate::state::UpdateCheck::Checking;
        let (tx, rx) = std::sync::mpsc::channel();
        self.update_check_rx = Some(rx);
        let current = env!("CARGO_PKG_VERSION").to_string();
        std::thread::spawn(move || {
            let result = fetch_latest_release(&current);
            let _ = tx.send(result);
        });
        cx.notify();
    }

    fn on_open_latest_release_page(
        &mut self,
        _: &OpenLatestReleasePage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let crate::state::UpdateCheck::Available { html_url, .. } = &self.state.update_check {
            if let Err(error) = open_external_url(html_url) {
                self.state.push_activity(
                    crate::state::ActivityTone::Error,
                    format!("Failed to open release page: {error}"),
                );
                cx.notify();
            }
        }
    }

    fn on_open_external_link(
        &mut self,
        action: &OpenExternalLink,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = open_external_url(&action.url) {
            self.state.push_activity(
                crate::state::ActivityTone::Error,
                format!("Failed to open {}: {error}", action.url),
            );
            cx.notify();
        }
    }

    /// Click handler for the popover's Install-update button. Closes the
    /// modal and dispatches NavigateToUrl with the precomputed
    /// `target_handle` (e.g. `capsule://...@latest`). The existing
    /// `webview::sync_from_state` route-change pipeline tears down the
    /// running session and `ato app session start` lazily installs the
    /// new version, so no extra install plumbing is needed here.
    fn on_install_capsule_update(
        &mut self,
        action: &InstallCapsuleUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Drop the active pane's banner immediately so the modal feels
        // responsive — the worker thread will repopulate it after the new
        // session settles. Stale `Available` would otherwise flicker until
        // the next check completes.
        if let Some(active) = self.state.active_capsule_pane() {
            self.state.capsule_updates.remove(&active.pane_id);
        }
        self.state.route_metadata_popover_open = false;
        let url = action.url.clone();
        window.dispatch_action(Box::new(NavigateToUrl { url }), cx);
        cx.notify();
    }

    fn on_toggle_theme(&mut self, _: &ToggleTheme, _window: &mut Window, cx: &mut Context<Self>) {
        self.state.toggle_theme();
        cx.notify();
    }

    fn on_toggle_auto_devtools(
        &mut self,
        _: &ToggleAutoDevtools,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.state.config.developer.auto_open_devtools;
        self.state
            .update_config(|c| c.developer.auto_open_devtools = !current);
        cx.notify();
    }

    fn on_focus_command_bar(
        &mut self,
        _: &FocusCommandBar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.focus_command_bar();
        self.sync_omnibar_with_state(window, cx, true);
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_next_workspace(
        &mut self,
        _: &NextWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.next_workspace();
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_previous_workspace(
        &mut self,
        _: &PreviousWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.previous_workspace();
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.state.create_new_tab();
        crate::state::persistence::save_tabs(&self.state);
        self.sync_omnibar_with_state(window, cx, false);
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_show_settings(&mut self, _: &ShowSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.state.settings_panel_open = false;
        self.state.open_settings_task();
        crate::state::persistence::save_tabs(&self.state);
        self.sync_omnibar_with_state(window, cx, true);
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    /// RFC: SURFACE_CLOSE_SEMANTICS §6.1 / §6.2 / §6.3 — explicit
    /// "Stop session" for the active pane. Called from omnibar
    /// suggestion + Cmd+Shift+W shortcut. The stop is synchronous
    /// because the user actively asked; failure surfaces as an
    /// activity entry.
    fn on_stop_active_session(
        &mut self,
        _: &crate::app::StopActiveSession,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.webviews.stop_active_session(&mut self.state);
        cx.notify();
    }

    /// RFC: SURFACE_CLOSE_SEMANTICS §6.2 — drain the retention table
    /// and graceful-stop every entry in the background. Active panes
    /// (visible to the user) are untouched: only sessions that were
    /// kept warm by recent pane-close events get stopped.
    fn on_stop_all_retained_sessions(
        &mut self,
        _: &crate::app::StopAllRetainedSessions,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stopped = self.webviews.stop_all_retained_sessions();
        self.state.retention_count = self.webviews.retention_count();
        self.state.push_activity(
            crate::state::ActivityTone::Info,
            format!("Stopping {stopped} retained session(s) in the background"),
        );
        tracing::info!(stopped, "stop_all_retained_sessions dispatched");
        cx.notify();
    }

    fn on_select_settings_tab(
        &mut self,
        action: &SelectSettingsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.set_settings_tab(action.tab);
        crate::state::persistence::save_tabs(&self.state);
        self.sync_omnibar_with_state(window, cx, false);
        cx.notify();
    }

    fn on_toggle_dev_console(
        &mut self,
        _: &ToggleDevConsole,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let size = window.bounds().size;
        let stage_bounds =
            compute_stage_bounds(&self.state, f32::from(size.width), f32::from(size.height));
        let active = self.state.active_web_pane();
        log_devtools(format!(
            "toggle_devtools shell_mode={:?} window=({:.1},{:.1}) stage={} active_pane={} active_route={}",
            self.state.shell_mode,
            f32::from(size.width),
            f32::from(size.height),
            format_bounds(stage_bounds),
            active.as_ref().map(|pane| pane.pane_id.to_string()).unwrap_or_else(|| "<none>".to_string()),
            active
                .as_ref()
                .map(|pane| pane.route.to_string())
                .unwrap_or_else(|| "<none>".to_string())
        ));

        // Clear any lingering GPUI DevConsole pane — native Safari Web Inspector is used instead.
        self.state.dismiss_dev_console();
        self.webviews.open_devtools_for_active_pane(&mut self.state);
        self.schedule_devtools_resyncs(window, cx);
        cx.notify();
    }

    fn schedule_devtools_resyncs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for delay_ms in DEVTOOLS_RESYNC_DELAYS_MS {
            cx.spawn_in(
                window,
                {
                    let delay = Duration::from_millis(*delay_ms);
                    move |this: WeakEntity<Self>, cx: &mut AsyncWindowContext| {
                        let mut async_cx = cx.clone();
                        async move {
                            async_cx.background_executor().timer(delay).await;
                            let _ = this.update_in(&mut async_cx, move |this, window, cx| {
                                let size = window.bounds().size;
                                let stage = compute_stage_bounds(
                                    &this.state,
                                    f32::from(size.width),
                                    f32::from(size.height),
                                );
                                log_devtools(format!(
                                    "scheduled_resync delay_ms={} window=({:.1},{:.1}) stage={} shell_mode={:?}",
                                    delay_ms,
                                    f32::from(size.width),
                                    f32::from(size.height),
                                    format_bounds(stage),
                                    this.state.shell_mode
                                ));
                                this.state.set_active_bounds(stage);
                                this.webviews.sync_from_state(window, &mut this.state);
                                this.sync_omnibar_with_state(window, cx, false);
                                cx.notify();
                            });
                        }
                    }
                },
            )
            .detach();
        }
    }

    fn on_select_task(&mut self, action: &SelectTask, window: &mut Window, cx: &mut Context<Self>) {
        self.state.select_task(action.task_id);
        crate::state::persistence::save_tabs(&self.state);
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_close_task(&mut self, action: &CloseTask, window: &mut Window, cx: &mut Context<Self>) {
        let pruned = self.state.close_task(action.task_id);
        if !pruned.is_empty() {
            self.webviews.prune_panes(&pruned, &mut self.state);
        }
        // sync_from_state needs to run so the active webview matches
        // the new active_task (or detaches when the workspace is empty).
        self.webviews.sync_from_state(window, &mut self.state);
        crate::state::persistence::save_tabs(&self.state);
        self.sync_omnibar_with_state(window, cx, false);
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_move_task(&mut self, action: &MoveTask, _window: &mut Window, cx: &mut Context<Self>) {
        self.state.move_task(action.task_id, action.to_index);
        crate::state::persistence::save_tabs(&self.state);
        cx.notify();
    }

    fn on_navigate_to_url(
        &mut self,
        action: &NavigateToUrl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.navigate_to_url(&action.url);
        crate::state::persistence::save_tabs(&self.state);
        self.sync_omnibar_with_state(window, cx, true);
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_previous_task(&mut self, _: &PreviousTask, window: &mut Window, cx: &mut Context<Self>) {
        self.state.previous_task();
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_next_task(&mut self, _: &NextTask, window: &mut Window, cx: &mut Context<Self>) {
        self.state.next_task();
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_split_pane(&mut self, _: &SplitPane, window: &mut Window, cx: &mut Context<Self>) {
        self.state.split_pane();
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_expand_split(&mut self, _: &ExpandSplit, window: &mut Window, cx: &mut Context<Self>) {
        self.state.expand_split();
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_shrink_split(&mut self, _: &ShrinkSplit, window: &mut Window, cx: &mut Context<Self>) {
        self.state.shrink_split();
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_dismiss_transient(
        &mut self,
        _: &DismissTransient,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.dismiss_transient();
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_toggle_route_metadata_popover(
        &mut self,
        _: &ToggleRouteMetadataPopover,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.toggle_route_metadata_popover();
        cx.notify();
    }

    fn on_select_route_metadata_tab(
        &mut self,
        action: &SelectRouteMetadataTab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.set_route_metadata_tab(action.tab);
        cx.notify();
    }

    fn on_quit(&mut self, _: &Quit, _window: &mut Window, cx: &mut Context<Self>) {
        // Surface the keep-or-clear dialog instead of quitting
        // straight away; ConfirmQuitKeep / ConfirmQuitClear /
        // CancelQuit resolve the prompt.
        self.state.pending_quit_confirmation = true;
        cx.notify();
    }

    fn on_cancel_quit(&mut self, _: &CancelQuit, _window: &mut Window, cx: &mut Context<Self>) {
        self.state.pending_quit_confirmation = false;
        cx.notify();
    }

    fn on_cycle_handle(&mut self, _: &CycleHandle, window: &mut Window, cx: &mut Context<Self>) {
        self.state.cycle_handle();
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_browser_back(&mut self, _: &BrowserBack, _: &mut Window, cx: &mut Context<Self>) {
        self.state.browser_back();
        cx.notify();
    }

    fn on_browser_forward(&mut self, _: &BrowserForward, _: &mut Window, cx: &mut Context<Self>) {
        self.state.browser_forward();
        cx.notify();
    }

    fn on_browser_reload(&mut self, _: &BrowserReload, _: &mut Window, cx: &mut Context<Self>) {
        self.state.browser_reload();
        cx.notify();
    }

    fn on_native_undo(&mut self, _: &NativeUndo, _: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn on_native_redo(&mut self, _: &NativeRedo, _: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn on_native_cut(&mut self, _: &NativeCut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.webviews.wants_host_focus(&self.state) {
            let _ = self.webviews.delegate_copy(&self.state, true);
        }
        cx.notify();
    }

    fn on_native_copy(&mut self, _: &NativeCopy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.webviews.wants_host_focus(&self.state) {
            let _ = self.webviews.delegate_copy(&self.state, false);
        }
        cx.notify();
    }

    fn on_native_paste(&mut self, _: &NativePaste, _: &mut Window, cx: &mut Context<Self>) {
        if !self.webviews.wants_host_focus(&self.state) {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                let _ = self.webviews.delegate_paste(&self.state, &text);
            }
        }
        cx.notify();
    }

    fn on_native_select_all(
        &mut self,
        _: &NativeSelectAll,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.webviews.wants_host_focus(&self.state) {
            let _ = self.webviews.delegate_select_all(&self.state);
        }
        cx.notify();
    }

    fn on_open_auth_in_browser(
        &mut self,
        _: &OpenAuthInBrowser,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Sign in via the CLI's bridge auth flow (PKCE + browser +
        // /v1/auth/bridge/poll + /exchange). The CLI process opens
        // the system browser, where the user has full passkey / MFA
        // support, then writes the resulting session token into the
        // CLI credential store. After that finishes,
        // verify_cli_ato_session can hand the token to the Dock
        // WebView and we never need to embed OAuth providers in our
        // WKWebView.
        let Some(sid) = self.find_active_auth_handoff_pane_id().and_then(|pane_id| {
            self.state.active_panes().iter().find_map(|p| {
                if p.id != pane_id {
                    return None;
                }
                let PaneSurface::AuthHandoff { session_id, .. } = &p.surface else {
                    return None;
                };
                Some(session_id.clone())
            })
        }) else {
            cx.notify();
            return;
        };
        if let Some(s) = self
            .state
            .auth_sessions
            .iter_mut()
            .find(|s| s.session_id == sid)
        {
            s.status = AuthSessionStatus::OpenedInBrowser;
        }

        let ato_bin = match crate::orchestrator::resolve_ato_binary() {
            Ok(path) => path,
            Err(error) => {
                self.state.push_activity(
                    crate::state::ActivityTone::Error,
                    format!("Could not locate ato binary for sign-in: {error}"),
                );
                cx.notify();
                return;
            }
        };
        // Spawn `ato login` non-blocking — the CLI prints a URL,
        // opens the browser, and polls /v1/auth/bridge/poll. When
        // it exits successfully the credential store on disk has the
        // session token. We watch the child from a thread and forward
        // the exit status back to the render loop via cli_login_rx;
        // poll_cli_login() then drives complete_ato_login on success.
        match std::process::Command::new(&ato_bin).arg("login").spawn() {
            Ok(mut child) => {
                self.state.push_activity(
                    crate::state::ActivityTone::Info,
                    "Started ato login. Complete sign-in in your browser.",
                );
                let (tx, rx) = std::sync::mpsc::channel();
                self.cli_login_rx = Some(rx);
                std::thread::spawn(move || {
                    let ok = child.wait().map(|status| status.success()).unwrap_or(false);
                    let _ = tx.send(ok);
                });
            }
            Err(error) => {
                self.state.push_activity(
                    crate::state::ActivityTone::Error,
                    format!("Failed to start ato login: {error}"),
                );
            }
        }
        self.sync_omnibar_with_state(window, cx, true);
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_open_local_registry(
        &mut self,
        _: &OpenLocalRegistry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.open_local_registry();
        self.sync_omnibar_with_state(window, cx, true);
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_open_cloud_dock(
        &mut self,
        _: &OpenCloudDock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.open_cloud_dock();
        self.sync_omnibar_with_state(window, cx, true);
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_sign_in_to_ato_run(
        &mut self,
        _: &SignInToAtoRun,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.open_cloud_dock();
        self.sync_omnibar_with_state(window, cx, true);
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_sign_out(&mut self, _: &SignOut, _window: &mut Window, cx: &mut Context<Self>) {
        self.state.sign_out();
        cx.notify();
    }

    fn on_cancel_auth_handoff(
        &mut self,
        _: &CancelAuthHandoff,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane_id) = self.find_active_auth_handoff_pane_id() {
            self.state.cancel_auth_handoff(pane_id);
            self.sync_focus_target(window, cx);
        }
        cx.notify();
    }

    fn on_resume_after_auth(
        &mut self,
        _: &ResumeAfterAuth,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane_id) = self.find_active_auth_handoff_pane_id() {
            self.state.resume_after_auth(pane_id);
            self.sync_focus_target(window, cx);
        }
        cx.notify();
    }

    fn on_allow_permission_once(
        &mut self,
        _: &AllowPermissionOnce,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.allow_permission_once();
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_allow_permission_for_session(
        &mut self,
        _: &AllowPermissionForSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.allow_permission_for_session();
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn on_deny_permission_prompt(
        &mut self,
        _: &DenyPermissionPrompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.deny_permission_prompt();
        self.sync_focus_target(window, cx);
        cx.notify();
    }

    fn find_active_auth_handoff_pane_id(&self) -> Option<PaneId> {
        self.state
            .active_panes()
            .iter()
            .find(|p| matches!(p.surface, PaneSurface::AuthHandoff { .. }))
            .map(|p| p.id)
    }

    fn sync_omnibar_with_state(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        force: bool,
    ) {
        if !force && matches!(self.state.shell_mode, ShellMode::CommandBar) {
            return;
        }

        let next = self.state.command_bar_text.clone();
        let current = self.omnibar.read(cx).value().to_string();
        if current == next {
            return;
        }

        self.omnibar.update(cx, |omnibar, cx| {
            omnibar.set_value(next, window, cx);
        });
    }

    /// Reconcile `self.config_modal` with `state.pending_config`.
    ///
    /// Called once per render pass before child elements are built, so
    /// `.when(self.config_modal.is_some())` further down sees a fresh
    /// view of the world. The modal is the *render projection* of
    /// `pending_config`; AppState is authoritative for "should the
    /// modal exist," this method just owns the local `InputState`
    /// allocations needed to actually paint it.
    fn sync_config_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match (&self.state.pending_config, &self.config_modal) {
            (None, None) => {}
            (None, Some(_)) => {
                // Pending request was cleared (Save / Cancel) — drop
                // the modal so its `InputState` entities are freed.
                self.config_modal = None;
            }
            (Some(req), None) => {
                self.config_modal = Some(modals::config_form::ConfigModal::new(
                    req.clone(),
                    window,
                    cx,
                ));
            }
            (Some(req), Some(modal)) => {
                if modal.should_rebuild_for(req) {
                    // Schema or capsule changed under us — rebuild
                    // wholesale rather than reconcile fields. Mid-
                    // session schema flux is rare; correctness wins
                    // over preserving stale input.
                    self.config_modal = Some(modals::config_form::ConfigModal::new(
                        req.clone(),
                        window,
                        cx,
                    ));
                }
            }
        }
    }

    /// Reconcile `self.consent_modal` with `state.pending_consent`.
    /// Mirror of `sync_config_modal` for the E302 consent flow. The
    /// modal is read-only (no `InputState`), so the snapshot is just
    /// the request itself; we still rebuild on identity drift so the
    /// rendered hashes never lag behind the live request.
    fn sync_consent_modal(&mut self) {
        match (&self.state.pending_consent, &self.consent_modal) {
            (None, None) => {}
            (None, Some(_)) => {
                self.consent_modal = None;
            }
            (Some(req), None) => {
                self.consent_modal = Some(modals::consent_form::ConsentModal::new(req.clone()));
            }
            (Some(req), Some(modal)) => {
                if modal.needs_rebuild(req) {
                    self.consent_modal = Some(modals::consent_form::ConsentModal::new(req.clone()));
                }
            }
        }
    }

    /// #117 — reconcile `self.resolution_modal` with
    /// `state.pending_resolution`. Differs from the legacy modal sync
    /// in that we *patch* the existing modal in place when new
    /// requirements are merged in (preserving keystroke state for
    /// already-shown secret fields), and only rebuild wholesale when
    /// the handle changes or a previously-shown field disappears.
    fn sync_resolution_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match (
            &self.state.pending_resolution,
            self.resolution_modal.as_mut(),
        ) {
            (None, None) => {}
            (None, Some(_)) => {
                self.resolution_modal = None;
            }
            (Some(req), None) => {
                self.resolution_modal = Some(modals::resolution_form::ResolutionModal::new(
                    req.clone(),
                    window,
                    cx,
                ));
            }
            (Some(req), Some(modal)) => {
                if modal.should_rebuild_for(req) {
                    self.resolution_modal = Some(modals::resolution_form::ResolutionModal::new(
                        req.clone(),
                        window,
                        cx,
                    ));
                } else {
                    modal.merge_inputs_for(req.clone(), window, cx);
                }
            }
        }
    }

    /// Handler for `ApproveConsentForm`. Calls
    /// `apply_capsule_consent` (which routes through
    /// `ato internal consent approve-execution-plan` on the CLI) and
    /// — on success — clears `pending_consent`, marks the per-handle
    /// retry budget consumed, and lets the next render re-arm the
    /// launch via `ensure_pending_local_launch`. On failure, the modal
    /// stays open with an activity entry; the user can click Approve
    /// again to retry the CLI call.
    fn on_approve_consent_form(
        &mut self,
        _: &ApproveConsentForm,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(modal) = self.consent_modal.as_ref() else {
            return;
        };
        let handle = modal.request.handle.clone();
        match crate::webview::apply_capsule_consent(&mut self.state, &handle) {
            Ok(()) => {
                self.state.push_activity(
                    ActivityTone::Info,
                    format!("Approved ExecutionPlan consent for {handle}; relaunching…"),
                );
            }
            Err(message) => {
                self.state.push_activity(
                    ActivityTone::Error,
                    format!("Failed to record consent for {handle}: {message}"),
                );
                // Leave modal open so the user can retry.
                return;
            }
        }
        cx.notify();
    }

    /// Handler for `CancelConsentForm`. Drops the pending consent and
    /// marks the active web pane as `LaunchFailed` so
    /// `ensure_pending_local_launch` won't immediately re-fire and
    /// re-trip the same E302. The user reopens the launch by
    /// re-entering the handle in the omnibar — Cancel does NOT count
    /// against the retry-once budget.
    fn on_cancel_consent_form(
        &mut self,
        _: &CancelConsentForm,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(modal) = self.consent_modal.as_ref() else {
            return;
        };
        let handle = modal.request.handle.clone();
        self.state.clear_pending_consent();
        self.state.reset_consent_retry_budget(&handle);
        if let Some(active) = self.state.active_web_pane() {
            let pane_id = active.pane_id;
            self.state
                .sync_web_session_state(pane_id, crate::state::WebSessionState::LaunchFailed);
        }
        self.state.push_activity(
            ActivityTone::Info,
            format!("Cancelled ExecutionPlan consent for {handle}."),
        );
        cx.notify();
    }

    /// Handler for the `SaveConfigForm` action emitted by the modal's
    /// "Save & Launch" button. Walks the form, persists each field
    /// according to its kind, clears `pending_config`, and lets the
    /// next render pass re-arm the launch via
    /// `ensure_pending_local_launch` (which is gated on
    /// `pending_config.is_none()` for the same handle).
    fn on_save_config_form(
        &mut self,
        _: &SaveConfigForm,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(modal) = self.config_modal.as_ref() else {
            return;
        };

        // Snapshot what we need before mutating AppState — `inputs`
        // entities are read against `cx` and we'll need the `request`
        // payload to grant the right capsule.
        let handle = modal.request.handle.clone();
        let mut secret_writes: Vec<(String, String)> = Vec::new();
        let mut config_writes: Vec<(String, String)> = Vec::new();
        for field in &modal.request.fields {
            let Some(input) = modal.inputs.get(&field.name) else {
                continue;
            };
            let value = input.read(cx).value().to_string();
            match &field.kind {
                ConfigKind::Secret => {
                    if value.is_empty() {
                        // Empty secret = the user left it blank. Save
                        // would just store an empty value and the
                        // retry would fail preflight again. Bail
                        // early so the modal stays visible; Day 7
                        // adds in-modal validation messaging.
                        return;
                    }
                    secret_writes.push((field.name.clone(), value));
                }
                ConfigKind::String | ConfigKind::Number => {
                    if value.is_empty() {
                        // Same logic as secrets: storing an empty
                        // string would just round-trip into an empty
                        // env var, which preflight rejects. Halt the
                        // save so the user can fill it in.
                        return;
                    }
                    config_writes.push((field.name.clone(), value));
                }
                ConfigKind::Enum { choices } => {
                    if value.is_empty() {
                        return;
                    }
                    // Defensive: the InputState is a free-text field
                    // (the dropdown lands in Day 6), so the user
                    // *can* type something outside `choices`. Reject
                    // here rather than write a value that the
                    // capsule will refuse anyway.
                    if !choices.iter().any(|c| c == &value) {
                        self.state.push_activity(
                            ActivityTone::Warning,
                            format!(
                                "'{value}' is not a valid choice for {}. Allowed: {}",
                                field.name,
                                choices.join(", ")
                            ),
                        );
                        return;
                    }
                    config_writes.push((field.name.clone(), value));
                }
            }
        }

        // Persist secrets and grant them to the capsule. The grant is
        // mandatory: `secrets_for_capsule(handle)` filters by grant,
        // so without it the retry would launch with an empty
        // `ATO_SECRET_*` env and trip the same E103.
        //
        // #57: surface persist failures to the activity bus instead of
        // silently swallowing them — without this, the UI would proceed
        // to "relaunching..." while the secret was never written and
        // the next launch would trip E103 again with no explanation.
        for (key, value) in secret_writes {
            if let Err(error) = self.state.add_secret(key.clone(), value) {
                self.state.push_activity(
                    crate::state::ActivityTone::Error,
                    format!("Failed to save secret '{key}': {error}"),
                );
                return;
            }
            if let Err(error) = self.state.grant_secret_to_capsule(&handle, &key) {
                self.state.push_activity(
                    crate::state::ActivityTone::Error,
                    format!("Failed to grant secret '{key}' to {handle}: {error}"),
                );
                return;
            }
        }

        // Persist non-secret config under the same handle. There's
        // no grant table here — the value is scoped to the capsule
        // by being keyed under its handle in `CapsuleConfigStore`.
        for (key, value) in config_writes {
            self.state.add_capsule_config(&handle, key, value);
        }

        self.state.clear_pending_config();
        self.state.push_activity(
            ActivityTone::Info,
            format!("Saved configuration; relaunching {handle}…"),
        );
        cx.notify();
    }

    /// Handler for `CancelConfigForm`. Drops the pending request and
    /// marks the active web pane as `LaunchFailed` so
    /// `ensure_pending_local_launch` won't immediately re-fire and
    /// re-trip the same E103. The user reopens the launch by
    /// re-entering the handle in the omnibar.
    fn on_cancel_config_form(
        &mut self,
        _: &CancelConfigForm,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(modal) = self.config_modal.as_ref() else {
            return;
        };
        let handle = modal.request.handle.clone();
        self.state.clear_pending_config();
        if let Some(active) = self.state.active_web_pane() {
            let pane_id = active.pane_id;
            self.state
                .sync_web_session_state(pane_id, crate::state::WebSessionState::LaunchFailed);
        }
        self.state.push_activity(
            ActivityTone::Info,
            format!("Cancelled configuration for {handle}."),
        );
        cx.notify();
    }

    /// #117 — handler for `SubmitResolutionForm`. Persists every
    /// secret in `pending_resolution.secrets`, approves every consent
    /// in `pending_resolution.consents`, then clears the unified
    /// pending request so `ensure_pending_local_launch` re-arms the
    /// launch on the next render. Iteration order: secrets first,
    /// then consents — secrets are runtime input that the consents'
    /// ExecutionPlans expect to see in env, but the CLI rederives
    /// plans on retry so the order is informational here, not load-
    /// bearing.
    ///
    /// On the first error (a secret that fails to persist, or a CLI
    /// invocation that fails to record consent), we surface an
    /// activity entry and bail — the modal stays open with the
    /// remaining work intact so the user can retry without losing the
    /// secrets / approvals that already succeeded.
    fn on_submit_resolution_form(
        &mut self,
        _: &SubmitResolutionForm,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(modal) = self.resolution_modal.as_ref() else {
            return;
        };
        let request = modal.request.clone();
        let handle = request.handle.clone();

        // Snapshot everything we need before mutating AppState, since
        // the submit path borrows `cx` to read input values.
        struct PendingSecretWrite {
            target: Option<String>,
            field_name: String,
            kind: ConfigKind,
            value: String,
        }
        let mut secret_writes: Vec<PendingSecretWrite> = Vec::new();
        for item in &request.secrets {
            for field in &item.fields {
                let Some(value) = modal.read_input(item.target.as_deref(), &field.name, cx) else {
                    continue;
                };
                if value.is_empty() {
                    // Same gating as the legacy save handler: an empty
                    // value would round-trip into an empty env var and
                    // fail preflight again. Halt so the user can fill
                    // in the field rather than dismissing the modal.
                    return;
                }
                if let ConfigKind::Enum { choices } = &field.kind {
                    if !choices.iter().any(|c| c == &value) {
                        self.state.push_activity(
                            ActivityTone::Warning,
                            format!(
                                "'{value}' is not a valid choice for {}. Allowed: {}",
                                field.name,
                                choices.join(", ")
                            ),
                        );
                        return;
                    }
                }
                secret_writes.push(PendingSecretWrite {
                    target: item.target.clone(),
                    field_name: field.name.clone(),
                    kind: field.kind.clone(),
                    value,
                });
            }
        }

        // Persist secrets and grant them to the capsule. Same as the
        // legacy save handler — secret-typed fields go to the
        // SecretStore + grant table; non-secret config goes to the
        // capsule-config map keyed by handle. The `target` is purely
        // informational at the schema level; the secret store key is
        // the field name.
        for write in secret_writes {
            match write.kind {
                ConfigKind::Secret => {
                    if let Err(error) = self.state.add_secret(write.field_name.clone(), write.value)
                    {
                        self.state.push_activity(
                            ActivityTone::Error,
                            format!("Failed to save secret '{}': {error}", write.field_name),
                        );
                        return;
                    }
                    if let Err(error) = self
                        .state
                        .grant_secret_to_capsule(&handle, &write.field_name)
                    {
                        self.state.push_activity(
                            ActivityTone::Error,
                            format!(
                                "Failed to grant secret '{}' to {handle}: {error}",
                                write.field_name
                            ),
                        );
                        return;
                    }
                }
                ConfigKind::String | ConfigKind::Number | ConfigKind::Enum { .. } => {
                    self.state
                        .add_capsule_config(&handle, write.field_name, write.value);
                }
            }
            // `target` is intentionally unused at write time — the
            // schema groups by target for display, but the SecretStore
            // and CapsuleConfigStore key on (handle, name).
            let _ = write.target;
        }

        // Approve every consent item via the same CLI plumbing the
        // legacy modal's Approve button uses. We call the orchestrator
        // helper directly with each item's identity tuple instead of
        // routing through `apply_capsule_consent` (which pulls from
        // the legacy single-slot `pending_consent`).
        for consent in &request.consents {
            if let Err(error) = crate::orchestrator::approve_execution_plan_consent(
                &consent.scoped_id,
                &consent.version,
                &consent.target_label,
                &consent.policy_segment_hash,
                &consent.provisioning_policy_hash,
            ) {
                self.state.push_activity(
                    ActivityTone::Error,
                    format!(
                        "Failed to record consent for target {}: {error:#}",
                        consent.target_label
                    ),
                );
                return;
            }
            self.state
                .mark_consent_retry_consumed(&handle, &consent.target_label);
        }

        let secret_count: usize = request.secrets.iter().map(|s| s.fields.len()).sum();
        let consent_count = request.consents.len();
        self.state.clear_pending_resolution();
        self.state.push_activity(
            ActivityTone::Info,
            format!(
                "Approved {} secret{} and {} ExecutionPlan{}; relaunching {handle}…",
                secret_count,
                if secret_count == 1 { "" } else { "s" },
                consent_count,
                if consent_count == 1 { "" } else { "s" },
            ),
        );
        cx.notify();
    }

    /// #117 step UI — advance from consent review to secrets entry.
    /// No-op when the modal is already on the secrets step or when
    /// the request has no secrets to advance to (single-step mode,
    /// where the Submit button is the right action). The mutation is
    /// on the modal's local state only — `pending_resolution` itself
    /// is unchanged so a concurrent merge-in still lands cleanly.
    fn on_resolution_form_next(
        &mut self,
        _: &ResolutionFormNext,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(modal) = self.resolution_modal.as_mut() {
            modal.advance_step();
            cx.notify();
        }
    }

    /// #117 step UI — go back from secrets entry to consent review.
    /// No-op when the request has no consents (single-step mode).
    /// Input state for the secrets form is preserved across the
    /// round-trip via the existing `inputs` map, so users don't lose
    /// keystrokes by stepping back to re-read a policy summary.
    fn on_resolution_form_back(
        &mut self,
        _: &ResolutionFormBack,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(modal) = self.resolution_modal.as_mut() {
            modal.retreat_step();
            cx.notify();
        }
    }

    /// #117 — handler for `CancelResolutionForm`. Drops the unified
    /// pending request and marks the active web pane `LaunchFailed`
    /// so `ensure_pending_local_launch` doesn't immediately re-trip
    /// the same requirements. The user re-opens the launch via the
    /// omnibar.
    fn on_cancel_resolution_form(
        &mut self,
        _: &CancelResolutionForm,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(modal) = self.resolution_modal.as_ref() else {
            return;
        };
        let handle = modal.request.handle.clone();
        self.state.clear_pending_resolution();
        if let Some(active) = self.state.active_web_pane() {
            let pane_id = active.pane_id;
            self.state
                .sync_web_session_state(pane_id, crate::state::WebSessionState::LaunchFailed);
        }
        self.state.push_activity(
            ActivityTone::Info,
            format!("Cancelled launch resolution for {handle}."),
        );
        cx.notify();
    }

    fn sync_favicons(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Two icon families share the cache: external-URL favicons
        // (key = origin, request URL = "{origin}/favicon.ico") and
        // capsule-manifest icons (key = absolute path or full URL,
        // request = the key itself). Same `Arc<Image>` cache, two
        // dispatches in `spawn_favicon_fetch`. Failed fetches are
        // retried after a short backoff so transient network / file
        // timing issues do not permanently strand the rail icon.
        let sources = self
            .state
            .sidebar_task_items()
            .into_iter()
            .filter_map(|task| match task.icon {
                SidebarTaskIconSpec::ExternalUrl { origin } => Some((origin, IconKind::Favicon)),
                SidebarTaskIconSpec::Image { source } => Some((source, IconKind::Direct)),
                SidebarTaskIconSpec::Monogram(_) | SidebarTaskIconSpec::SystemIcon(_) => None,
            })
            .collect::<Vec<_>>();
        let now = std::time::Instant::now();

        for (key, kind) in sources {
            // Carry the prior failure count forward through Loading so
            // the post-fetch resolver can bump it on another miss; that
            // is what `MAX_FAVICON_ATTEMPTS` clamps against. Without
            // this, we'd retry a permanently broken origin every
            // `FAVICON_RETRY_DELAY` for the lifetime of the app.
            let prior_attempts = match self.favicon_cache.get(&key) {
                Some(FaviconState::Failed { attempts, .. }) => *attempts,
                _ => 0,
            };
            let should_fetch = match self.favicon_cache.get(&key) {
                Some(state) => state.should_fetch(now, FAVICON_RETRY_DELAY),
                None => true,
            };
            if !should_fetch {
                continue;
            }

            tracing::info!(
                target: TARGET_FAVICON,
                source = %key,
                kind = kind.label(),
                prior_attempts,
                "starting icon image fetch"
            );
            self.favicon_cache
                .insert(key.clone(), FaviconState::Loading { prior_attempts });
            self.spawn_favicon_fetch(key, kind, window, cx);
        }
    }

    fn spawn_favicon_fetch(
        &mut self,
        key: String,
        kind: IconKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(
            window,
            move |this: WeakEntity<Self>, cx: &mut AsyncWindowContext| {
                let mut async_cx = cx.clone();
                async move {
                    let key_for_fetch = key.clone();
                    let image = async_cx
                        .background_spawn(async move {
                            match kind {
                                IconKind::Favicon => fetch_favicon_image(&key_for_fetch),
                                IconKind::Direct => fetch_direct_image(&key_for_fetch),
                            }
                        })
                        .await;

                    let _ = this.update_in(&mut async_cx, move |this, _window, cx| {
                        // Recover the prior failure count from the
                        // Loading entry the dispatch loop wrote in
                        // sync_favicons. If this fetch failed too,
                        // bump the count; once it hits MAX_FAVICON_ATTEMPTS
                        // FaviconState::should_fetch returns false
                        // forever and the rail falls back to the globe
                        // glyph instead of pinging a broken origin
                        // every retry interval.
                        let prior_attempts = match this.favicon_cache.get(&key) {
                            Some(FaviconState::Loading { prior_attempts }) => *prior_attempts,
                            _ => 0,
                        };
                        this.favicon_cache.insert(
                            key.clone(),
                            match image {
                                Some(image) => {
                                    tracing::info!(
                                        target: TARGET_FAVICON,
                                        source = %key,
                                        kind = kind.label(),
                                        "resolved icon image"
                                    );
                                    FaviconState::Ready(image)
                                }
                                None => {
                                    // Outcome summary, not a bug — sites without
                                    // any usable favicon are common (TiddlyWiki,
                                    // bare dev servers, intranet hosts). WARN so a
                                    // single line per origin × attempt remains
                                    // visible at default log level for diagnosing
                                    // genuinely broken servers, without the per-
                                    // candidate noise.
                                    tracing::warn!(
                                        target: TARGET_FAVICON,
                                        source = %key,
                                        kind = kind.label(),
                                        attempts = prior_attempts + 1,
                                        "failed to resolve icon image"
                                    );
                                    FaviconState::Failed {
                                        failed_at: std::time::Instant::now(),
                                        attempts: prior_attempts + 1,
                                    }
                                }
                            },
                        );
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    fn sync_focus_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.webviews.sync_responder_target(&mut self.state);

        if self.webviews.wants_host_focus(&self.state) {
            let handle = if matches!(self.state.shell_mode, ShellMode::CommandBar) {
                self.omnibar.focus_handle(cx)
            } else {
                self.focus_handle.clone()
            };
            window.focus(&handle, cx);
        }
    }

    fn drain_open_urls(&mut self) -> bool {
        let urls = self.open_url_bridge.drain_urls();
        if urls.is_empty() {
            return false;
        }

        for url in urls {
            self.state.handle_host_route(&url);
        }
        true
    }

    /// Drains host-level action requests queued by the MCP automation
    /// socket (`host_dispatch_action`). Each entry is invoked here so
    /// the originating Operator never needed macOS Accessibility
    /// permission. Bound to the redesign's window-open helpers; unknown
    /// names log a warning rather than panic.
    fn drain_pending_host_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.pending_host_actions.is_empty() {
            return;
        }
        let actions: Vec<String> = self.state.pending_host_actions.drain(..).collect();
        for name in actions {
            tracing::info!(action = %name, "host action dispatched via automation socket");
            match name.as_str() {
                "OpenAppWindowExperiment" => {
                    window.dispatch_action(Box::new(crate::app::OpenAppWindowExperiment), cx);
                }
                // "OpenLauncherWindow" was retired in Stage D of the
                // system-capsule refactor — ShowSettings now reaches
                // the ato-settings system capsule directly.
                "OpenCardSwitcher" => {
                    window.dispatch_action(Box::new(crate::app::OpenCardSwitcher), cx);
                }
                "OpenStartWindow" => {
                    window.dispatch_action(Box::new(crate::app::OpenStartWindow), cx);
                }
                "OpenStoreWindow" => {
                    window.dispatch_action(Box::new(crate::app::OpenStoreWindow), cx);
                }
                "ShowSettings" => {
                    window.dispatch_action(Box::new(crate::app::ShowSettings), cx);
                }
                "OpenDockWindow" => {
                    window.dispatch_action(Box::new(crate::app::OpenDockWindow), cx);
                }
                other => {
                    tracing::warn!(
                        action = %other,
                        "host action not recognized — extend drain_pending_host_actions to add it"
                    );
                }
            }
        }
    }
}

impl Render for DesktopShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_pending_host_actions(window, cx);
        let handled_open_urls = self.drain_open_urls();
        let size = window.bounds().size;
        let stage_bounds =
            compute_stage_bounds(&self.state, f32::from(size.width), f32::from(size.height));
        self.state.set_active_bounds(stage_bounds);

        // Update dock_is_open flag *before* sync_from_state so that
        // ListPanes automation commands return the correct pane list.
        let dock_entity = cx
            .try_global::<crate::window::dock::DockEntitySlot>()
            .and_then(|s| s.0.clone());
        self.webviews.set_dock_open(dock_entity.is_some());

        self.webviews.sync_from_state(window, &mut self.state);

        // Dispatch automation commands targeting the dock WebView.
        if let Some(entity) = dock_entity {
            let dock_ref = entity.read(cx);
            self.webviews
                .dispatch_dock_automation_requests(&mut self.state, Some(&dock_ref.webview));
        } else {
            self.webviews
                .dispatch_dock_automation_requests(&mut self.state, None);
        }

        self.sync_omnibar_with_state(window, cx, false);
        if handled_open_urls {
            self.sync_focus_target(window, cx);
        }
        self.sync_favicons(window, cx);
        self.poll_capsule_search();
        self.poll_cli_login();
        self.poll_update_check();
        self.poll_capsule_updates();
        self.sync_config_modal(window, cx);
        self.sync_consent_modal();
        self.sync_resolution_modal(window, cx);
        let omnibar_value = self.omnibar.read(cx).value().to_string();
        self.maybe_trigger_capsule_search(&omnibar_value);
        let omnibar_suggestions = self.state.omnibar_suggestions(&omnibar_value);
        let active_pane_count = self.state.active_panes().len();
        let command_bar = matches!(self.state.shell_mode, ShellMode::CommandBar);
        // Hide the active WebView whenever a GPUI overlay needs to
        // paint above it. WKWebView is a native NSView and always
        // renders on top of GPUI's CALayer tree, so any in-app
        // modal (omnibar suggestions, the missing-env config form,
        // the permission prompt, the quit confirmation, the config
        // modal) is invisible until we toggle the WebView off.
        let hide_for_overlay = (command_bar && !omnibar_suggestions.is_empty())
            || self.state.pending_config.is_some()
            || self.state.pending_consent.is_some()
            || self.state.active_permission_prompt().is_some()
            || self.state.pending_quit_confirmation
            || self.state.route_metadata_popover_open
            || self.state.settings_panel_open;
        self.webviews
            .set_overlay_hides_webview(hide_for_overlay, &mut self.state);
        let route_metadata_overlay_route = if self.state.route_metadata_popover_open {
            self.state
                .active_capsule_detail_host_panel_route()
                .map(|route| route.url())
        } else {
            None
        };
        let route_metadata_overlay_bounds = route_metadata_overlay_route
            .as_ref()
            .map(|_| route_metadata_overlay_webview_bounds(stage_bounds));
        let route_metadata_overlay_payload = if self.state.route_metadata_popover_open {
            crate::webview::overlay_host_panel_payload(&self.state)
        } else {
            None
        };
        self.webviews.sync_overlay_host_panel(
            window,
            route_metadata_overlay_route,
            route_metadata_overlay_bounds,
            route_metadata_overlay_payload,
            &mut self.state,
        );
        let theme = Theme::from_mode(self.state.theme_mode);

        let body = div()
            .flex_1()
            .size_full()
            .flex()
            .overflow_hidden()
            .child(render_task_rail(&self.state, &self.favicon_cache, &theme))
            .child(
                div()
                    .flex_1()
                    .size_full()
                    .relative()
                    .flex()
                    .flex_col()
                    .child(render_stage(
                        &self.state,
                        stage_bounds,
                        active_pane_count,
                        &theme,
                        &self.launcher_search,
                    ))
                    .when(self.state.active_permission_prompt().is_some(), |this| {
                        this.child(render_permission_prompt_overlay(&self.state, &theme))
                    })
                    // #117 — the unified resolution modal takes
                    // precedence over the legacy single-slot modals.
                    // When `pending_resolution` is set the orchestrator
                    // drain has already merged any E103/E302 surfaces
                    // into it; we explicitly skip the legacy overlays
                    // so the user never sees two modals stacked.
                    .when(self.resolution_modal.is_some(), |this| {
                        let modal = self
                            .resolution_modal
                            .as_ref()
                            .expect("resolution_modal checked above");
                        this.child(modals::resolution_form::render_resolution_modal_overlay(
                            modal, &theme,
                        ))
                    })
                    .when(
                        self.resolution_modal.is_none() && self.config_modal.is_some(),
                        |this| {
                            // Fallback: the legacy E103 modal renders
                            // only when no unified resolution modal is
                            // in flight. In practice the orchestrator
                            // drain stopped writing to `pending_config`
                            // with #117, so this branch is dead today —
                            // it stays as a safety net for any caller
                            // that still calls `set_pending_config`
                            // directly (e.g. tests).
                            let modal = self
                                .config_modal
                                .as_ref()
                                .expect("config_modal checked above");
                            this.child(modals::config_form::render_config_modal_overlay(
                                modal, &theme,
                            ))
                        },
                    )
                    .when(
                        self.resolution_modal.is_none() && self.consent_modal.is_some(),
                        |this| {
                            // Fallback for legacy consent surface — see
                            // the config-modal branch above for why
                            // this is now dead in production.
                            let modal = self
                                .consent_modal
                                .as_ref()
                                .expect("consent_modal checked above");
                            this.child(modals::consent_form::render_consent_modal_overlay(
                                modal, &theme,
                            ))
                        },
                    )
                    .when(self.state.route_metadata_popover_open, |this| {
                        this.child(render_route_metadata_popover(&self.state, &theme))
                    })
                    .when(self.state.settings_panel_open, |this| {
                        this.child(render_settings_overlay(&self.state, &theme))
                    }),
            );

        div()
            .key_context("AtoDesktopShell")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.canvas_bg)
            .text_color(theme.canvas_text)
            .on_action(cx.listener(Self::on_toggle_theme))
            .on_action(cx.listener(Self::on_toggle_auto_devtools))
            .on_action(cx.listener(Self::on_focus_command_bar))
            .on_action(cx.listener(Self::on_show_settings))
            .on_action(cx.listener(Self::on_select_settings_tab))
            .on_action(cx.listener(Self::on_select_route_metadata_tab))
            .on_action(cx.listener(Self::on_toggle_dev_console))
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_select_task))
            .on_action(cx.listener(Self::on_close_task))
            .on_action(cx.listener(Self::on_move_task))
            .on_action(cx.listener(Self::on_navigate_to_url))
            .on_action(cx.listener(Self::on_next_workspace))
            .on_action(cx.listener(Self::on_previous_workspace))
            .on_action(cx.listener(Self::on_next_task))
            .on_action(cx.listener(Self::on_previous_task))
            .on_action(cx.listener(Self::on_split_pane))
            .on_action(cx.listener(Self::on_expand_split))
            .on_action(cx.listener(Self::on_shrink_split))
            .on_action(cx.listener(Self::on_dismiss_transient))
            .on_action(cx.listener(Self::on_toggle_route_metadata_popover))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_cancel_quit))
            .on_action(cx.listener(Self::on_stop_active_session))
            .on_action(cx.listener(Self::on_stop_all_retained_sessions))
            .on_action(cx.listener(Self::on_cycle_handle))
            .on_action(cx.listener(Self::on_browser_back))
            .on_action(cx.listener(Self::on_browser_forward))
            .on_action(cx.listener(Self::on_browser_reload))
            .on_action(cx.listener(Self::on_native_undo))
            .on_action(cx.listener(Self::on_native_redo))
            .on_action(cx.listener(Self::on_native_cut))
            .on_action(cx.listener(Self::on_native_copy))
            .on_action(cx.listener(Self::on_native_paste))
            .on_action(cx.listener(Self::on_native_select_all))
            .on_action(cx.listener(Self::on_open_auth_in_browser))
            .on_action(cx.listener(Self::on_open_local_registry))
            .on_action(cx.listener(Self::on_open_cloud_dock))
            .on_action(cx.listener(Self::on_sign_in_to_ato_run))
            .on_action(cx.listener(Self::on_sign_out))
            .on_action(cx.listener(Self::on_cancel_auth_handoff))
            .on_action(cx.listener(Self::on_resume_after_auth))
            .on_action(cx.listener(Self::on_allow_permission_once))
            .on_action(cx.listener(Self::on_allow_permission_for_session))
            .on_action(cx.listener(Self::on_deny_permission_prompt))
            .on_action(cx.listener(Self::on_save_config_form))
            .on_action(cx.listener(Self::on_cancel_config_form))
            .on_action(cx.listener(Self::on_approve_consent_form))
            .on_action(cx.listener(Self::on_cancel_consent_form))
            .on_action(cx.listener(Self::on_submit_resolution_form))
            .on_action(cx.listener(Self::on_cancel_resolution_form))
            .on_action(cx.listener(Self::on_resolution_form_next))
            .on_action(cx.listener(Self::on_resolution_form_back))
            .on_action(cx.listener(Self::on_check_for_updates))
            .on_action(cx.listener(Self::on_open_latest_release_page))
            .on_action(cx.listener(Self::on_open_external_link))
            .on_action(cx.listener(Self::on_install_capsule_update))
            .on_drop::<ExternalPaths>(cx.listener(|this, paths: &ExternalPaths, _window, _cx| {
                let path_vec = paths.paths().to_vec();
                this.state.launch_dropped_paths(path_vec);
            }))
            // Ambient glow — dark theme only
            .when(theme.ambient_glow_top.a > 0.0, |d| {
                d.child(div().absolute().top_0().left_0().right_0().h(px(200.0)).bg(
                    linear_gradient(
                        180.,
                        linear_color_stop(theme.ambient_glow_top, 0.),
                        linear_color_stop(hsla(220.0 / 360.0, 0.30, 0.20, 0.0), 1.),
                    ),
                ))
            })
            .child(render_command_chrome(
                window,
                &self.state,
                &self.omnibar,
                &omnibar_value,
                &omnibar_suggestions,
                command_bar,
                &theme,
            ))
            .when_some(self.state.workspace_loading_progress(), |this, progress| {
                this.child(render_boot_progress_strip(progress, &theme))
            })
            .child(body)
            .when(self.state.pending_quit_confirmation, |this| {
                this.child(render_quit_dialog(&theme))
            })
    }
}

fn render_quit_dialog(theme: &theme::Theme) -> impl IntoElement {
    let backdrop = hsla(0.0, 0.0, 0.0, 0.45);
    let panel_bg = theme.settings_panel_bg;
    let panel_border = theme.panel_border;
    let text_primary = theme.text_primary;
    let text_secondary = theme.text_secondary;

    div()
        .id("quit-confirm-overlay")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(backdrop)
        // Click on the backdrop = cancel.
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.dispatch_action(Box::new(CancelQuit), cx);
        })
        .child(
            div()
                .id("quit-confirm-dialog")
                .w(px(420.0))
                .p(px(24.0))
                .rounded(px(12.0))
                .bg(panel_bg)
                .border_1()
                .border_color(panel_border)
                .shadow_lg()
                .flex()
                .flex_col()
                .gap(px(14.0))
                // Eat clicks so they don't bubble to the backdrop.
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .child(
                    div()
                        .text_size(px(16.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(text_primary)
                        .child("Quit Ato Desktop"),
                )
                .child(div().text_size(px(13.0)).text_color(text_secondary).child(
                    "Keep your current tabs for the next launch, or clear them and start fresh?",
                ))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.0))
                        .child(quit_dialog_button(
                            "Cancel",
                            theme,
                            QuitDialogButtonKind::Neutral,
                            |window, cx| {
                                window.dispatch_action(Box::new(CancelQuit), cx);
                            },
                        ))
                        .child(quit_dialog_button(
                            "Clear & Quit",
                            theme,
                            QuitDialogButtonKind::Danger,
                            |window, cx| {
                                window.dispatch_action(Box::new(ConfirmQuitClear), cx);
                            },
                        ))
                        .child(quit_dialog_button(
                            "Keep & Quit",
                            theme,
                            QuitDialogButtonKind::Primary,
                            |window, cx| {
                                window.dispatch_action(Box::new(ConfirmQuitKeep), cx);
                            },
                        )),
                ),
        )
}

#[derive(Clone, Copy)]
enum QuitDialogButtonKind {
    Primary,
    Neutral,
    Danger,
}

fn quit_dialog_button(
    label: &'static str,
    theme: &theme::Theme,
    kind: QuitDialogButtonKind,
    on_click: impl Fn(&mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let (bg, fg, border) = match kind {
        QuitDialogButtonKind::Primary => (theme.accent, gpui::white(), theme.accent),
        QuitDialogButtonKind::Neutral => (
            theme.surface_hover,
            theme.text_primary,
            theme.border_default,
        ),
        QuitDialogButtonKind::Danger => (
            theme.surface_hover,
            hsla(0.0, 0.7, 0.5, 1.0),
            theme.border_default,
        ),
    };
    div()
        .id(label)
        .px(px(14.0))
        .py(px(8.0))
        .rounded(px(6.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_color(fg)
        .text_size(px(13.0))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .child(label)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx);
        })
}

impl Focusable for DesktopShell {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[derive(Copy, Clone)]
enum IconKind {
    /// Origin → `{origin}/favicon.ico` (legacy ExternalUrl tabs).
    Favicon,
    /// `source` is a fully-resolved location: absolute filesystem
    /// path, `file://` / `http(s)://` URL, or a `data:` URL. Set by
    /// the capsule-manifest icon plumbing.
    Direct,
}

impl IconKind {
    fn label(self) -> &'static str {
        match self {
            Self::Favicon => "favicon",
            Self::Direct => "direct",
        }
    }
}

fn fetch_direct_image(source: &str) -> Option<Arc<Image>> {
    tracing::info!(target: TARGET_FAVICON, source, "fetching direct icon image");
    if source.starts_with("http://") || source.starts_with("https://") {
        return fetch_image_from_url(source);
    }
    if let Some(rest) = source.strip_prefix("file://") {
        return fetch_image_from_path(std::path::Path::new(rest));
    }
    if source.starts_with("data:") {
        return decode_data_url_image(source);
    }
    fetch_image_from_path(std::path::Path::new(source))
}

fn fetch_image_from_path(path: &std::path::Path) -> Option<Arc<Image>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(
                target: TARGET_FAVICON,
                path = %path.display(),
                error = %error,
                "failed to read icon image file"
            );
            return None;
        }
    };
    // Prefer byte sniffing over the file extension for the same
    // reason `fetch_image_from_url_with_headers` prefers it over the
    // server-declared content-type: extensions lie too (an `.ico` file
    // that is actually PNG, an SVG saved as `.png`, etc.). The
    // extension only acts as a tiebreaker when sniffing fails.
    let Some(format) = sniff_image_format(&bytes).or_else(|| format_for_extension(path)) else {
        tracing::error!(
            target: TARGET_FAVICON,
            path = %path.display(),
            "failed to determine icon image format"
        );
        return None;
    };
    tracing::info!(
        target: TARGET_FAVICON,
        path = %path.display(),
        bytes = bytes.len(),
        "loaded icon image file"
    );
    Some(arc_image_from_bytes(format, bytes))
}

fn fetch_image_from_url(url: &str) -> Option<Arc<Image>> {
    fetch_image_from_url_with_headers(url, /*reject_non_image=*/ false)
}

/// HTTP image fetcher shared by the favicon path and the capsule-manifest
/// icon path. We always send a Safari-shaped User-Agent (mirroring the
/// Wry webview at `webview.rs:1569`) and `Accept: image/*` because:
///
/// * Cloudflare-fronted origins (including `ato.run`) frequently 403
///   default `ureq/...` UAs, leaving the sidebar showing the globe
///   placeholder forever.
/// * Some servers return 406 / serve HTML for `Accept: */*`. Asking
///   for image-only Content-Type lets them route to the asset
///   correctly.
///
/// `reject_non_image = true` makes the fetcher refuse any response
/// whose `Content-Type` is not `image/*` *before* parsing bytes.
/// Without it, the previous favicon path fell through to
/// `ImageFormat::Ico` for `text/html` 200 responses (typical of SPA
/// catch-all routes), masking non-image content as a corrupt icon.
fn fetch_image_from_url_with_headers(url: &str, reject_non_image: bool) -> Option<Arc<Image>> {
    tracing::info!(target: TARGET_FAVICON, url, reject_non_image, "fetching icon image URL");
    let response = match ureq::get(url)
        .set("User-Agent", FAVICON_USER_AGENT)
        .set("Accept", "image/*")
        .call()
    {
        Ok(response) => response,
        Err(error) => {
            // Per-candidate miss (404, network blip, etc.). The umbrella
            // `failed to resolve any favicon candidate` WARN summarizes the
            // outcome at the end of the fallback walk; logging each candidate
            // failure at ERROR drowns the rail in noise for sites that simply
            // don't ship a favicon (TiddlyWiki, blank dev servers).
            tracing::debug!(
                target: TARGET_FAVICON,
                url,
                error = %error,
                "icon image URL request failed"
            );
            return None;
        }
    };

    let content_type = response
        .header("content-type")
        .or_else(|| response.header("Content-Type"))
        .map(|value| value.split(';').next().unwrap_or(value).trim().to_string());

    if reject_non_image
        && !content_type
            .as_deref()
            .map(|ct| ct.starts_with("image/"))
            .unwrap_or(false)
    {
        tracing::debug!(
            target: TARGET_FAVICON,
            url,
            content_type = ?content_type,
            "icon image URL returned non-image content type"
        );
        return None;
    }

    let mut bytes = Vec::new();
    if let Err(error) = response.into_reader().read_to_end(&mut bytes) {
        // Mid-stream read failure is unusual enough to surface — keep at
        // WARN so a flaky upstream still leaves a breadcrumb at default
        // log level.
        tracing::warn!(
            target: TARGET_FAVICON,
            url,
            error = %error,
            "failed to read icon image URL body"
        );
        return None;
    }
    if bytes.is_empty() {
        tracing::debug!(target: TARGET_FAVICON, url, "icon image URL returned an empty body");
        return None;
    }
    let Some(format) = determine_image_format(&bytes, content_type.as_deref()) else {
        tracing::debug!(
            target: TARGET_FAVICON,
            url,
            content_type = ?content_type,
            bytes = bytes.len(),
            "failed to determine icon image URL format"
        );
        return None;
    };
    tracing::info!(
        target: TARGET_FAVICON,
        url,
        content_type = ?content_type,
        bytes = bytes.len(),
        format = ?format,
        "loaded icon image URL"
    );
    Some(arc_image_from_bytes(format, bytes))
}

/// Pick an image format for a fetched body, preferring magic-byte
/// sniffing over the declared `Content-Type`.
///
/// Real-world favicon servers lie about their content type all the
/// time. Notably Google's gstatic CDN and MDN serve **PNG bytes** at
/// `/favicon.ico` with `Content-Type: image/x-icon` — every browser
/// content-sniffs and renders them as PNG, but if we trust the header
/// we hand the bytes to `ico::IconDir::read` which correctly rejects
/// the file ("Invalid reserved field value in ICONDIR (was 20617, but
/// must be 0)" — 20617 = 0x5089 = the first two bytes of PNG magic).
/// Sniffing first matches the browser behavior; falling back to the
/// declared content-type only when sniff fails (truncated body, etc.)
/// preserves correctness for unusual formats whose magic bytes we
/// don't enumerate in `sniff_image_format`.
fn determine_image_format(bytes: &[u8], content_type: Option<&str>) -> Option<ImageFormat> {
    sniff_image_format(bytes).or_else(|| content_type.and_then(image_format_from_content_type))
}

/// Safari UA used by Wry on `webview.rs:1569`. Reusing it makes origins
/// treat the favicon probe like the eventual page load — same UA, same
/// bot-detection / WAF outcome — instead of seeing two unrelated
/// clients (one of which gets 403'd).
const FAVICON_USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 ",
    "(KHTML, like Gecko) Version/17.0 Safari/605.1.15 AtoDesktop/",
    env!("CARGO_PKG_VERSION")
);

fn decode_data_url_image(url: &str) -> Option<Arc<Image>> {
    // Minimal `data:image/<fmt>;base64,...` decoder. We only need to
    // handle the base64 form because the icon plumbing is the only
    // producer and we control its output.
    let Some(body) = url.strip_prefix("data:") else {
        tracing::error!(target: TARGET_FAVICON, "icon data URL did not start with data:");
        return None;
    };
    let Some((meta, payload)) = body.split_once(',') else {
        tracing::error!(target: TARGET_FAVICON, "icon data URL was missing comma separator");
        return None;
    };
    let mime = meta.split(';').next().unwrap_or("");
    // Sniff the (possibly base64) payload first — same reason as
    // `fetch_image_from_url_with_headers`, the declared mime can lie.
    // For base64 payloads this mostly matters when the encoded bytes
    // happen to start with magic that disagrees with the data URL's
    // mime field; we trust the bytes.
    let Some(format) = determine_image_format(payload.as_bytes(), Some(mime)) else {
        tracing::error!(
            target: TARGET_FAVICON,
            mime,
            "failed to determine icon data URL image format"
        );
        return None;
    };
    let bytes = if meta.contains(";base64") {
        use base64::Engine;
        match base64::engine::general_purpose::STANDARD.decode(payload) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::error!(
                    target: TARGET_FAVICON,
                    mime,
                    error = %error,
                    "failed to decode icon data URL base64 payload"
                );
                return None;
            }
        }
    } else {
        payload.as_bytes().to_vec()
    };
    tracing::info!(target: TARGET_FAVICON, mime, bytes = bytes.len(), "decoded icon data URL image");
    Some(arc_image_from_bytes(format, bytes))
}

fn format_for_extension(path: &std::path::Path) -> Option<ImageFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "svg" => Some(ImageFormat::Svg),
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "ico" => Some(ImageFormat::Ico),
        _ => None,
    }
}

/// Build an `Arc<Image>` for GPUI, normalizing ICO and SVG to PNG first.
///
/// The pinned `gpui` rev mishandles SVG bytes when fed through
/// `Image::from_bytes`: `Image::to_image_data` (gpui platform.rs:2109-2113)
/// passes `to_brga = false` to the SVG renderer, leaving the rendered
/// buffer in RGBA-PA when the rest of GPUI's render pipeline expects
/// BGRA. The result on macOS Metal is an effectively invisible image.
/// `image::load_from_memory_with_format` for ICO also occasionally
/// fails on multi-resolution icons whose entries are PNG-encoded
/// (Google's `/favicon.ico` is one such file).
///
/// Pre-rasterizing both to PNG bypasses both special cases and routes
/// every favicon through the well-trodden raster path that demonstrably
/// works (PNG / WEBP / JPEG / GIF / BMP).
fn arc_image_from_bytes(format: ImageFormat, bytes: Vec<u8>) -> Arc<Image> {
    let (final_format, final_bytes) = normalize_icon_bytes(format, bytes);
    Arc::new(Image::from_bytes(final_format, final_bytes))
}

fn normalize_icon_bytes(format: ImageFormat, bytes: Vec<u8>) -> (ImageFormat, Vec<u8>) {
    match format {
        ImageFormat::Svg => match render_svg_to_png(&bytes, SVG_RASTER_TARGET_PX) {
            Some(png) => {
                tracing::info!(
                    input_bytes = bytes.len(),
                    output_bytes = png.len(),
                    "normalized SVG icon to PNG"
                );
                (ImageFormat::Png, png)
            }
            None => {
                tracing::error!(
                    input_bytes = bytes.len(),
                    "SVG-to-PNG normalization failed; falling back to original SVG bytes"
                );
                (format, bytes)
            }
        },
        ImageFormat::Ico => match transcode_ico_to_png(&bytes) {
            Some(png) => {
                tracing::info!(
                    input_bytes = bytes.len(),
                    output_bytes = png.len(),
                    "normalized ICO icon to PNG"
                );
                (ImageFormat::Png, png)
            }
            None => {
                tracing::error!(
                    input_bytes = bytes.len(),
                    "ICO-to-PNG normalization failed; falling back to original ICO bytes"
                );
                (format, bytes)
            }
        },
        ImageFormat::Tiff => match transcode_to_png(&bytes, image::ImageFormat::Tiff) {
            Some(png) => (ImageFormat::Png, png),
            None => (format, bytes),
        },
        _ => (format, bytes),
    }
}

/// Pixel size to rasterize SVG favicons at. The rail glyph itself is
/// 22 px (sidebar `APP_ICON_SIZE`); 96 px gives the GPU a clean
/// downscale at @1x and still looks crisp at @2x retina without
/// blowing up tiny-skia allocations for SVGs that declare large
/// natural sizes.
const SVG_RASTER_TARGET_PX: u32 = 96;

/// Lenient ICO → PNG conversion via the `ico` crate.
///
/// The `image` crate's ICO parser strictly validates ICONDIRENTRY's
/// `wPlanes` and `wBitCount` fields and rejects real-world `favicon.ico`
/// files served by Google (gstatic), MDN, and many other major sites
/// because they carry non-canonical reserved/planes bytes — even
/// though every browser renders them fine. The `ico` crate handles
/// both PNG-encoded and DIB-encoded entries leniently, so we route all
/// ICO normalization through it and re-encode the resulting RGBA
/// pixels as PNG via the `image` crate.
fn transcode_ico_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let icon_dir = match ico::IconDir::read(std::io::Cursor::new(bytes)) {
        Ok(dir) => dir,
        Err(error) => {
            tracing::error!(target: TARGET_FAVICON, error = %error, "ico crate failed to parse ICO directory");
            return None;
        }
    };
    // Pick the entry with the largest pixel area. Browsers normally
    // pick the entry closest to the target display size; for our 22 px
    // sidebar chip rendered up to @2x retina, anything ≥ 32 px works
    // and the largest available has the most fidelity to downscale
    // from.
    let entry = icon_dir.entries().iter().max_by_key(|entry| {
        let w = entry.width() as u64;
        let h = entry.height() as u64;
        w.saturating_mul(h)
    })?;
    let icon_image = match entry.decode() {
        Ok(image) => image,
        Err(error) => {
            tracing::error!(target: TARGET_FAVICON, error = %error, "ico crate failed to decode entry");
            return None;
        }
    };
    let width = icon_image.width();
    let height = icon_image.height();
    let rgba = icon_image.rgba_data().to_vec();
    let Some(buffer) = image::RgbaImage::from_raw(width, height, rgba) else {
        tracing::error!(
            target: TARGET_FAVICON,
            width,
            height,
            "rgba buffer length mismatch from ICO entry decode"
        );
        return None;
    };
    let mut out = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut out);
    if let Err(error) =
        image::DynamicImage::ImageRgba8(buffer).write_to(&mut cursor, image::ImageFormat::Png)
    {
        tracing::error!(target: TARGET_FAVICON, error = %error, "PNG re-encode failed for ICO entry");
        return None;
    }
    Some(out)
}

fn transcode_to_png(bytes: &[u8], format: image::ImageFormat) -> Option<Vec<u8>> {
    let img = match image::load_from_memory_with_format(bytes, format) {
        Ok(img) => img,
        Err(error) => {
            tracing::error!(
                target: TARGET_FAVICON,
                ?format,
                error = %error,
                "image decode failed during PNG transcode"
            );
            return None;
        }
    };
    let mut out = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut out);
    if let Err(error) = img.write_to(&mut cursor, image::ImageFormat::Png) {
        tracing::error!(
            target: TARGET_FAVICON,
            ?format,
            error = %error,
            "PNG re-encode failed during transcode"
        );
        return None;
    }
    Some(out)
}

fn render_svg_to_png(bytes: &[u8], target_px: u32) -> Option<Vec<u8>> {
    let options = usvg::Options::default();
    let tree = match usvg::Tree::from_data(bytes, &options) {
        Ok(tree) => tree,
        Err(error) => {
            tracing::error!(target: TARGET_FAVICON, error = %error, "usvg parse failed during SVG normalization");
            return None;
        }
    };
    let svg_size = tree.size();
    let max_dim = svg_size.width().max(svg_size.height()).max(1.0);
    let scale = (target_px as f32) / max_dim;
    let pixmap_w = (svg_size.width() * scale).round().max(1.0) as u32;
    let pixmap_h = (svg_size.height() * scale).round().max(1.0) as u32;
    let mut pixmap = match tiny_skia::Pixmap::new(pixmap_w, pixmap_h) {
        Some(pm) => pm,
        None => {
            tracing::error!(
                target: TARGET_FAVICON,
                pixmap_w,
                pixmap_h,
                "tiny_skia Pixmap::new failed during SVG normalization"
            );
            return None;
        }
    };
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    match pixmap.encode_png() {
        Ok(png) => Some(png),
        Err(error) => {
            tracing::error!(target: TARGET_FAVICON, error = %error, "tiny_skia PNG encode failed");
            None
        }
    }
}

fn fetch_favicon_image(origin: &str) -> Option<Arc<Image>> {
    tracing::info!(target: TARGET_FAVICON, origin, "fetching favicon image candidates");

    // First: parse `<link rel="icon">` declarations from the origin's
    // root HTML. Required for SPA / catch-all servers where
    // `/favicon.ico`, `/favicon.svg`, etc. all 200 with `text/html`
    // (e.g. grok.com), which the strict-content-type fetcher correctly
    // refuses but which leaves the rail showing a globe forever
    // without this path.
    for url in fetch_html_link_icon_candidates(origin) {
        tracing::info!(target: TARGET_FAVICON, origin, url, "trying link-icon candidate");
        if let Some(image) =
            fetch_image_from_url_with_headers(&url, /*reject_non_image=*/ true)
        {
            tracing::info!(target: TARGET_FAVICON, origin, url, "resolved link-icon candidate");
            return Some(image);
        }
    }

    // Fallback: well-known paths. Try `/favicon.ico` first (most
    // universal), then `.svg` (Vite/Next.js default), then
    // `apple-touch-icon.png`.
    for url in favicon_candidate_urls(origin) {
        tracing::info!(target: TARGET_FAVICON, origin, url, "trying favicon candidate");
        if let Some(image) =
            fetch_image_from_url_with_headers(&url, /*reject_non_image=*/ true)
        {
            tracing::info!(target: TARGET_FAVICON, origin, url, "resolved favicon candidate");
            return Some(image);
        }
    }
    tracing::warn!(target: TARGET_FAVICON, origin, "failed to resolve any favicon candidate");
    None
}

/// Fetch the origin root HTML and extract `<link rel="icon">` hrefs.
///
/// Returns an empty Vec on any network/parse failure so callers can
/// fall through to the well-known-paths probe. URL resolution uses the
/// response's final URL (after redirects), so an origin that 301s
/// `https://example.com` → `https://www.example.com` still resolves
/// relative `<link href="...">` against `www.`.
fn fetch_html_link_icon_candidates(origin: &str) -> Vec<String> {
    if !matches!(
        url::Url::parse(origin).ok().as_ref().map(url::Url::scheme),
        Some("http") | Some("https")
    ) {
        return Vec::new();
    }
    let url = format!("{origin}/");
    tracing::info!(target: TARGET_FAVICON, origin, url, "fetching origin HTML for link-icon parsing");
    let response = match ureq::get(&url)
        .set("User-Agent", FAVICON_USER_AGENT)
        .set("Accept", "text/html,application/xhtml+xml")
        .call()
    {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(
                target: TARGET_FAVICON,
                origin,
                url,
                error = %error,
                "origin HTML request failed; skipping link-icon parse"
            );
            return Vec::new();
        }
    };
    let final_url = response.get_url().to_string();
    let mut bytes = Vec::new();
    // Cap the read at 2 MB. Real-world `<head>` is well under this; the
    // limit just guards against a tar-pit endpoint that streams an
    // unbounded `text/html` body.
    if let Err(error) = response
        .into_reader()
        .take(2 * 1024 * 1024)
        .read_to_end(&mut bytes)
    {
        tracing::error!(
            target: TARGET_FAVICON,
            origin,
            error = %error,
            "origin HTML body read failed; skipping link-icon parse"
        );
        return Vec::new();
    }
    let body = String::from_utf8_lossy(&bytes);
    let candidates = parse_link_icon_candidates(&body, &final_url);
    tracing::info!(
        target: TARGET_FAVICON,
        origin,
        final_url,
        candidate_count = candidates.len(),
        "parsed link-icon candidates from origin HTML"
    );
    candidates
}

fn image_format_from_content_type(content_type: &str) -> Option<ImageFormat> {
    match content_type {
        "image/x-icon" | "image/vnd.microsoft.icon" => Some(ImageFormat::Ico),
        "image/svg+xml" => Some(ImageFormat::Svg),
        other => ImageFormat::from_mime_type(other),
    }
}

fn sniff_image_format(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(ImageFormat::Png);
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(ImageFormat::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    // SVG sniff — tolerate XML prologue and stray whitespace before
    // the root element, which renderers like Inkscape emit by
    // default.
    let prefix = std::str::from_utf8(&bytes[..bytes.len().min(256)]).unwrap_or("");
    let trimmed = prefix.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with("<?xml") || trimmed.starts_with("<svg") {
        return Some(ImageFormat::Svg);
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Some(ImageFormat::Webp);
    }
    if bytes.starts_with(b"BM") {
        return Some(ImageFormat::Bmp);
    }
    if bytes.starts_with(&[0x49, 0x49, 0x2a, 0x00]) || bytes.starts_with(&[0x4d, 0x4d, 0x00, 0x2a])
    {
        return Some(ImageFormat::Tiff);
    }
    if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) || bytes.starts_with(&[0x00, 0x00, 0x02, 0x00])
    {
        return Some(ImageFormat::Ico);
    }

    let prefix = bytes.iter().take(256).copied().collect::<Vec<_>>();
    let text = String::from_utf8_lossy(&prefix).to_ascii_lowercase();
    if text.contains("<svg") {
        return Some(ImageFormat::Svg);
    }

    None
}

fn compute_stage_bounds(_state: &AppState, width: f32, height: f32) -> PaneBounds {
    PaneBounds {
        x: RAIL_WIDTH + STAGE_PADDING,
        y: CHROME_HEIGHT + STAGE_PADDING,
        width: (width - RAIL_WIDTH - STAGE_PADDING * 2.0).max(240.0),
        height: (height - CHROME_HEIGHT - STAGE_PADDING * 2.0).max(180.0),
    }
}

fn inset_bounds(bounds: PaneBounds, inset: f32) -> PaneBounds {
    PaneBounds {
        x: bounds.x + inset,
        y: bounds.y + inset,
        width: (bounds.width - inset * 2.0).max(1.0),
        height: (bounds.height - inset * 2.0).max(1.0),
    }
}

fn route_metadata_overlay_webview_bounds(stage_bounds: PaneBounds) -> PaneBounds {
    stage_bounds
}

fn render_boot_progress_strip(progress: f32, theme: &Theme) -> impl IntoElement {
    // 2px strip flush against the chrome's bottom border. Filled
    // section uses theme.accent; track uses surface_hover so the
    // strip is visible against either light or dark panel_bg.
    let progress = progress.clamp(0.0, 1.0);
    div().h(px(2.0)).w_full().bg(theme.surface_hover).child(
        div()
            .h(px(2.0))
            .w(gpui::relative(progress))
            .bg(theme.accent),
    )
}

/// Render the capsule-update slot inside the route-info popover.
///
/// `Idle` / `Checking` are silent — the popover was opened to read metadata,
/// and a fast network round-trip would otherwise flicker. Once the worker
/// posts a `UpToDate { current }` we surface a calm subtitle, an
/// `Available` lights up an accent banner with an Install-update button,
/// and `Failed` collapses to a muted single-line error so the user has
/// some feedback if the registry was unreachable.
fn render_capsule_update_section(
    update: Option<&crate::state::CapsuleUpdate>,
    theme: &Theme,
) -> AnyElement {
    use crate::state::CapsuleUpdate;
    match update {
        None | Some(CapsuleUpdate::Idle) | Some(CapsuleUpdate::Checking) => {
            div().w(px(0.0)).into_any_element()
        }
        Some(CapsuleUpdate::UpToDate { current }) => div()
            .mt_1()
            .text_size(px(11.0))
            .text_color(theme.text_tertiary)
            .child(format!("v{current} (latest)"))
            .into_any_element(),
        Some(CapsuleUpdate::Failed { message }) => div()
            .mt_1()
            .text_size(px(11.0))
            .text_color(theme.text_tertiary)
            .child(format!("Update check failed: {message}"))
            .into_any_element(),
        Some(CapsuleUpdate::Available {
            current,
            latest,
            target_handle,
        }) => {
            let target_handle = target_handle.clone();
            div()
                .mt_2()
                .rounded(px(10.0))
                .bg(theme.accent_subtle)
                .border_1()
                .border_color(theme.accent_border)
                .p_3()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(11.5))
                        .font_weight(FontWeight(600.0))
                        .text_color(theme.text_primary)
                        .child("Update available"),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_secondary)
                        .child(format!("v{current} → v{latest}")),
                )
                .child(
                    div()
                        .id("capsule-update-install-button")
                        .mt_1()
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(6.0))
                        .bg(theme.accent)
                        .text_size(px(11.5))
                        .font_weight(FontWeight(600.0))
                        .text_color(gpui::white())
                        .cursor_pointer()
                        .child("Install update")
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            cx.stop_propagation();
                            window.dispatch_action(
                                Box::new(InstallCapsuleUpdate {
                                    url: target_handle.clone(),
                                }),
                                cx,
                            );
                        }),
                )
                .into_any_element()
        }
    }
}

fn render_route_metadata_popover(state: &AppState, theme: &Theme) -> AnyElement {
    if let Some(route) = state.active_capsule_detail_host_panel_route() {
        return render_route_metadata_host_panel_overlay(&route, theme).into_any_element();
    }

    let active_web = state.active_web_pane();
    let active = state.active_capsule_pane().or_else(|| {
        active_web
            .clone()
            .map(|pane| crate::state::ActiveCapsulePane {
                pane_id: pane.pane_id,
                title: pane.title,
                route: pane.route,
                session: pane.session,
                source_label: pane.source_label,
                trust_state: pane.trust_state,
                restricted: pane.restricted,
                snapshot_label: pane.snapshot_label,
                canonical_handle: pane.canonical_handle,
                session_id: pane.session_id,
                adapter: pane.adapter,
                manifest_path: pane.manifest_path,
                runtime_label: pane.runtime_label,
                display_strategy: pane.display_strategy,
                log_path: pane.log_path,
                local_url: pane.local_url,
                healthcheck_url: pane.healthcheck_url,
                invoke_url: pane.invoke_url,
                served_by: pane.served_by,
            })
    });
    let Some(active) = active else {
        return div().into_any_element();
    };

    let pane_id = active.pane_id;
    let route_label = active.route.to_string();
    let canonical_handle = active
        .canonical_handle
        .clone()
        .unwrap_or_else(|| route_label.clone());
    let publisher = capsule_publisher_label(&canonical_handle);
    let version_label = active
        .snapshot_label
        .clone()
        .unwrap_or_else(|| "unversioned".to_string());
    let trust_label = active.trust_state.clone().unwrap_or_else(|| {
        if active.restricted {
            "untrusted"
        } else {
            "pending"
        }
        .to_string()
    });
    let trust_accent = capsule_trust_color(&trust_label, theme);
    let session_label = capsule_session_label(active.session.clone());
    let quick_open_url = active
        .local_url
        .clone()
        .or_else(|| active.invoke_url.clone())
        .or_else(|| active.healthcheck_url.clone());
    let log_entries: Vec<crate::state::CapsuleLogEntry> = state
        .capsule_logs
        .get(&pane_id)
        .map(|entries| {
            let take = entries.len().min(10);
            entries[entries.len() - take..].to_vec()
        })
        .unwrap_or_default();

    let panel = div()
        .id("route-metadata-panel")
        .size_full()
        .rounded(px(14.0))
        .bg(theme.settings_panel_bg)
        .border_1()
        .border_color(theme.border_default)
        .shadow(vec![BoxShadow {
            color: hsla(0.0, 0.0, 0.0, 0.30),
            offset: point(px(0.0), px(12.0)),
            blur_radius: px(36.0),
            spread_radius: px(0.0),
        }])
        .flex()
        .flex_col()
        .overflow_hidden()
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .child(render_capsule_detail_header(
            &active,
            &publisher,
            &version_label,
            &trust_label,
            trust_accent,
            quick_open_url.clone(),
            theme,
        ))
        .child(render_capsule_detail_nav(
            state.route_metadata_active_tab,
            theme,
        ))
        .child(render_capsule_detail_body(
            state,
            &active,
            active_web.as_ref(),
            &canonical_handle,
            &route_label,
            &publisher,
            &session_label,
            &version_label,
            log_entries,
            quick_open_url,
            theme,
        ));

    div()
        .id("route-metadata-backdrop")
        .absolute()
        .inset_0()
        .bg(hsla(0.0, 0.0, 0.0, 0.20))
        .p(px(10.0))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.dispatch_action(Box::new(ToggleRouteMetadataPopover), cx);
        })
        .child(panel)
        .into_any_element()
}

fn render_route_metadata_host_panel_overlay(
    route: &crate::state::HostPanelRoute,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .id("route-metadata-backdrop")
        .absolute()
        .inset_0()
        .bg(hsla(0.0, 0.0, 0.0, 0.0))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.dispatch_action(Box::new(ToggleRouteMetadataPopover), cx);
        })
        .child(
            div()
                .id("route-metadata-panel")
                .size_full()
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .flex()
                .items_start()
                .justify_end()
                .p(px(14.0))
                .child(
                    div()
                        .rounded(px(999.0))
                        .bg(theme.panel_bg)
                        .border_1()
                        .border_color(theme.border_subtle)
                        .px(px(12.0))
                        .py(px(6.0))
                        .text_size(px(11.0))
                        .text_color(theme.text_secondary)
                        .child(route.label()),
                ),
        )
}

fn render_capsule_detail_header(
    active: &crate::state::ActiveCapsulePane,
    publisher: &str,
    version_label: &str,
    trust_label: &str,
    trust_accent: gpui::Hsla,
    quick_open_url: Option<String>,
    theme: &Theme,
) -> Div {
    let restart_enabled = !matches!(
        active.session,
        crate::state::WebSessionState::Detached | crate::state::WebSessionState::Closed
    );
    let open_enabled = quick_open_url.is_some();

    div()
        .px(px(20.0))
        .py(px(16.0))
        .border_b_1()
        .border_color(theme.border_subtle)
        .flex()
        .items_center()
        .justify_between()
        .gap(px(20.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(14.0))
                .child(
                    div()
                        .w(px(42.0))
                        .h(px(42.0))
                        .rounded(px(14.0))
                        .bg(theme.accent_subtle)
                        .border_1()
                        .border_color(theme.accent_border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(18.0))
                        .text_color(theme.accent)
                        .child("◉"),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .text_size(px(19.0))
                                        .font_weight(FontWeight(650.0))
                                        .text_color(theme.text_primary)
                                        .child(active.title.clone()),
                                )
                                .child(render_capsule_meta_pill(version_label, theme))
                                .child(render_capsule_trust_pill(trust_label, trust_accent, theme)),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.text_secondary)
                                .child(format!("{}  •  {}", publisher, active.route)),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(render_capsule_header_button("Stop", false, None, theme))
                .child(render_capsule_header_button(
                    "Restart",
                    restart_enabled,
                    Some(Box::new(crate::app::BrowserReload)),
                    theme,
                ))
                .child(render_capsule_header_button(
                    "Open",
                    open_enabled,
                    quick_open_url
                        .map(|url| Box::new(OpenExternalLink { url }) as Box<dyn gpui::Action>),
                    theme,
                ))
                .child(
                    div()
                        .px(px(12.0))
                        .py(px(7.0))
                        .rounded(px(8.0))
                        .bg(theme.settings_card_bg)
                        .border_1()
                        .border_color(theme.border_subtle)
                        .text_size(px(12.0))
                        .text_color(theme.text_primary)
                        .cursor_pointer()
                        .child("Close")
                        .on_mouse_down(MouseButton::Left, |_, window, cx| {
                            cx.stop_propagation();
                            window.dispatch_action(Box::new(ToggleRouteMetadataPopover), cx);
                        }),
                ),
        )
}

fn render_capsule_detail_nav(active_tab: CapsuleDetailTab, theme: &Theme) -> Div {
    div()
        .px(px(20.0))
        .pt(px(8.0))
        .pb(px(6.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .children(CapsuleDetailTab::ALL.into_iter().map(|tab| {
            render_capsule_detail_nav_item(tab, active_tab == tab, theme).into_any_element()
        }))
}

fn render_capsule_detail_nav_item(
    tab: CapsuleDetailTab,
    active: bool,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .id(("capsule-detail-tab", capsule_detail_tab_index(tab)))
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .cursor_pointer()
        .bg(if active {
            theme.accent_subtle
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .text_size(px(12.0))
        .font_weight(FontWeight(if active { 650.0 } else { 500.0 }))
        .text_color(if active {
            theme.accent
        } else {
            theme.text_disabled
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(SelectRouteMetadataTab { tab }), cx);
        })
        .child(tab.label())
}

#[allow(clippy::too_many_arguments)]
fn render_capsule_detail_body(
    state: &AppState,
    active: &crate::state::ActiveCapsulePane,
    active_web: Option<&crate::state::ActiveWebPane>,
    canonical_handle: &str,
    route_label: &str,
    publisher: &str,
    session_label: &str,
    version_label: &str,
    log_entries: Vec<crate::state::CapsuleLogEntry>,
    quick_open_url: Option<String>,
    theme: &Theme,
) -> impl IntoElement {
    let pane_id = active.pane_id;
    let network_logs: Vec<&crate::state::NetworkLogEntry> = state
        .network_logs
        .iter()
        .filter(|entry| entry.pane_id == pane_id)
        .collect();

    div()
        .overflow_y_scrollbar()
        .flex_1()
        .px(px(20.0))
        .pb(px(20.0))
        .child(match state.route_metadata_active_tab {
            CapsuleDetailTab::Overview => render_capsule_overview_page(
                state,
                active,
                route_label,
                publisher,
                session_label,
                &log_entries,
                &network_logs,
                quick_open_url,
                theme,
            ),
            CapsuleDetailTab::Permissions => render_capsule_permissions_page(
                state,
                active,
                active_web,
                canonical_handle,
                &network_logs,
                theme,
            ),
            CapsuleDetailTab::Logs => {
                render_capsule_logs_page(active, active_web, &log_entries, theme)
            }
            CapsuleDetailTab::Update => {
                render_capsule_update_page(state, active, version_label, theme)
            }
            CapsuleDetailTab::Api => render_capsule_api_page(
                state,
                active,
                active_web,
                canonical_handle,
                &network_logs,
                theme,
            ),
        })
}

fn render_capsule_overview_page(
    _state: &AppState,
    active: &crate::state::ActiveCapsulePane,
    route_label: &str,
    publisher: &str,
    session_label: &str,
    log_entries: &[crate::state::CapsuleLogEntry],
    network_logs: &[&crate::state::NetworkLogEntry],
    quick_open_url: Option<String>,
    theme: &Theme,
) -> Div {
    let unique_domains = unique_domain_count(network_logs);
    let latest_egress = network_logs
        .last()
        .map(|entry| entry.url.clone())
        .unwrap_or_else(|| "No egress observed".to_string());
    let storage_label = active
        .manifest_path
        .as_deref()
        .map(|_| "Manifest + state paths tracked")
        .unwrap_or("Storage mounts pending");

    div()
        .pt(px(8.0))
        .flex()
        .flex_col()
        .gap(px(16.0))
        .child(
            div()
                .flex()
                .gap(px(12.0))
                .children(vec![
                    render_capsule_summary_card(
                        "Runtime",
                        active.runtime_label.as_deref().unwrap_or("unknown runtime"),
                        active.display_strategy.as_deref().unwrap_or("foundation profile pending"),
                        theme,
                    )
                    .into_any_element(),
                    render_capsule_summary_card(
                        "Resources",
                        "CPU / Mem live telemetry",
                        "Collector not connected yet",
                        theme,
                    )
                    .into_any_element(),
                    render_capsule_summary_card(
                        "Network",
                        &format!("{} domains active", unique_domains),
                        &latest_egress,
                        theme,
                    )
                    .into_any_element(),
                    render_capsule_summary_card("Storage", storage_label, canonical_storage_hint(active), theme)
                        .into_any_element(),
                ]),
        )
        .child(render_capsule_section(
            "Identity",
            vec![
                render_capsule_detail_row("Capsule", &active.title, route_label, theme).into_any_element(),
                render_capsule_detail_row("Publisher", publisher, active.source_label.as_deref().unwrap_or("registry"), theme).into_any_element(),
                render_capsule_detail_row("Status", session_label, "Uptime and last launch timestamps surface here when runtime metrics are available.", theme).into_any_element(),
            ],
            theme,
        ))
        .child(render_capsule_section(
            "Quick actions",
            vec![
                render_capsule_detail_row("Start", route_label, "Re-dispatch the capsule route when the session is down.", theme).into_any_element(),
                render_capsule_detail_row("Restart", "Browser reload", "Uses the active pane reload flow today.", theme).into_any_element(),
                render_capsule_detail_row(
                    "Open",
                    quick_open_url.as_deref().unwrap_or("No external URL available"),
                    "Opens the served URL in the system browser when available.",
                    theme,
                )
                .into_any_element(),
            ],
            theme,
        ))
        .child(render_capsule_section(
            "Recent activity",
            if log_entries.is_empty() {
                vec![render_capsule_empty("No capsule activity recorded yet.", theme).into_any_element()]
            } else {
                log_entries
                    .iter()
                    .rev()
                    .map(|entry| render_capsule_activity_row(entry, theme).into_any_element())
                    .collect()
            },
            theme,
        ))
        .child(render_capsule_section(
            "Runtime notes",
            vec![
                render_capsule_detail_row(
                    "Foundation",
                    active.display_strategy.as_deref().unwrap_or("guest-webview"),
                    "Execution profile currently surfaced from launch metadata.",
                    theme,
                )
                .into_any_element(),
                render_capsule_detail_row(
                    "Manifest",
                    active.manifest_path.as_deref().unwrap_or("not resolved"),
                    "Resolved manifest path for the running capsule.",
                    theme,
                )
                .into_any_element(),
            ],
            theme,
        ))
}

fn render_capsule_permissions_page(
    state: &AppState,
    active: &crate::state::ActiveCapsulePane,
    active_web: Option<&crate::state::ActiveWebPane>,
    canonical_handle: &str,
    network_logs: &[&crate::state::NetworkLogEntry],
    theme: &Theme,
) -> Div {
    let granted_envs = state
        .secret_store
        .grants
        .get(canonical_handle)
        .cloned()
        .unwrap_or_default();
    let allowlist_detail = if state.config.sandbox.default_egress_allow.is_empty() {
        None
    } else {
        Some(state.config.sandbox.default_egress_allow.join(", "))
    };
    let capabilities = active_web
        .map(|pane| pane.capabilities.clone())
        .unwrap_or_default();

    div()
        .pt(px(8.0))
        .flex()
        .flex_col()
        .gap(px(16.0))
        .child(render_capsule_section(
            "Network",
            vec![
                render_capsule_detail_row(
                    "Egress allow",
                    if state.config.sandbox.default_egress_allow.is_empty() {
                        "No explicit allowlist"
                    } else {
                        "Configured allow hosts"
                    },
                    if state.config.sandbox.default_egress_allow.is_empty() {
                        "Block all egress remains the effective default."
                    } else {
                        allowlist_detail.as_deref().unwrap_or("")
                    },
                    theme,
                )
                .into_any_element(),
                render_capsule_detail_row(
                    "CIDR allow",
                    "None",
                    "No raw IP ranges are granted for this capsule yet.",
                    theme,
                )
                .into_any_element(),
                render_capsule_detail_row(
                    "Block all egress",
                    if state.config.sandbox.default_egress_allow.is_empty() {
                        "Enabled"
                    } else {
                        "Disabled"
                    },
                    "A kill switch lives here; wiring to mutable policy comes next.",
                    theme,
                )
                .into_any_element(),
            ]
            .into_iter()
            .chain(if network_logs.is_empty() {
                vec![
                    render_capsule_empty("No live connections captured for this capsule.", theme)
                        .into_any_element(),
                ]
            } else {
                network_logs
                    .iter()
                    .rev()
                    .take(6)
                    .map(|entry| {
                        render_capsule_detail_row(
                            entry.method.as_str(),
                            entry.url.as_str(),
                            &format!(
                                "status={} • {}{}",
                                entry
                                    .status
                                    .map(|code| code.to_string())
                                    .unwrap_or_else(|| "pending".to_string()),
                                entry.duration_ms.unwrap_or_default(),
                                if entry.url.contains("tail") {
                                    " ms • tailnet"
                                } else {
                                    " ms"
                                }
                            ),
                            theme,
                        )
                        .into_any_element()
                    })
                    .collect()
            })
            .collect(),
            theme,
        ))
        .child(render_capsule_section(
            "Filesystem",
            vec![
                render_capsule_detail_row(
                    "Manifest",
                    active.manifest_path.as_deref().unwrap_or("Unavailable"),
                    "Reveal and wipe flows land here once mount management is wired.",
                    theme,
                )
                .into_any_element(),
                render_capsule_detail_row(
                    "Logs",
                    active.log_path.as_deref().unwrap_or("Unavailable"),
                    "Current runtime log target for the capsule.",
                    theme,
                )
                .into_any_element(),
                render_capsule_detail_row(
                    "Additional paths",
                    "No extra mounts detected",
                    "Read-only and read-write path grants will appear here.",
                    theme,
                )
                .into_any_element(),
            ],
            theme,
        ))
        .child(render_capsule_section(
            "Environment",
            if granted_envs.is_empty() {
                vec![render_capsule_empty(
                    "No explicit env grants recorded for this capsule.",
                    theme,
                )
                .into_any_element()]
            } else {
                granted_envs
                    .iter()
                    .map(|key| {
                        render_capsule_detail_row(
                            key,
                            "••••••••",
                            "Reveal once / Edit / Remove flows can bind to this row later.",
                            theme,
                        )
                        .into_any_element()
                    })
                    .collect()
            },
            theme,
        ))
        .child(render_capsule_section(
            "Role / Capabilities",
            if capabilities.is_empty() {
                vec![render_capsule_empty(
                    "No capability grants surfaced for the active pane.",
                    theme,
                )
                .into_any_element()]
            } else {
                capabilities
                    .iter()
                    .map(|capability| {
                        render_capsule_detail_row(
                            capability.as_str(),
                            "granted",
                            if active.restricted {
                                "grant source: session • tier: Tier2 / unsafe gated"
                            } else {
                                "grant source: manifest • tier: Tier1"
                            },
                            theme,
                        )
                        .into_any_element()
                    })
                    .collect()
            },
            theme,
        ))
        .child(
            div()
                .flex()
                .justify_end()
                .child(render_capsule_footer_button(
                    "Reset to manifest defaults",
                    false,
                    theme,
                )),
        )
}

fn render_capsule_logs_page(
    active: &crate::state::ActiveCapsulePane,
    active_web: Option<&crate::state::ActiveWebPane>,
    log_entries: &[crate::state::CapsuleLogEntry],
    theme: &Theme,
) -> Div {
    let service_tabs = capsule_service_tabs(active, active_web);
    let error_count = log_entries
        .iter()
        .filter(|entry| entry.tone == crate::state::ActivityTone::Error)
        .count();

    div()
        .pt(px(8.0))
        .flex()
        .flex_col()
        .gap(px(16.0))
        .child(render_capsule_section(
            "Filter bar",
            vec![
                render_capsule_filter_row(
                    &[
                        "All services",
                        "error/warn/info/debug",
                        "Search logs",
                        "Last hour",
                    ],
                    theme,
                )
                .into_any_element(),
                render_capsule_service_tab_row(&service_tabs, error_count, theme)
                    .into_any_element(),
            ],
            theme,
        ))
        .child(render_capsule_section(
            "Log stream",
            if log_entries.is_empty() {
                vec![
                    render_capsule_empty("No log lines captured for this capsule yet.", theme)
                        .into_any_element(),
                ]
            } else {
                log_entries
                    .iter()
                    .rev()
                    .map(|entry| render_capsule_log_row(entry, active, theme).into_any_element())
                    .collect()
            },
            theme,
        ))
        .child(render_capsule_section(
            "Tail controls",
            vec![
                render_capsule_detail_row(
                    "Follow",
                    "off",
                    "tail -f mode can bind to this toggle once streaming logs land.",
                    theme,
                )
                .into_any_element(),
                render_capsule_detail_row(
                    "Actions",
                    "Copy / Export / Clear",
                    "Structured field expansion will appear in the detail rail for a selected row.",
                    theme,
                )
                .into_any_element(),
            ],
            theme,
        ))
}

fn render_capsule_update_page(
    state: &AppState,
    active: &crate::state::ActiveCapsulePane,
    version_label: &str,
    theme: &Theme,
) -> Div {
    div()
        .pt(px(8.0))
        .flex()
        .flex_col()
        .gap(px(16.0))
        .child(render_capsule_section(
            "Current",
            vec![
                render_capsule_detail_row(
                    "Version",
                    version_label,
                    active.trust_state.as_deref().unwrap_or("signature status pending"),
                    theme,
                )
                .into_any_element(),
                render_capsule_detail_row(
                    "Installed at",
                    active.manifest_path.as_deref().unwrap_or("unknown"),
                    "Install timestamp becomes available once history is persisted.",
                    theme,
                )
                .into_any_element(),
            ],
            theme,
        ))
        .child(render_capsule_section(
            "Available",
            vec![render_capsule_update_section(state.capsule_updates.get(&active.pane_id), theme)],
            theme,
        ))
        .child(render_capsule_section(
            "Policy",
            vec![
                render_capsule_detail_row(
                    "Update channel",
                    "stable",
                    "Per-capsule override will sit here: stable / beta / nightly / pinned.",
                    theme,
                )
                .into_any_element(),
                render_capsule_detail_row(
                    "Auto-update",
                    "off",
                    "Capsule-specific auto-update is visible here once writable policy lands.",
                    theme,
                )
                .into_any_element(),
            ],
            theme,
        ))
        .child(render_capsule_section(
            "Changelog",
            vec![render_capsule_empty(
                "Release notes markdown will render here after the registry exposes per-version changelogs.",
                theme,
            )
            .into_any_element()],
            theme,
        ))
        .child(render_capsule_section(
            "History",
            vec![render_capsule_empty(
                "Install history and rollback actions appear here once version history is persisted locally.",
                theme,
            )
            .into_any_element()],
            theme,
        ))
}

fn render_capsule_api_page(
    state: &AppState,
    active: &crate::state::ActiveCapsulePane,
    active_web: Option<&crate::state::ActiveWebPane>,
    canonical_handle: &str,
    network_logs: &[&crate::state::NetworkLogEntry],
    theme: &Theme,
) -> Div {
    let granted_envs = state
        .secret_store
        .grants
        .get(canonical_handle)
        .cloned()
        .unwrap_or_default();
    let inbound_rows: Vec<AnyElement> = [
        active.local_url.as_deref().map(|value| {
            render_capsule_detail_row(
                "Local URL",
                value,
                "Served endpoint for browser-based access.",
                theme,
            )
            .into_any_element()
        }),
        active.healthcheck_url.as_deref().map(|value| {
            render_capsule_detail_row(
                "Health",
                value,
                "Readiness probe published by the capsule.",
                theme,
            )
            .into_any_element()
        }),
        active.invoke_url.as_deref().map(|value| {
            render_capsule_detail_row(
                "Invoke",
                value,
                "Programmatic invoke route exposed by the guest.",
                theme,
            )
            .into_any_element()
        }),
    ]
    .into_iter()
    .flatten()
    .collect();

    div()
        .pt(px(8.0))
        .flex()
        .flex_col()
        .gap(px(16.0))
        .child(render_capsule_section(
            "Inbound",
            if inbound_rows.is_empty() {
                vec![render_capsule_empty(
                    "No inbound endpoints are exposed for this capsule.",
                    theme,
                )
                .into_any_element()]
            } else {
                inbound_rows
            },
            theme,
        ))
        .child(render_capsule_section(
            "Outbound",
            if granted_envs.is_empty() {
                vec![render_capsule_empty(
                    "No outbound credentials are bound to this capsule.",
                    theme,
                )
                .into_any_element()]
            } else {
                granted_envs
                    .iter()
                    .map(|key| {
                        render_capsule_detail_row(
                            key,
                            "••••••••",
                            "FD injection / env injection choice can be attached here.",
                            theme,
                        )
                        .into_any_element()
                    })
                    .collect()
            },
            theme,
        ))
        .child(render_capsule_section(
            "Schema registry",
            vec![render_capsule_detail_row(
                "Resolved runtime",
                active.runtime_label.as_deref().unwrap_or("unknown"),
                "std.* alias resolution and schema hash materialize in this block.",
                theme,
            )
            .into_any_element()],
            theme,
        ))
        .child(render_capsule_section(
            "IPC",
            vec![
                render_capsule_detail_row(
                    "Session",
                    active.session_id.as_deref().unwrap_or("no session id"),
                    "Disconnect session is attached to the broker row when IPC control lands.",
                    theme,
                )
                .into_any_element(),
                render_capsule_detail_row(
                    "Capabilities",
                    &active_web
                        .map(|pane| {
                            pane.capabilities
                                .iter()
                                .map(|capability| capability.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_else(|| "none".to_string()),
                    "Current broker grants for the active pane.",
                    theme,
                )
                .into_any_element(),
                render_capsule_detail_row(
                    "Recent requests",
                    &network_logs.len().to_string(),
                    "Recent outbound request activity is unified with the network stream above.",
                    theme,
                )
                .into_any_element(),
            ],
            theme,
        ))
}

fn render_capsule_section(title: &str, rows: Vec<AnyElement>, theme: &Theme) -> Div {
    div()
        .rounded(px(14.0))
        .bg(theme.settings_card_bg)
        .border_1()
        .border_color(theme.border_subtle)
        .p_4()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight(620.0))
                .text_color(theme.text_primary)
                .child(title.to_string()),
        )
        .children(rows)
}

fn render_capsule_detail_row(label: &str, value: &str, detail: &str, theme: &Theme) -> Div {
    div()
        .py(px(8.0))
        .border_b_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.03))
        .flex()
        .items_start()
        .justify_between()
        .gap(px(14.0))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(FontWeight(520.0))
                        .text_color(theme.text_primary)
                        .child(value.to_string()),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .line_height(px(16.0))
                        .text_color(theme.text_disabled)
                        .child(detail.to_string()),
                ),
        )
}

fn render_capsule_activity_row(entry: &crate::state::CapsuleLogEntry, theme: &Theme) -> Div {
    let tone_color = match entry.tone {
        crate::state::ActivityTone::Error => hsla(0.0 / 360.0, 0.65, 0.50, 1.0),
        crate::state::ActivityTone::Warning => hsla(38.0 / 360.0, 0.85, 0.50, 1.0),
        crate::state::ActivityTone::Info => theme.text_primary,
    };

    div()
        .rounded(px(10.0))
        .bg(theme.settings_body_bg)
        .border_1()
        .border_color(theme.settings_body_border)
        .p_3()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .child(
            div()
                .text_size(px(10.5))
                .text_color(theme.text_tertiary)
                .child(entry.stage.as_str()),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(tone_color)
                .child(entry.message.clone()),
        )
}

fn render_capsule_summary_card(title: &str, value: &str, detail: &str, theme: &Theme) -> Div {
    div()
        .flex_1()
        .min_w(px(180.0))
        .rounded(px(12.0))
        .bg(theme.settings_card_bg)
        .border_1()
        .border_color(theme.border_subtle)
        .p_3()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .text_size(px(10.5))
                .text_color(theme.text_tertiary)
                .child(title.to_string()),
        )
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight(620.0))
                .text_color(theme.text_primary)
                .child(value.to_string()),
        )
        .child(
            div()
                .text_size(px(10.5))
                .line_height(px(16.0))
                .text_color(theme.text_disabled)
                .child(detail.to_string()),
        )
}

fn render_capsule_empty(message: &str, theme: &Theme) -> Div {
    div()
        .rounded(px(10.0))
        .bg(theme.settings_body_bg)
        .border_1()
        .border_color(theme.settings_body_border)
        .p_3()
        .text_size(px(11.0))
        .line_height(px(18.0))
        .text_color(theme.text_disabled)
        .child(message.to_string())
}

fn render_capsule_filter_row(labels: &[&str], theme: &Theme) -> Div {
    div().flex().items_center().gap(px(8.0)).children(
        labels
            .iter()
            .map(|label| render_capsule_meta_pill(label, theme).into_any_element()),
    )
}

fn render_capsule_service_tab_row(
    service_tabs: &[String],
    error_count: usize,
    theme: &Theme,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .children(service_tabs.iter().enumerate().map(|(index, label)| {
            let text = if index == 0 && error_count > 0 {
                format!("{} ({})", label, error_count)
            } else {
                label.clone()
            };
            render_capsule_meta_pill(&text, theme).into_any_element()
        }))
}

fn render_capsule_log_row(
    entry: &crate::state::CapsuleLogEntry,
    active: &crate::state::ActiveCapsulePane,
    theme: &Theme,
) -> Div {
    let level = match entry.tone {
        crate::state::ActivityTone::Error => "error",
        crate::state::ActivityTone::Warning => "warn",
        crate::state::ActivityTone::Info => "info",
    };

    div()
        .rounded(px(10.0))
        .bg(theme.settings_body_bg)
        .border_1()
        .border_color(theme.settings_body_border)
        .p_3()
        .flex()
        .items_start()
        .gap(px(12.0))
        .child(
            div()
                .text_size(px(10.5))
                .text_color(theme.text_tertiary)
                .child("now"),
        )
        .child(
            div().text_size(px(10.5)).text_color(theme.accent).child(
                active
                    .served_by
                    .clone()
                    .or_else(|| active.adapter.clone())
                    .unwrap_or_else(|| "All".to_string()),
            ),
        )
        .child(
            div()
                .text_size(px(10.5))
                .text_color(theme.text_secondary)
                .child(level),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(theme.text_primary)
                .child(entry.message.clone()),
        )
}

fn render_capsule_meta_pill(label: &str, theme: &Theme) -> Div {
    div()
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(999.0))
        .bg(theme.settings_body_bg)
        .border_1()
        .border_color(theme.settings_body_border)
        .text_size(px(10.5))
        .text_color(theme.text_secondary)
        .child(label.to_string())
}

fn render_capsule_trust_pill(label: &str, accent: gpui::Hsla, theme: &Theme) -> Div {
    div()
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(999.0))
        .bg(theme.settings_body_bg)
        .border_1()
        .border_color(theme.settings_body_border)
        .text_size(px(10.5))
        .font_weight(FontWeight(650.0))
        .text_color(accent)
        .child(label.to_string())
}

fn render_capsule_header_button(
    label: &str,
    enabled: bool,
    action: Option<Box<dyn gpui::Action>>,
    theme: &Theme,
) -> Div {
    let button = div()
        .px(px(12.0))
        .py(px(7.0))
        .rounded(px(8.0))
        .bg(if enabled {
            theme.settings_card_bg
        } else {
            theme.settings_body_bg
        })
        .border_1()
        .border_color(theme.border_subtle)
        .text_size(px(12.0))
        .text_color(if enabled {
            theme.text_primary
        } else {
            theme.text_disabled
        })
        .child(label.to_string());

    match action {
        Some(action) if enabled => {
            button
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    cx.stop_propagation();
                    window.dispatch_action(action.boxed_clone(), cx);
                })
        }
        _ => button,
    }
}

fn render_capsule_footer_button(label: &str, enabled: bool, theme: &Theme) -> Div {
    div()
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .bg(theme.settings_body_bg)
        .border_1()
        .border_color(theme.settings_body_border)
        .text_size(px(12.0))
        .text_color(if enabled {
            theme.text_primary
        } else {
            theme.text_disabled
        })
        .child(label.to_string())
}

fn capsule_detail_tab_index(tab: CapsuleDetailTab) -> usize {
    match tab {
        CapsuleDetailTab::Overview => 1,
        CapsuleDetailTab::Permissions => 2,
        CapsuleDetailTab::Logs => 3,
        CapsuleDetailTab::Update => 4,
        CapsuleDetailTab::Api => 5,
    }
}

fn capsule_publisher_label(handle: &str) -> String {
    let trimmed = handle.trim_start_matches("capsule://");
    let mut parts = trimmed.split('/');
    let host = parts.next().unwrap_or("capsule");
    let publisher = parts.next().unwrap_or(host);
    format!("{} / {}", host, publisher)
}

fn capsule_trust_color(label: &str, theme: &Theme) -> gpui::Hsla {
    let normalized = label.to_ascii_lowercase();
    if normalized.contains("verified") || normalized.contains("trusted") {
        hsla(145.0 / 360.0, 0.68, 0.55, 1.0)
    } else if normalized.contains("untrusted") || normalized.contains("failed") {
        hsla(38.0 / 360.0, 0.88, 0.58, 1.0)
    } else {
        theme.text_secondary
    }
}

fn capsule_session_label(session: crate::state::WebSessionState) -> &'static str {
    match session {
        crate::state::WebSessionState::Detached => "stopped",
        crate::state::WebSessionState::Resolving => "starting",
        crate::state::WebSessionState::Materializing => "materializing",
        crate::state::WebSessionState::Launching => "running",
        crate::state::WebSessionState::Mounted => "running",
        crate::state::WebSessionState::Closed => "stopped",
        crate::state::WebSessionState::LaunchFailed => "failed",
    }
}

fn capsule_service_tabs(
    active: &crate::state::ActiveCapsulePane,
    active_web: Option<&crate::state::ActiveWebPane>,
) -> Vec<String> {
    let mut tabs = vec!["All".to_string()];
    if let Some(service) = active.served_by.as_deref() {
        tabs.push(service.to_string());
    }
    if let Some(adapter) = active.adapter.as_deref() {
        if !tabs.iter().any(|tab| tab == adapter) {
            tabs.push(adapter.to_string());
        }
    }
    if let Some(web) = active_web {
        if !tabs.iter().any(|tab| tab == &web.profile) {
            tabs.push(web.profile.clone());
        }
    }
    tabs
}

fn unique_domain_count(network_logs: &[&crate::state::NetworkLogEntry]) -> usize {
    let mut hosts = Vec::new();
    for entry in network_logs {
        if let Ok(url) = url::Url::parse(&entry.url) {
            if let Some(host) = url.host_str() {
                let host = host.to_string();
                if !hosts.iter().any(|existing| existing == &host) {
                    hosts.push(host);
                }
            }
        }
    }
    hosts.len()
}

fn canonical_storage_hint(active: &crate::state::ActiveCapsulePane) -> &str {
    if active.log_path.is_some() {
        "State mount size will surface alongside log + cache mounts."
    } else {
        "No persistent state paths have been surfaced yet."
    }
}

fn render_permission_prompt_overlay(state: &AppState, theme: &Theme) -> impl IntoElement {
    let Some(prompt) = state.active_permission_prompt() else {
        return div();
    };

    let command_label = prompt
        .command
        .as_deref()
        .map(|command| format!("Command: {command}"))
        .unwrap_or_else(|| "Command: capability probe".to_string());

    div()
        .absolute()
        .inset_0()
        .bg(hsla(0.0, 0.0, 0.0, 0.42))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(480.0))
                .max_w(px(560.0))
                .rounded(px(18.0))
                .bg(theme.panel_bg)
                .border_1()
                .border_color(theme.accent_border)
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.22),
                    offset: point(px(0.0), px(18.0)),
                    blur_radius: px(48.0),
                    spread_radius: px(0.0),
                }])
                .p_5()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(theme.text_primary)
                        .child("Permission Request"),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.text_secondary)
                        .child(format!(
                            "{} requested {}.",
                            prompt.route_label, prompt.capability
                        )),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(command_label),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(
                        "This overlay is drawn by the host. The guest cannot spoof or dismiss it.",
                    ),
                )
                .child(
                    div()
                        .mt_2()
                        .flex()
                        .gap_2()
                        .justify_end()
                        .child(render_permission_button(
                            "Allow Once",
                            theme.accent_subtle,
                            theme.accent_border,
                            theme.text_primary,
                            AllowPermissionOnce,
                        ))
                        .child(render_permission_button(
                            "Allow for Session",
                            theme.omnibar_rest_bg,
                            theme.omnibar_rest_border,
                            theme.text_primary,
                            AllowPermissionForSession,
                        ))
                        .child(render_permission_button(
                            "Deny",
                            theme.panel_bg,
                            theme.border_default,
                            theme.text_secondary,
                            DenyPermissionPrompt,
                        )),
                ),
        )
}

fn render_permission_button<A: gpui::Action + Clone + 'static>(
    label: &'static str,
    bg: gpui::Hsla,
    border: gpui::Hsla,
    text: gpui::Hsla,
    action: A,
) -> impl IntoElement {
    div()
        .rounded(px(10.0))
        .px_3()
        .py_2()
        .border_1()
        .border_color(border)
        .bg(bg)
        .cursor_pointer()
        .text_size(px(11.5))
        .text_color(text)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.dispatch_action(Box::new(action.clone()), cx);
        })
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::{
        determine_image_format, fetch_image_from_url_with_headers, transcode_ico_to_png,
        ImageFormat,
    };

    /// Real PNG header (8-byte magic + an IHDR chunk for a 1×1 image).
    /// Used to simulate the wire payload Google's gstatic and MDN
    /// favicon URLs serve under `Content-Type: image/x-icon`.
    const TINY_PNG_HEADER: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

    #[test]
    fn determine_format_prefers_sniff_over_lying_content_type() {
        // Google / MDN serve PNG bytes as `image/x-icon`. The browser
        // content-sniffs and renders PNG; we must do the same or
        // `transcode_ico_to_png` rejects the bytes ("Invalid reserved
        // field value in ICONDIR (was 20617, but must be 0)" — those
        // bytes are the first two of PNG magic).
        assert_eq!(
            determine_image_format(TINY_PNG_HEADER, Some("image/x-icon")),
            Some(ImageFormat::Png)
        );
        assert_eq!(
            determine_image_format(TINY_PNG_HEADER, Some("image/vnd.microsoft.icon")),
            Some(ImageFormat::Png)
        );
    }

    #[test]
    fn determine_format_returns_real_ico_when_bytes_are_ico() {
        // ICO directory header: reserved=0, type=1 (ICO), count=1.
        // The format-decision function must NOT regress for genuine ICOs.
        let mut ico = vec![0u8, 0, 1, 0, 1, 0];
        ico.extend_from_slice(&[0u8; 16]); // single dir entry
        assert_eq!(
            determine_image_format(&ico, Some("image/x-icon")),
            Some(ImageFormat::Ico)
        );
    }

    #[test]
    fn determine_format_falls_back_to_content_type_when_sniff_fails() {
        // Empty / unrecognized bytes must still resolve to a format
        // when the server declares one. Otherwise a slow / chunked
        // response that hasn't streamed magic bytes yet would 404.
        let unknown = b"unrecognized";
        assert_eq!(
            determine_image_format(unknown, Some("image/png")),
            Some(ImageFormat::Png)
        );
        assert_eq!(determine_image_format(unknown, None), None);
    }

    #[test]
    fn determine_format_recognizes_svg_regardless_of_content_type() {
        // Some Vite dev servers and a few CDNs serve SVG as
        // `application/octet-stream` or `text/plain`. Sniffing keeps
        // those rendering instead of failing the format-decision step.
        let svg = b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        assert_eq!(
            determine_image_format(svg, Some("application/octet-stream")),
            Some(ImageFormat::Svg)
        );
    }

    /// Build a real ICO with a single 4×4 BGRA entry using the `ico`
    /// crate as the encoder, then verify our decoder round-trips it
    /// back to a non-empty PNG. Using the same crate to encode/decode
    /// keeps the fixture independent of any browser-specific quirks
    /// while still exercising the DIB-encoded path that browsers like
    /// Google's gstatic favicon and MDN's ICO actually use (i.e. the
    /// path the prior `image`-crate-only normalization rejected).
    #[test]
    fn round_trips_dib_encoded_ico_to_png() {
        let mut rgba = Vec::with_capacity(4 * 4 * 4);
        for y in 0..4 {
            for x in 0..4 {
                rgba.extend_from_slice(&[(x * 60) as u8, (y * 60) as u8, 0, 255]);
            }
        }
        let icon_image = ico::IconImage::from_rgba_data(4, 4, rgba);
        let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
        dir.add_entry(ico::IconDirEntry::encode_as_bmp(&icon_image).expect("encode BMP entry"));
        let mut ico_bytes = Vec::new();
        dir.write(&mut ico_bytes).expect("write ICO");

        let png = transcode_ico_to_png(&ico_bytes).expect("transcode succeeded");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn picks_largest_entry_when_multiple_resolutions_exist() {
        let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
        for size in [4u32, 8, 16] {
            let pixels = (0..size * size * 4).map(|i| i as u8).collect();
            let img = ico::IconImage::from_rgba_data(size, size, pixels);
            dir.add_entry(ico::IconDirEntry::encode_as_bmp(&img).expect("encode entry"));
        }
        let mut ico_bytes = Vec::new();
        dir.write(&mut ico_bytes).expect("write ICO");

        let png = transcode_ico_to_png(&ico_bytes).expect("transcode succeeded");
        // Decode the produced PNG and assert the dimensions match the
        // largest entry (16×16) — proves we picked the right one.
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("decode produced PNG");
        assert_eq!(decoded.width(), 16);
        assert_eq!(decoded.height(), 16);
    }

    #[test]
    fn rejects_garbage_bytes() {
        assert!(transcode_ico_to_png(b"not an ICO").is_none());
        assert!(transcode_ico_to_png(&[]).is_none());
    }

    /// Live regression tests against the actual Google and MDN
    /// `/favicon.ico` URLs that triggered the original failure: both
    /// servers return PNG bytes labeled `Content-Type: image/x-icon`,
    /// so the icon-fetch pipeline must content-sniff and produce an
    /// `Arc<Image>` with `format == Png`.
    ///
    /// Marked `#[ignore]` because they require network access — run
    /// locally with `cargo test --bin ato-desktop -- --ignored
    /// google_or_mdn`. CI is unaffected by transient outages on the
    /// remote servers.
    #[test]
    #[ignore = "requires network access to gstatic.com / developer.mozilla.org"]
    fn google_or_mdn_favicon_url_resolves_to_png_image() {
        for url in [
            "https://www.gstatic.com/images/branding/searchlogo/ico/favicon.ico",
            "https://developer.mozilla.org/favicon.ico",
        ] {
            let image = fetch_image_from_url_with_headers(url, /*reject_non_image=*/ true)
                .unwrap_or_else(|| panic!("expected to resolve favicon for {url}"));
            assert_eq!(
                image.format,
                ImageFormat::Png,
                "expected PNG bytes after sniff for {url} (server lies via image/x-icon)"
            );
            assert!(
                !image.bytes.is_empty(),
                "favicon body for {url} unexpectedly empty"
            );
            assert!(
                image.bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                "favicon body for {url} did not start with PNG magic"
            );
        }
    }

    /// Same idea as the favicon test but against ato.run's SVG
    /// favicon: the pipeline must run it through `render_svg_to_png`
    /// and return a PNG-formatted `Arc<Image>` (not a raw SVG, which
    /// the pinned `gpui` rev mishandles).
    #[test]
    #[ignore = "requires network access to ato.run"]
    fn ato_run_svg_favicon_url_resolves_to_png_image() {
        let url = "https://ato.run/favicon.svg";
        let image = fetch_image_from_url_with_headers(url, /*reject_non_image=*/ true)
            .unwrap_or_else(|| panic!("expected to resolve favicon for {url}"));
        assert_eq!(image.format, ImageFormat::Png);
        assert!(image.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}

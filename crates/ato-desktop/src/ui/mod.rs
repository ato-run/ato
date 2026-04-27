mod chrome;
mod modals;
mod panels;
mod share;
mod sidebar;
mod theme;

use theme::Theme;

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, hsla, linear_color_stop, linear_gradient, point, px, AsyncWindowContext, BoxShadow,
    Context, Entity, ExternalPaths, FocusHandle, Focusable, FontWeight, Image, ImageFormat,
    IntoElement, MouseButton, Render, WeakEntity, Window,
};
use gpui_component::input::{InputEvent, InputState};

use self::chrome::render_command_chrome;
use self::panels::render_stage;
use self::sidebar::{favicon_request_url, render_task_rail, FaviconState};

use crate::app::{
    AllowPermissionForSession, AllowPermissionOnce, BrowserBack, BrowserForward, BrowserReload,
    CancelAuthHandoff, CancelConfigForm, CancelQuit, CloseTask, ConfirmQuitClear, ConfirmQuitKeep,
    CycleHandle, DenyPermissionPrompt, DismissTransient, ExpandSplit, FocusCommandBar, MoveTask,
    NativeCopy, NativeCut, NativePaste, NativeRedo, NativeSelectAll, NativeUndo, NavigateToUrl,
    NewTab, NextTask, NextWorkspace, OpenAuthInBrowser, OpenCloudDock, OpenLocalRegistry,
    OpenUrlBridge, PreviousTask, PreviousWorkspace, Quit, ResumeAfterAuth, SaveConfigForm,
    SelectTask, ShowSettings, ShrinkSplit, SignInToAtoRun, SignOut, SplitPane,
    ToggleAutoDevtools, ToggleDevConsole, ToggleTheme,
};
use crate::orchestrator::cleanup_stale_capsule_sessions;
use crate::state::{
    ActivityTone, AppState, AuthSessionStatus, PaneBounds, PaneId, PaneSurface, ShellMode,
    SidebarTaskIconSpec,
};
use crate::terminal::TerminalSessionManager;
use crate::webview::WebViewManager;
use capsule_wire::config::ConfigKind;

pub(super) const CHROME_HEIGHT: f32 = 48.0;
pub(super) const RAIL_WIDTH: f32 = 52.0;
pub(super) const STAGE_PADDING: f32 = 0.0;

const DEVTOOLS_DEBUG_ENV: &str = "ATO_DESKTOP_DEVTOOLS_DEBUG";
const DEVTOOLS_RESYNC_DELAYS_MS: &[u64] = &[32, 96, 192];

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
    /// Lazy-allocated by `render` whenever `state.pending_config`
    /// flips from `None → Some` (or to a different request). Owns
    /// the per-field `InputState` entities so keystroke/cursor state
    /// survives across re-renders. Dropped when `pending_config`
    /// returns to `None`.
    config_modal: Option<modals::config_form::ConfigModal>,
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
        let launcher_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search, command, or ask AI…")
        });
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
            config_modal: None,
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
        let current = self.state.config.auto_open_devtools;
        self.state
            .update_config(|c| c.auto_open_devtools = !current);
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
        self.state.show_settings_panel();
        self.sync_omnibar_with_state(window, cx, true);
        self.sync_focus_target(window, cx);
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
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane_id) = self.find_active_auth_handoff_pane_id() {
            // Look up session_id from the pane surface
            let start_url = self.state.active_panes().iter().find_map(|p| {
                if p.id == pane_id {
                    if let PaneSurface::AuthHandoff { session_id, .. } = &p.surface {
                        self.state
                            .auth_sessions
                            .iter()
                            .find(|s| &s.session_id == session_id)
                            .map(|s| s.start_url.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            });
            if let Some(url) = start_url {
                let _ = std::process::Command::new("open").arg(&url).status();
                // Update session status
                if let Some(pane) = self.state.active_panes().iter().find(|p| p.id == pane_id) {
                    if let PaneSurface::AuthHandoff { session_id, .. } = &pane.surface {
                        let sid = session_id.clone();
                        if let Some(s) = self
                            .state
                            .auth_sessions
                            .iter_mut()
                            .find(|s| s.session_id == sid)
                        {
                            s.status = AuthSessionStatus::OpenedInBrowser;
                        }
                    }
                }
            }
        }
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
        for (key, value) in secret_writes {
            self.state.add_secret(key.clone(), value);
            self.state.grant_secret_to_capsule(&handle, &key);
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

    fn sync_favicons(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let origins = self
            .state
            .sidebar_task_items()
            .into_iter()
            .filter_map(|task| match task.icon {
                SidebarTaskIconSpec::ExternalUrl { origin } => Some(origin),
                SidebarTaskIconSpec::Monogram(_) | SidebarTaskIconSpec::SystemIcon(_) => None,
            })
            .collect::<Vec<_>>();

        for origin in origins {
            if self.favicon_cache.contains_key(&origin) {
                continue;
            }

            self.favicon_cache
                .insert(origin.clone(), FaviconState::Loading);
            self.spawn_favicon_fetch(origin, window, cx);
        }
    }

    fn spawn_favicon_fetch(&mut self, origin: String, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(
            window,
            move |this: WeakEntity<Self>, cx: &mut AsyncWindowContext| {
                let mut async_cx = cx.clone();
                async move {
                    let origin_for_fetch = origin.clone();
                    let image = async_cx
                        .background_spawn(async move { fetch_favicon_image(&origin_for_fetch) })
                        .await;

                    let _ = this.update_in(&mut async_cx, move |this, _window, cx| {
                        this.favicon_cache.insert(
                            origin,
                            match image {
                                Some(image) => FaviconState::Ready(image),
                                None => FaviconState::Failed,
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
}

impl Render for DesktopShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handled_open_urls = self.drain_open_urls();
        let size = window.bounds().size;
        let stage_bounds =
            compute_stage_bounds(&self.state, f32::from(size.width), f32::from(size.height));
        self.state.set_active_bounds(stage_bounds);
        self.webviews.sync_from_state(window, &mut self.state);
        self.sync_omnibar_with_state(window, cx, false);
        if handled_open_urls {
            self.sync_focus_target(window, cx);
        }
        self.sync_favicons(window, cx);
        self.poll_capsule_search();
        self.sync_config_modal(window, cx);
        let omnibar_value = self.omnibar.read(cx).value().to_string();
        self.maybe_trigger_capsule_search(&omnibar_value);
        let omnibar_suggestions = self.state.omnibar_suggestions(&omnibar_value);
        let active_pane_count = self.state.active_panes().len();
        let command_bar = matches!(self.state.shell_mode, ShellMode::CommandBar);
        // Hide the active WebView while the omnibar is open with
        // suggestions so the dropdown can paint above the WKWebView
        // NSView (which always sits on top of GPUI's CALayer tree).
        let hide_for_omnibar = command_bar && !omnibar_suggestions.is_empty();
        self.webviews
            .set_overlay_hides_webview(hide_for_omnibar, &mut self.state);
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
                    .when(self.config_modal.is_some(), |this| {
                        // The modal renders only when AppState requested
                        // it AND `sync_config_modal` has populated the
                        // local entity — both must be true. The
                        // `Option::as_ref().unwrap()` is safe because the
                        // `is_some()` guard above runs before this child
                        // call inside `.when`.
                        let modal = self
                            .config_modal
                            .as_ref()
                            .expect("config_modal checked above");
                        this.child(modals::config_form::render_config_modal_overlay(
                            modal, &theme,
                        ))
                    }),
            );

        div()
            .key_context("DeskyShell")
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
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_cancel_quit))
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
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(text_secondary)
                        .child(
                            "Keep your current tabs for the next launch, or clear them and start fresh?",
                        ),
                )
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
        QuitDialogButtonKind::Neutral => {
            (theme.surface_hover, theme.text_primary, theme.border_default)
        }
        QuitDialogButtonKind::Danger => {
            (theme.surface_hover, hsla(0.0, 0.7, 0.5, 1.0), theme.border_default)
        }
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

fn fetch_favicon_image(origin: &str) -> Option<Arc<Image>> {
    let request_url = favicon_request_url(origin)?;
    let response = ureq::get(&request_url).call().ok()?;
    let content_type = response
        .header("content-type")
        .or_else(|| response.header("Content-Type"))
        .map(|value| value.split(';').next().unwrap_or(value).trim().to_string());

    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes).ok()?;
    if bytes.is_empty() {
        return None;
    }

    let format = content_type
        .as_deref()
        .and_then(image_format_from_content_type)
        .or_else(|| sniff_image_format(&bytes))
        .unwrap_or(ImageFormat::Ico);

    Some(Arc::new(Image::from_bytes(format, bytes)))
}

fn image_format_from_content_type(content_type: &str) -> Option<ImageFormat> {
    match content_type {
        "image/x-icon" | "image/vnd.microsoft.icon" => Some(ImageFormat::Ico),
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

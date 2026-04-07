mod chrome;
mod panels;
mod share;
mod sidebar;

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, hsla, linear_color_stop, linear_gradient, point, px, rgb, AsyncWindowContext, BoxShadow,
    Context, Entity, FocusHandle, Focusable, Image, ImageFormat, IntoElement, Render,
    WeakEntity, Window,
};
use gpui_component::input::{InputEvent, InputState};

use self::chrome::render_command_chrome;
use self::panels::render_stage;
use self::share::render_preview_card;
use self::sidebar::{favicon_request_url, render_task_rail, FaviconState};

use crate::app::{
    BrowserBack, BrowserForward, BrowserReload, CycleHandle, DismissTransient, ExpandSplit,
    FocusCommandBar, NavigateToUrl, NewTab, NextTask, NextWorkspace, PreviousTask,
    PreviousWorkspace, SelectTask, ShrinkSplit, SplitPane, ToggleOverview,
    ShowSettings,
};
use crate::orchestrator::cleanup_stale_guest_sessions;
use crate::state::{ActivityTone, AppState, PaneBounds, ShellMode, SidebarTaskIconSpec};
use crate::webview::WebViewManager;

pub(super) const CHROME_HEIGHT: f32 = 48.0;
pub(super) const RAIL_WIDTH: f32 = 52.0;
pub(super) const STAGE_PADDING: f32 = 0.0;
pub(super) const OVERVIEW_HEIGHT: f32 = 210.0;

pub struct DesktopShell {
    state: AppState,
    omnibar: Entity<InputState>,
    focus_handle: FocusHandle,
    favicon_cache: HashMap<String, FaviconState>,
    webviews: WebViewManager,
}

impl DesktopShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut state = AppState::demo();
        let focus_handle = cx.focus_handle();
        let omnibar = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("")
                .default_value(state.command_bar_text.clone())
        });
        match cleanup_stale_guest_sessions() {
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
        window.focus(&focus_handle, cx);

        cx.subscribe_in(
            &omnibar,
            window,
            |this: &mut Self, omnibar, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let url = omnibar.read(cx).value().to_string();
                    window.dispatch_action(Box::new(NavigateToUrl { url }), cx);
                }
                InputEvent::Change | InputEvent::Focus | InputEvent::Blur => {
                    this.sync_omnibar_with_state(window, cx, false);
                    cx.notify();
                }
            },
        )
        .detach();

        cx.observe_window_bounds(window, |this, window, cx| {
            let size = window.bounds().size;
            let stage =
                compute_stage_bounds(&this.state, f32::from(size.width), f32::from(size.height));
            this.state.set_active_bounds(stage);
            this.webviews.sync_from_state(window, &mut this.state);
            this.sync_omnibar_with_state(window, cx, false);
            cx.notify();
        })
        .detach();

        Self {
            state,
            omnibar,
            focus_handle,
            favicon_cache: HashMap::new(),
            webviews,
        }
    }

    fn on_focus_command_bar(
        &mut self,
        _: &FocusCommandBar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.focus_command_bar();
        self.sync_omnibar_with_state(window, cx, true);
        window.focus(&self.omnibar.focus_handle(cx), cx);
        cx.notify();
    }

    fn on_toggle_overview(&mut self, _: &ToggleOverview, _: &mut Window, cx: &mut Context<Self>) {
        self.state.toggle_overview();
        cx.notify();
    }

    fn on_next_workspace(&mut self, _: &NextWorkspace, _: &mut Window, cx: &mut Context<Self>) {
        self.state.next_workspace();
        cx.notify();
    }

    fn on_previous_workspace(
        &mut self,
        _: &PreviousWorkspace,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.previous_workspace();
        cx.notify();
    }

    fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.state.create_new_tab();
        self.sync_omnibar_with_state(window, cx, false);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn on_show_settings(
        &mut self,
        _: &ShowSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.show_settings_panel();
        self.sync_omnibar_with_state(window, cx, true);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn on_select_task(
        &mut self,
        action: &SelectTask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.select_task(action.task_id);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn on_navigate_to_url(
        &mut self,
        action: &NavigateToUrl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.navigate_to_url(&action.url);
        self.sync_omnibar_with_state(window, cx, true);
        cx.notify();
    }

    fn on_previous_task(
        &mut self,
        _: &PreviousTask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.previous_task();
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn on_next_task(&mut self, _: &NextTask, window: &mut Window, cx: &mut Context<Self>) {
        self.state.next_task();
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn on_split_pane(&mut self, _: &SplitPane, _: &mut Window, cx: &mut Context<Self>) {
        self.state.split_pane();
        cx.notify();
    }

    fn on_expand_split(&mut self, _: &ExpandSplit, _: &mut Window, cx: &mut Context<Self>) {
        self.state.expand_split();
        cx.notify();
    }

    fn on_shrink_split(&mut self, _: &ShrinkSplit, _: &mut Window, cx: &mut Context<Self>) {
        self.state.shrink_split();
        cx.notify();
    }

    fn on_dismiss_transient(
        &mut self,
        _: &DismissTransient,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.dismiss_transient();
        cx.notify();
    }

    fn on_cycle_handle(&mut self, _: &CycleHandle, _: &mut Window, cx: &mut Context<Self>) {
        self.state.cycle_handle();
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

    fn sync_favicons(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let origins = self
            .state
            .sidebar_task_items()
            .into_iter()
            .filter_map(|task| match task.icon {
                SidebarTaskIconSpec::ExternalUrl { origin } => Some(origin),
                SidebarTaskIconSpec::Monogram(_) => None,
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
}

impl Render for DesktopShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = window.bounds().size;
        let stage_bounds =
            compute_stage_bounds(&self.state, f32::from(size.width), f32::from(size.height));
        self.state.set_active_bounds(stage_bounds);
        self.webviews.sync_from_state(window, &mut self.state);
        self.sync_omnibar_with_state(window, cx, false);
        self.sync_favicons(window, cx);
        let omnibar_value = self.omnibar.read(cx).value().to_string();
        let omnibar_suggestions = self.state.omnibar_suggestions(&omnibar_value);
        let active_pane_count = self.state.active_panes().len();
        let overview = matches!(self.state.shell_mode, ShellMode::Overview);
        let command_bar = matches!(self.state.shell_mode, ShellMode::CommandBar);

        let body = div()
            .flex_1()
            .size_full()
            .flex()
            .overflow_hidden()
            .child(render_task_rail(&self.state, &self.favicon_cache))
            .child(
                div()
                    .flex_1()
                    .size_full()
                    .relative()
                    .flex()
                    .flex_col()
                    .child(render_stage(&self.state, stage_bounds, active_pane_count))
                    .when(overview, |this| {
                        this.child(render_overview_overlay(&self.state))
                    }),
            );

        div()
            .key_context("DeskyShell")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            // Base background matching mock: #1a1a1e
            .bg(rgb(0x1a1a1e))
            .text_color(rgb(0xf0f0f2))
            .on_action(cx.listener(Self::on_focus_command_bar))
            .on_action(cx.listener(Self::on_toggle_overview))
            .on_action(cx.listener(Self::on_show_settings))
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_select_task))
            .on_action(cx.listener(Self::on_navigate_to_url))
            .on_action(cx.listener(Self::on_next_workspace))
            .on_action(cx.listener(Self::on_previous_workspace))
            .on_action(cx.listener(Self::on_next_task))
            .on_action(cx.listener(Self::on_previous_task))
            .on_action(cx.listener(Self::on_split_pane))
            .on_action(cx.listener(Self::on_expand_split))
            .on_action(cx.listener(Self::on_shrink_split))
            .on_action(cx.listener(Self::on_dismiss_transient))
            .on_action(cx.listener(Self::on_cycle_handle))
            .on_action(cx.listener(Self::on_browser_back))
            .on_action(cx.listener(Self::on_browser_forward))
            .on_action(cx.listener(Self::on_browser_reload))
            // Subtle ambient glow at the top
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(200.0))
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(hsla(220.0 / 360.0, 0.30, 0.20, 0.20), 0.),
                        linear_color_stop(hsla(220.0 / 360.0, 0.30, 0.20, 0.0), 1.),
                    )),
            )
            .child(render_command_chrome(
                window,
                &self.state,
                &self.omnibar,
                &omnibar_value,
                &omnibar_suggestions,
                command_bar,
            ))
            .child(body)
    }
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

fn compute_stage_bounds(state: &AppState, width: f32, height: f32) -> PaneBounds {
    let overview_height = if matches!(state.shell_mode, ShellMode::Overview) {
        OVERVIEW_HEIGHT
    } else {
        0.0
    };

    PaneBounds {
        x: RAIL_WIDTH + STAGE_PADDING,
        y: CHROME_HEIGHT + STAGE_PADDING,
        width: (width - RAIL_WIDTH - STAGE_PADDING * 2.0).max(240.0),
        height: (height - CHROME_HEIGHT - STAGE_PADDING * 2.0 - overview_height).max(180.0),
    }
}

/// Overview overlay matching the mock's workspace-overlay with centered
/// focus card and horizontal task rail.
fn render_overview_overlay(state: &AppState) -> impl IntoElement {
    let tasks = state
        .active_workspace()
        .map(|workspace| workspace.tasks.clone())
        .unwrap_or_default();
    let active_id = state.active_task().map(|task| task.id).unwrap_or_default();

    div()
        .absolute()
        .inset_0()
        // Semi-transparent backdrop matching mock's rgba(0,0,0,0.6)
        .bg(hsla(0.0, 0.0, 0.0, 0.60))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(980.0))
                .max_w_full()
                .flex()
                .flex_col()
                .gap_5()
                // Header
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            div()
                                .text_size(px(15.0))
                                .text_color(hsla(0.0, 0.0, 1.0, 0.32))
                                .child("⧉"),
                        )
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(gpui::FontWeight(500.0))
                                .text_color(hsla(0.0, 0.0, 1.0, 0.55))
                                .child("Workspaces"),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(hsla(0.0, 0.0, 1.0, 0.32))
                                .bg(hsla(0.0, 0.0, 1.0, 0.05))
                                .px_2()
                                .py(px(2.0))
                                .rounded(px(10.0))
                                .child(format!("{}", tasks.len())),
                        ),
                )
                // Task cards
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .children(tasks.into_iter().map(move |task| {
                            let is_active = task.id == active_id;
                            div()
                                .flex_1()
                                .rounded(px(18.0))
                                .bg(if is_active {
                                    hsla(225.0 / 360.0, 0.18, 0.18, 0.96)
                                } else {
                                    hsla(240.0 / 360.0, 0.06, 0.16, 0.96)
                                })
                                .border_1()
                                .when(is_active, |this| {
                                    this.border_2().border_color(rgb(0x3b82f6)).shadow(vec![
                                        BoxShadow {
                                            color: hsla(217.0 / 360.0, 0.88, 0.60, 0.15),
                                            offset: point(px(0.), px(0.)),
                                            blur_radius: px(20.),
                                            spread_radius: px(0.),
                                        },
                                        BoxShadow {
                                            color: hsla(0.0, 0.0, 0.0, 0.5),
                                            offset: point(px(0.), px(20.)),
                                            blur_radius: px(60.),
                                            spread_radius: px(0.),
                                        },
                                    ])
                                })
                                .when(!is_active, |this| {
                                    this.border_color(hsla(0.0, 0.0, 1.0, 0.06)).shadow(vec![
                                        BoxShadow {
                                            color: hsla(0.0, 0.0, 0.0, 0.4),
                                            offset: point(px(0.), px(12.)),
                                            blur_radius: px(32.),
                                            spread_radius: px(0.),
                                        },
                                    ])
                                })
                                .overflow_hidden()
                                .cursor_pointer()
                                .p_4()
                                .child(render_preview_card(is_active, &task.title, &task.preview))
                        })),
                ),
        )
}

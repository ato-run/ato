mod chrome;
mod panels;
mod share;
mod sidebar;

use gpui::prelude::*;
use gpui::{
    div, hsla, linear_color_stop, linear_gradient, point, px, rgb, BoxShadow, Context,
    IntoElement, Render, Window,
};

use self::chrome::render_command_chrome;
use self::panels::render_stage;
use self::share::render_preview_card;
use self::sidebar::render_workspace_rail;

use crate::app::{
    CycleHandle, DismissTransient, ExpandSplit, FocusCommandBar, NextTask, NextWorkspace,
    PreviousTask, PreviousWorkspace, ShrinkSplit, SplitPane, ToggleOverview,
};
use crate::orchestrator::cleanup_stale_guest_sessions;
use crate::state::{ActivityTone, AppState, PaneBounds, ShellMode};
use crate::webview::WebViewManager;

pub(super) const CHROME_HEIGHT: f32 = 48.0;
pub(super) const RAIL_WIDTH: f32 = 52.0;
pub(super) const STAGE_PADDING: f32 = 16.0;
pub(super) const OVERVIEW_HEIGHT: f32 = 210.0;

pub struct DesktopShell {
    state: AppState,
    webviews: WebViewManager,
}

impl DesktopShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut state = AppState::demo();
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

        let mut webviews = WebViewManager::new();
        let size = window.bounds().size;
        let stage = compute_stage_bounds(&state, f32::from(size.width), f32::from(size.height));
        state.set_active_bounds(stage);
        webviews.sync_from_state(window, &mut state);

        cx.observe_window_bounds(window, |this, window, cx| {
            let size = window.bounds().size;
            let stage =
                compute_stage_bounds(&this.state, f32::from(size.width), f32::from(size.height));
            this.state.set_active_bounds(stage);
            this.webviews.sync_from_state(window, &mut this.state);
            cx.notify();
        })
        .detach();

        Self { state, webviews }
    }

    fn on_focus_command_bar(
        &mut self,
        _: &FocusCommandBar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.focus_command_bar();
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

    fn on_next_task(&mut self, _: &NextTask, _: &mut Window, cx: &mut Context<Self>) {
        self.state.next_task();
        cx.notify();
    }

    fn on_previous_task(&mut self, _: &PreviousTask, _: &mut Window, cx: &mut Context<Self>) {
        self.state.previous_task();
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
}

impl Render for DesktopShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = window.bounds().size;
        let stage_bounds =
            compute_stage_bounds(&self.state, f32::from(size.width), f32::from(size.height));
        self.state.set_active_bounds(stage_bounds);
        self.webviews.sync_from_state(window, &mut self.state);

        let task_title = self
            .state
            .active_task()
            .map(|task| task.title.clone())
            .unwrap_or_default();
        let active_route = self
            .state
            .active_web_pane()
            .map(|pane| pane.route.to_string())
            .unwrap_or_default();
        let active_pane_count = self.state.active_panes().len();
        let overview = matches!(self.state.shell_mode, ShellMode::Overview);
        let command_bar = matches!(self.state.shell_mode, ShellMode::CommandBar);

        let body = div()
            .flex_1()
            .flex()
            .overflow_hidden()
            .child(render_workspace_rail(&self.state))
            .child(
                div()
                    .flex_1()
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
            .size_full()
            .font_family("Geist")
            // Base background matching mock: #1a1a1e
            .bg(rgb(0x1a1a1e))
            .text_color(rgb(0xf0f0f2))
            .on_action(cx.listener(Self::on_focus_command_bar))
            .on_action(cx.listener(Self::on_toggle_overview))
            .on_action(cx.listener(Self::on_next_workspace))
            .on_action(cx.listener(Self::on_previous_workspace))
            .on_action(cx.listener(Self::on_next_task))
            .on_action(cx.listener(Self::on_previous_task))
            .on_action(cx.listener(Self::on_split_pane))
            .on_action(cx.listener(Self::on_expand_split))
            .on_action(cx.listener(Self::on_shrink_split))
            .on_action(cx.listener(Self::on_dismiss_transient))
            .on_action(cx.listener(Self::on_cycle_handle))
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
                command_bar,
                &task_title,
                &active_route,
            ))
            .child(body)
    }
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
                                    this.border_2()
                                        .border_color(rgb(0x3b82f6))
                                        .shadow(vec![
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
                                    this.border_color(hsla(0.0, 0.0, 1.0, 0.06))
                                        .shadow(vec![BoxShadow {
                                            color: hsla(0.0, 0.0, 0.0, 0.4),
                                            offset: point(px(0.), px(12.)),
                                            blur_radius: px(32.),
                                            spread_radius: px(0.),
                                        }])
                                })
                                .overflow_hidden()
                                .cursor_pointer()
                                .p_4()
                                .child(render_preview_card(
                                    is_active,
                                    &task.title,
                                    &task.preview,
                                ))
                        })),
                ),
        )
}

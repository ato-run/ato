mod auth_handoff;
mod inspector;
mod launcher;
mod launcher_v2;
mod settings;

use gpui::prelude::*;
use gpui::{
    div, linear_color_stop, linear_gradient, point, px, AnyElement, BoxShadow, IntoElement,
};
use gpui_component::resizable::h_resizable;
use gpui_component::skeleton::Skeleton;

use crate::state::{AppState, PaneBounds, PaneSurface, WebPane, WebSessionState};

use super::theme::Theme;
use super::STAGE_PADDING;
use auth_handoff::render_auth_handoff_panel;
use inspector::render_capsule_inspector_panel;
use launcher_v2::render_launcher_panel_v2;
use settings::render_settings_panel;

pub(super) fn render_stage(
    state: &AppState,
    _stage_bounds: PaneBounds,
    active_pane_count: usize,
    theme: &Theme,
) -> impl IntoElement {
    let panes = state.active_panes();
    let has_split = active_pane_count > 1;

    let content = if has_split {
        panes
            .iter()
            .fold(h_resizable("stage-panes"), |group, pane| {
                group.child(render_stage_pane(pane, state, theme))
            })
            .into_any_element()
    } else {
        div()
            .flex()
            .flex_1()
            .size_full()
            .child(
                div().flex().flex_1().size_full().relative().child(
                    panes
                        .first()
                        .map(|pane| render_stage_pane(pane, state, theme))
                        .unwrap_or_else(|| div().flex_1().into_any_element()),
                ),
            )
            .into_any_element()
    };

    let stage_shadow_far = theme.stage_shadow_far;
    let stage_shadow_near = theme.stage_shadow_near;

    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .size_full()
        .m(px(STAGE_PADDING))
        .rounded(px(14.0))
        .bg(theme.stage_bg)
        .border_1()
        .border_color(theme.stage_border)
        .shadow(vec![
            BoxShadow {
                color: stage_shadow_far,
                offset: point(px(0.), px(8.)),
                blur_radius: px(40.),
                spread_radius: px(0.),
            },
            BoxShadow {
                color: stage_shadow_near,
                offset: point(px(0.), px(1.)),
                blur_radius: px(3.),
                spread_radius: px(0.),
            },
        ])
        .overflow_hidden()
        .child(content)
}

fn render_stage_pane(pane: &crate::state::Pane, state: &AppState, theme: &Theme) -> AnyElement {
    match &pane.surface {
        PaneSurface::Web(web) => render_web_pane(web, theme).into_any_element(),
        PaneSurface::Native { body } => {
            render_settings_panel(body, state, theme).into_any_element()
        }
        PaneSurface::Inspector => render_capsule_inspector_panel(state, theme).into_any_element(),
        PaneSurface::AuthHandoff { session_id, .. } => {
            if let Some(session) = state
                .auth_sessions
                .iter()
                .find(|s| &s.session_id == session_id)
            {
                render_auth_handoff_panel(session, theme).into_any_element()
            } else {
                div().flex_1().into_any_element()
            }
        }
        PaneSurface::Launcher => div()
            .flex_1()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(240.0))
            .bg(linear_gradient(
                180.,
                linear_color_stop(theme.pane_bg_top, 0.),
                linear_color_stop(theme.pane_bg_bottom, 1.),
            ))
            .child(
                div()
                    .flex_1()
                    .relative()
                    .size_full()
                    .child(render_launcher_panel_v2(theme)),
            )
            .into_any_element(),
    }
}

fn render_web_pane(web: &WebPane, theme: &Theme) -> gpui::Div {
    let launching = matches!(
        web.session,
        WebSessionState::Resolving | WebSessionState::Materializing | WebSessionState::Launching
    );
    let pane_top = theme.pane_bg_top;
    let pane_bottom = theme.pane_bg_bottom;

    div()
        .flex_1()
        .flex()
        .flex_col()
        .min_w(px(240.0))
        .bg(linear_gradient(
            180.,
            linear_color_stop(pane_top, 0.),
            linear_color_stop(pane_bottom, 1.),
        ))
        .child(
            div()
                .flex_1()
                .relative()
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(pane_top, 0.),
                    linear_color_stop(pane_bottom, 1.),
                ))
                .when(launching, |this| {
                    this.child(
                        div()
                            .absolute()
                            .inset_0()
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .children((0..5).map(|_| Skeleton::new())),
                    )
                }),
        )
}

mod launcher;
mod settings;

use gpui::prelude::*;
use gpui::{
    div, hsla, linear_color_stop, linear_gradient, point, px, rgb, AnyElement, BoxShadow,
    IntoElement,
};
use gpui_component::resizable::h_resizable;
use gpui_component::skeleton::Skeleton;

use crate::state::{AppState, PaneBounds, PaneSurface, WebPane, WebSessionState};

use super::STAGE_PADDING;
use launcher::render_launcher_panel;
use settings::render_settings_panel;

pub(super) fn render_stage(
    state: &AppState,
    _stage_bounds: PaneBounds,
    active_pane_count: usize,
) -> impl IntoElement {
    let panes = state.active_panes();
    let has_split = active_pane_count > 1;

    let content = if has_split {
        panes
            .iter()
            .fold(h_resizable("stage-panes"), |group, pane| {
                group.child(render_stage_pane(pane))
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
                        .map(|pane| render_stage_pane(pane))
                        .unwrap_or_else(|| div().flex_1().into_any_element()),
                ),
            )
            .into_any_element()
    };

    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .size_full()
        .m(px(STAGE_PADDING))
        // Rounded outer container matching mock's elevated surface
        .rounded(px(14.0))
        .bg(hsla(228.0 / 360.0, 0.16, 0.13, 1.0))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.06))
        .shadow(vec![
            BoxShadow {
                color: hsla(0.0, 0.0, 0.0, 0.45),
                offset: point(px(0.), px(8.)),
                blur_radius: px(40.),
                spread_radius: px(0.),
            },
            BoxShadow {
                color: hsla(0.0, 0.0, 0.0, 0.3),
                offset: point(px(0.), px(1.)),
                blur_radius: px(3.),
                spread_radius: px(0.),
            },
        ])
        .overflow_hidden()
        .child(content)
}

fn render_stage_pane(pane: &crate::state::Pane) -> AnyElement {
    match &pane.surface {
        PaneSurface::Web(web) => render_web_pane(web).into_any_element(),
        PaneSurface::Native { body } => render_settings_panel(body).into_any_element(),
        PaneSurface::Launcher => div()
            .flex_1()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(240.0))
            .bg(linear_gradient(
                180.,
                linear_color_stop(rgb(0x1d1d23), 0.),
                linear_color_stop(rgb(0x19191f), 1.),
            ))
            .child(
                div()
                    .flex_1()
                    .relative()
                    .size_full()
                    .child(render_launcher_panel()),
            )
            .into_any_element(),
    }
}

fn render_web_pane(web: &WebPane) -> gpui::Div {
    let launching = matches!(web.session, WebSessionState::Launching);

    div()
        .flex_1()
        .flex()
        .flex_col()
        .min_w(px(240.0))
        .bg(linear_gradient(
            180.,
            linear_color_stop(rgb(0x1d1d23), 0.),
            linear_color_stop(rgb(0x19191f), 1.),
        ))
        .child(
            div()
                .flex_1()
                .relative()
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(rgb(0x1e1e22), 0.),
                    linear_color_stop(rgb(0x1a1a1f), 1.),
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

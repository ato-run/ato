mod settings;

use gpui::prelude::*;
use gpui::{
    div, hsla, linear_color_stop, linear_gradient, point, px, rgb, AnyElement, BoxShadow,
    IntoElement,
};
use gpui_component::resizable::h_resizable;
use gpui_component::skeleton::Skeleton;

use crate::state::{AppState, PaneBounds, PaneSurface, WebPane, WebSessionState};

use super::share::{render_pane_header, session_label};
use super::STAGE_PADDING;
use settings::render_settings_panel;

pub(super) fn render_stage(
    state: &AppState,
    _stage_bounds: PaneBounds,
    active_pane_count: usize,
) -> impl IntoElement {
    let panes = state.active_panes();
    let _has_split = active_pane_count > 1;

    div()
        .flex_1()
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
        .child(
            panes
                .iter()
                .fold(h_resizable("stage-panes"), |group, pane| {
                    let content: AnyElement = match &pane.surface {
                        PaneSurface::Web(web) => {
                            render_web_pane(pane.title.clone(), web).into_any_element()
                        }
                        PaneSurface::Native { body } => {
                            render_settings_panel(pane.title.clone(), body).into_any_element()
                        }
                    };
                    group.child(content)
                }),
        )
}

fn render_web_pane(title: String, web: &WebPane) -> gpui::Div {
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
        .child(render_pane_header(
            title,
            Some(web.route.to_string()),
            web.profile.clone(),
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
                })
                .when(!launching, |this| {
                    this
                        // Ambient top glow
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .h(px(82.0))
                                .bg(linear_gradient(
                                    180.,
                                    linear_color_stop(
                                        hsla(224.0 / 360.0, 0.42, 0.26, 0.16),
                                        0.,
                                    ),
                                    linear_color_stop(
                                        hsla(224.0 / 360.0, 0.42, 0.26, 0.0),
                                        1.,
                                    ),
                                )),
                        )
                        // Session info badge (top-left)
                        .child(
                            div()
                                .absolute()
                                .top_4()
                                .left_4()
                                .rounded(px(6.0))
                                .bg(hsla(240.0 / 360.0, 0.10, 0.17, 0.96))
                                .border_1()
                                .border_color(hsla(0.0, 0.0, 1.0, 0.08))
                                .shadow(vec![BoxShadow {
                                    color: hsla(0.0, 0.0, 0.0, 0.2),
                                    offset: point(px(0.), px(4.)),
                                    blur_radius: px(12.),
                                    spread_radius: px(0.),
                                }])
                                .px_3()
                                .py(px(5.0))
                                .text_xs()
                                .text_color(rgb(0xc6cbd2))
                                .child(format!(
                                    "{} \u{00b7} {}",
                                    web.profile,
                                    session_label(web.session.clone())
                                )),
                        )
                        // Route badge (bottom-left)
                        .child(
                            div()
                                .absolute()
                                .bottom_4()
                                .left_4()
                                .rounded(px(6.0))
                                .bg(hsla(240.0 / 360.0, 0.10, 0.17, 0.96))
                                .border_1()
                                .border_color(hsla(0.0, 0.0, 1.0, 0.08))
                                .shadow(vec![BoxShadow {
                                    color: hsla(0.0, 0.0, 0.0, 0.2),
                                    offset: point(px(0.), px(4.)),
                                    blur_radius: px(12.),
                                    spread_radius: px(0.),
                                }])
                                .px_3()
                                .py(px(5.0))
                                .text_xs()
                                .text_color(rgb(0x8a9098))
                                .child(web.route.to_string()),
                        )
                }),
        )
}

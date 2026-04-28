mod auth_handoff;
mod capsule_runtime;
mod devtools;
mod inspector;
mod launcher;
mod launcher_v2;
mod settings;

use gpui::prelude::*;
use gpui::{
    div, linear_color_stop, linear_gradient, point, px, AnyElement, BoxShadow, Entity, FontWeight,
    IntoElement,
};
use gpui_component::input::InputState;
use gpui_component::resizable::h_resizable;
use gpui_component::skeleton::Skeleton;
use gpui_component::spinner::Spinner;
use gpui_component::{Sizable, Size};

use crate::state::{
    ActivityTone, AppState, GuestRoute, PaneBounds, PaneSurface, WebPane, WebSessionState,
};

use super::theme::{task_hue, Theme};
use super::STAGE_PADDING;
use auth_handoff::render_auth_handoff_panel;
use capsule_runtime::render_capsule_runtime_panel;
use devtools::render_dev_console_panel;
use inspector::render_capsule_inspector_panel;
use launcher_v2::render_launcher_panel_v2;
use settings::render_settings_panel;

pub(super) fn render_stage(
    state: &AppState,
    _stage_bounds: PaneBounds,
    active_pane_count: usize,
    theme: &Theme,
    launcher_search: &Entity<InputState>,
) -> impl IntoElement {
    let panes = state.active_panes();
    let has_split = active_pane_count > 1;

    let content = if has_split {
        panes
            .iter()
            .fold(h_resizable("stage-panes"), |group, pane| {
                group.child(render_stage_pane(pane, state, theme, launcher_search))
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
                        .map(|pane| render_stage_pane(pane, state, theme, launcher_search))
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

fn render_stage_pane(
    pane: &crate::state::Pane,
    state: &AppState,
    theme: &Theme,
    launcher_search: &Entity<InputState>,
) -> AnyElement {
    match &pane.surface {
        PaneSurface::Web(web) => render_web_pane(web, state, theme).into_any_element(),
        PaneSurface::Native { body } => {
            render_settings_panel(body, state, theme).into_any_element()
        }
        PaneSurface::DevConsole => render_dev_console_panel(state, theme).into_any_element(),
        PaneSurface::CapsuleStatus(capsule) => {
            render_capsule_runtime_panel(capsule, theme).into_any_element()
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
                    .child(render_launcher_panel_v2(state, theme, launcher_search)),
            )
            .into_any_element(),
        PaneSurface::Terminal(_terminal) => div()
            .flex_1()
            .size_full()
            .bg(gpui::black())
            .child(
                div()
                    .flex_1()
                    .size_full()
                    .id(pane.id)
                    .child(gpui::div().flex_1().text_sm().text_color(gpui::white())),
            )
            .into_any_element(),
    }
}

fn render_web_pane(web: &WebPane, state: &AppState, theme: &Theme) -> gpui::Div {
    let launching = matches!(
        web.session,
        WebSessionState::Resolving | WebSessionState::Materializing | WebSessionState::Launching
    );
    let failed = web.session == WebSessionState::LaunchFailed;
    let is_share = web.source_label.as_deref() == Some("share");
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
                .when(launching && is_share, |this| {
                    this.child(render_share_loading_overlay(web, theme))
                })
                .when(launching && !is_share, |this| {
                    this.child(render_generic_loading_overlay(web, theme))
                })
                .when(failed, |this| {
                    this.child(render_launch_failed_overlay(web, state, theme))
                }),
        )
}

fn render_share_loading_overlay(web: &WebPane, theme: &Theme) -> impl IntoElement {
    let share_id = match &web.route {
        GuestRoute::CapsuleHandle { label, .. } => {
            label.strip_prefix("share:").unwrap_or(label).to_string()
        }
        _ => "…".to_string(),
    };

    let active_step: usize = match web.session {
        WebSessionState::Resolving => 1,
        WebSessionState::Materializing => 2,
        WebSessionState::Launching => 3,
        _ => 1,
    };

    let step_label = match web.session {
        WebSessionState::Resolving => "Downloading capsule…",
        WebSessionState::Materializing => "Installing dependencies…",
        WebSessionState::Launching => "Starting app…",
        _ => "Loading…",
    };

    let (label, hue) = web_pane_loading_identity(web);

    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p(px(32.0))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(24.0))
                .w(px(360.0))
                .child(render_app_loading_visual(&label, hue, 112.0, 60.0, theme))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_size(px(20.0))
                                .font_weight(FontWeight(600.0))
                                .text_color(theme.text_primary)
                                .child("Opening Capsule"),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.accent)
                                .child(share_id),
                        ),
                )
                .child(render_step_indicator(active_step, theme))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_tertiary)
                        .child(step_label),
                ),
        )
}

fn render_generic_loading_overlay(web: &WebPane, theme: &Theme) -> impl IntoElement {
    let (label, hue) = web_pane_loading_identity(web);
    let stage_label = match web.session {
        WebSessionState::Resolving => "Resolving capsule…",
        WebSessionState::Materializing => "Installing dependencies…",
        WebSessionState::Launching => "Starting app…",
        _ => "Loading…",
    };
    let title = web
        .canonical_handle
        .clone()
        .or_else(|| match &web.route {
            GuestRoute::ExternalUrl(url) => url.host_str().map(|h| h.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| web.route.to_string());

    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p(px(32.0))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(20.0))
                .child(render_app_loading_visual(&label, hue, 112.0, 60.0, theme))
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(theme.text_primary)
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_tertiary)
                        .child(stage_label),
                ),
        )
}

/// Pick a stable label/hue for the loading visual from the pane's
/// own data (no need to walk up to the parent task). Prefers the
/// canonical handle, then the URL host, then the pane title.
/// `partition_id` is the seed because it is unique per pane and
/// stable across reroutes; `task_hue` keeps the loader's color in
/// sync with the rail icon for the same pane.
fn web_pane_loading_identity(web: &WebPane) -> (String, f32) {
    let label_source: String = web
        .canonical_handle
        .clone()
        .or_else(|| match &web.route {
            GuestRoute::ExternalUrl(url) => url.host_str().map(|h| h.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| web.partition_id.clone());
    let label: String = label_source
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_else(|| "•".to_string());
    let mut seed: u64 = 1469598103934665603; // FNV-1a offset basis, gives decent spread.
    for byte in web.partition_id.as_bytes() {
        seed ^= *byte as u64;
        seed = seed.wrapping_mul(1099511628211);
    }
    (label, task_hue(seed))
}

/// Centered loading visual — a rotating spinner ring with the app's
/// monogram in the middle. Replaces the Skeleton stripes that the
/// stage rendered while a guest was resolving/materializing/launching;
/// users now see *which* app is loading instead of a generic placeholder.
///
/// `outer_size` controls the spinner ring diameter, `inner_size`
/// the monogram tile.
fn render_app_loading_visual(
    label: &str,
    hue: f32,
    outer_size: f32,
    inner_size: f32,
    theme: &Theme,
) -> impl IntoElement {
    let saturation = 0.55_f32;
    let lightness = 0.50_f32;
    let monogram_bg = linear_gradient(
        135.,
        linear_color_stop(gpui::hsla(hue / 360.0, saturation, lightness, 1.0), 0.),
        linear_color_stop(
            gpui::hsla(
                ((hue + 30.0) % 360.0) / 360.0,
                saturation * 0.9,
                lightness * 0.85,
                1.0,
            ),
            1.,
        ),
    );

    div()
        .relative()
        .w(px(outer_size))
        .h(px(outer_size))
        .flex()
        .items_center()
        .justify_center()
        .child(
            // Rotating ring — gpui_component's Spinner handles the
            // animation. Sized to the outer diameter so it visually
            // wraps the monogram below.
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Spinner::new()
                        .with_size(Size::Size(px(outer_size)))
                        .color(gpui::hsla(hue / 360.0, saturation, lightness, 0.95)),
                ),
        )
        .child(
            div()
                .w(px(inner_size))
                .h(px(inner_size))
                .rounded(px(inner_size * 0.22))
                .flex()
                .items_center()
                .justify_center()
                .bg(monogram_bg)
                .border_1()
                .border_color(theme.border_default)
                .text_color(gpui::white())
                .text_size(px(inner_size * 0.42))
                .font_weight(FontWeight::BOLD)
                .child(label.to_string()),
        )
}

fn render_step_indicator(active_step: usize, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(render_step_dot(active_step >= 1, "Download", theme))
        .child(div().w(px(24.0)).h(px(1.0)).bg(theme.border_subtle))
        .child(render_step_dot(active_step >= 2, "Install", theme))
        .child(div().w(px(24.0)).h(px(1.0)).bg(theme.border_subtle))
        .child(render_step_dot(active_step >= 3, "Start", theme))
}

fn render_step_dot(active: bool, label: &'static str, theme: &Theme) -> impl IntoElement {
    let dot_color = if active {
        theme.accent
    } else {
        theme.text_disabled
    };
    let label_color = if active {
        theme.text_secondary
    } else {
        theme.text_disabled
    };

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.0))
        .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(dot_color))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(label_color)
                .child(label),
        )
}

fn render_launch_failed_overlay(
    web: &WebPane,
    state: &AppState,
    theme: &Theme,
) -> impl IntoElement {
    let share_id = match &web.route {
        GuestRoute::CapsuleHandle { label, .. } => {
            Some(label.strip_prefix("share:").unwrap_or(label).to_string())
        }
        _ => None,
    };

    let last_error = state
        .activity
        .iter()
        .rev()
        .find(|a| a.tone == ActivityTone::Error)
        .map(|a| {
            // Take first non-empty line to keep the UI clean
            a.message
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(&a.message)
                .to_string()
        })
        .unwrap_or_else(|| "Failed to launch capsule.".to_string());

    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p(px(32.0))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(12.0))
                .w(px(400.0))
                .child(div().text_size(px(28.0)).child("⚠"))
                .child(
                    div()
                        .text_size(px(17.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(theme.text_primary)
                        .child("Failed to open capsule"),
                )
                .when_some(share_id, |this, id| {
                    this.child(div().text_size(px(12.0)).text_color(theme.accent).child(id))
                })
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_secondary)
                        .child(last_error),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_tertiary)
                        .child("Re-enter the URL in the omnibar to retry"),
                ),
        )
}

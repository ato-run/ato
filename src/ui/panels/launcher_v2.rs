use gpui::prelude::*;
use gpui::{
    div, hsla, img, point, px, AnyElement, BoxShadow, FontWeight, IntoElement, MouseButton,
    ObjectFit,
};

use super::super::theme::Theme;
use crate::app::{FocusCommandBar, OpenCloudDock, OpenLocalRegistry, SignInToAtoRun};
use crate::state::{AppState, DesktopAuthStatus, LauncherAction, ThemeMode};

pub(in crate::ui) fn render_launcher_panel_v2(state: &AppState, theme: &Theme) -> impl IntoElement {
    div()
        .relative()
        .size_full()
        .child(
            img("bg_launcher.jpg")
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(ObjectFit::Cover),
        )
        .when(theme.mode == ThemeMode::Dark, |d| {
            d.child(
                div()
                    .absolute()
                    .inset_0()
                    .bg(hsla(240.0 / 360.0, 0.15, 0.08, 0.75)),
            )
        })
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(18.0))
                        .py(px(36.0))
                        .child(render_date_chip())
                        .child(render_search_bar())
                        .child(render_primary_actions(state))
                        .child(render_status_card(state))
                        .child(render_pinned_chips()),
                )
                .child(render_bottom_bar()),
        )
}

fn launcher_action_specs(state: &AppState) -> Vec<(LauncherAction, &'static str, &'static str)> {
    state
        .launcher_actions()
        .into_iter()
        .map(|action| match action {
            LauncherAction::OpenLocalRegistry => (
                action,
                "Open Local Registry",
                "Browse the local dock registry at 127.0.0.1:8787",
            ),
            LauncherAction::OpenCloudDock => (
                action,
                "Open Cloud Dock",
                "Open your ato.run dock, or start browser sign-in first",
            ),
            LauncherAction::SignInToAtoRun => (
                action,
                "Sign in to ato.run",
                "Continue authentication in the browser and return here automatically",
            ),
        })
        .collect()
}

fn render_date_chip() -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .px(px(12.0))
        .py(px(5.0))
        .rounded(px(999.0))
        .bg(hsla(60.0 / 360.0, 0.06, 0.933, 1.0))
        .border_1()
        .border_color(hsla(60.0 / 360.0, 0.05, 0.897, 1.0))
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight(500.0))
                .text_color(hsla(0.0, 0.0, 0.478, 1.0))
                .child("WED, APRIL 9"),
        )
}

fn render_search_bar() -> impl IntoElement {
    div()
        .w(px(560.0))
        .flex()
        .items_center()
        .h(px(52.0))
        .gap(px(12.0))
        .px(px(18.0))
        .bg(hsla(0.0, 0.0, 1.0, 1.0))
        .border_1()
        .border_color(hsla(217.0 / 360.0, 0.50, 0.55, 0.35))
        .rounded(px(16.0))
        .shadow(vec![
            BoxShadow {
                color: hsla(217.0 / 360.0, 0.75, 0.55, 0.08),
                offset: point(px(0.0), px(0.0)),
                blur_radius: px(16.0),
                spread_radius: px(2.0),
            },
            BoxShadow {
                color: hsla(60.0 / 360.0, 0.05, 0.0, 0.07),
                offset: point(px(0.0), px(2.0)),
                blur_radius: px(12.0),
                spread_radius: px(0.0),
            },
        ])
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.dispatch_action(Box::new(FocusCommandBar), cx);
        })
        .child(
            div()
                .text_size(px(15.0))
                .text_color(hsla(217.0 / 360.0, 0.75, 0.45, 0.60))
                .child("⌘"),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(14.0))
                .text_color(hsla(0.0, 0.0, 0.478, 1.0))
                .child("Search, command, or ask AI…"),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .px(px(7.0))
                        .py(px(3.0))
                        .bg(hsla(60.0 / 360.0, 0.06, 0.933, 1.0))
                        .border_1()
                        .border_color(hsla(60.0 / 360.0, 0.05, 0.847, 1.0))
                        .rounded(px(6.0))
                        .text_size(px(10.0))
                        .text_color(hsla(0.0, 0.0, 0.478, 1.0))
                        .child("⌘ K"),
                )
                .child(
                    div()
                        .text_size(px(14.0))
                        .text_color(hsla(217.0 / 360.0, 0.75, 0.45, 0.70))
                        .child("✦"),
                ),
        )
}

fn render_primary_actions(state: &AppState) -> impl IntoElement {
    div()
        .w(px(560.0))
        .flex()
        .flex_col()
        .bg(hsla(0.0, 0.0, 1.0, 0.96))
        .border_1()
        .border_color(hsla(60.0 / 360.0, 0.05, 0.847, 1.0))
        .rounded(px(18.0))
        .shadow(vec![BoxShadow {
            color: hsla(60.0 / 360.0, 0.05, 0.0, 0.06),
            offset: point(px(0.0), px(8.0)),
            blur_radius: px(18.0),
            spread_radius: px(0.0),
        }])
        .child(panel_header("LAUNCHER", "Pinned actions"))
        .child(panel_divider())
        .children(
            launcher_action_specs(state)
                .into_iter()
                .enumerate()
                .map(|(index, (action, title, detail))| {
                    let row = action_row(action, title, detail);
                    if index == 0 {
                        row
                    } else {
                        div().child(panel_divider()).child(row).into_any_element()
                    }
                }),
        )
}

fn render_status_card(state: &AppState) -> impl IntoElement {
    let (badge, detail) = match state.desktop_auth.status {
        DesktopAuthStatus::SignedIn => (
            "Signed in",
            state
                .desktop_auth
                .publisher_handle
                .as_deref()
                .map(|handle| format!("Cloud Dock will open /dock/{handle}"))
                .unwrap_or_else(|| "Cloud Dock will fall back to /dock".to_string()),
        ),
        DesktopAuthStatus::AwaitingBrowser => (
            "Browser handoff",
            "ato.run sign-in is continuing in your default browser".to_string(),
        ),
        DesktopAuthStatus::Failed => (
            "Needs attention",
            "Automatic return failed. Finish sign-in in the browser or use Done.".to_string(),
        ),
        DesktopAuthStatus::SignedOut => (
            "Signed out",
            "Cloud Dock will open browser sign-in before returning to desktop".to_string(),
        ),
    };

    div()
        .w(px(560.0))
        .flex()
        .items_center()
        .justify_between()
        .px(px(18.0))
        .py(px(14.0))
        .bg(hsla(0.0, 0.0, 1.0, 0.92))
        .border_1()
        .border_color(hsla(60.0 / 360.0, 0.05, 0.847, 1.0))
        .rounded(px(16.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(hsla(0.0, 0.0, 0.478, 1.0))
                        .child("ato.run"),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(hsla(0.0, 0.0, 0.090, 1.0))
                        .child(badge),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(hsla(0.0, 0.0, 0.478, 1.0))
                        .child(detail),
                ),
        )
        .child(
            div()
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(999.0))
                .bg(hsla(217.0 / 360.0, 0.75, 0.45, 0.08))
                .text_size(px(10.0))
                .text_color(hsla(217.0 / 360.0, 0.75, 0.45, 1.0))
                .child("browser return enabled"),
        )
}

fn panel_header(title: &'static str, action: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(16.0))
        .py(px(10.0))
        .child(
            div()
                .text_size(px(10.0))
                .font_weight(FontWeight(600.0))
                .text_color(hsla(0.0, 0.0, 0.478, 1.0))
                .child(title),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(hsla(217.0 / 360.0, 0.75, 0.45, 1.0))
                .child(action),
        )
}

fn panel_divider() -> impl IntoElement {
    div()
        .h(px(1.0))
        .mx(px(16.0))
        .bg(hsla(60.0 / 360.0, 0.05, 0.897, 1.0))
}

fn action_row(action: LauncherAction, title: &'static str, detail: &'static str) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .px(px(16.0))
        .py(px(14.0))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(60.0 / 360.0, 0.05, 0.96, 1.0)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| match action {
            LauncherAction::OpenLocalRegistry => {
                window.dispatch_action(Box::new(OpenLocalRegistry), cx);
            }
            LauncherAction::OpenCloudDock => {
                window.dispatch_action(Box::new(OpenCloudDock), cx);
            }
            LauncherAction::SignInToAtoRun => {
                window.dispatch_action(Box::new(SignInToAtoRun), cx);
            }
        })
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(hsla(0.0, 0.0, 0.090, 1.0))
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(hsla(0.0, 0.0, 0.478, 1.0))
                        .child(detail),
                ),
        )
        .child(
            div()
                .text_size(px(16.0))
                .text_color(hsla(217.0 / 360.0, 0.75, 0.45, 0.8))
                .child("→"),
        )
        .into_any_element()
}

fn render_pinned_chips() -> impl IntoElement {
    div()
        .w(px(560.0))
        .flex()
        .gap(px(8.0))
        .child(pinned_chip("ato.run", 217.0, 0.80, 0.50))
        .child(pinned_chip("Cloud Dock", 161.0, 0.55, 0.42))
        .child(pinned_chip("Local Registry", 43.0, 0.90, 0.44))
        .child(pinned_chip("GitHub Auth", 0.0, 0.0, 0.30))
}

fn pinned_chip(label: &'static str, hue_deg: f32, sat: f32, lit: f32) -> impl IntoElement {
    let hue = hue_deg / 360.0;
    div()
        .flex()
        .items_center()
        .gap(px(7.0))
        .px(px(11.0))
        .py(px(7.0))
        .rounded(px(10.0))
        .bg(hsla(0.0, 0.0, 1.0, 1.0))
        .border_1()
        .border_color(hsla(60.0 / 360.0, 0.05, 0.847, 1.0))
        .shadow(vec![BoxShadow {
            color: hsla(60.0 / 360.0, 0.05, 0.0, 0.05),
            offset: point(px(0.0), px(1.0)),
            blur_radius: px(4.0),
            spread_radius: px(0.0),
        }])
        .child(
            div()
                .w(px(7.0))
                .h(px(7.0))
                .rounded(px(999.0))
                .bg(hsla(hue, sat, lit, 1.0)),
        )
        .child(
            div()
                .text_size(px(12.0))
                .font_weight(FontWeight(500.0))
                .text_color(hsla(0.0, 0.0, 0.333, 1.0))
                .child(label),
        )
}

fn render_bottom_bar() -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(4.0))
        .pb(px(24.0))
        .child(bottom_action("+ New Workspace"))
        .child(bottom_action("↓ Import"))
        .child(bottom_action("⊞ Templates"))
        .child(bottom_action("⌨ Shortcuts"))
}

fn bottom_action(label: &'static str) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(5.0))
        .px(px(10.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .bg(hsla(60.0 / 360.0, 0.06, 0.933, 1.0))
        .border_1()
        .border_color(hsla(60.0 / 360.0, 0.05, 0.897, 1.0))
        .text_size(px(10.5))
        .text_color(hsla(0.0, 0.0, 0.333, 1.0))
        .child(label)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::launcher_action_specs;
    use crate::state::{AppState, DesktopAuthStatus};

    #[test]
    fn launcher_shows_sign_in_when_signed_out() {
        let state = AppState::demo();
        let titles = launcher_action_specs(&state)
            .into_iter()
            .map(|(_, title, _)| title)
            .collect::<Vec<_>>();
        assert!(titles.contains(&"Sign in to ato.run"));
    }

    #[test]
    fn launcher_hides_sign_in_when_handle_is_available() {
        let mut state = AppState::demo();
        state.desktop_auth.status = DesktopAuthStatus::SignedIn;
        state.desktop_auth.publisher_handle = Some("koh0920".to_string());

        let titles = launcher_action_specs(&state)
            .into_iter()
            .map(|(_, title, _)| title)
            .collect::<Vec<_>>();
        assert!(!titles.contains(&"Sign in to ato.run"));
    }
}

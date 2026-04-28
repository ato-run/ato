use gpui::prelude::*;
use gpui::{div, hsla, px, Div, FontWeight, MouseButton};
use gpui_component::scroll::ScrollableElement;

use super::super::theme::Theme;
use crate::app::{
    CheckForUpdates, OpenLatestReleasePage, SignInToAtoRun, SignOut, ToggleAutoDevtools,
    ToggleTheme,
};
use crate::state::{AppState, DesktopAuthStatus, ThemeMode, UpdateCheck};

pub(super) fn render_settings_panel(body: &str, state: &AppState, theme: &Theme) -> Div {
    let body_text = body.to_string();

    div()
        .w(px(360.0))
        .min_w(px(260.0))
        .bg(theme.settings_panel_bg)
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(
            div()
                .flex_1()
                .overflow_y_scrollbar()
                .p_4()
                .flex()
                .flex_col()
                .gap_4()
                // Account section — sign-in / sign-out + identity
                .child(render_account_card(state, theme))
                // Appearance section
                .child(render_appearance_card(state, theme))
                // Updates section
                .child(render_updates_card(state, theme))
                // Terminal section
                .child(render_terminal_card(state, theme))
                // Developer section
                .child(render_developer_card(state, theme))
                // Secrets section
                .child(render_secrets_card(state, theme))
                // Egress Policy section
                .child(render_egress_card(state, theme))
                // Diagnostics section
                .child(render_diagnostics_card(&body_text, theme)),
        )
}

fn render_appearance_card(state: &AppState, theme: &Theme) -> Div {
    div()
        .rounded(px(12.0))
        .bg(theme.settings_card_bg)
        .border_1()
        .border_color(theme.settings_card_border)
        .p_4()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(12.0))
                .font_weight(FontWeight(600.0))
                .text_color(theme.text_primary)
                .child("Appearance"),
        )
        .child(
            div()
                .flex()
                .rounded(px(8.0))
                .bg(theme.settings_body_bg)
                .border_1()
                .border_color(theme.settings_card_border)
                .overflow_hidden()
                .child(theme_chip(
                    "Light",
                    state.theme_mode == ThemeMode::Light,
                    theme,
                ))
                .child(theme_chip(
                    "Dark",
                    state.theme_mode == ThemeMode::Dark,
                    theme,
                )),
        )
}

fn render_updates_card(state: &AppState, theme: &Theme) -> Div {
    let current = env!("CARGO_PKG_VERSION");
    let (status_label, status_color, latest_value): (String, gpui::Hsla, Option<String>) =
        match &state.update_check {
            UpdateCheck::Idle => ("Not checked yet".to_string(), theme.text_disabled, None),
            UpdateCheck::Checking => ("Checking…".to_string(), theme.text_secondary, None),
            UpdateCheck::UpToDate { .. } => (
                "Up to date".to_string(),
                theme.accent,
                Some(format!("v{current}")),
            ),
            UpdateCheck::Available { latest, .. } => (
                format!("v{latest} available"),
                hsla(38.0 / 360.0, 0.85, 0.50, 1.0),
                Some(format!("v{latest}")),
            ),
            UpdateCheck::Failed { message } => (
                format!("Check failed: {message}"),
                hsla(0.0, 0.7, 0.5, 1.0),
                None,
            ),
        };

    let action_button: gpui::Stateful<Div> = match &state.update_check {
        UpdateCheck::Available { .. } => {
            updates_action_button("Open release", theme, true, move |window, cx| {
                window.dispatch_action(Box::new(OpenLatestReleasePage), cx);
            })
        }
        UpdateCheck::Checking => updates_action_button(
            "Checking…",
            theme,
            false,
            // No-op while in flight; the dispatcher's idempotency
            // guard would also drop a re-entered CheckForUpdates,
            // but disabling the button is the clearer signal.
            move |_, _| {},
        ),
        _ => updates_action_button("Check now", theme, false, move |window, cx| {
            window.dispatch_action(Box::new(CheckForUpdates), cx);
        }),
    };

    settings_card("Updates", theme)
        .child(settings_row("Current", &format!("v{current}"), theme))
        .when_some(latest_value, |this, latest| {
            this.child(settings_row("Latest", &latest, theme))
        })
        .child(settings_row("Status", &status_label, theme).text_color(status_color))
        .child(div().flex().justify_end().pt_2().child(action_button))
}

fn updates_action_button(
    label: &'static str,
    theme: &Theme,
    accent: bool,
    on_click: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<Div> {
    let (bg, fg, border) = if accent {
        (theme.accent, gpui::white(), theme.accent)
    } else {
        (
            theme.surface_hover,
            theme.text_primary,
            theme.border_default,
        )
    };
    div()
        .id(label)
        .px(px(14.0))
        .py(px(7.0))
        .rounded(px(6.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_color(fg)
        .text_size(px(12.0))
        .font_weight(FontWeight(500.0))
        .cursor_pointer()
        .child(label)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx);
        })
}

fn render_account_card(state: &AppState, theme: &Theme) -> Div {
    let auth = &state.desktop_auth;
    let (status_label, status_color) = match auth.status {
        DesktopAuthStatus::SignedOut => ("Signed out", theme.text_secondary),
        DesktopAuthStatus::AwaitingBrowser => ("Waiting for browser…", theme.text_secondary),
        DesktopAuthStatus::SignedIn => ("Signed in", theme.accent),
        DesktopAuthStatus::Failed => ("Sign-in failed", hsla(0.0, 0.7, 0.5, 1.0)),
    };
    let handle = auth
        .publisher_handle
        .as_deref()
        .filter(|h| !h.is_empty())
        .map(|h| format!("@{h}"))
        .unwrap_or_else(|| "—".to_string());
    let origin = auth.last_login_origin.as_deref().unwrap_or("—");

    let action_button: gpui::Stateful<Div> = match auth.status {
        DesktopAuthStatus::SignedIn => {
            account_action_button("Sign out", theme, true, move |window, cx| {
                window.dispatch_action(Box::new(SignOut), cx);
            })
        }
        DesktopAuthStatus::AwaitingBrowser => {
            account_action_button("Cancel", theme, false, move |window, cx| {
                // Re-using SignOut as the cancel path keeps the
                // logic single-source.
                window.dispatch_action(Box::new(SignOut), cx);
            })
        }
        DesktopAuthStatus::Failed | DesktopAuthStatus::SignedOut => {
            account_action_button("Sign in", theme, false, move |window, cx| {
                window.dispatch_action(Box::new(SignInToAtoRun), cx);
            })
        }
    };

    settings_card("Account", theme)
        .child(settings_row("Status", status_label, theme).text_color(status_color))
        .child(settings_row("Handle", &handle, theme))
        .child(settings_row("Origin", origin, theme))
        .child(div().flex().justify_end().pt_2().child(action_button))
}

fn account_action_button(
    label: &'static str,
    theme: &Theme,
    danger: bool,
    on_click: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<Div> {
    let (bg, fg, border) = if danger {
        (
            theme.surface_hover,
            hsla(0.0, 0.7, 0.5, 1.0),
            theme.border_default,
        )
    } else {
        (theme.accent, gpui::white(), theme.accent)
    };
    div()
        .id(label)
        .px(px(14.0))
        .py(px(7.0))
        .rounded(px(6.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_color(fg)
        .text_size(px(12.0))
        .font_weight(FontWeight(500.0))
        .cursor_pointer()
        .child(label)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx);
        })
}

fn render_terminal_card(state: &AppState, theme: &Theme) -> Div {
    let font_size = state.config.terminal_font_size;
    let max_sessions = state.config.terminal_max_sessions;

    settings_card("Terminal", theme)
        .child(settings_row("Font size", &format!("{font_size}px"), theme))
        .child(settings_row(
            "Max sessions",
            &format!("{max_sessions}"),
            theme,
        ))
}

fn render_secrets_card(state: &AppState, theme: &Theme) -> Div {
    let secrets = &state.secret_store.secrets;

    let card = settings_card("Secrets", theme);

    if secrets.is_empty() {
        card.child(
            div()
                .text_size(px(11.0))
                .text_color(theme.text_disabled)
                .child("No secrets configured. Secrets can be injected into capsules as environment variables."),
        )
    } else {
        card.child(
            div()
                .rounded(px(10.0))
                .bg(theme.settings_body_bg)
                .border_1()
                .border_color(theme.settings_body_border)
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .children(secrets.iter().enumerate().map(|(i, secret)| {
                    let masked = "•".repeat(secret.value.len().min(16));
                    let grants_count = state
                        .secret_store
                        .grants
                        .values()
                        .filter(|keys| keys.contains(&secret.key))
                        .count();

                    div()
                        .id(("secret", i))
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(1.0))
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .font_weight(FontWeight(500.0))
                                        .text_color(theme.text_primary)
                                        .child(secret.key.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.0))
                                        .text_color(theme.text_disabled)
                                        .child(format!(
                                            "{masked}  ({grants_count} capsule{})",
                                            if grants_count == 1 { "" } else { "s" }
                                        )),
                                ),
                        )
                })),
        )
    }
}

fn render_developer_card(state: &AppState, theme: &Theme) -> Div {
    let auto_devtools = state.config.auto_open_devtools;

    settings_card("Developer", theme).child(settings_toggle_row(
        "Auto-open DevTools",
        auto_devtools,
        theme,
    ))
}

fn render_egress_card(state: &AppState, theme: &Theme) -> Div {
    let hosts = &state.config.default_egress_allow;

    let card = settings_card("Default Egress Policy", theme);

    if hosts.is_empty() {
        card.child(
            div()
                .text_size(px(11.0))
                .text_color(theme.text_disabled)
                .child("localhost only (no default allow hosts)"),
        )
    } else {
        card.child(
            div()
                .rounded(px(10.0))
                .bg(theme.settings_body_bg)
                .border_1()
                .border_color(theme.settings_body_border)
                .p_3()
                .flex()
                .flex_col()
                .gap_1()
                .children(hosts.iter().map(|host| {
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_secondary)
                        .child(host.clone())
                })),
        )
    }
}

fn render_diagnostics_card(body_text: &str, theme: &Theme) -> Div {
    settings_card("Agent diagnostics", theme)
        .child(
            div()
                .text_size(px(11.0))
                .line_height(px(18.0))
                .text_color(theme.text_disabled)
                .child("Companion native pane for host-side state and diagnostics."),
        )
        .child(
            div()
                .rounded(px(10.0))
                .bg(theme.settings_body_bg)
                .border_1()
                .border_color(theme.settings_body_border)
                .p_4()
                .text_sm()
                .line_height(px(22.0))
                .text_color(theme.text_disabled)
                .child(body_text.to_string()),
        )
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn settings_card(title: &str, theme: &Theme) -> Div {
    div()
        .rounded(px(12.0))
        .bg(theme.settings_card_bg)
        .border_1()
        .border_color(theme.settings_card_border)
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_size(px(12.0))
                .font_weight(FontWeight(600.0))
                .text_color(theme.text_primary)
                .child(title.to_string()),
        )
}

fn settings_row(label: &str, value: &str, theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme.text_secondary)
                .child(label.to_string()),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme.text_disabled)
                .child(value.to_string()),
        )
}

fn settings_toggle_row(label: &str, active: bool, theme: &Theme) -> Div {
    let accent = theme.accent;
    let accent_subtle = theme.accent_subtle;
    let text_secondary = theme.text_secondary;
    let border_default = theme.border_default;

    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(11.0))
                .text_color(text_secondary)
                .child(label.to_string()),
        )
        .child(
            div()
                .id("toggle-auto-devtools")
                .w(px(36.0))
                .h(px(20.0))
                .rounded(px(10.0))
                .cursor_pointer()
                .border_1()
                .border_color(if active { accent } else { border_default })
                .bg(if active {
                    accent_subtle
                } else {
                    hsla(0.0, 0.0, 0.0, 0.0)
                })
                .flex()
                .items_center()
                .px(px(2.0))
                .child(
                    div()
                        .w(px(14.0))
                        .h(px(14.0))
                        .rounded_full()
                        .bg(if active { accent } else { text_secondary })
                        .when(active, |this| this.ml_auto()),
                )
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    window.dispatch_action(Box::new(ToggleAutoDevtools), cx);
                }),
        )
}

fn theme_chip(label: &'static str, active: bool, theme: &Theme) -> impl IntoElement {
    let accent = theme.accent;
    let accent_subtle = theme.accent_subtle;
    let text_secondary = theme.text_secondary;

    div()
        .px(px(12.0))
        .py(px(4.0))
        .cursor_pointer()
        .text_size(px(11.0))
        .font_weight(FontWeight(500.0))
        .bg(if active {
            accent_subtle
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .text_color(if active { accent } else { text_secondary })
        .when(!active, |this| {
            this.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.dispatch_action(Box::new(ToggleTheme), cx);
            })
        })
        .child(label)
}

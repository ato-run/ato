use gpui::prelude::*;
use gpui::{div, px, FontWeight, MouseButton};

use super::super::theme::Theme;
use crate::app::{CancelAuthHandoff, OpenAuthInBrowser, ResumeAfterAuth};
use crate::state::{AuthMode, AuthSession, AuthSessionStatus};

pub(super) fn render_auth_handoff_panel(session: &AuthSession, theme: &Theme) -> impl IntoElement {
    let first_party = matches!(session.auth_mode, AuthMode::FirstPartyNative);
    let status_text = match (first_party, session.status) {
        (_, AuthSessionStatus::Created) => "Opening browser…",
        (true, AuthSessionStatus::OpenedInBrowser) => {
            "ato.run sign-in is continuing in your browser. Desktop will resume automatically when the callback returns."
        }
        (false, AuthSessionStatus::OpenedInBrowser) => {
            "Sign in is continuing in your browser."
        }
        (_, AuthSessionStatus::Completed) => "Sign-in complete. Click Done to return.",
        (_, AuthSessionStatus::Failed) => {
            "Automatic return did not complete. Finish sign-in in the browser, then use Done to continue."
        }
        (_, AuthSessionStatus::Cancelled) => "Sign-in was cancelled.",
    };
    let origin = session.origin.clone();

    let accent = theme.accent;
    let text_primary = theme.text_primary;
    let text_secondary = theme.text_secondary;
    let settings_panel_bg = theme.settings_panel_bg;

    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(20.0))
        .bg(settings_panel_bg)
        .child(div().text_size(px(36.0)).text_color(accent).child("🔒"))
        .child(
            div()
                .text_size(px(15.0))
                .font_weight(FontWeight(600.0))
                .text_color(text_primary)
                .child(if first_party {
                    "Sign in to ato.run".to_string()
                } else {
                    format!("Sign in to {origin}")
                }),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(text_secondary)
                .max_w(px(320.0))
                .child(status_text),
        )
        .child(
            div()
                .flex()
                .gap(px(8.0))
                .child(action_button(
                    "Open Browser",
                    theme,
                    true,
                    |_, window, cx| {
                        window.dispatch_action(Box::new(OpenAuthInBrowser), cx);
                    },
                ))
                .child(action_button("Done", theme, false, |_, window, cx| {
                    window.dispatch_action(Box::new(ResumeAfterAuth), cx);
                }))
                .child(action_button("Cancel", theme, false, |_, window, cx| {
                    window.dispatch_action(Box::new(CancelAuthHandoff), cx);
                })),
        )
}

fn action_button(
    label: &'static str,
    theme: &Theme,
    primary: bool,
    handler: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let bg = if primary {
        theme.accent
    } else {
        theme.surface_hover
    };
    let fg = if primary {
        gpui::hsla(0.0, 0.0, 1.0, 1.0)
    } else {
        theme.text_secondary
    };

    div()
        .px(px(14.0))
        .py(px(7.0))
        .rounded(px(8.0))
        .cursor_pointer()
        .text_size(px(12.0))
        .font_weight(FontWeight(500.0))
        .bg(bg)
        .text_color(fg)
        .on_mouse_down(MouseButton::Left, handler)
        .child(label)
}

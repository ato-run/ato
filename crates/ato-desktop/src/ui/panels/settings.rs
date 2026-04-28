use gpui::prelude::*;
use gpui::{div, hsla, px, AnyElement, Div, FontWeight, MouseButton};
use gpui_component::scroll::ScrollableElement;

use super::super::theme::Theme;
use crate::app::{
    CheckForUpdates, OpenLatestReleasePage, SelectSettingsTab, SignInToAtoRun, SignOut,
    ToggleAutoDevtools, ToggleTheme,
};
use crate::state::{AppState, DesktopAuthStatus, SettingsTab, ThemeMode, UpdateCheck};

pub(super) fn render_settings_panel(body: &str, state: &AppState, theme: &Theme) -> Div {
    let body_text = body.to_string();

    div()
        .size_full()
        .min_w(px(720.0))
        .bg(theme.settings_panel_bg)
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(render_tab_bar(state, theme))
        .child(
            div()
                .flex_1()
                .h_full()
                .bg(theme.settings_panel_bg)
                .child(render_page_content(state, theme, &body_text)),
        )
}

fn render_icon_dot(active: bool, theme: &Theme) -> Div {
    div()
        .w(px(14.0))
        .h(px(14.0))
        .rounded(px(4.0))
        .bg(if active {
            theme.accent
        } else {
            theme.surface_hover
        })
        .border_1()
        .border_color(if active {
            theme.accent_border
        } else {
            theme.settings_body_border
        })
}

fn render_tab_bar(state: &AppState, theme: &Theme) -> Div {
    div()
        .h(px(42.0))
        .border_b_1()
        .border_color(theme.border_subtle)
        .bg(hsla(0.0, 0.0, 1.0, 0.02))
        .px(px(28.0))
        .flex()
        .items_center()
        .gap(px(2.0))
        .children(SettingsTab::ALL.into_iter().map(|tab| {
            render_tab_button(tab, state.settings_active_tab == tab, theme).into_any_element()
        }))
}

fn render_tab_button(tab: SettingsTab, active: bool, theme: &Theme) -> impl IntoElement {
    div()
        .id(("settings-tab", settings_tab_index(tab)))
        .h_full()
        .px(px(14.0))
        .cursor_pointer()
        .flex()
        .items_center()
        .gap(px(6.0))
        .border_b_2()
        .border_color(if active {
            theme.accent
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .text_color(if active {
            theme.text_primary
        } else {
            theme.text_disabled
        })
        .child(render_icon_dot(active, theme))
        .child(
            div()
                .text_size(px(11.5))
                .font_weight(FontWeight(if active { 600.0 } else { 500.0 }))
                .child(tab.label()),
        )
        .when_some(tab.badge(), |this, label| {
            this.child(render_tab_count(label, active, theme))
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(SelectSettingsTab { tab }), cx);
        })
}

fn render_tab_count(label: &str, active: bool, theme: &Theme) -> Div {
    div()
        .px(px(5.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .bg(if active {
            theme.accent_subtle
        } else {
            theme.settings_body_bg
        })
        .text_size(px(8.5))
        .font_weight(FontWeight(700.0))
        .text_color(if active {
            theme.accent
        } else {
            theme.text_disabled
        })
        .child(label.to_string())
}

fn render_page_content(state: &AppState, theme: &Theme, body_text: &str) -> impl IntoElement {
    div()
        .flex_1()
        .overflow_y_scrollbar()
        .child(match state.settings_active_tab {
            SettingsTab::General => render_general_page(state, theme),
            SettingsTab::Account => render_account_page(state, theme),
            SettingsTab::Runtime => render_runtime_page(state, theme),
            SettingsTab::Sandbox => render_sandbox_page(state, theme),
            SettingsTab::Trust => render_trust_page(state, theme),
            SettingsTab::Registry => render_registry_page(theme),
            SettingsTab::Projection => render_projection_page(theme),
            SettingsTab::Developer => render_developer_page(state, theme),
            SettingsTab::About => render_about_page(state, theme, body_text),
        })
}

fn render_general_page(state: &AppState, theme: &Theme) -> Div {
    render_page_shell(
        "General",
        "Desktop defaults, appearance, and update behavior for the shell.",
        vec![
            render_page_column(vec![
                render_group(
                    "Startup",
                    vec![
                        render_toggle_row(
                            "Launch at login",
                            "Start the shell when your session begins.",
                            false,
                            theme,
                        )
                        .into_any_element(),
                        render_toggle_row(
                            "Show in menu bar",
                            "Keep Ato reachable from the desktop chrome.",
                            true,
                            theme,
                        )
                        .into_any_element(),
                        render_toggle_row(
                            "Show What's New",
                            "Open release notes after successful upgrades.",
                            true,
                            theme,
                        )
                        .into_any_element(),
                    ],
                    theme,
                ),
                render_group(
                    "Appearance",
                    vec![
                        render_value_row("Language", "System", "Follow the desktop locale.", theme)
                            .into_any_element(),
                        render_theme_mode_row(state, theme).into_any_element(),
                    ],
                    theme,
                ),
            ]),
            render_page_column(vec![render_updates_group(state, theme)]),
        ],
        theme,
    )
}

fn render_account_page(state: &AppState, theme: &Theme) -> Div {
    let auth = &state.desktop_auth;
    let handle = auth
        .publisher_handle
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| format!("@{value}"))
        .unwrap_or_else(|| "Signed out".to_string());
    let origin = auth.last_login_origin.as_deref().unwrap_or("ato.run");
    let masked_token = if matches!(auth.status, DesktopAuthStatus::SignedIn) {
        "•••••••••••"
    } else {
        "not issued"
    };

    render_page_shell(
        "Account",
        "Identity, device credentials, and local auth material.",
        vec![
            render_page_column(vec![
                render_account_group(state, theme),
                render_group(
                    "Credentials",
                    vec![
                        render_value_row(
                            "ATO_TOKEN",
                            masked_token,
                            "Stored locally for the desktop shell.",
                            theme,
                        )
                        .into_any_element(),
                        render_path_row(
                            "credentials.toml",
                            format_home_path(".ato/credentials.toml"),
                            theme,
                        )
                        .into_any_element(),
                        render_value_row(
                            "Origin",
                            origin,
                            "Last successful auth callback origin.",
                            theme,
                        )
                        .into_any_element(),
                    ],
                    theme,
                ),
            ]),
            render_page_column(vec![render_group(
                "Identity",
                vec![
                    render_value_row("Publisher", &handle, "Connected ato.run identity.", theme)
                        .into_any_element(),
                    render_value_row(
                        "Device name",
                        &default_device_name(),
                        "Host-visible device label used by companion flows.",
                        theme,
                    )
                    .into_any_element(),
                    render_value_row(
                        "Sessions",
                        &state.auth_sessions.len().to_string(),
                        "Active or recent browser handoffs tracked locally.",
                        theme,
                    )
                    .into_any_element(),
                ],
                theme,
            )]),
        ],
        theme,
    )
}

fn render_runtime_page(state: &AppState, theme: &Theme) -> Div {
    render_page_shell(
        "Runtime",
        "Workspace paths, cache behavior, and sandbox execution defaults.",
        vec![
            render_page_column(vec![
                render_group(
                    "Cache",
                    vec![
                        render_path_row("Cache location", format_home_path(".ato/cache"), theme)
                            .into_any_element(),
                        render_value_row(
                            "Terminal font",
                            &format!("{} px", state.config.terminal_font_size),
                            "Shared terminal font size baseline.",
                            theme,
                        )
                        .into_any_element(),
                        render_value_row(
                            "Max sessions",
                            &state.config.terminal_max_sessions.to_string(),
                            "Concurrent host terminal sessions allowed.",
                            theme,
                        )
                        .into_any_element(),
                    ],
                    theme,
                ),
                render_group(
                    "Workspace",
                    vec![
                        render_path_row(
                            "Workspace root",
                            format_home_path(".ato/workspaces"),
                            theme,
                        )
                        .into_any_element(),
                        render_value_row(
                            "Watch debounce",
                            "300 ms",
                            "Applied to file watching and hot-reload style flows.",
                            theme,
                        )
                        .into_any_element(),
                    ],
                    theme,
                ),
            ]),
            render_page_column(vec![render_group(
                "Sandbox tier policy",
                vec![
                    render_value_row(
                        "Default policy",
                        "Tier 1 only",
                        "Restrictive default until a capsule requests more power.",
                        theme,
                    )
                    .into_any_element(),
                    render_value_row(
                        "Unsafe execution",
                        "Always confirm",
                        "Prompts before elevated runtime routes.",
                        theme,
                    )
                    .into_any_element(),
                    render_toggle_row(
                        "CAPSULE_ALLOW_UNSAFE",
                        "Shadow env flag surfaced for debugging.",
                        false,
                        theme,
                    )
                    .into_any_element(),
                ],
                theme,
            )]),
        ],
        theme,
    )
}

fn render_sandbox_page(state: &AppState, theme: &Theme) -> Div {
    let egress_summary = if state.config.default_egress_allow.is_empty() {
        "Deny-all"
    } else {
        "Allowlist"
    };

    render_page_shell(
        "Sandbox",
        "Engine availability, egress defaults, and host bridge sockets.",
        vec![
            render_page_column(vec![
                render_group(
                    "Nacelle",
                    vec![render_toggle_row(
                        "Nacelle engine",
                        "Required for stricter runtime tiers.",
                        true,
                        theme,
                    )
                    .into_any_element()],
                    theme,
                ),
                render_group(
                    "Network egress",
                    vec![
                        render_value_row(
                            "Default policy",
                            egress_summary,
                            "Inherited by new capsule sessions.",
                            theme,
                        )
                        .into_any_element(),
                        render_hosts_row(&state.config.default_egress_allow, theme)
                            .into_any_element(),
                    ],
                    theme,
                ),
            ]),
            render_page_column(vec![
                render_group(
                    "Tailnet",
                    vec![
                        render_toggle_row(
                            "Tailnet sidecar",
                            "Enable tsnet-backed companion routing.",
                            false,
                            theme,
                        )
                        .into_any_element(),
                        render_value_row(
                            "Control plane",
                            "https://hs.ato.run",
                            "Default Headscale endpoint.",
                            theme,
                        )
                        .into_any_element(),
                    ],
                    theme,
                ),
                render_group(
                    "Host bridge sockets",
                    vec![service_row("nacelle.sock", "listening", true, theme).into_any_element()],
                    theme,
                ),
            ]),
        ],
        theme,
    )
}

fn render_trust_page(state: &AppState, theme: &Theme) -> Div {
    let publisher_count = state
        .desktop_auth
        .publisher_handle
        .as_ref()
        .map(|_| 1usize)
        .unwrap_or(0);

    render_page_shell(
        "Trust Store",
        "Publisher trust posture and revocation policy for capsule execution.",
        vec![
            render_page_column(vec![render_group(
                "Revocation",
                vec![
                    render_value_row(
                        "Update frequency",
                        "24 hours",
                        "Refresh trust material on a steady cadence.",
                        theme,
                    )
                    .into_any_element(),
                    render_value_row(
                        "Source",
                        "DNS TXT",
                        "Current revocation source for publisher material.",
                        theme,
                    )
                    .into_any_element(),
                    render_value_row(
                        "Unknown publishers",
                        "Always prompt",
                        "Trust-on-first-use remains explicit by default.",
                        theme,
                    )
                    .into_any_element(),
                ],
                theme,
            )]),
            render_page_column(vec![render_group(
                "Known publishers",
                if publisher_count == 0 {
                    vec![render_empty_group_message(
                            "No verified publishers are stored yet. Publisher trust will appear here after a successful signed launch.",
                            theme,
                        )
                        .into_any_element()]
                } else {
                    vec![render_trust_entry(
                        state
                            .desktop_auth
                            .publisher_handle
                            .as_deref()
                            .unwrap_or("ato.run"),
                        "Verified",
                        hsla(145.0 / 360.0, 0.68, 0.55, 1.0),
                        theme,
                    )
                    .into_any_element()]
                },
                theme,
            )]),
        ],
        theme,
    )
}

fn render_registry_page(theme: &Theme) -> Div {
    render_page_shell(
        "Registry",
        "Endpoints for the public store and any private capsule registries.",
        vec![
            render_page_column(vec![render_group(
                "Store API",
                vec![
                    render_value_row(
                        "Store API URL",
                        "https://api.ato.run",
                        "Primary package and metadata API.",
                        theme,
                    )
                    .into_any_element(),
                    render_value_row(
                        "Store site URL",
                        "https://ato.run",
                        "Primary human-facing registry UI.",
                        theme,
                    )
                    .into_any_element(),
                ],
                theme,
            )]),
            render_page_column(vec![render_group(
                "Private registries",
                vec![
                    render_empty_group_message("No private registries configured.", theme)
                        .into_any_element(),
                    render_value_row(
                        "Local registry port",
                        "8080",
                        "Default port for ato registry serve.",
                        theme,
                    )
                    .into_any_element(),
                ],
                theme,
            )]),
        ],
        theme,
    )
}

fn render_projection_page(theme: &Theme) -> Div {
    render_page_shell(
        "Delivery",
        "Projection and native-install defaults for desktop-facing capsules.",
        vec![
            render_page_column(vec![render_group(
                "Projection",
                vec![
                    render_toggle_row(
                        "Enable by default",
                        "Apply projection for supported install flows.",
                        false,
                        theme,
                    )
                    .into_any_element(),
                    render_value_row(
                        "Projection directory",
                        "/Applications",
                        "Target directory for projected apps on macOS.",
                        theme,
                    )
                    .into_any_element(),
                ],
                theme,
            )]),
            render_page_column(vec![render_group(
                "Installed capsules",
                vec![
                    render_empty_group_message("No projected capsules installed.", theme)
                        .into_any_element(),
                ],
                theme,
            )]),
        ],
        theme,
    )
}

fn render_developer_page(state: &AppState, theme: &Theme) -> Div {
    render_page_shell(
        "Developer",
        "Diagnostics, logging, and experimental behavior toggles for host-side development.",
        vec![
            render_page_column(vec![
                render_group(
                    "Logging",
                    vec![
                        render_value_row(
                            "Log level",
                            "warn",
                            "Current desktop logging verbosity.",
                            theme,
                        )
                        .into_any_element(),
                        render_value_row(
                            "Log output",
                            "stderr",
                            "Host logging sink for the desktop shell.",
                            theme,
                        )
                        .into_any_element(),
                    ],
                    theme,
                ),
                render_group(
                    "Developer tools",
                    vec![render_auto_devtools_row(state, theme).into_any_element()],
                    theme,
                ),
            ]),
            render_page_column(vec![render_group(
                "Experimental features",
                vec![
                    render_toggle_row(
                        "Parallel branch execution",
                        "Reserved for future multi-branch orchestration.",
                        true,
                        theme,
                    )
                    .into_any_element(),
                    render_toggle_row(
                        "Projected file preview",
                        "Preview native-installed files before commit.",
                        false,
                        theme,
                    )
                    .into_any_element(),
                    render_toggle_row(
                        "Hot-reload capsules",
                        "Auto-refresh source-backed capsules on change.",
                        false,
                        theme,
                    )
                    .into_any_element(),
                ],
                theme,
            )]),
        ],
        theme,
    )
}

fn render_about_page(state: &AppState, theme: &Theme, body_text: &str) -> Div {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let version = env!("CARGO_PKG_VERSION");

    render_page_shell(
        "About",
        "Version details, service posture, and a compact diagnostics snapshot.",
        vec![
            render_page_column(vec![
                render_group(
                    "Version",
                    vec![
                        render_value_row(
                            "ato-desktop",
                            &format!("v{version}"),
                            "Current desktop shell build.",
                            theme,
                        )
                        .into_any_element(),
                        render_value_row(
                            "Theme",
                            if state.theme_mode == ThemeMode::Dark {
                                "Dark"
                            } else {
                                "Light"
                            },
                            "Resolved host theme mode.",
                            theme,
                        )
                        .into_any_element(),
                    ],
                    theme,
                ),
                render_group(
                    "System",
                    vec![
                        render_value_row("OS", os, "Compiled target OS.", theme).into_any_element(),
                        render_value_row("Arch", arch, "Compiled target architecture.", theme)
                            .into_any_element(),
                    ],
                    theme,
                ),
            ]),
            render_page_column(vec![
                render_group(
                    "Running services",
                    vec![
                        service_row("nacelle", "running", true, theme).into_any_element(),
                        service_row("ato-tsnetd", "idle", false, theme).into_any_element(),
                    ],
                    theme,
                ),
                render_group(
                    "Diagnostics",
                    vec![render_diagnostics_block(body_text, theme).into_any_element()],
                    theme,
                ),
            ]),
        ],
        theme,
    )
}

fn render_page_shell(title: &str, description: &str, columns: Vec<Div>, theme: &Theme) -> Div {
    div()
        .px(px(36.0))
        .py(px(30.0))
        .flex()
        .flex_col()
        .gap(px(24.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .text_size(px(20.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(theme.text_primary)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .line_height(px(20.0))
                        .text_color(theme.text_tertiary)
                        .child(description.to_string()),
                ),
        )
        .child(
            div()
                .flex()
                .items_start()
                .gap(px(24.0))
                .children(columns.into_iter().map(|column| column.into_any_element())),
        )
}

fn render_page_column(groups: Vec<Div>) -> Div {
    div()
        .w(px(360.0))
        .min_w(px(320.0))
        .flex()
        .flex_col()
        .gap(px(18.0))
        .children(groups.into_iter().map(|group| group.into_any_element()))
}

fn render_group(title: &str, rows: Vec<AnyElement>, theme: &Theme) -> Div {
    div()
        .rounded(px(14.0))
        .bg(theme.settings_card_bg)
        .border_1()
        .border_color(theme.settings_card_border)
        .p_4()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(
            div()
                .pb(px(4.0))
                .border_b_1()
                .border_color(theme.border_subtle)
                .text_size(px(9.5))
                .font_weight(FontWeight(600.0))
                .text_color(theme.text_disabled)
                .child(title.to_uppercase()),
        )
        .children(rows)
}

fn render_value_row(label: &str, value: &str, description: &str, theme: &Theme) -> Div {
    render_setting_row(
        label,
        description,
        render_value_chip(value, theme).into_any_element(),
        theme,
    )
}

fn render_path_row(label: &str, path: String, theme: &Theme) -> Div {
    render_setting_row(
        label,
        "",
        render_path_chip(&path, theme).into_any_element(),
        theme,
    )
}

fn render_toggle_row(label: &str, description: &str, active: bool, theme: &Theme) -> Div {
    render_setting_row(
        label,
        description,
        render_toggle(active, theme).into_any_element(),
        theme,
    )
}

fn render_setting_row(label: &str, description: &str, control: AnyElement, theme: &Theme) -> Div {
    div()
        .py(px(6.0))
        .border_b_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.025))
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(FontWeight(450.0))
                        .text_color(theme.text_primary)
                        .child(label.to_string()),
                )
                .when(!description.is_empty(), |this| {
                    this.child(
                        div()
                            .text_size(px(10.5))
                            .line_height(px(16.0))
                            .text_color(theme.text_tertiary)
                            .child(description.to_string()),
                    )
                }),
        )
        .child(control)
}

fn render_value_chip(value: &str, theme: &Theme) -> Div {
    div()
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .bg(theme.settings_body_bg)
        .border_1()
        .border_color(theme.settings_body_border)
        .text_size(px(11.0))
        .text_color(theme.text_secondary)
        .child(value.to_string())
}

fn render_path_chip(value: &str, theme: &Theme) -> Div {
    div()
        .max_w(px(220.0))
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .bg(theme.settings_body_bg)
        .border_1()
        .border_color(theme.settings_body_border)
        .text_size(px(10.5))
        .text_color(theme.text_tertiary)
        .child(value.to_string())
}

fn render_toggle(active: bool, theme: &Theme) -> Div {
    div()
        .w(px(36.0))
        .h(px(20.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(if active {
            theme.accent
        } else {
            theme.border_default
        })
        .bg(if active {
            theme.accent
        } else {
            theme.surface_hover
        })
        .px(px(2.0))
        .flex()
        .items_center()
        .child(
            div()
                .w(px(14.0))
                .h(px(14.0))
                .rounded_full()
                .bg(gpui::white())
                .when(active, |this| this.ml_auto()),
        )
}

fn render_updates_group(state: &AppState, theme: &Theme) -> Div {
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

    render_group(
        "Updates",
        vec![
            render_value_row(
                "Current",
                &format!("v{current}"),
                "Installed desktop shell version.",
                theme,
            )
            .into_any_element(),
            render_value_row(
                "Latest",
                latest_value.as_deref().unwrap_or("Pending"),
                "Most recent registry release check result.",
                theme,
            )
            .into_any_element(),
            render_status_row("Status", &status_label, status_color, theme).into_any_element(),
            render_updates_action(state, theme).into_any_element(),
        ],
        theme,
    )
}

fn render_status_row(label: &str, value: &str, color: gpui::Hsla, theme: &Theme) -> Div {
    div()
        .py(px(6.0))
        .border_b_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.025))
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(12.5))
                .font_weight(FontWeight(450.0))
                .text_color(theme.text_primary)
                .child(label.to_string()),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(color)
                .child(value.to_string()),
        )
}

fn render_updates_action(state: &AppState, theme: &Theme) -> Div {
    let (label, accent): (&'static str, bool) = match &state.update_check {
        UpdateCheck::Available { .. } => ("Open release", true),
        UpdateCheck::Checking => ("Checking…", false),
        _ => ("Check now", false),
    };

    let mut button = action_button(label, accent, theme);
    match &state.update_check {
        UpdateCheck::Available { .. } => {
            button = button.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.dispatch_action(Box::new(OpenLatestReleasePage), cx);
            });
        }
        UpdateCheck::Checking => {}
        _ => {
            button = button.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.dispatch_action(Box::new(CheckForUpdates), cx);
            });
        }
    }
    div().pt(px(4.0)).flex().justify_end().child(button)
}

fn render_account_group(state: &AppState, theme: &Theme) -> Div {
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
        .filter(|value| !value.is_empty())
        .map(|value| format!("@{value}"))
        .unwrap_or_else(|| "—".to_string());

    let mut button = match auth.status {
        DesktopAuthStatus::SignedIn => {
            action_button("Sign out", false, theme).text_color(hsla(0.0, 0.7, 0.58, 1.0))
        }
        DesktopAuthStatus::AwaitingBrowser => action_button("Cancel", false, theme),
        DesktopAuthStatus::Failed | DesktopAuthStatus::SignedOut => {
            action_button("Sign in", true, theme)
        }
    };

    match auth.status {
        DesktopAuthStatus::SignedIn | DesktopAuthStatus::AwaitingBrowser => {
            button = button.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.dispatch_action(Box::new(SignOut), cx);
            });
        }
        DesktopAuthStatus::Failed | DesktopAuthStatus::SignedOut => {
            button = button.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.dispatch_action(Box::new(SignInToAtoRun), cx);
            });
        }
    }

    render_group(
        "Authentication",
        vec![
            render_status_row("Status", status_label, status_color, theme).into_any_element(),
            render_value_row(
                "Handle",
                &handle,
                "Primary publisher identity for the shell.",
                theme,
            )
            .into_any_element(),
            div()
                .pt(px(4.0))
                .flex()
                .justify_end()
                .child(button)
                .into_any_element(),
        ],
        theme,
    )
}

fn render_theme_mode_row(state: &AppState, theme: &Theme) -> Div {
    div()
        .py(px(6.0))
        .border_b_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.025))
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(FontWeight(450.0))
                        .text_color(theme.text_primary)
                        .child("Theme"),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .line_height(px(16.0))
                        .text_color(theme.text_tertiary)
                        .child("Choose the shell theme. System is shown as a visual placeholder for future behavior."),
                ),
        )
        .child(
            div()
                .rounded(px(7.0))
                .bg(theme.settings_body_bg)
                .border_1()
                .border_color(theme.settings_body_border)
                .p_1()
                .flex()
                .items_center()
                .gap(px(2.0))
                .child(theme_pill("System", false, false, theme))
                .child(theme_pill("Light", state.theme_mode == ThemeMode::Light, true, theme))
                .child(theme_pill("Dark", state.theme_mode == ThemeMode::Dark, true, theme)),
        )
}

fn theme_pill(label: &'static str, active: bool, interactive: bool, theme: &Theme) -> Div {
    div()
        .px(px(12.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .cursor_pointer()
        .bg(if active {
            theme.accent_subtle
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .text_size(px(11.0))
        .font_weight(FontWeight(500.0))
        .text_color(if active {
            theme.accent
        } else {
            theme.text_disabled
        })
        .when(interactive && !active, |this| {
            this.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.dispatch_action(Box::new(ToggleTheme), cx);
            })
        })
        .child(label)
}

fn render_auto_devtools_row(state: &AppState, theme: &Theme) -> Div {
    div()
        .py(px(6.0))
        .border_b_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.025))
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(FontWeight(450.0))
                        .text_color(theme.text_primary)
                        .child("Auto-open DevTools"),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .line_height(px(16.0))
                        .text_color(theme.text_tertiary)
                        .child("Open the native web inspector after a capsule mounts."),
                ),
        )
        .child(
            div()
                .id("toggle-auto-devtools")
                .cursor_pointer()
                .child(render_toggle(state.config.auto_open_devtools, theme))
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    cx.stop_propagation();
                    window.dispatch_action(Box::new(ToggleAutoDevtools), cx);
                }),
        )
}

fn render_hosts_row(hosts: &[String], theme: &Theme) -> Div {
    let text = if hosts.is_empty() {
        "localhost only".to_string()
    } else {
        hosts.join(", ")
    };
    render_value_row(
        "Allow hosts",
        &text,
        "Default allowlist applied when egress is not fully denied.",
        theme,
    )
}

fn service_row(name: &str, status: &str, running: bool, theme: &Theme) -> Div {
    div()
        .py(px(6.0))
        .border_b_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.025))
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(if running {
                    hsla(145.0 / 360.0, 0.68, 0.55, 1.0)
                } else {
                    theme.text_disabled
                }))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(theme.text_secondary)
                        .child(name.to_string()),
                ),
        )
        .child(
            div()
                .text_size(px(10.5))
                .text_color(if running {
                    theme.accent
                } else {
                    theme.text_disabled
                })
                .child(status.to_string()),
        )
}

fn render_diagnostics_block(body_text: &str, theme: &Theme) -> Div {
    div()
        .rounded(px(10.0))
        .bg(theme.settings_body_bg)
        .border_1()
        .border_color(theme.settings_body_border)
        .p_3()
        .text_size(px(10.5))
        .line_height(px(18.0))
        .text_color(theme.text_disabled)
        .child(if body_text.is_empty() {
            "Companion native pane for host-side state and diagnostics.".to_string()
        } else {
            body_text.to_string()
        })
}

fn render_empty_group_message(message: &str, theme: &Theme) -> Div {
    div()
        .py(px(6.0))
        .text_size(px(11.0))
        .line_height(px(18.0))
        .text_color(theme.text_disabled)
        .child(message.to_string())
}

fn render_trust_entry(label: &str, state_label: &str, accent: gpui::Hsla, theme: &Theme) -> Div {
    div()
        .py(px(6.0))
        .border_b_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.025))
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight(500.0))
                        .text_color(theme.text_primary)
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme.text_disabled)
                        .child("Publisher fingerprint available from signed session metadata."),
                ),
        )
        .child(
            div()
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(hsla(0.0, 0.0, 1.0, 0.04))
                .text_size(px(9.5))
                .font_weight(FontWeight(600.0))
                .text_color(accent)
                .child(state_label.to_string()),
        )
}

fn action_button(label: &'static str, accent: bool, theme: &Theme) -> gpui::Stateful<Div> {
    let (bg, fg, border) = if accent {
        (theme.accent, gpui::white(), theme.accent)
    } else {
        (
            theme.settings_body_bg,
            theme.text_secondary,
            theme.settings_body_border,
        )
    };

    div()
        .id(label)
        .px(px(12.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_size(px(11.0))
        .font_weight(FontWeight(500.0))
        .text_color(fg)
        .cursor_pointer()
        .child(label)
}

fn format_home_path(suffix: &str) -> String {
    format!("~/{suffix}")
}

fn default_device_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "local-device".to_string())
}

fn settings_tab_index(tab: SettingsTab) -> usize {
    match tab {
        SettingsTab::General => 1,
        SettingsTab::Account => 2,
        SettingsTab::Runtime => 3,
        SettingsTab::Sandbox => 4,
        SettingsTab::Trust => 5,
        SettingsTab::Registry => 6,
        SettingsTab::Projection => 7,
        SettingsTab::Developer => 8,
        SettingsTab::About => 9,
    }
}

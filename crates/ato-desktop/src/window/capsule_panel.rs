use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, hsla, point, px, rgb, size, svg, AnyElement, AnyWindowHandle, App, Bounds, BoxShadow,
    Context, FontWeight, IntoElement, MouseButton, Pixels, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, WindowBackgroundAppearance, WindowBounds, WindowDecorations,
    WindowKind, WindowOptions,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::TitleBar;

use crate::app::{
    OpenContentWindowLogs, OpenContentWindowSettings, OpenStoreWindow, RestartContentWindow,
    StopContentWindow,
};
use crate::window::content_windows::{
    CapsuleWindowContext, CapsuleWindowStatus, OpenContentWindows,
};
use crate::window::ControlBarController;

const PANEL_W: f32 = 388.0;
const PANEL_H: f32 = 476.0;
const SETTINGS_W: f32 = 860.0;
const SETTINGS_H: f32 = 700.0;

#[derive(Default)]
pub struct CapsulePanelWindowSlot(pub Option<AnyWindowHandle>);

impl gpui::Global for CapsulePanelWindowSlot {}

#[derive(Default)]
pub struct CapsuleSettingsWindowSlot(pub Option<AnyWindowHandle>);

impl gpui::Global for CapsuleSettingsWindowSlot {}

#[derive(Clone, Debug)]
enum CapsulePanelModel {
    Empty,
    Unmanaged(UnmanagedPanel),
    Managed(ManagedPanel),
}

#[derive(Clone, Debug)]
struct UnmanagedPanel {
    title: String,
    url: String,
}

#[derive(Clone, Debug)]
struct ManagedPanel {
    window_id: u64,
    title: String,
    publisher: String,
    handle: String,
    version: String,
    current_url: String,
    local_url: Option<String>,
    session_id: Option<String>,
    status: CapsuleWindowStatus,
    trust_state: String,
    runtime_label: Option<String>,
    display_strategy: Option<String>,
    capabilities: Vec<String>,
    log_path: Option<String>,
    restricted: bool,
    error_message: Option<String>,
}

impl ManagedPanel {
    fn from_context(window_id: u64, context: &CapsuleWindowContext) -> Self {
        let handle = context.active_handle().to_string();
        let publisher = handle
            .rsplit_once('/')
            .map(|(publisher, _)| publisher.to_string())
            .unwrap_or_else(|| "local capsule".to_string());
        Self {
            window_id,
            title: context.title.clone(),
            publisher,
            handle,
            version: context.version_label().to_string(),
            current_url: context.current_url.clone(),
            local_url: context.local_url.clone(),
            session_id: context.session_id.clone(),
            status: context.status.clone(),
            trust_state: context.trust_state.clone(),
            runtime_label: context.runtime_label.clone(),
            display_strategy: context.display_strategy.clone(),
            capabilities: context.capabilities.clone(),
            log_path: context.log_path.clone(),
            restricted: context.restricted,
            error_message: context.error_message.clone(),
        }
    }
}

pub fn open_capsule_panel_window(cx: &mut App) -> Result<()> {
    let existing = cx.global::<CapsulePanelWindowSlot>().0;
    if let Some(handle) = existing {
        let close_result = handle.update(cx, |_, window, _| window.remove_window());
        cx.set_global(CapsulePanelWindowSlot(None));
        if close_result.is_ok() {
            return Ok(());
        }
    }

    let model = snapshot_frontmost_panel_model(cx);
    if let CapsulePanelModel::Managed(model) = model {
        let options = WindowOptions {
            titlebar: Some(TitleBar::title_bar_options()),
            focus: true,
            show: true,
            is_movable: true,
            is_resizable: true,
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(SETTINGS_W), px(SETTINGS_H)),
                cx,
            ))),
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        };
        return open_capsule_settings_window_with_model(cx, model, options);
    }

    let options = WindowOptions {
        titlebar: None,
        focus: true,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        window_bounds: Some(WindowBounds::Windowed(panel_bounds(cx))),
        window_decorations: Some(WindowDecorations::Client),
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };

    let handle = cx.open_window(options, move |window, cx| {
        let shell = cx.new(|_cx| CapsulePanelWindow {
            model: model.clone(),
        });
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    cx.set_global(CapsulePanelWindowSlot(Some(*handle)));
    Ok(())
}

pub fn open_capsule_settings_window(cx: &mut App, window_id: u64) -> Result<()> {
    if let Some(existing) = cx.global::<CapsuleSettingsWindowSlot>().0 {
        let _ = existing.update(cx, |_, window, _| window.remove_window());
        cx.set_global(CapsuleSettingsWindowSlot(None));
    }

    let Some(model) = snapshot_managed_panel(cx, window_id) else {
        return Ok(());
    };

    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        is_movable: true,
        is_resizable: true,
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(SETTINGS_W), px(SETTINGS_H)),
            cx,
        ))),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    open_capsule_settings_window_with_model(cx, model, options)
}

pub fn open_demo_capsule_settings_window(cx: &mut App) -> Result<()> {
    if let Some(existing) = cx.global::<CapsuleSettingsWindowSlot>().0 {
        let _ = existing.update(cx, |_, window, _| window.remove_window());
        cx.set_global(CapsuleSettingsWindowSlot(None));
    }

    let model = ManagedPanel {
        window_id: 0,
        title: "Scroll Repro Capsule".to_string(),
        publisher: "aodd".to_string(),
        handle: "local/aodd-scroll-repro".to_string(),
        version: "0.1.0".to_string(),
        current_url: "http://127.0.0.1:43123/index.html".to_string(),
        local_url: Some("http://127.0.0.1:43123".to_string()),
        session_id: Some("sess-aodd-scroll-repro".to_string()),
        status: CapsuleWindowStatus::Ready,
        trust_state: "trusted".to_string(),
        runtime_label: Some("source/node".to_string()),
        display_strategy: Some("webview".to_string()),
        capabilities: (1..=36).map(|i| format!("capability-{i:02}")).collect(),
        log_path: Some("/Users/egamikohsuke/.ato/logs/aodd-scroll-repro.log".to_string()),
        restricted: true,
        error_message: Some(
            "Demonstration settings payload with enough content to require scrolling.".to_string(),
        ),
    };
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        is_movable: true,
        is_resizable: true,
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(SETTINGS_W), px(SETTINGS_H)),
            cx,
        ))),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    open_capsule_settings_window_with_model(cx, model, options)
}

fn open_capsule_settings_window_with_model(
    cx: &mut App,
    model: ManagedPanel,
    options: WindowOptions,
) -> Result<()> {
    let handle = cx.open_window(options, move |window, cx| {
        let shell = cx.new(|_cx| CapsuleSettingsWindow {
            model: model.clone(),
            scroll_handle: ScrollHandle::default(),
        });
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    cx.set_global(CapsuleSettingsWindowSlot(Some(*handle)));
    Ok(())
}

fn snapshot_frontmost_panel_model(cx: &App) -> CapsulePanelModel {
    let Some(entry) = cx.global::<OpenContentWindows>().frontmost() else {
        return CapsulePanelModel::Empty;
    };
    panel_model_from_entry(
        entry.handle.window_id().as_u64(),
        entry.capsule.as_ref(),
        &entry,
    )
}

fn snapshot_managed_panel(cx: &App, window_id: u64) -> Option<ManagedPanel> {
    let entry = cx.global::<OpenContentWindows>().get(window_id)?.clone();
    let context = entry.capsule.as_ref()?;
    Some(ManagedPanel::from_context(window_id, context))
}

fn panel_model_from_entry(
    window_id: u64,
    context: Option<&CapsuleWindowContext>,
    entry: &crate::window::content_windows::ContentWindowEntry,
) -> CapsulePanelModel {
    match context {
        Some(context) => CapsulePanelModel::Managed(ManagedPanel::from_context(window_id, context)),
        None => CapsulePanelModel::Unmanaged(UnmanagedPanel {
            title: entry.title.to_string(),
            url: entry.url.to_string(),
        }),
    }
}

fn panel_bounds(cx: &mut App) -> Bounds<Pixels> {
    let panel_size = size(px(PANEL_W), px(PANEL_H));
    let control_bar = cx.global::<ControlBarController>().handle;
    if let Some(handle) = control_bar {
        if let Ok(bounds) = handle.update(cx, |_, window, _| window.bounds()) {
            let left = bounds.origin.x + (bounds.size.width - panel_size.width) / 2.0;
            let top = bounds.origin.y + bounds.size.height + px(12.0);
            return Bounds {
                origin: point(left, top),
                size: panel_size,
            };
        }
    }
    Bounds::centered(None, panel_size, cx)
}

struct CapsulePanelWindow {
    model: CapsulePanelModel,
}

impl Render for CapsulePanelWindow {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut Context<Self>) -> impl IntoElement {
        floating_panel_shell(render_panel_body(&self.model))
    }
}

struct CapsuleSettingsWindow {
    model: ManagedPanel,
    scroll_handle: ScrollHandle,
}

impl Render for CapsuleSettingsWindow {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let model = &self.model;
        let log_enabled = model.log_path.is_some();
        let window_id = model.window_id;

        div().size_full().bg(rgb(0xf8fafc)).p(px(24.0)).child(
            div()
                .size_full()
                .rounded(px(24.0))
                .bg(rgb(0xffffff))
                .border_1()
                .border_color(rgb(0xe5e7eb))
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.12),
                    offset: point(px(0.0), px(16.0)),
                    blur_radius: px(42.0),
                    spread_radius: px(0.0),
                }])
                .p(px(24.0))
                .flex()
                .flex_col()
                .gap(px(18.0))
                .child(render_settings_header(model))
                .child(render_settings_tab_strip())
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h(px(0.0))
                        .child(
                            div()
                                .id("capsule-settings-scroll")
                                .size_full()
                                .track_scroll(&self.scroll_handle)
                                .overflow_y_scroll()
                                .child(
                                    div()
                                        .w_full()
                                        .flex()
                                        .flex_col()
                                        .gap(px(14.0))
                                        .child(render_settings_overview(model))
                                        .child(render_settings_permissions(model))
                                        .child(render_settings_configuration(model))
                                        .child(render_settings_runtime(model))
                                        .child(render_settings_logs(model))
                                        .child(render_settings_advanced(model)),
                                ),
                        )
                        .vertical_scrollbar(&self.scroll_handle),
                )
                .child(
                    div()
                        .pt(px(4.0))
                        .flex()
                        .justify_end()
                        .gap(px(10.0))
                        .child(secondary_button("Restart", move |_, window, cx| {
                            window
                                .dispatch_action(Box::new(RestartContentWindow { window_id }), cx);
                        }))
                        .child(secondary_button("Stop", move |_, window, cx| {
                            window.dispatch_action(Box::new(StopContentWindow { window_id }), cx);
                        }))
                        .child(secondary_button_enabled(
                            "Open logs",
                            log_enabled,
                            move |_, window, cx| {
                                window.dispatch_action(
                                    Box::new(OpenContentWindowLogs { window_id }),
                                    cx,
                                );
                            },
                        )),
                ),
        )
    }
}

fn floating_panel_shell(body: AnyElement) -> impl IntoElement {
    div()
        .size_full()
        .bg(hsla(0.0, 0.0, 0.0, 0.0))
        .p(px(8.0))
        .child(
            div()
                .size_full()
                .rounded(px(24.0))
                .bg(rgb(0xffffff))
                .border_1()
                .border_color(hsla(0.0, 0.0, 0.0, 0.08))
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.22),
                    offset: point(px(0.0), px(18.0)),
                    blur_radius: px(42.0),
                    spread_radius: px(0.0),
                }])
                .overflow_hidden()
                .child(body),
        )
}

fn render_panel_body(model: &CapsulePanelModel) -> AnyElement {
    match model {
        CapsulePanelModel::Empty => render_message_panel(
            "No active content window",
            "Open a capsule window, then click the capsule icon again.",
        ),
        CapsulePanelModel::Unmanaged(panel) => render_unmanaged_panel(panel),
        CapsulePanelModel::Managed(panel) => render_managed_summary(panel),
    }
}

fn render_message_panel(title: &str, body: &str) -> AnyElement {
    div()
        .size_full()
        .p(px(20.0))
        .flex()
        .flex_col()
        .gap(px(16.0))
        .child(render_panel_header("Capsule"))
        .child(
            div()
                .flex_1()
                .rounded(px(20.0))
                .bg(rgb(0xf8fafc))
                .border_1()
                .border_color(rgb(0xe5e7eb))
                .p(px(18.0))
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(8.0))
                .child(section_title(title))
                .child(section_body(body)),
        )
        .into_any_element()
}

fn render_unmanaged_panel(panel: &UnmanagedPanel) -> AnyElement {
    div()
        .size_full()
        .p(px(20.0))
        .flex()
        .flex_col()
        .gap(px(16.0))
        .child(render_panel_header("Capsule"))
        .child(
            div()
                .rounded(px(20.0))
                .bg(rgb(0xf8fafc))
                .border_1()
                .border_color(rgb(0xe5e7eb))
                .p(px(18.0))
                .flex()
                .flex_col()
                .gap(px(14.0))
                .child(unmanaged_hero())
                .child(section_body(
                    "This page is not managed by a capsule. The capsule icon only shows launch/session controls when the focused window belongs to a running capsule.",
                ))
                .child(render_inline_kv("Window", panel.title.as_str()))
                .child(render_inline_kv("Current page", panel.url.as_str())),
        )
        .child(
            div()
                .mt_auto()
                .flex()
                .justify_end()
                .gap(px(10.0))
                .child(secondary_button("Open Store", move |_, window, cx| {
                    window.dispatch_action(Box::new(OpenStoreWindow), cx);
                    window.remove_window();
                }))
                .child(close_button()),
        )
        .into_any_element()
}

fn render_managed_summary(panel: &ManagedPanel) -> AnyElement {
    let log_enabled = panel.log_path.is_some();
    let window_id = panel.window_id;

    div()
        .size_full()
        .p(px(20.0))
        .flex()
        .flex_col()
        .gap(px(14.0))
        .child(render_panel_header("Capsule"))
        .child(render_summary_hero(panel))
        .child(render_summary_section(
            "Current page",
            vec![
                render_inline_kv("URL", panel.current_url.as_str()),
                render_inline_kv(
                    "Local origin",
                    panel
                        .local_url
                        .as_deref()
                        .unwrap_or("No local origin reported"),
                ),
            ],
        ))
        .child(render_summary_section(
            "Permissions",
            vec![div()
                .flex()
                .flex_wrap()
                .gap(px(8.0))
                .children(render_capability_chips(&panel.capabilities))
                .into_any_element()],
        ))
        .child(render_summary_section(
            "Session",
            vec![
                render_inline_kv(
                    "Runtime",
                    panel.runtime_label.as_deref().unwrap_or("Unknown"),
                ),
                render_inline_kv(
                    "Display",
                    panel.display_strategy.as_deref().unwrap_or("Unknown"),
                ),
            ],
        ))
        .child(
            div()
                .mt_auto()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(primary_button("Capsule Settings", move |_, window, cx| {
                    window.dispatch_action(Box::new(OpenContentWindowSettings { window_id }), cx);
                    window.remove_window();
                }))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .gap(px(8.0))
                        .child(secondary_button("Restart", move |_, window, cx| {
                            window
                                .dispatch_action(Box::new(RestartContentWindow { window_id }), cx);
                            window.remove_window();
                        }))
                        .child(secondary_button("Stop", move |_, window, cx| {
                            window.dispatch_action(Box::new(StopContentWindow { window_id }), cx);
                            window.remove_window();
                        }))
                        .child(secondary_button_enabled(
                            "Logs",
                            log_enabled,
                            move |_, window, cx| {
                                if log_enabled {
                                    window.dispatch_action(
                                        Box::new(OpenContentWindowLogs { window_id }),
                                        cx,
                                    );
                                }
                                window.remove_window();
                            },
                        )),
                ),
        )
        .into_any_element()
}

fn render_summary_hero(panel: &ManagedPanel) -> impl IntoElement {
    div()
        .rounded(px(22.0))
        .bg(rgb(0xf8fafc))
        .border_1()
        .border_color(rgb(0xe5e7eb))
        .p(px(18.0))
        .flex()
        .flex_col()
        .gap(px(14.0))
        .child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap(px(12.0))
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(12.0))
                        .child(capsule_icon_tile())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .text_size(px(18.0))
                                        .font_weight(FontWeight(680.0))
                                        .text_color(rgb(0x111827))
                                        .child(panel.title.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(rgb(0x6b7280))
                                        .child(format!("{} · {}", panel.publisher, panel.handle)),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap(px(8.0))
                                        .child(render_meta_chip(
                                            panel.version.as_str(),
                                            rgb(0xffffff),
                                            rgb(0x334155),
                                            rgb(0xe2e8f0),
                                        ))
                                        .child(render_status_chip(panel.status.clone()))
                                        .child(render_trust_chip(panel.trust_state.as_str()))
                                        .when(panel.restricted, |this| {
                                            this.child(render_meta_chip(
                                                "Restricted",
                                                rgb(0xfffbeb),
                                                rgb(0xb45309),
                                                rgb(0xfde68a),
                                            ))
                                        }),
                                ),
                        ),
                )
                .child(
                    div()
                        .rounded(px(999.0))
                        .bg(rgb(0xffffff))
                        .border_1()
                        .border_color(rgb(0xe5e7eb))
                        .px(px(10.0))
                        .py(px(6.0))
                        .text_size(px(11.0))
                        .font_weight(FontWeight(650.0))
                        .text_color(rgb(0x475569))
                        .child("launch/session"),
                ),
        )
        .when_some(panel.error_message.as_ref(), |this, message| {
            this.child(
                div()
                    .rounded(px(14.0))
                    .bg(rgb(0xfef2f2))
                    .border_1()
                    .border_color(rgb(0xfca5a5))
                    .p(px(12.0))
                    .text_size(px(12.0))
                    .line_height(px(18.0))
                    .text_color(rgb(0x991b1b))
                    .child(message.clone()),
            )
        })
}

fn render_panel_header(title: &str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .child(
            div()
                .text_size(px(12.0))
                .font_weight(FontWeight(700.0))
                .text_color(rgb(0x64748b))
                .child(title.to_string()),
        )
        .child(
            div()
                .w(px(28.0))
                .h(px(28.0))
                .rounded_full()
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.0))
                .text_color(rgb(0x64748b))
                .hover(|style| style.bg(rgb(0xf4f4f5)))
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    cx.stop_propagation();
                    window.remove_window();
                })
                .child("×"),
        )
}

fn render_summary_section(title: &str, rows: Vec<AnyElement>) -> AnyElement {
    div()
        .rounded(px(18.0))
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe5e7eb))
        .p(px(14.0))
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight(700.0))
                .text_color(rgb(0x64748b))
                .child(title.to_string()),
        )
        .children(rows)
        .into_any_element()
}

fn render_settings_header(model: &ManagedPanel) -> impl IntoElement {
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap(px(20.0))
        .child(
            div()
                .flex()
                .items_start()
                .gap(px(16.0))
                .child(capsule_icon_tile())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_size(px(26.0))
                                .font_weight(FontWeight(700.0))
                                .text_color(rgb(0x0f172a))
                                .child(model.title.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(rgb(0x64748b))
                                .child(model.handle.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(px(8.0))
                                .child(render_meta_chip(
                                    model.version.as_str(),
                                    rgb(0xffffff),
                                    rgb(0x334155),
                                    rgb(0xe2e8f0),
                                ))
                                .child(render_status_chip(model.status.clone()))
                                .child(render_trust_chip(model.trust_state.as_str()))
                                .when(model.restricted, |this| {
                                    this.child(render_meta_chip(
                                        "Restricted",
                                        rgb(0xfffbeb),
                                        rgb(0xb45309),
                                        rgb(0xfde68a),
                                    ))
                                }),
                        ),
                ),
        )
        .child(
            div()
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(rgb(0x64748b))
                .child("Capsule settings"),
        )
}

fn render_settings_tab_strip() -> impl IntoElement {
    div().flex().items_center().gap(px(8.0)).children(
        [
            "Overview",
            "Permissions",
            "Configuration",
            "Runtime",
            "Logs",
            "Advanced",
        ]
        .into_iter()
        .map(|label| {
            render_meta_chip(label, rgb(0xf8fafc), rgb(0x475569), rgb(0xe2e8f0)).into_any_element()
        }),
    )
}

fn render_settings_overview(model: &ManagedPanel) -> AnyElement {
    render_settings_section(
        "Overview",
        vec![
            render_inline_kv("Current page", model.current_url.as_str()),
            render_inline_kv(
                "Local URL",
                model
                    .local_url
                    .as_deref()
                    .unwrap_or("No local URL reported"),
            ),
            render_inline_kv(
                "Session",
                model.session_id.as_deref().unwrap_or("Pending session"),
            ),
            render_inline_kv("Handle", model.handle.as_str()),
        ],
    )
}

fn render_settings_permissions(model: &ManagedPanel) -> AnyElement {
    render_settings_section(
        "Permissions",
        vec![
            render_inline_kv(
                "Sandbox",
                if model.restricted {
                    "Restricted"
                } else {
                    "Standard"
                },
            ),
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.0))
                .children(render_capability_chips(&model.capabilities))
                .into_any_element(),
        ],
    )
}

fn render_settings_configuration(model: &ManagedPanel) -> AnyElement {
    render_settings_section(
        "Configuration",
        vec![
            render_inline_kv("Publisher", model.publisher.as_str()),
            render_inline_kv("Version", model.version.as_str()),
            render_inline_kv(
                "Trust",
                if model.trust_state.is_empty() {
                    "Unknown"
                } else {
                    model.trust_state.as_str()
                },
            ),
            div()
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(rgb(0x64748b))
                .child(
                    "Editable capsule configuration is not wired into Focus View yet. This sheet is tied to the active launch/session rather than global app settings."
                        .to_string(),
                )
                .into_any_element(),
        ],
    )
}

fn render_settings_runtime(model: &ManagedPanel) -> AnyElement {
    render_settings_section(
        "Runtime",
        vec![
            render_inline_kv(
                "Runtime",
                model.runtime_label.as_deref().unwrap_or("Unknown"),
            ),
            render_inline_kv(
                "Display",
                model.display_strategy.as_deref().unwrap_or("Unknown"),
            ),
            render_inline_kv("Status", model.status.label()),
        ],
    )
}

fn render_settings_logs(model: &ManagedPanel) -> AnyElement {
    render_settings_section(
        "Logs",
        vec![
            render_inline_kv(
                "Log path",
                model.log_path.as_deref().unwrap_or("No log path reported"),
            ),
            model
                .error_message
                .as_ref()
                .map(|message| {
                    div()
                        .rounded(px(12.0))
                        .bg(rgb(0xfef2f2))
                        .border_1()
                        .border_color(rgb(0xfca5a5))
                        .p(px(12.0))
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(rgb(0x991b1b))
                        .child(message.clone())
                        .into_any_element()
                })
                .unwrap_or_else(|| {
                    div()
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(rgb(0x64748b))
                        .child(
                            "Use the Open logs action to inspect the active session log."
                                .to_string(),
                        )
                        .into_any_element()
                }),
        ],
    )
}

fn render_settings_advanced(model: &ManagedPanel) -> AnyElement {
    render_settings_section(
        "Advanced",
        vec![
            render_inline_kv("Handle", model.handle.as_str()),
            render_inline_kv(
                "Session ID",
                model.session_id.as_deref().unwrap_or("Pending session"),
            ),
            render_inline_kv(
                "Restricted",
                if model.restricted { "true" } else { "false" },
            ),
        ],
    )
}

fn render_settings_section(title: &str, rows: Vec<AnyElement>) -> AnyElement {
    div()
        .rounded(px(18.0))
        .bg(rgb(0xf8fafc))
        .border_1()
        .border_color(rgb(0xe5e7eb))
        .p(px(16.0))
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(section_title(title))
        .children(rows)
        .into_any_element()
}

fn render_inline_kv(label: &str, value: &str) -> AnyElement {
    div()
        .flex()
        .justify_between()
        .gap(px(14.0))
        .child(
            div()
                .text_size(px(11.5))
                .text_color(rgb(0x6b7280))
                .child(label.to_string()),
        )
        .child(
            div()
                .max_w(px(460.0))
                .text_size(px(11.5))
                .text_color(rgb(0x111827))
                .child(value.to_string()),
        )
        .into_any_element()
}

fn render_capability_chips(capabilities: &[String]) -> Vec<AnyElement> {
    if capabilities.is_empty() {
        return vec![render_meta_chip(
            "No host capability grants reported",
            rgb(0xf8fafc),
            rgb(0x64748b),
            rgb(0xe2e8f0),
        )
        .into_any_element()];
    }
    capabilities
        .iter()
        .map(|capability| {
            render_meta_chip(capability, rgb(0xf5f3ff), rgb(0x5b21b6), rgb(0xddd6fe))
                .into_any_element()
        })
        .collect()
}

fn render_status_chip(status: CapsuleWindowStatus) -> impl IntoElement {
    let (background, foreground, border) = match status {
        CapsuleWindowStatus::Ready => (rgb(0xecfdf5), rgb(0x047857), rgb(0xa7f3d0)),
        CapsuleWindowStatus::Starting => (rgb(0xfffbeb), rgb(0xb45309), rgb(0xfde68a)),
        CapsuleWindowStatus::Failed => (rgb(0xfef2f2), rgb(0xb91c1c), rgb(0xfca5a5)),
    };
    render_meta_chip(status.label(), background, foreground, border)
}

fn render_trust_chip(trust_state: &str) -> impl IntoElement {
    let label = if trust_state.is_empty() {
        "unknown"
    } else {
        trust_state
    };
    render_meta_chip(label, rgb(0xffffff), rgb(0x475569), rgb(0xe2e8f0))
}

fn render_meta_chip(
    label: &str,
    background: gpui::Rgba,
    foreground: gpui::Rgba,
    border: gpui::Rgba,
) -> impl IntoElement {
    div()
        .px(px(9.0))
        .py(px(5.0))
        .rounded(px(999.0))
        .bg(background)
        .border_1()
        .border_color(border)
        .text_size(px(10.5))
        .font_weight(FontWeight(650.0))
        .text_color(foreground)
        .child(label.to_string())
}

fn primary_button(
    label: &str,
    on_click: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .w_full()
        .py(px(10.0))
        .rounded(px(12.0))
        .bg(rgb(0x111827))
        .text_color(rgb(0xffffff))
        .text_size(px(12.5))
        .font_weight(FontWeight(680.0))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .hover(|style| style.bg(rgb(0x1f2937)))
        .on_mouse_down(MouseButton::Left, on_click)
        .child(label.to_string())
}

fn secondary_button(
    label: &str,
    on_click: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut App) + 'static,
) -> impl IntoElement {
    secondary_button_enabled(label, true, on_click)
}

fn secondary_button_enabled(
    label: &str,
    enabled: bool,
    on_click: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .flex_1()
        .py(px(9.0))
        .rounded(px(12.0))
        .bg(if enabled {
            rgb(0xf8fafc)
        } else {
            rgb(0xf1f5f9)
        })
        .border_1()
        .border_color(rgb(0xe2e8f0))
        .text_color(if enabled {
            rgb(0x334155)
        } else {
            rgb(0x94a3b8)
        })
        .text_size(px(12.0))
        .font_weight(FontWeight(650.0))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .when(enabled, |this| this.hover(|style| style.bg(rgb(0xf1f5f9))))
        .when(enabled, |this| {
            this.on_mouse_down(MouseButton::Left, on_click)
        })
        .child(label.to_string())
}

fn close_button() -> impl IntoElement {
    div()
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(10.0))
        .bg(rgb(0xf4f4f5))
        .text_color(rgb(0x334155))
        .font_weight(FontWeight(650.0))
        .text_size(px(12.0))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(0xe4e4e7)))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            cx.stop_propagation();
            window.remove_window();
        })
        .child("Close")
}

fn capsule_icon_tile() -> impl IntoElement {
    div()
        .w(px(46.0))
        .h(px(46.0))
        .rounded(px(16.0))
        .bg(rgb(0xe0e7ff))
        .border_1()
        .border_color(rgb(0xc7d2fe))
        .flex()
        .items_center()
        .justify_center()
        .child(
            svg()
                .path(SharedString::from("icons/capsule.svg"))
                .size(px(22.0))
                .text_color(rgb(0x4f46e5)),
        )
}

fn unmanaged_hero() -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(
            div()
                .w(px(44.0))
                .h(px(44.0))
                .rounded(px(16.0))
                .bg(rgb(0xf1f5f9))
                .border_1()
                .border_color(rgb(0xe2e8f0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(18.0))
                .text_color(rgb(0x64748b))
                .child("◌"),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(section_title("This page is not managed by a capsule"))
                .child(section_body(
                    "Open a running capsule app to view launch/session controls.",
                )),
        )
}

fn section_title(title: &str) -> impl IntoElement {
    div()
        .text_size(px(16.0))
        .font_weight(FontWeight(680.0))
        .text_color(rgb(0x111827))
        .child(title.to_string())
}

fn section_body(body: &str) -> impl IntoElement {
    div()
        .text_size(px(12.5))
        .line_height(px(20.0))
        .text_color(rgb(0x64748b))
        .child(body.to_string())
}

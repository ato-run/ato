use gpui::{
    actions, point, px, size, App, AppContext, Bounds, KeyBinding, TitlebarOptions, WindowBounds,
    WindowDecorations, WindowOptions,
};

use crate::ui::DesktopShell;

const TRAFFIC_LIGHT_X: f32 = 14.0;
const TRAFFIC_LIGHT_Y: f32 = 14.0;

actions!(
    ato_desktop,
    [
        FocusCommandBar,
        ToggleOverview,
        NextWorkspace,
        PreviousWorkspace,
        NextTask,
        PreviousTask,
        SplitPane,
        ExpandSplit,
        ShrinkSplit,
        DismissTransient,
        CycleHandle,
        Quit
    ]
);

pub fn run() {
    gpui_platform::application().run(|cx: &mut App| {
        gpui_component::init(cx);

        // Scope the shell shortcuts so guest webviews do not inherit host commands.
        cx.bind_keys([
            KeyBinding::new("cmd-k", FocusCommandBar, Some("DeskyShell")),
            KeyBinding::new("cmd-b", ToggleOverview, Some("DeskyShell")),
            KeyBinding::new("ctrl-tab", NextWorkspace, Some("DeskyShell")),
            KeyBinding::new("ctrl-shift-tab", PreviousWorkspace, Some("DeskyShell")),
            KeyBinding::new("cmd-]", NextTask, Some("DeskyShell")),
            KeyBinding::new("cmd-[", PreviousTask, Some("DeskyShell")),
            KeyBinding::new("cmd-\\", SplitPane, Some("DeskyShell")),
            KeyBinding::new("cmd-alt-right", ExpandSplit, Some("DeskyShell")),
            KeyBinding::new("cmd-alt-left", ShrinkSplit, Some("DeskyShell")),
            KeyBinding::new("tab", CycleHandle, Some("DeskyShell")),
            KeyBinding::new("escape", DismissTransient, Some("DeskyShell")),
            KeyBinding::new("cmd-q", Quit, None),
        ]);

        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1440.0), px(920.0)), cx);

        // Let GPUI draw the shell chrome so the window feels like an in-app surface.
        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(TRAFFIC_LIGHT_X), px(TRAFFIC_LIGHT_Y))),
                    ..Default::default()
                }),
                focus: true,
                show: true,
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_decorations: Some(WindowDecorations::Client),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| DesktopShell::new(window, cx)),
        )
        .expect("failed to open ato-desktop window");

        cx.activate(true);
    });
}

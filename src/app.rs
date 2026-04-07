use gpui::{
    actions, px, size, Action, App, AppContext, AssetSource, Bounds, KeyBinding, SharedString,
    WindowBounds, WindowDecorations, WindowOptions,
};
use serde::Deserialize;
use std::borrow::Cow;

use crate::ui::DesktopShell;
use gpui_component::TitleBar;

actions!(
    ato_desktop,
    [
        FocusCommandBar,
        ShowSettings,
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
        BrowserBack,
        BrowserForward,
        BrowserReload,
        NewTab,
        Quit
    ]
);

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct NavigateToUrl {
    pub url: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct SelectTask {
    pub task_id: usize,
}

struct LocalAssetSource(std::path::PathBuf);

impl AssetSource for LocalAssetSource {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let full_path = self.0.join(path);
        if let Ok(data) = std::fs::read(&full_path) {
            Ok(Some(Cow::Owned(data)))
        } else {
            println!("Debug: Failed to load asset: {}", full_path.display());
            Ok(None)
        }
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

pub fn run() {
    let assets_dir = std::env::current_dir().unwrap().join("assets");
    gpui_platform::application()
        .with_assets(LocalAssetSource(assets_dir))
        .run(|cx: &mut App| {
            gpui_component::init(cx);

            // Scope the shell shortcuts so guest webviews do not inherit host commands.
            cx.bind_keys([
                KeyBinding::new("cmd-k", FocusCommandBar, Some("DeskyShell")),
                KeyBinding::new("cmd-b", ToggleOverview, Some("DeskyShell")),
                KeyBinding::new("cmd-,", ShowSettings, Some("DeskyShell")),
                KeyBinding::new("ctrl-tab", NextWorkspace, Some("DeskyShell")),
                KeyBinding::new("ctrl-shift-tab", PreviousWorkspace, Some("DeskyShell")),
                KeyBinding::new("cmd-]", NextTask, Some("DeskyShell")),
                KeyBinding::new("cmd-[", PreviousTask, Some("DeskyShell")),
                KeyBinding::new("cmd-\\", SplitPane, Some("DeskyShell")),
                KeyBinding::new("cmd-alt-right", ExpandSplit, Some("DeskyShell")),
                KeyBinding::new("cmd-alt-left", ShrinkSplit, Some("DeskyShell")),
                KeyBinding::new("tab", CycleHandle, Some("DeskyShell")),
                KeyBinding::new("cmd-t", NewTab, Some("DeskyShell")),
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
                    titlebar: Some(TitleBar::title_bar_options()),
                    focus: true,
                    show: true,
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_decorations: Some(WindowDecorations::Client),
                    ..Default::default()
                },
                |window, cx| {
                    let shell = cx.new(|cx| DesktopShell::new(window, cx));
                    cx.new(|cx| gpui_component::Root::new(shell, window, cx))
                },
            )
            .expect("failed to open ato-desktop window");

            cx.activate(true);
        });
}

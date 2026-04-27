use gpui::{
    actions, px, size, Action, App, AppContext, AssetSource, Bounds, KeyBinding, SharedString,
    WindowBounds, WindowDecorations, WindowOptions,
};
#[cfg(target_os = "macos")]
use gpui::{Menu, MenuItem, OsAction, SystemMenuType};
#[cfg(target_os = "macos")]
use gpui_component::input;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::ui::DesktopShell;
use gpui::AsyncApp;
use gpui_component::TitleBar;

actions!(
    ato_desktop,
    [
        FocusCommandBar,
        ShowSettings,
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
        NativeUndo,
        NativeRedo,
        NativeCut,
        NativeCopy,
        NativePaste,
        NativeSelectAll,
        ToggleTheme,
        OpenLocalRegistry,
        OpenCloudDock,
        SignInToAtoRun,
        OpenAuthInBrowser,
        CancelAuthHandoff,
        ResumeAfterAuth,
        AllowPermissionOnce,
        AllowPermissionForSession,
        DenyPermissionPrompt,
        SaveConfigForm,
        CancelConfigForm,
        ToggleDevConsole,
        ToggleAutoDevtools,
        Quit,
        ConfirmQuitKeep,
        ConfirmQuitClear,
        CancelQuit
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

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct CloseTask {
    pub task_id: usize,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct MoveTask {
    pub task_id: usize,
    pub to_index: usize,
}

struct LocalAssetSource(std::path::PathBuf);

#[derive(Default)]
pub struct OpenUrlBridge {
    pending: Mutex<VecDeque<String>>,
    async_app: Mutex<Option<AsyncApp>>,
    refresh_scheduled: Arc<AtomicBool>,
}

impl OpenUrlBridge {
    pub fn push_urls(&self, urls: Vec<String>) {
        if urls.is_empty() {
            return;
        }

        if let Ok(mut pending) = self.pending.lock() {
            pending.extend(urls);
        }

        self.schedule_refresh();
    }

    pub fn install_async_app(&self, async_app: AsyncApp) {
        if let Ok(mut slot) = self.async_app.lock() {
            *slot = Some(async_app.clone());
        }
        self.schedule_refresh();
    }

    pub fn drain_urls(&self) -> Vec<String> {
        let Ok(mut pending) = self.pending.lock() else {
            return Vec::new();
        };
        pending.drain(..).collect()
    }

    fn schedule_refresh(&self) {
        let async_app = self
            .async_app
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().cloned());

        let Some(async_app) = async_app else {
            return;
        };

        if self.refresh_scheduled.swap(true, Ordering::AcqRel) {
            return;
        }

        let refresh_app = async_app.clone();
        let refresh_scheduled = self.refresh_scheduled.clone();
        async_app
            .foreground_executor()
            .spawn(async move {
                refresh_app.refresh();
                refresh_scheduled.store(false, Ordering::Release);
            })
            .detach();
    }
}

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
    let assets_dir = resolve_assets_dir().expect("failed to resolve ato-desktop assets directory");
    let open_url_bridge = Arc::new(OpenUrlBridge::default());
    let application = gpui_platform::application().with_assets(LocalAssetSource(assets_dir));
    application.on_open_urls({
        let open_url_bridge = open_url_bridge.clone();
        move |urls| {
            open_url_bridge.push_urls(urls);
        }
    });
    application.run(move |cx: &mut App| {
        gpui_component::init(cx);
        open_url_bridge.install_async_app(cx.to_async());

        // Scope the shell shortcuts so guest webviews do not inherit host commands.
        cx.bind_keys([
            KeyBinding::new("cmd-k", FocusCommandBar, Some("DeskyShell")),
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
            KeyBinding::new("cmd-z", NativeUndo, Some("Pane")),
            KeyBinding::new("cmd-shift-z", NativeRedo, Some("Pane")),
            KeyBinding::new("cmd-x", NativeCut, Some("Pane")),
            KeyBinding::new("cmd-c", NativeCopy, Some("Pane")),
            KeyBinding::new("cmd-v", NativePaste, Some("Pane")),
            KeyBinding::new("cmd-a", NativeSelectAll, Some("Pane")),
            KeyBinding::new("cmd-alt-i", ToggleDevConsole, None),
            KeyBinding::new("cmd-r", BrowserReload, Some("DeskyShell")),
            KeyBinding::new("cmd-left", BrowserBack, Some("DeskyShell")),
            KeyBinding::new("cmd-right", BrowserForward, Some("DeskyShell")),
            KeyBinding::new("cmd-q", Quit, None),
        ]);

        #[cfg(target_os = "macos")]
        install_app_menus(cx);

        cx.on_action(|_: &NativeUndo, _: &mut App| {});
        cx.on_action(|_: &NativeRedo, _: &mut App| {});
        cx.on_action(|_: &NativeCut, _: &mut App| {});
        cx.on_action(|_: &NativeCopy, _: &mut App| {});
        cx.on_action(|_: &NativePaste, _: &mut App| {});
        cx.on_action(|_: &NativeSelectAll, _: &mut App| {});
        // Quit is intercepted by DesktopShell so it can prompt the
        // user to keep or clear persisted tabs. ConfirmQuitKeep /
        // ConfirmQuitClear / CancelQuit are the resolution actions.
        cx.on_action(|_: &ConfirmQuitKeep, cx| cx.quit());
        cx.on_action(|_: &ConfirmQuitClear, cx| {
            if let Some(home) = dirs::home_dir() {
                let path = home.join(".ato").join("desktop-tabs.json");
                let _ = std::fs::remove_file(&path);
            }
            cx.quit();
        });
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
            {
                let open_url_bridge = open_url_bridge.clone();
                move |window, cx| {
                    let shell = cx.new(|cx| DesktopShell::new(window, cx, open_url_bridge.clone()));
                    cx.new(|cx| gpui_component::Root::new(shell, window, cx))
                }
            },
        )
        .expect("failed to open ato-desktop window");

        cx.activate(true);
    });
}

#[cfg(target_os = "macos")]
fn install_app_menus(cx: &mut App) {
    cx.set_menus(vec![
        Menu {
            name: "ato-desktop".into(),
            items: vec![
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Undo", NativeUndo, OsAction::Undo),
                MenuItem::os_action("Redo", NativeRedo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::action("Cut", NativeCut),
                MenuItem::action("Copy", NativeCopy),
                MenuItem::action("Paste", NativePaste),
                MenuItem::separator(),
                MenuItem::action("Delete", input::Delete),
                MenuItem::action("Delete Previous Word", input::DeleteToPreviousWordStart),
                MenuItem::action("Delete Next Word", input::DeleteToNextWordEnd),
                MenuItem::separator(),
                MenuItem::action("Find", input::Search),
                MenuItem::separator(),
                MenuItem::action("Select All", NativeSelectAll),
            ],
            disabled: false,
        },
    ]);
}

fn resolve_assets_dir() -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("ATO_DESKTOP_ASSETS_DIR") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return Ok(path);
        }
    }

    let cwd_assets = std::env::current_dir()?.join("assets");
    if cwd_assets.is_dir() {
        return Ok(cwd_assets);
    }

    let exe = std::env::current_exe()?;
    let macos_dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("ato-desktop executable has no parent directory"))?;

    let bundled_assets = macos_dir.parent().and_then(|contents| {
        contents
            .parent()
            .map(|_| contents.join("Resources").join("assets"))
    });
    if let Some(path) = bundled_assets {
        if path.is_dir() {
            return Ok(path);
        }
    }

    let sibling_assets = macos_dir.join("assets");
    if sibling_assets.is_dir() {
        return Ok(sibling_assets);
    }

    Err(anyhow::anyhow!(
        "ato-desktop assets directory was not found; set ATO_DESKTOP_ASSETS_DIR or run from the app root"
    ))
}

#[cfg(test)]
mod tests {
    use super::resolve_assets_dir;

    #[test]
    fn resolve_assets_dir_finds_workspace_assets() {
        let path = resolve_assets_dir().expect("workspace assets should resolve");
        assert!(path.ends_with("assets"));
        assert!(path.is_dir());
    }
}

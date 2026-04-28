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
        SignOut,
        OpenAuthInBrowser,
        CancelAuthHandoff,
        ResumeAfterAuth,
        AllowPermissionOnce,
        AllowPermissionForSession,
        DenyPermissionPrompt,
        SaveConfigForm,
        CancelConfigForm,
        ToggleRouteMetadataPopover,
        ToggleDevConsole,
        ToggleAutoDevtools,
        CheckForUpdates,
        OpenLatestReleasePage,
        Quit,
        ConfirmQuitKeep,
        ConfirmQuitClear,
        CancelQuit,
        // RFC: SURFACE_CLOSE_SEMANTICS §6 — explicit Stop UI. The
        // shortcut on `StopActiveSession` is provisional; if a
        // platform / keymap conflict surfaces we re-bind without
        // changing the action name.
        StopActiveSession,
        StopAllRetainedSessions
    ]
);

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct NavigateToUrl {
    pub url: String,
}

/// Hand a URL to the OS so it opens in the user's default browser
/// (or whatever app is registered for the scheme). Used by the
/// route-metadata popover to make local_url / healthcheck_url /
/// invoke_url click-through to the same dev server the WebView is
/// rendering, but in a real browser for inspection / DevTools.
#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct OpenExternalLink {
    pub url: String,
}

/// Trigger the active pane to navigate to a registry handle pinned to a
/// newer version (e.g. `capsule://ato.run/foo/bar@1.2.3`). Dispatched by
/// the Install-update button in the route-metadata popover. The desktop
/// reuses the existing NavigateToUrl flow, so there's no extra install
/// plumbing — `ato app session start` lazily fetches & installs whatever
/// version isn't cached yet.
#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct InstallCapsuleUpdate {
    pub url: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct SelectTask {
    pub task_id: usize,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct SelectSettingsTab {
    pub tab: crate::state::SettingsTab,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Action)]
#[action(namespace = ato_desktop, no_json)]
pub struct SelectRouteMetadataTab {
    pub tab: crate::state::CapsuleDetailTab,
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
        let bg = async_app.background_executor().clone();
        let refresh_scheduled = self.refresh_scheduled.clone();
        async_app
            .foreground_executor()
            .spawn(async move {
                // Defer to a future tick. Without this, install_async_app
                // and the macOS first-launch on_open_urls callback both
                // run while GPUI's App RefCell is already mut-borrowed
                // (we are inside application.run() / an AppKit selector
                // when they fire). Calling refresh() right away then
                // double-borrows and panics with
                // "RefCell already borrowed" at gpui async_context.rs.
                // A 16 ms timer (≈ one render frame) yields control back
                // to the GPUI event loop so the original borrow drops
                // before refresh() runs.
                bg.timer(std::time::Duration::from_millis(16)).await;
                refresh_app.refresh();
                refresh_scheduled.store(false, Ordering::Release);
            })
            .detach();
    }
}

impl AssetSource for LocalAssetSource {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        // Local override first — lets us ship our own bg images,
        // automation/, preload/, etc. under crates/ato-desktop/assets/.
        let full_path = self.0.join(path);
        if let Ok(data) = std::fs::read(&full_path) {
            return Ok(Some(Cow::Owned(data)));
        }
        // Fall back to the gpui-component bundle for icons/*.svg etc.
        // gpui_component widgets (Icon, Close button) reference paths
        // like "icons/close.svg" that live inside gpui-component's
        // RustEmbed bundle, not under our local assets/ tree.
        match gpui_component_assets::Assets.load(path) {
            Ok(Some(data)) => Ok(Some(data)),
            _ => {
                println!("Debug: Failed to load asset: {}", full_path.display());
                Ok(None)
            }
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        // Delegate to gpui-component-assets so widgets that enumerate
        // (e.g. icon pickers) see the bundled SVGs.
        gpui_component_assets::Assets.list(path)
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
            KeyBinding::new("cmd-k", FocusCommandBar, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-,", ShowSettings, Some("AtoDesktopShell")),
            KeyBinding::new("ctrl-tab", NextWorkspace, Some("AtoDesktopShell")),
            KeyBinding::new("ctrl-shift-tab", PreviousWorkspace, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-]", NextTask, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-[", PreviousTask, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-\\", SplitPane, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-alt-right", ExpandSplit, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-alt-left", ShrinkSplit, Some("AtoDesktopShell")),
            KeyBinding::new("tab", CycleHandle, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-t", NewTab, Some("AtoDesktopShell")),
            KeyBinding::new("escape", DismissTransient, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-z", NativeUndo, Some("Pane")),
            KeyBinding::new("cmd-shift-z", NativeRedo, Some("Pane")),
            KeyBinding::new("cmd-x", NativeCut, Some("Pane")),
            KeyBinding::new("cmd-c", NativeCopy, Some("Pane")),
            KeyBinding::new("cmd-v", NativePaste, Some("Pane")),
            KeyBinding::new("cmd-a", NativeSelectAll, Some("Pane")),
            KeyBinding::new("cmd-alt-i", ToggleDevConsole, None),
            KeyBinding::new("cmd-r", BrowserReload, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-left", BrowserBack, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-right", BrowserForward, Some("AtoDesktopShell")),
            KeyBinding::new("cmd-q", Quit, None),
            // RFC: SURFACE_CLOSE_SEMANTICS §6.3 — provisional Stop
            // shortcut. Cmd+W remains "close pane" (now retains the
            // session); Cmd+Shift+W is the explicit "stop session"
            // action that actively kills the process.
            KeyBinding::new("cmd-shift-w", StopActiveSession, Some("AtoDesktopShell")),
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

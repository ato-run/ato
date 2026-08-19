//! Ato Desktop Tauri composition root.
//!
//! `main` hosts only the bundled launcher and is the sole window with native
//! capability. `home` hosts the remote PWA without capability, and `app-*`
//! windows host one verified loopback surface each. Every top-level navigation
//! is classified by [`navigation::classify`] and routed to the WebView, the OS
//! browser, or Rust-side intent parsing.

mod binary;
mod host;
mod navigation;
mod windows;

use tauri::webview::NewWindowResponse;
use tauri::{Manager, Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

use navigation::{NavigationAction, NavigationRole};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            host::desktop_info,
            host::computation_execute,
            host::run_inspect,
            host::pick_project,
            host::open_home,
            host::open_web_surface,
        ])
        .setup(|app| {
            build_main_window(app.handle())?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Ato desktop shell");
}

fn build_main_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    let navigation_app = app.clone();
    let new_window_app = app.clone();
    WebviewWindowBuilder::new(
        app,
        navigation::MAIN_WINDOW_LABEL,
        WebviewUrl::App("index.html".into()),
    )
    .title("Ato")
    .inner_size(1200.0, 800.0)
    .min_inner_size(760.0, 540.0)
    .on_navigation(
        move |url| match navigation::classify(url, &NavigationRole::Main) {
            NavigationAction::Allow => true,
            NavigationAction::DispatchIntent => {
                dispatch_intent(&navigation_app, url);
                false
            }
            NavigationAction::OpenExternal => {
                open_external(&navigation_app, url);
                false
            }
            NavigationAction::Deny => false,
        },
    )
    .on_new_window(move |url, _features| {
        open_external(&new_window_app, &url);
        NewWindowResponse::Deny
    })
    .build()?;
    Ok(())
}

pub(crate) fn build_home_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    if let Some(home) = app.get_webview_window(navigation::HOME_WINDOW_LABEL) {
        home.show()?;
        home.set_focus()?;
        return Ok(());
    }

    let navigation_app = app.clone();
    let new_window_app = app.clone();
    let home_url: Url = navigation::HOME_ORIGIN
        .parse()
        .expect("HOME_ORIGIN is a valid URL");
    WebviewWindowBuilder::new(
        app,
        navigation::HOME_WINDOW_LABEL,
        WebviewUrl::External(home_url),
    )
    .title("Ato Home")
    .inner_size(1200.0, 800.0)
    .min_inner_size(760.0, 540.0)
    .on_navigation(
        move |url| match navigation::classify(url, &NavigationRole::Home) {
            NavigationAction::Allow => true,
            NavigationAction::DispatchIntent => {
                dispatch_intent(&navigation_app, url);
                false
            }
            NavigationAction::OpenExternal => {
                open_external(&navigation_app, url);
                false
            }
            NavigationAction::Deny => false,
        },
    )
    .on_new_window(move |url, _features| {
        open_external(&new_window_app, &url);
        NewWindowResponse::Deny
    })
    .build()?;
    Ok(())
}

pub(crate) fn open_app_window(
    app: &tauri::AppHandle,
    label: &str,
    url: Url,
    origin: &str,
) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(label) {
        window.show().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
        return Ok(());
    }

    let navigation_app = app.clone();
    let new_window_app = app.clone();
    let origin = origin.to_owned();
    WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        .title(label)
        .inner_size(1100.0, 760.0)
        .min_inner_size(640.0, 480.0)
        .on_navigation(move |target| {
            match navigation::classify(
                target,
                &NavigationRole::App {
                    origin: origin.clone(),
                },
            ) {
                NavigationAction::Allow => true,
                NavigationAction::OpenExternal => {
                    open_external(&navigation_app, target);
                    false
                }
                NavigationAction::DispatchIntent | NavigationAction::Deny => false,
            }
        })
        .on_new_window(move |url, _features| {
            open_external(&new_window_app, &url);
            NewWindowResponse::Deny
        })
        .build()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn dispatch_intent(app: &tauri::AppHandle, url: &Url) {
    match navigation::parse_intent(url) {
        Some(navigation::Intent::OpenHome) => {
            let _ = build_home_window(app);
        }
        None => {
            eprintln!("ato-desktop-tauri: rejected unknown intent {url}");
        }
    }
}

fn open_external(app: &tauri::AppHandle, url: &Url) {
    if matches!(url.scheme(), "http" | "https" | "mailto")
        && let Err(error) = app.opener().open_url(url.as_str(), None::<&str>)
    {
        eprintln!("ato-desktop-tauri: could not open external URL: {error}");
    }
}

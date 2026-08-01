//! Ato Desktop Tauri composition root.
//!
//! `main` hosts only the bundled offline Launcher and is the sole window with
//! native capabilities. `home` hosts the remote PWA without capabilities.

mod host;
mod proxy;

use std::sync::Arc;

use tauri::webview::NewWindowResponse;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

pub(crate) const MAIN_WINDOW_LABEL: &str = "main";
pub(crate) const HOME_WINDOW_LABEL: &str = "home";
const HOME_ORIGIN: &str = "https://app.ato.run";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_view_proxy = Arc::new(proxy::AppViewProxy::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(host::DesktopHost::new())
        .invoke_handler(tauri::generate_handler![
            host::runner_status,
            host::runner_start,
            host::runner_stop,
            host::open_home,
        ])
        .register_asynchronous_uri_scheme_protocol(
            proxy::ATOVIEW_SCHEME,
            move |_context, request, responder| {
                let proxy = app_view_proxy.clone();
                tauri::async_runtime::spawn(async move {
                    responder.respond(proxy.handle(request).await);
                });
            },
        )
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
    WebviewWindowBuilder::new(app, MAIN_WINDOW_LABEL, WebviewUrl::App("index.html".into()))
        .title("Ato")
        .inner_size(1200.0, 800.0)
        .min_inner_size(760.0, 540.0)
        .resizable(true)
        .on_navigation(move |url| {
            if is_local_launcher_url(url) {
                return true;
            }
            if url.scheme() == "ato" {
                dispatch_native_intent(&navigation_app, url.as_str());
                return false;
            }
            open_external_url(&navigation_app, url);
            false
        })
        .on_new_window(move |url, _features| {
            open_external_url(&new_window_app, &url);
            NewWindowResponse::Deny
        })
        .build()?;
    Ok(())
}

pub(crate) fn build_home_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    if let Some(home) = app.get_webview_window(HOME_WINDOW_LABEL) {
        home.show()?;
        home.set_focus()?;
        return Ok(());
    }

    let navigation_app = app.clone();
    let new_window_app = app.clone();
    let home_url = HOME_ORIGIN.parse().expect("HOME_ORIGIN is a valid URL");
    let marker = format!(
        "if (window.location.origin === {origin:?}) {{ Object.defineProperty(window, '__ATO_DESKTOP__', {{ value: Object.freeze({{ version: {version:?}, platform: {platform:?} }}), configurable: false, writable: false }}); }}",
        origin = HOME_ORIGIN,
        version = env!("CARGO_PKG_VERSION"),
        platform = std::env::consts::OS,
    );

    WebviewWindowBuilder::new(app, HOME_WINDOW_LABEL, WebviewUrl::External(home_url))
        .title("Ato Home")
        .inner_size(1200.0, 800.0)
        .min_inner_size(760.0, 540.0)
        .initialization_script(marker)
        .on_navigation(move |url| {
            if url.scheme() == "ato" {
                dispatch_native_intent(&navigation_app, url.as_str());
                return false;
            }
            if is_trusted_home_url(url) || url.scheme() == "about" {
                return true;
            }
            open_external_url(&navigation_app, url);
            false
        })
        .on_new_window(move |url, _features| {
            open_external_url(&new_window_app, &url);
            NewWindowResponse::Deny
        })
        .build()?;
    Ok(())
}

fn dispatch_native_intent(app: &tauri::AppHandle, uri: &str) {
    let host = app.state::<host::DesktopHost>();
    if let Err(error) = host.dispatch_intent_uri(uri) {
        eprintln!("ato-desktop: rejected intent {uri}: {error}");
    }
}

fn open_external_url(app: &tauri::AppHandle, url: &tauri::Url) {
    if matches!(url.scheme(), "http" | "https" | "mailto")
        && let Err(error) = app.opener().open_url(url.as_str(), None::<&str>)
    {
        eprintln!("ato-desktop: could not open external URL: {error}");
    }
}

fn is_local_launcher_url(url: &tauri::Url) -> bool {
    url.scheme() == "tauri"
        || (url.scheme() == "http" && url.host_str() == Some("tauri.localhost"))
        || (cfg!(debug_assertions)
            && matches!(url.scheme(), "http" | "https")
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1")))
}

fn is_trusted_home_url(url: &tauri::Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("app.ato.run")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_origin_is_pinned() {
        assert!(is_local_launcher_url(
            &"tauri://localhost/".parse().unwrap()
        ));
        assert!(is_local_launcher_url(
            &"http://tauri.localhost/".parse().unwrap()
        ));
        assert!(!is_local_launcher_url(
            &"https://app.ato.run/".parse().unwrap()
        ));
        assert!(!is_local_launcher_url(
            &"atoview://session.app.ato.run/".parse().unwrap()
        ));
    }

    #[test]
    fn home_origin_is_exact_and_https_only() {
        assert!(is_trusted_home_url(
            &"https://app.ato.run/store".parse().unwrap()
        ));
        assert!(!is_trusted_home_url(
            &"http://app.ato.run/store".parse().unwrap()
        ));
        assert!(!is_trusted_home_url(
            &"https://evil.app.ato.run/".parse().unwrap()
        ));
        assert!(!is_trusted_home_url(
            &"https://app.ato.run.evil.example/".parse().unwrap()
        ));
    }
}

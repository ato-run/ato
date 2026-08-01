//! Ato Desktop Tauri composition root.
//!
//! `main` hosts only the bundled offline Launcher and is the sole window with
//! native capabilities. `home` hosts the remote PWA without capabilities.

mod host;
mod proxy;

use std::sync::Arc;
use std::time::Duration;

use tauri::webview::NewWindowResponse;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

pub(crate) const MAIN_WINDOW_LABEL: &str = "main";
pub(crate) const HOME_WINDOW_LABEL: &str = "home";
const HOME_ORIGIN: &str = "https://app.ato.run";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_view_proxy = Arc::new(proxy::AppViewProxy::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(host::DesktopHost::new())
        .invoke_handler(tauri::generate_handler![
            host::runner_status,
            host::runner_start,
            host::runner_stop,
            host::open_home,
            host::library_list,
            host::library_inspect,
            host::library_install,
            host::library_update,
            host::library_rollback,
            host::library_remove,
            host::library_repair,
            host::operation_status,
            host::operation_cancel,
            host::session_list,
            host::session_launch,
            host::session_focus,
            host::session_close,
            host::session_stop,
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
            spawn_retention_maintenance(app.handle());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Ato desktop shell");
}

fn spawn_retention_maintenance(app: &tauri::AppHandle) {
    let app = app.clone();
    std::thread::Builder::new()
        .name("ato-retained-session-maintenance".to_owned())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(30));
            let host = app.state::<host::DesktopHost>();
            match host.sweep_retained() {
                Ok(stopped) if !stopped.is_empty() => {
                    let _ = app.emit(runner::events::SESSION_CHANGED, stopped);
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = app.emit(
                        runner::events::OPERATION_FAILED,
                        serde_json::json!({
                            "kind": "retained_session_cleanup",
                            "message": error.to_string(),
                        }),
                    );
                }
            }
        })
        .expect("retained-session maintenance thread starts");
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
                dispatch_home_intent(&navigation_app, url);
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

pub(crate) fn open_app_window(
    app: &tauri::AppHandle,
    envelope: &serde_json::Value,
) -> Result<(), String> {
    let session = envelope
        .get("session")
        .ok_or_else(|| "session launch response is missing session".to_string())?;
    let session_id = session
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "session launch response is missing session_id".to_string())?;
    if focus_app_window(app, session_id).is_ok() {
        return Ok(());
    }
    let local_url = session
        .pointer("/web/local_url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "session does not expose a web surface".to_string())?;
    let url: tauri::Url = local_url
        .parse()
        .map_err(|error| format!("invalid session web URL: {error}"))?;
    if !is_loopback_session_url(&url) {
        return Err("session web URL is not a loopback HTTP(S) origin".to_string());
    }
    let handle = session
        .get("handle")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(session_id)
        .to_owned();
    let label = app_window_label(session_id);
    let allowed_origin = url_origin(&url);
    let navigation_app = app.clone();
    let close_session_id = session_id.to_owned();
    let close_handle = handle.clone();
    let window = WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        .title(&handle)
        .inner_size(1100.0, 760.0)
        .min_inner_size(640.0, 480.0)
        .on_navigation(move |target| {
            if url_origin(target) == allowed_origin {
                return true;
            }
            open_external_url(&navigation_app, target);
            false
        })
        .build()
        .map_err(|error| error.to_string())?;
    let close_window = window.clone();
    let close_app = app.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = close_window.hide();
            let host = close_app.state::<host::DesktopHost>();
            let _ = host.retain_session(&close_session_id, &close_handle);
        }
    });
    Ok(())
}

pub(crate) fn focus_app_window(app: &tauri::AppHandle, session_id: &str) -> Result<(), String> {
    let window = app
        .get_webview_window(&app_window_label(session_id))
        .ok_or_else(|| format!("no app window for session {session_id}"))?;
    window.show().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

pub(crate) fn close_app_window(app: &tauri::AppHandle, session_id: &str) -> Result<(), String> {
    let window = app
        .get_webview_window(&app_window_label(session_id))
        .ok_or_else(|| format!("no app window for session {session_id}"))?;
    window.hide().map_err(|error| error.to_string())
}

fn app_window_label(session_id: &str) -> String {
    let safe = session_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("app-{safe}")
}

fn is_loopback_session_url(url: &tauri::Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"))
        && url.username().is_empty()
        && url.password().is_none()
}

fn url_origin(url: &tauri::Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or_default()
    )
}

#[derive(Clone, serde::Serialize)]
struct InstallHandoff {
    source_kind: String,
    source: String,
}

fn dispatch_home_intent(app: &tauri::AppHandle, url: &tauri::Url) {
    if let Some(handoff) = parse_install_handoff(url) {
        if let Some(main) = app.get_webview_window(MAIN_WINDOW_LABEL) {
            let _ = main.show();
            let _ = main.set_focus();
        }
        if let Err(error) = app.emit(runner::events::INSTALL_REQUESTED, handoff) {
            eprintln!("ato-desktop: could not emit install handoff: {error}");
        }
        return;
    }
    dispatch_native_intent(app, url.as_str());
}

fn parse_install_handoff(url: &tauri::Url) -> Option<InstallHandoff> {
    if url.scheme() != "ato" || url.host_str() != Some("desktop") || url.path() != "/install" {
        return None;
    }
    let mut source = None;
    let mut source_kind = "store".to_string();
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "source" if source.is_none() => source = Some(value.into_owned()),
            "kind" if matches!(value.as_ref(), "store" | "github") => {
                source_kind = value.into_owned();
            }
            "kind" => return None,
            _ => {}
        }
    }
    let source = source.filter(|value| !value.trim().is_empty())?;
    Some(InstallHandoff {
        source_kind,
        source,
    })
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

    #[test]
    fn install_handoff_accepts_only_the_narrow_desktop_route() {
        let handoff = parse_install_handoff(
            &"ato://desktop/install?source=acme%2Fchat&kind=store"
                .parse()
                .unwrap(),
        )
        .expect("valid handoff");
        assert_eq!(handoff.source_kind, "store");
        assert_eq!(handoff.source, "acme/chat");
        assert!(
            parse_install_handoff(
                &"ato://desktop/install?kind=local&source=/etc"
                    .parse()
                    .unwrap()
            )
            .is_none()
        );
        assert!(
            parse_install_handoff(&"ato://runner/install?source=acme/chat".parse().unwrap())
                .is_none()
        );
    }

    #[test]
    fn app_window_urls_are_loopback_only() {
        assert!(is_loopback_session_url(
            &"http://127.0.0.1:4317/".parse().unwrap()
        ));
        assert!(is_loopback_session_url(
            &"http://localhost:4317/".parse().unwrap()
        ));
        assert!(!is_loopback_session_url(
            &"https://app.ato.run/".parse().unwrap()
        ));
        assert!(!is_loopback_session_url(
            &"http://127.0.0.1.evil.example/".parse().unwrap()
        ));
    }
}

//! Ato Desktop — Tauri shell entry.
//!
//! Migration principle: this shell has NO UI of its own. It hosts the bundled
//! `ato-pwa` Desktop build (a local-asset WebView) and exposes a typed
//! [`host::DesktopHost`] command/event adapter over the abstracted `runner`
//! crate (which supervises the `ato` CLI — the sole owner of capsule execution)
//! and the shared `protocol::intent` verb vocabulary.
//!
//! Privileged commands are granted ONLY to the bundled first-party UI window
//! (see `capabilities/default.json`, scoped to the `main` window label). Remote
//! origins (including https://ato.run) are never given Tauri capabilities.

mod host;
mod proxy;

use std::sync::Arc;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// Window label the capability (`capabilities/default.json`) is scoped to.
const MAIN_WINDOW_LABEL: &str = "main";

/// Run the desktop shell. Mobile-ready entry point (Tauri v2 convention) so the
/// same shell can target mobile/IoT hosts as the `runner` abstraction grows.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // The same-origin app-view proxy holds the guest app-view cookie server-side
    // so the preview iframe never depends on a cross-site browser cookie.
    let app_view_proxy = Arc::new(proxy::AppViewProxy::new());

    tauri::Builder::default()
        .manage(host::DesktopHost::new())
        .invoke_handler(tauri::generate_handler![
            host::runner_status,
            host::runner_start,
            host::runner_stop,
            host::dispatch_privileged_intent,
            host::dispatch_intent_uri,
        ])
        .register_asynchronous_uri_scheme_protocol(
            proxy::ATOVIEW_SCHEME,
            move |_ctx, request, responder| {
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

/// Build the single `main` window hosting the bundled PWA and intercept its
/// `ato://` intent navigations.
///
/// The bundled PWA emits intents exactly as it does in the GPUI shell — by
/// navigating to an `ato://…` URL (see ato-pwa `src/desktop/bridge.ts`). Here
/// that navigation never loads a page: [`WebviewWindowBuilder::on_navigation`]
/// intercepts every `ato://` URL, hands it to the host, and cancels the
/// navigation so the PWA stays put. Trust is the window itself — this is the
/// bundled first-party local asset — not a web origin, so the origin allowlist
/// classifier stays in the GPUI shell and only the verb parsing is shared.
fn build_main_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    let handle = app.clone();
    WebviewWindowBuilder::new(app, MAIN_WINDOW_LABEL, WebviewUrl::default())
        .title("Ato")
        .inner_size(1200.0, 800.0)
        .resizable(true)
        .on_navigation(move |url| {
            if url.scheme() != "ato" {
                // Everything that is not an intent navigates normally.
                return true;
            }
            let host = handle.state::<host::DesktopHost>();
            if let Err(err) = host.dispatch_intent_uri(url.as_str()) {
                // Unwired / malformed intents are logged, never navigated to.
                eprintln!("ato-desktop: unhandled intent {url}: {err}");
            }
            // An `ato://` URL must never actually load — always cancel.
            false
        })
        .build()?;
    Ok(())
}

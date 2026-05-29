mod app;
mod automation;
mod bridge;
mod cli_envelope;
mod cli_install;
mod config;
mod egress_policy;
mod egress_proxy;
mod github_manifest_draft;
mod install_lifecycle_dashboard;
mod ipc;
mod localization;
mod logging;
mod netd;
mod orchestrator;
mod retention;
mod secret_bridge;
mod settings;
mod source_import_api;
mod source_import_runner;
mod source_import_session;
mod state;
mod surface_timing;
mod system_capsule;
mod terminal;
mod ui;
mod userland;
mod webview;
mod webview_init_guard;
mod window;

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("ato-desktop {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let _log_guard = logging::init_tracing();

    // On Windows, GPUI renders into a DirectComposition swapchain
    // (WS_EX_NOREDIRECTIONBITMAP). Child Wry/WebView2 HWNDs are not part of
    // that visual tree, so they are occluded and every WebView-backed window
    // paints black/blank. Disabling DirectComposition makes the WebView2 child
    // windows composite normally while DWM accent transparency (used for the
    // control-bar pill) keeps working. We default it on but honour an explicit
    // override so power users can still opt back into DirectComposition.
    #[cfg(target_os = "windows")]
    if std::env::var_os("GPUI_DISABLE_DIRECT_COMPOSITION").is_none() {
        // SAFETY: set on the main thread before the GPUI platform is created
        // (which reads this env var). No other thread reads this variable.
        unsafe { std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "1") };
        tracing::info!(
            "Windows: defaulting GPUI_DISABLE_DIRECT_COMPOSITION=1 so WebView2 children composite"
        );
    }

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        pid = std::process::id(),
        "ato-desktop starting",
    );

    let skip_onboarding = std::env::args().any(|a| a == "--skip-onboarding");

    match crate::orchestrator::resolve_ato_binary() {
        Ok(path) => tracing::info!(?path, "resolved ato binary"),
        Err(error) => tracing::warn!(?error, "could not resolve ato binary at startup"),
    }

    app::run(skip_onboarding);
}

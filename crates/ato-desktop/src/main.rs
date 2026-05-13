mod app;
mod automation;
mod bridge;
mod cli_envelope;
mod cli_install;
mod config;
mod egress_policy;
mod egress_proxy;
mod logging;
mod orchestrator;
mod retention;
mod settings;
mod state;
mod surface_timing;
mod system_capsule;
mod terminal;
mod ui;
mod userland;
mod webview;
mod window;

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("ato-desktop {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let _log_guard = logging::init_tracing();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        pid = std::process::id(),
        "ato-desktop starting",
    );

    app::run();
}

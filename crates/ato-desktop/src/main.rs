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
mod terminal;
mod ui;
mod userland;
mod webview;

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("ato-desktop {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    tracing_subscriber::fmt()
        .with_env_filter(logging::build_env_filter())
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    app::run();
}

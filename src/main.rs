mod app;
mod automation;
mod bridge;
mod egress_policy;
mod egress_proxy;
mod orchestrator;
mod state;
mod terminal;
mod ui;
mod webview;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ato_desktop=info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    app::run();
}

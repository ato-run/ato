use anyhow::Result;
use ato_connected_realization_worker::{ConnectedWorker, WorkerConfig, run_netns_surface_relay};
use clap::Parser;

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args
        .get(1)
        .is_some_and(|arg| arg == "__netns-surface-relay")
    {
        return run_netns_surface_relay(&args[2..]);
    }
    let config = WorkerConfig::parse();
    ConnectedWorker::new(config)?.run()
}

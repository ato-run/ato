use anyhow::Result;
use ato_connected_realization_worker::runtime_launch::sandbox_exec;
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
    // Re-entry from INSIDE the sandbox. bwrap sets up the namespaces, then
    // execs this binary as the Landlock shim, which restricts itself and execs
    // the workload. Handled before clap because it is not a user-facing
    // subcommand and must not appear in `--help`.
    //
    //   ato-connected-realization-worker sandbox-exec --policy <file> -- <argv...>
    if args.get(1).is_some_and(|arg| arg == "sandbox-exec") {
        let policy = args
            .iter()
            .position(|arg| arg == "--policy")
            .and_then(|index| args.get(index + 1))
            .ok_or_else(|| anyhow::anyhow!("sandbox-exec: --policy is required"))?;
        let workload = args
            .iter()
            .position(|arg| arg == "--")
            .map(|index| args[index + 1..].to_vec())
            .unwrap_or_default();
        return sandbox_exec::run(std::path::Path::new(policy), &workload);
    }

    let config = WorkerConfig::parse();
    ConnectedWorker::new(config)?.run()
}

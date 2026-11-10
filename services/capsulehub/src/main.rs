//! `ato-capsulehub` — self-hosted capsulehub coordinator.
//!
//! Runs the blind ciphertext store described in `docs/ops/capsulehub.md`. The
//! same binary is what ato.run/capsulehub runs; there is no separate hosted
//! implementation.

use std::process::ExitCode;

use capsulehub::{Config, serve};
use clap::Parser;

fn main() -> ExitCode {
    init_tracing();

    let config = match Config::parse().into_server_config() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("ato-capsulehub: {error}");
            return ExitCode::from(2);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("ato-capsulehub: failed to build tokio runtime: {error}");
            return ExitCode::from(2);
        }
    };

    match runtime.block_on(serve(config)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ato-capsulehub: {error}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    // RUST_LOG=capsulehub=debug enables request-level diagnostics. Default is
    // `info` so an operator sees startup, listener, and retention events only.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("capsulehub=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

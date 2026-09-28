//! Acceptance entry point to the existing Runtime service, not a new executor.
use anyhow::{Context, Result};
use ato_formation_worker::runtime_network::{ServeConfig, serve};
fn main() -> Result<()> {
    let a: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        a.len() == 6,
        "usage: proposal_runtime API TOKEN_FILE WORK OUT SHIM"
    );
    serve(&ServeConfig {
        api: a[1].clone(),
        token: std::fs::read_to_string(&a[2]).context("token file")?,
        work_root: (&a[3]).into(),
        out_dir: (&a[4]).into(),
        shim: (&a[5]).into(),
        browser_verifier: None,
        poll: std::time::Duration::from_millis(150),
        max_tickets: None,
    })
}

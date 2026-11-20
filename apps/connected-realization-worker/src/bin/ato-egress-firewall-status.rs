//! Read-only host admission probe for the systemd socket-activated service.
use anyhow::{Result, ensure};
use ato_connected_realization_worker::runtime_launch::network_broker::inspect_operator_firewall;

fn main() -> Result<()> {
    ensure!(
        std::env::args_os().len() == 1,
        "this probe has no arguments"
    );
    inspect_operator_firewall()?;
    println!("ready");
    Ok(())
}

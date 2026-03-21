use anyhow::{bail, Result};
use std::path::PathBuf;

pub struct GuestArgs {
    pub sync_path: PathBuf,
}

pub fn execute(args: GuestArgs) -> Result<()> {
    let _ = args.sync_path;
    bail!(
        "`ato guest` is currently unsupported on Windows because the upstream WASI runtime stack does not compile on the current Windows Rust toolchain"
    )
}

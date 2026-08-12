//! Detached local Capsule Session Supervisor and built-in PTY Driver.
//!
//! This is intentionally exposed only through `ato internal capsule-session`.

#[cfg(not(unix))]
use std::path::Path;

#[cfg(not(unix))]
use anyhow::{Result, bail};

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::{attach, kill, list, serve, start, status, watchdog};

#[cfg(not(unix))]
fn unsupported() -> Result<()> {
    bail!("detached Capsule Sessions require owner-only Unix control IPC in this release")
}

#[cfg(not(unix))]
pub(crate) fn start(_bundle: &Path, _into: &Path, _no_attach: bool) -> Result<()> {
    unsupported()
}

#[cfg(not(unix))]
pub(crate) fn serve(_session: &str, _bundle: &Path, _into: &Path) -> Result<()> {
    unsupported()
}

#[cfg(not(unix))]
pub(crate) fn attach(_session: &str, _observe: bool) -> Result<()> {
    unsupported()
}

#[cfg(not(unix))]
pub(crate) fn status(_session: &str) -> Result<()> {
    unsupported()
}

#[cfg(not(unix))]
pub(crate) fn kill(_session: &str) -> Result<()> {
    unsupported()
}

#[cfg(not(unix))]
pub(crate) fn list() -> Result<()> {
    unsupported()
}

#[cfg(not(unix))]
pub(crate) fn watchdog(
    _pid: u32,
    _pgid: i32,
    _process_start_identity: &str,
    _lease_fd: i32,
) -> Result<()> {
    unsupported()
}

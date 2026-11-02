//! Host-owned resource boundary for unattended private processes.
//!
//! Namespace isolation protects network and files, but does not cap memory,
//! processes or CPU. A private worker must live in its own bounded cgroup v2;
//! workloads inherit that cgroup and cannot access its control filesystem.

use anyhow::Result;
#[cfg(any(target_os = "linux", test))]
use anyhow::{Context, ensure};

#[cfg(any(target_os = "linux", test))]
const MAX_MEMORY: u64 = 1024 * 1024 * 1024;
#[cfg(any(target_os = "linux", test))]
const MAX_SWAP: u64 = 256 * 1024 * 1024;
#[cfg(any(target_os = "linux", test))]
const MAX_PIDS: u64 = 128;
#[cfg(any(target_os = "linux", test))]
const MAX_CPU_CORES: u128 = 1;

#[cfg(any(target_os = "linux", test))]
fn check_limits(memory: &str, swap: &str, pids: &str, cpu: &str) -> Result<()> {
    let finite = |value: &str| -> Result<u64> {
        let parsed: u64 = value
            .trim()
            .parse()
            .context("cgroup limit must be finite")?;
        ensure!(parsed > 0, "cgroup limit must be positive");
        Ok(parsed)
    };
    ensure!(
        finite(memory)? <= MAX_MEMORY,
        "private memory limit too large"
    );
    let swap: u64 = swap
        .trim()
        .parse()
        .context("cgroup swap limit must be finite")?;
    ensure!(swap <= MAX_SWAP, "private swap limit too large");
    ensure!(finite(pids)? <= MAX_PIDS, "private process limit too large");
    let values: Vec<_> = cpu.split_whitespace().collect();
    ensure!(values.len() == 2, "invalid cgroup CPU limit");
    let quota = finite(values[0])? as u128;
    let period = finite(values[1])? as u128;
    ensure!(
        quota <= period * MAX_CPU_CORES,
        "private CPU quota too large"
    );
    Ok(())
}

/// Read the worker's actual cgroup, not an environment claim. A missing,
/// unbounded or inaccessible controller fails closed.
#[cfg(target_os = "linux")]
pub fn verify_current() -> Result<()> {
    use std::path::{Component, Path};
    let membership = std::fs::read_to_string("/proc/self/cgroup")?;
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::/"))
        .context("cgroup v2 membership required")?;
    let path = Path::new(relative);
    ensure!(
        path.components()
            .all(|part| matches!(part, Component::Normal(_))),
        "invalid cgroup path"
    );
    let root = Path::new("/sys/fs/cgroup").canonicalize()?;
    let dir = root.join(path).canonicalize()?;
    ensure!(
        dir.starts_with(&root) && dir != root,
        "invalid cgroup boundary"
    );
    let read = |name: &str| {
        std::fs::read_to_string(dir.join(name))
            .with_context(|| format!("missing cgroup controller {name}"))
    };
    check_limits(
        &read("memory.max")?,
        &read("memory.swap.max")?,
        &read("pids.max")?,
        &read("cpu.max")?,
    )
}

#[cfg(not(target_os = "linux"))]
pub fn verify_current() -> Result<()> {
    anyhow::bail!("private resource isolation requires Linux cgroup v2")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_finite_bounded_controllers_are_admitted() {
        assert!(check_limits("1073741824", "0", "128", "100000 100000").is_ok());
        for limits in [
            ("max", "0", "128", "100000 100000"),
            ("1073741825", "0", "128", "100000 100000"),
            ("1073741824", "max", "128", "100000 100000"),
            ("1073741824", "0", "129", "100000 100000"),
            ("1073741824", "0", "128", "max 100000"),
            ("1073741824", "0", "128", "100001 100000"),
            ("1073741824", "0", "128", "100000 0"),
        ] {
            assert!(
                check_limits(limits.0, limits.1, limits.2, limits.3).is_err(),
                "{limits:?}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires an explicitly bounded cgroup v2 scope"]
    fn current_worker_is_inside_a_bounded_cgroup() {
        verify_current().expect("bounded cgroup v2");
    }
}

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

/// A container started by a separate daemon does not inherit the worker's
/// cgroup. Private multi-process realizations must prove that the worker and
/// the daemon-selected workload parent share the same bounded ancestor.
#[cfg(target_os = "linux")]
pub fn verify_common_parent(parent: &std::path::Path) -> Result<()> {
    use std::path::{Component, Path};
    let root = Path::new("/sys/fs/cgroup").canonicalize()?;
    let parent = parent.canonicalize()?;
    ensure!(
        parent.starts_with(&root) && parent != root,
        "invalid private common cgroup parent"
    );
    let membership = std::fs::read_to_string("/proc/self/cgroup")?;
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::/"))
        .context("cgroup v2 membership required")?;
    ensure!(
        Path::new(relative)
            .components()
            .all(|part| matches!(part, Component::Normal(_))),
        "invalid cgroup path"
    );
    let current = root.join(relative).canonicalize()?;
    ensure!(
        current.starts_with(&parent) && current != parent,
        "worker is outside the private common cgroup parent"
    );
    let read = |name: &str| {
        std::fs::read_to_string(parent.join(name))
            .with_context(|| format!("missing common cgroup controller {name}"))
    };
    check_limits(
        &read("memory.max")?,
        &read("memory.swap.max")?,
        &read("pids.max")?,
        &read("cpu.max")?,
    )?;
    verify_current()
}

#[cfg(not(target_os = "linux"))]
pub fn verify_common_parent(_parent: &std::path::Path) -> Result<()> {
    anyhow::bail!("private common resource isolation requires Linux cgroup v2")
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

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires an explicitly bounded common cgroup v2 parent"]
    fn current_worker_shares_bounded_parent_with_container_placement() {
        let parent = std::env::var_os("ATO_PRIVATE_COMMON_CGROUP").expect("common parent path");
        verify_common_parent(std::path::Path::new(&parent)).expect("bounded common cgroup");
        assert!(verify_common_parent(std::path::Path::new("/sys/fs/cgroup")).is_err());
        assert!(verify_common_parent(std::path::Path::new("/sys/fs/cgroup/system.slice")).is_err());
    }
}

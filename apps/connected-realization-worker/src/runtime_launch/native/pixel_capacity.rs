//! Local physical admission for a pair of OCI containers. The selected cgroup
//! parent is applied to BOTH containers; dockerd's own service cgroup is not a
//! workload limit. These are instantaneous measurements, not future guarantees.
use anyhow::{Context, Result, ensure};
use ato_adapter_oci::OciResourceLimits;
use serde::Deserialize;
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};

// The physical backend caps geometry at 4096x2160. Leave room outside the
// containers for its one bounded BGRX capture/coverage and common transport
// buffers. This overhead is Runner policy, never a change to K/D resources.
const RUNNER_FRAME_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(target_os = "linux")]
use std::{
    io::Write,
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub(crate) struct PixelOwner {
    pub socket: PathBuf,
    pub docker_parent: String,
    cgroup: PathBuf,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Capacity {
    pub memory_available: u64,
    pub cpu_millis: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Info {
    cgroup_driver: String,
    cgroup_version: String,
    #[serde(rename = "NCPU")]
    ncpu: u64,
    mem_total: u64,
}

fn bounded_file(path: &Path) -> Result<String> {
    let mut text = String::new();
    std::fs::File::open(path)?
        .take(65537)
        .read_to_string(&mut text)?;
    ensure!(text.len() <= 65536, "oversized physical capacity evidence");
    Ok(text)
}
fn cpus(text: &str) -> Result<u64> {
    let mut seen = std::collections::BTreeSet::new();
    for range in text.trim().split(',') {
        let (a, b) = range.split_once('-').unwrap_or((range, range));
        let (a, b) = (a.parse::<u32>()?, b.parse::<u32>()?);
        ensure!(a <= b && b < 4096, "invalid effective CPU set");
        for cpu in a..=b {
            ensure!(seen.insert(cpu), "overlapping effective CPU set");
        }
    }
    ensure!(!seen.is_empty(), "unknown effective CPU set");
    Ok(seen.len() as u64)
}
fn quota(text: &str) -> Result<Option<u64>> {
    let fields: Vec<_> = text.split_whitespace().collect();
    ensure!(fields.len() == 2, "unknown effective CPU quota");
    let period: u64 = fields[1].parse()?;
    ensure!(period > 0, "invalid CPU quota period");
    if fields[0] == "max" {
        return Ok(None);
    }
    let value = fields[0]
        .parse::<u64>()?
        .checked_mul(1000)
        .context("CPU quota overflow")?
        / period;
    ensure!(value > 0, "invalid CPU quota");
    Ok(Some(value))
}
fn memory(text: &str) -> Result<(u64, u64)> {
    let field = |name: &str| -> Result<u64> {
        let line = text
            .lines()
            .find(|line| line.starts_with(name))
            .context("missing host memory measurement")?;
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(
            fields.len() == 3 && fields[2] == "kB",
            "invalid host memory measurement"
        );
        fields[1]
            .parse::<u64>()?
            .checked_mul(1024)
            .context("host memory overflow")
    };
    let (total, available) = (field("MemTotal:")?, field("MemAvailable:")?);
    ensure!(
        total > 0 && available <= total,
        "invalid available host memory"
    );
    Ok((total, available))
}
fn parent_path(driver: &str, parent: &str) -> Result<PathBuf> {
    ensure!(
        parent.len() <= 256
            && parent
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_.-".contains(&b))
            && !parent.split('/').any(|part| part == "." || part == ".."),
        "invalid cgroup parent"
    );
    match driver {
        // Deliberately only existing root-level slices; nested systemd unit
        // escaping is unsupported rather than guessed from a unit name.
        "systemd" => {
            let stem = parent
                .strip_suffix(".slice")
                .context("systemd cgroup parent must be a slice")?;
            ensure!(
                !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "unsupported systemd cgroup parent"
            );
            Ok(PathBuf::from(parent))
        }
        "cgroupfs" => {
            let path = Path::new(parent);
            ensure!(
                path.is_absolute()
                    && path
                        .components()
                        .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
                    && !parent.contains("//")
                    && !parent.ends_with('/'),
                "invalid cgroupfs parent"
            );
            Ok(path.strip_prefix("/")?.to_path_buf())
        }
        _ => anyhow::bail!("unsupported local Docker cgroup driver"),
    }
}

#[cfg(not(target_os = "linux"))]
fn info(_: &Path) -> Result<Info> {
    anyhow::bail!("Pixel local capacity requires Linux")
}
#[cfg(target_os = "linux")]
fn info(socket: &Path) -> Result<Info> {
    let mut stream =
        UnixStream::connect(socket).context("connect local Docker management socket")?;
    let peer = rustix::net::sockopt::socket_peercred(&stream)
        .context("read local Docker management principal")?;
    ensure!(
        peer.uid.is_root() && peer.pid.as_raw_pid() > 0,
        "unknown local Docker management principal"
    );
    let peer_pid = peer.pid.as_raw_pid();
    // Socket activation normally reports root PID 1, not dockerd's PID.
    // Remote daemons and peers outside this PID namespace cannot satisfy this.
    let comm = bounded_file(&PathBuf::from(format!("/proc/{peer_pid}/comm")))?;
    ensure!(
        comm.trim() == "dockerd" || (peer_pid == 1 && comm.trim() == "systemd"),
        "unsupported Docker management peer"
    );
    ensure!(
        !Path::new("/.dockerenv").exists()
            && !Path::new("/run/.containerenv").exists()
            && !Path::new("/run/systemd/container").exists(),
        "Pixel capacity requires a local host boundary"
    );
    let init = bounded_file(Path::new("/proc/1/comm"))?;
    let init_cgroup = bounded_file(Path::new("/proc/1/cgroup"))?;
    ensure!(
        init.trim() == "systemd"
            && init_cgroup
                .lines()
                .any(|line| matches!(line, "0::/" | "0::/init.scope")),
        "unknown local init/cgroup namespace boundary"
    );
    let mountinfo = bounded_file(Path::new("/proc/self/mountinfo"))?;
    ensure!(
        mountinfo.lines().any(|line| {
            let Some((left, right)) = line.split_once(" - ") else {
                return false;
            };
            let fields: Vec<_> = left.split_whitespace().collect();
            fields.get(3) == Some(&"/")
                && fields.get(4) == Some(&"/sys/fs/cgroup")
                && right.starts_with("cgroup2 ")
        }),
        "unknown effective cgroup mount ancestry"
    );
    ensure!(
        !mountinfo.lines().any(|line| {
            line.split_whitespace()
                .nth(4)
                .is_some_and(|mount| mount.starts_with("/sys/fs/cgroup/"))
        }),
        "ambiguous nested cgroup mounts"
    );
    let deadline = Instant::now() + Duration::from_millis(500);
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    stream.write_all(b"GET /info HTTP/1.0\r\nHost: docker\r\nConnection: close\r\n\r\n")?;
    let mut response = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("Docker capacity probe timeout")?;
        stream.set_read_timeout(Some(remaining))?;
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        ensure!(
            response.len() + count <= 262144,
            "oversized Docker capacity evidence"
        );
        response.extend_from_slice(&buffer[..count]);
    }
    ensure!(
        response.starts_with(b"HTTP/1.0 200 ") || response.starts_with(b"HTTP/1.1 200 "),
        "Docker capacity probe refused"
    );
    let body = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("invalid Docker capacity response")?
        + 4;
    serde_json::from_slice(&response[body..]).context("invalid Docker capacity evidence")
}

impl PixelOwner {
    pub(crate) fn verify_container(&self, container: &ato_adapter_oci::OciHandle) -> Result<()> {
        container.verify_cgroup_parent(&self.docker_parent, &self.cgroup)
    }
    pub fn new(socket: PathBuf, docker_parent: String) -> Result<Self> {
        ensure!(socket.is_absolute(), "local Docker socket must be absolute");
        let info = info(&socket)?;
        ensure!(info.cgroup_version == "2", "Pixel requires cgroup v2");
        let cgroup = parent_path(&info.cgroup_driver, &docker_parent)?;
        let this = Self {
            socket,
            docker_parent,
            cgroup,
        };
        this.measure()?;
        Ok(this)
    }
    pub fn measure(&self) -> Result<Capacity> {
        let info = info(&self.socket)?;
        ensure!(
            info.cgroup_version == "2"
                && parent_path(&info.cgroup_driver, &self.docker_parent)? == self.cgroup,
            "Docker cgroup configuration changed"
        );
        let (total, available) = memory(&bounded_file(Path::new("/proc/meminfo"))?)?;
        ensure!(
            total == info.mem_total && info.ncpu > 0,
            "Docker and host capacity disagree"
        );
        measure(
            Path::new("/sys/fs/cgroup"),
            &self.cgroup,
            available,
            info.ncpu,
        )
    }
    pub fn admit(&self, app: &OciResourceLimits, helper: &OciResourceLimits) -> Result<Capacity> {
        let capacity = self.measure()?;
        capacity.admit(app, helper)?;
        Ok(capacity)
    }
}
impl Capacity {
    fn admit(&self, app: &OciResourceLimits, helper: &OciResourceLimits) -> Result<()> {
        let memory = app
            .memory_bytes
            .checked_add(helper.memory_bytes)
            .and_then(|bytes| bytes.checked_add(RUNNER_FRAME_HEADROOM_BYTES))
            .context("combined GUI memory overflow")?;
        let cpu = app
            .cpu_limit_millis
            .checked_add(helper.cpu_limit_millis)
            .context("combined GUI CPU overflow")?;
        ensure!(
            memory <= self.memory_available && cpu <= self.cpu_millis,
            "resource_insufficient: app plus display helper exceeds measured local capacity"
        );
        Ok(())
    }
}
fn measure(root: &Path, parent: &Path, available: u64, ncpu: u64) -> Result<Capacity> {
    let mut result = Capacity {
        memory_available: available,
        cpu_millis: ncpu.checked_mul(1000).context("CPU capacity overflow")?,
    };
    let mut directory = root.to_path_buf();
    // Read every effective ancestor, including root. No cgroup symlink or
    // guessed daemon-service limit can substitute for the selected parent.
    for component in std::iter::once(None).chain(parent.components().map(Some)) {
        if let Some(Component::Normal(part)) = component {
            directory.push(part);
        } else {
            ensure!(component.is_none(), "invalid cgroup ancestor");
        }
        let metadata = std::fs::symlink_metadata(&directory)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "unknown cgroup parent"
        );
        let cpu = cpus(&bounded_file(&directory.join("cpuset.cpus.effective"))?)?
            .checked_mul(1000)
            .context("CPU capacity overflow")?;
        result.cpu_millis = result.cpu_millis.min(cpu);
        // The actual cgroup-v2 root is exempt from resource control and has
        // no cpu.max/memory.max/current interfaces. Only that exact verified
        // mount root may omit them; every non-root ancestor must be observed.
        // https://docs.kernel.org/admin-guide/cgroup-v2.html
        if let Some(text) = control(&directory.join("cpu.max"), directory == root)?
            && let Some(quota) = quota(&text)?
        {
            result.cpu_millis = result.cpu_millis.min(quota);
        }
        if let Some(limit) = control(&directory.join("memory.max"), directory == root)? {
            let current = bounded_file(&directory.join("memory.current"))?
                .trim()
                .parse::<u64>()?;
            if limit.trim() != "max" {
                let limit: u64 = limit.trim().parse()?;
                ensure!(limit > 0, "unknown cgroup memory limit");
                result.memory_available =
                    result.memory_available.min(limit.saturating_sub(current));
            }
        }
    }
    Ok(result)
}
fn control(path: &Path, actual_root: bool) -> Result<Option<String>> {
    match bounded_file(path) {
        Ok(text) => Ok(Some(text)),
        Err(error)
            if actual_root
                && error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error).context("unknown effective cgroup control"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits(memory_bytes: u64, cpu_limit_millis: u64) -> OciResourceLimits {
        OciResourceLimits {
            memory_bytes,
            cpu_limit_millis,
            pids_limit: 32,
        }
    }
    #[test]
    fn aggregate_helper_overhead_cannot_exceed_capacity_or_overflow() {
        let capacity = Capacity {
            memory_available: 384 + RUNNER_FRAME_HEADROOM_BYTES,
            cpu_millis: 1000,
        };
        assert!(capacity.admit(&limits(128, 500), &limits(256, 500)).is_ok());
        assert!(
            capacity
                .admit(&limits(129, 500), &limits(256, 500))
                .is_err()
        );
        assert!(
            capacity
                .admit(&limits(128, 501), &limits(256, 500))
                .is_err()
        );
        assert!(
            capacity
                .admit(&limits(u64::MAX, u64::MAX), &limits(1, 1))
                .is_err()
        );
    }
    #[test]
    fn malformed_or_ambiguous_capacity_is_unknown() {
        for text in ["", "0-2,2", "4-1", "4096", "0,,1"] {
            assert!(cpus(text).is_err(), "{text}");
        }
        assert_eq!(cpus("0-3,5\n").unwrap(), 5);
        for text in ["max", "1 0", "0 1000", "max 1000 extra", "-1 1000"] {
            assert!(quota(text).is_err(), "{text}");
        }
        assert_eq!(quota("150000 100000").unwrap(), Some(1500));
        assert_eq!(quota("max 100000").unwrap(), None);
        assert!(memory("MemTotal: 4 kB\n").is_err());
        assert!(parent_path("systemd", "system-unknown.slice").is_err());
        assert!(parent_path("cgroupfs", "/a/../b").is_err());
        assert!(parent_path("unknown", "a").is_err());
    }
    #[test]
    fn effective_common_parent_not_daemon_sibling_limits_both_workloads() {
        let root = tempfile::tempdir().unwrap();
        for (path, quota, limit, current, cpuset) in [
            ("", "max 100000", "max", "600", "0-3"),
            ("system.slice", "150000 100000", "1000", "400", "0-2"),
            (
                "system.slice/docker.service",
                "10000 100000",
                "50",
                "40",
                "0",
            ),
        ] {
            let dir = root.path().join(path);
            std::fs::create_dir_all(&dir).unwrap();
            for (name, text) in [
                ("cpu.max", quota),
                ("memory.max", limit),
                ("memory.current", current),
                ("cpuset.cpus.effective", cpuset),
            ] {
                std::fs::write(dir.join(name), text).unwrap();
            }
        }
        assert_eq!(
            measure(root.path(), Path::new("system.slice"), 800, 4).unwrap(),
            Capacity {
                memory_available: 600,
                cpu_millis: 1500
            }
        );
        for file in ["cpu.max", "memory.max", "memory.current"] {
            std::fs::remove_file(root.path().join(file)).unwrap();
        }
        assert_eq!(
            measure(root.path(), Path::new("system.slice"), 800, 4).unwrap(),
            Capacity {
                memory_available: 600,
                cpu_millis: 1500
            }
        );
        std::fs::remove_file(root.path().join("system.slice/cpu.max")).unwrap();
        assert!(measure(root.path(), Path::new("system.slice"), 800, 4).is_err());
    }
}

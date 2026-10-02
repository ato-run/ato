//! Measured host resources, reported in the heartbeat.
//!
//! The control plane admits a Derivation onto a Runner by comparing the
//! Derivation's host requirement with what the Runner measured. Every value
//! here therefore names where it came from, and a value with no trustworthy
//! source is reported as absent — never guessed. Absent does not satisfy a
//! requirement.
//!
//! Memory in particular: inside a container `/proc/meminfo` describes the
//! HOST. On a rented GPU pod it showed 503 GiB while the pod was allowed
//! 62 GiB. So the cgroup limit is the only local source admission may use,
//! and `/proc/meminfo` is carried as a diagnostic only.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use serde::Serialize;

/// cgroup v1 reports "no limit" as a huge page-rounded number, not a word.
/// Anything at or above this is not a real limit.
const CGROUP_V1_UNLIMITED_BYTES: u64 = 1 << 60;
const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryResources {
    /// The container's hard limit. `None` when no finite limit is readable.
    pub cgroup_limit_mib: Option<u64>,
    /// Which interface produced `cgroup_limit_mib`.
    pub cgroup_source: Option<&'static str>,
    /// Diagnostic only. Never an admission input.
    pub proc_meminfo_mib: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CpuResources {
    /// CPU quota in thousandths of a core. `None` when unlimited or unreadable.
    pub cgroup_quota_millis: Option<u64>,
    /// Diagnostic only: CPUs the kernel shows, which may be the host's.
    pub online: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Accelerator {
    pub vendor: &'static str,
    pub vram_mib: u64,
    pub driver: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostResources {
    pub memory: MemoryResources,
    pub cpu: CpuResources,
    /// Free space on the filesystem that holds the Runner's work root.
    pub scratch_mib: Option<u64>,
    /// Only devices that answered a query. Empty when none did.
    pub accelerators: Vec<Accelerator>,
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
}

/// The container's memory limit: cgroup v2 first, then v1. `max`, the v1
/// "unlimited" sentinel, an unreadable file and a malformed value all yield
/// `None` — they are the same answer: no finite limit is known.
pub fn probe_memory(cgroup_root: &Path, proc_root: &Path) -> MemoryResources {
    let v2 = read_trimmed(&cgroup_root.join("memory.max"))
        .and_then(|value| value.parse::<u64>().ok())
        .map(|bytes| (bytes, "cgroup_v2"));
    let v1 = || {
        read_trimmed(&cgroup_root.join("memory").join("memory.limit_in_bytes"))
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|bytes| *bytes < CGROUP_V1_UNLIMITED_BYTES)
            .map(|bytes| (bytes, "cgroup_v1"))
    };
    let limit = v2.or_else(v1).filter(|(bytes, _)| *bytes > 0);
    MemoryResources {
        cgroup_limit_mib: limit.map(|(bytes, _)| bytes / MIB),
        cgroup_source: limit.map(|(_, source)| source),
        proc_meminfo_mib: proc_meminfo_total_mib(proc_root),
    }
}

fn proc_meminfo_total_mib(proc_root: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(proc_root.join("meminfo")).ok()?;
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kib = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    Some(kib / 1024)
}

/// CPU quota: cgroup v2 `cpu.max` ("<quota> <period>" or "max <period>"),
/// then v1 `cpu.cfs_quota_us` / `cpu.cfs_period_us` (quota -1 = unlimited).
pub fn probe_cpu(cgroup_root: &Path) -> CpuResources {
    let millis = |quota: u64, period: u64| (period > 0).then(|| quota * 1000 / period);
    let v2 = read_trimmed(&cgroup_root.join("cpu.max")).and_then(|value| {
        let mut parts = value.split_whitespace();
        let quota = parts.next()?.parse::<u64>().ok()?;
        let period = parts.next()?.parse::<u64>().ok()?;
        millis(quota, period)
    });
    let v1 = || {
        let dir = cgroup_root.join("cpu");
        let quota = read_trimmed(&dir.join("cpu.cfs_quota_us"))?
            .parse::<i64>()
            .ok()?;
        let period = read_trimmed(&dir.join("cpu.cfs_period_us"))?
            .parse::<u64>()
            .ok()?;
        u64::try_from(quota)
            .ok()
            .and_then(|quota| millis(quota, period))
    };
    CpuResources {
        cgroup_quota_millis: v2.or_else(v1).filter(|millis| *millis > 0),
        online: std::thread::available_parallelism()
            .ok()
            .map(|count| count.get() as u64),
    }
}

/// Free space where the Runner actually writes, not on `/`.
pub fn probe_scratch_mib(work_root: &Path) -> Option<u64> {
    fs2::available_space(work_root)
        .ok()
        .map(|bytes| bytes / MIB)
}

/// Parse `nvidia-smi --query-gpu=memory.total,driver_version
/// --format=csv,noheader,nounits`. A line that does not parse is dropped: a
/// device that cannot state its memory is not reported.
pub fn parse_nvidia_smi(output: &str) -> Vec<Accelerator> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(',').map(str::trim);
            let vram_mib = fields.next()?.parse::<u64>().ok()?;
            let driver = fields.next()?.to_owned();
            (vram_mib > 0 && !driver.is_empty()).then_some(Accelerator {
                vendor: "nvidia",
                vram_mib,
                driver,
            })
        })
        .collect()
}

fn probe_accelerators() -> Vec<Accelerator> {
    Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.total,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| parse_nvidia_smi(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

impl HostResources {
    pub fn probe(work_root: &Path) -> Self {
        let cgroup_root = Path::new("/sys/fs/cgroup");
        Self {
            memory: probe_memory(cgroup_root, Path::new("/proc")),
            cpu: probe_cpu(cgroup_root),
            scratch_mib: probe_scratch_mib(work_root),
            accelerators: probe_accelerators(),
        }
    }
}

/// Probed once per process: the host a Runner starts on does not change.
pub fn cached(work_root: &Path) -> &'static HostResources {
    static RESOURCES: OnceLock<HostResources> = OnceLock::new();
    RESOURCES.get_or_init(|| HostResources::probe(work_root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn roots() -> (tempfile::TempDir, tempfile::TempDir) {
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
    }

    #[test]
    fn cgroup_v2_finite_limit_is_reported() {
        let (cgroup, proc_root) = roots();
        write(cgroup.path(), "memory.max", "68719476736\n");
        let memory = probe_memory(cgroup.path(), proc_root.path());
        assert_eq!(memory.cgroup_limit_mib, Some(65536));
        assert_eq!(memory.cgroup_source, Some("cgroup_v2"));
    }

    #[test]
    fn cgroup_v1_limit_is_reported_as_measured_on_a_rented_pod() {
        let (cgroup, proc_root) = roots();
        // The exact value read on a RunPod pod whose provider reported 62 GB.
        write(
            cgroup.path(),
            "memory/memory.limit_in_bytes",
            "61999996928\n",
        );
        let memory = probe_memory(cgroup.path(), proc_root.path());
        assert_eq!(memory.cgroup_limit_mib, Some(59127));
        assert_eq!(memory.cgroup_source, Some("cgroup_v1"));
    }

    #[test]
    fn cgroup_v2_wins_when_both_exist() {
        let (cgroup, proc_root) = roots();
        write(cgroup.path(), "memory.max", "1073741824\n");
        write(
            cgroup.path(),
            "memory/memory.limit_in_bytes",
            "2147483648\n",
        );
        let memory = probe_memory(cgroup.path(), proc_root.path());
        assert_eq!(memory.cgroup_limit_mib, Some(1024));
        assert_eq!(memory.cgroup_source, Some("cgroup_v2"));
    }

    #[test]
    fn cgroup_v2_max_falls_through_to_v1() {
        let (cgroup, proc_root) = roots();
        write(cgroup.path(), "memory.max", "max\n");
        write(
            cgroup.path(),
            "memory/memory.limit_in_bytes",
            "2147483648\n",
        );
        let memory = probe_memory(cgroup.path(), proc_root.path());
        assert_eq!(memory.cgroup_limit_mib, Some(2048));
        assert_eq!(memory.cgroup_source, Some("cgroup_v1"));
    }

    #[test]
    fn unlimited_or_missing_memory_is_unknown_even_when_proc_meminfo_is_huge() {
        let (cgroup, proc_root) = roots();
        write(
            proc_root.path(),
            "meminfo",
            "MemTotal:       527433728 kB\nMemFree: 1 kB\n",
        );

        // No cgroup file at all.
        let memory = probe_memory(cgroup.path(), proc_root.path());
        assert_eq!(memory.cgroup_limit_mib, None);
        assert_eq!(memory.cgroup_source, None);
        // The host total is visible, and it is only a diagnostic.
        assert_eq!(memory.proc_meminfo_mib, Some(515072));

        // v2 says max, v1 says the unlimited sentinel.
        write(cgroup.path(), "memory.max", "max\n");
        write(
            cgroup.path(),
            "memory/memory.limit_in_bytes",
            "9223372036854771712\n",
        );
        let memory = probe_memory(cgroup.path(), proc_root.path());
        assert_eq!(memory.cgroup_limit_mib, None);

        // Garbage is not a limit either.
        write(cgroup.path(), "memory.max", "not-a-number\n");
        write(cgroup.path(), "memory/memory.limit_in_bytes", "\n");
        assert_eq!(
            probe_memory(cgroup.path(), proc_root.path()).cgroup_limit_mib,
            None
        );
    }

    #[test]
    fn cpu_quota_v2_v1_and_unlimited() {
        let (cgroup, _) = roots();
        assert_eq!(probe_cpu(cgroup.path()).cgroup_quota_millis, None);

        write(cgroup.path(), "cpu/cpu.cfs_quota_us", "1190000\n");
        write(cgroup.path(), "cpu/cpu.cfs_period_us", "100000\n");
        assert_eq!(probe_cpu(cgroup.path()).cgroup_quota_millis, Some(11900));

        write(cgroup.path(), "cpu/cpu.cfs_quota_us", "-1\n");
        assert_eq!(probe_cpu(cgroup.path()).cgroup_quota_millis, None);

        write(cgroup.path(), "cpu.max", "200000 100000\n");
        assert_eq!(probe_cpu(cgroup.path()).cgroup_quota_millis, Some(2000));

        write(cgroup.path(), "cpu.max", "max 100000\n");
        assert_eq!(probe_cpu(cgroup.path()).cgroup_quota_millis, None);
    }

    #[test]
    fn nvidia_smi_output_is_parsed_and_bad_lines_are_dropped() {
        assert_eq!(
            parse_nvidia_smi("46068, 550.144.03\n16376, 550.144.03\n"),
            vec![
                Accelerator {
                    vendor: "nvidia",
                    vram_mib: 46068,
                    driver: "550.144.03".into()
                },
                Accelerator {
                    vendor: "nvidia",
                    vram_mib: 16376,
                    driver: "550.144.03".into()
                },
            ]
        );
        assert!(parse_nvidia_smi("").is_empty());
        assert!(parse_nvidia_smi("[N/A], 550.144.03\n").is_empty());
        assert!(parse_nvidia_smi("NVIDIA-SMI has failed\n").is_empty());
    }

    #[test]
    fn scratch_is_measured_on_the_work_root() {
        let work_root = tempfile::tempdir().unwrap();
        assert!(probe_scratch_mib(work_root.path()).is_some());
        assert_eq!(probe_scratch_mib(&work_root.path().join("missing")), None);
    }

    #[test]
    fn serialized_shape_is_stable() {
        let resources = HostResources {
            memory: MemoryResources {
                cgroup_limit_mib: Some(59127),
                cgroup_source: Some("cgroup_v1"),
                proc_meminfo_mib: Some(515072),
            },
            cpu: CpuResources {
                cgroup_quota_millis: Some(11900),
                online: Some(112),
            },
            scratch_mib: Some(140000),
            accelerators: vec![Accelerator {
                vendor: "nvidia",
                vram_mib: 46068,
                driver: "550.144.03".into(),
            }],
        };
        assert_eq!(
            serde_json::to_value(&resources).unwrap(),
            serde_json::json!({
                "memory": {
                    "cgroup_limit_mib": 59127,
                    "cgroup_source": "cgroup_v1",
                    "proc_meminfo_mib": 515072
                },
                "cpu": { "cgroup_quota_millis": 11900, "online": 112 },
                "scratch_mib": 140000,
                "accelerators": [
                    { "vendor": "nvidia", "vram_mib": 46068, "driver": "550.144.03" }
                ]
            })
        );
    }
}

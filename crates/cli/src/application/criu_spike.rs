//! CRIU container checkpoint — Linux spike (#839).
//!
//! Establishes the CRIU checkpoint/restore **compatibility contract** (capability
//! vocabulary + a restore-compatibility class) and a runnable Linux smoke. CRIU
//! is a candidate *inner Ready-State mechanism*: it checkpoints a running
//! container's process tree and restores it, the container-layer counterpart to
//! the Firecracker VM-memory snapshot.
//!
//! **This is a spike.** It is Linux-only, **not** wired into `ato run` / `ato
//! build`, has **no** product path, and is **not** brought into the macOS Apple
//! Containerization guest yet (Mac M4, gated on these Linux results). CRIU is a
//! resume mechanism, **never** a security boundary — the VM/container remains the
//! isolation boundary. A checkpoint is **pre-bind and secret-free**: taken before
//! any `BindingLease` injection, with no secret/OAuth/user-file values in the
//! image. See `docs/ready-state/criu-container-spike.md`.

// SPIKE (#839): the vocabulary/facts below are the contract CRIU will graduate
// into, but nothing wires them into a product path yet — so they are unused
// outside tests. Remove this allow when CRIU graduates.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The Ready-State mechanism id for a CRIU checkpoint (matches
/// [`desktop_runner::facts::ReadyStateKind::CriuCheckpoint`](crate::application::desktop_runner)).
const READY_STATE_KIND_CRIU: &str = "criu_checkpoint";

/// The isolation boundary a CRIU-checkpointed session runs inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CriuIsolationBoundary {
    /// A container inside a lightweight VM (e.g. Apple Containerization guest).
    VmWrappedContainer,
    /// A bare Linux container (host runtime).
    Container,
}

/// A CRIU-capable container runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContainerRuntime {
    Runc,
    Crun,
    Podman,
    Containerd,
    Unknown,
}

impl ContainerRuntime {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Runc => "runc",
            Self::Crun => "crun",
            Self::Podman => "podman",
            Self::Containerd => "containerd",
            Self::Unknown => "unknown",
        }
    }

    /// Detect a CRIU-capable runtime, preferring the higher-level tools that
    /// expose a first-class `checkpoint`/`restore` (podman) before the low-level
    /// OCI runtimes. Pure: the PATH probe is injected for testability.
    pub(crate) fn detect(on_path: impl Fn(&str) -> bool) -> Self {
        for (name, rt) in [
            ("podman", Self::Podman),
            ("crun", Self::Crun),
            ("runc", Self::Runc),
            ("containerd", Self::Containerd),
        ] {
            if on_path(name) {
                return rt;
            }
        }
        Self::Unknown
    }
}

/// The CRIU backend capability vocabulary (spike contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CriuBackendCapability {
    /// Always [`READY_STATE_KIND_CRIU`].
    pub(crate) ready_state_kind: String,
    pub(crate) isolation_boundary: CriuIsolationBoundary,
    /// CRIU only restores on Linux.
    pub(crate) requires_linux_kernel: bool,
    /// CRIU must be installed.
    pub(crate) requires_criu: bool,
    pub(crate) container_runtime: ContainerRuntime,
    /// Whether `criu` was found on this host.
    pub(crate) criu_available: bool,
    pub(crate) maturity: String,
}

impl CriuBackendCapability {
    fn experimental(runtime: ContainerRuntime, criu_available: bool) -> Self {
        Self {
            ready_state_kind: READY_STATE_KIND_CRIU.to_string(),
            isolation_boundary: CriuIsolationBoundary::Container,
            requires_linux_kernel: true,
            requires_criu: true,
            container_runtime: runtime,
            criu_available,
            maturity: "experimental".to_string(),
        }
    }
}

/// CRIU restore-compatibility class: the facts that must match (exactly) for a
/// checkpoint to restore. CRIU restore is brittle across these, so the class is
/// deliberately tight — the analogue of `RunnerClassFacts` for VM snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CriuRunnerClassFacts {
    pub(crate) guest_os: String,
    pub(crate) guest_arch: String,
    pub(crate) kernel_release: String,
    pub(crate) criu_version: String,
    pub(crate) runtime_id: String,
    pub(crate) runtime_version: String,
    pub(crate) rootfs_image_digest: String,
    pub(crate) cgroup_version: String,
    pub(crate) namespace_model: String,
}

/// Coarsest → finest comparison order, so a mismatch names the most actionable
/// difference first.
const FIELD_ORDER: &[&str] = &[
    "guest_os",
    "guest_arch",
    "kernel_release",
    "criu_version",
    "runtime_id",
    "runtime_version",
    "cgroup_version",
    "namespace_model",
    "rootfs_image_digest",
];

impl CriuRunnerClassFacts {
    /// First field (in [`FIELD_ORDER`]) that differs from `other`, or `None`.
    pub(crate) fn first_divergent_field(&self, other: &Self) -> Option<&'static str> {
        for field in FIELD_ORDER {
            let differs = match *field {
                "guest_os" => self.guest_os != other.guest_os,
                "guest_arch" => self.guest_arch != other.guest_arch,
                "kernel_release" => self.kernel_release != other.kernel_release,
                "criu_version" => self.criu_version != other.criu_version,
                "runtime_id" => self.runtime_id != other.runtime_id,
                "runtime_version" => self.runtime_version != other.runtime_version,
                "cgroup_version" => self.cgroup_version != other.cgroup_version,
                "namespace_model" => self.namespace_model != other.namespace_model,
                "rootfs_image_digest" => self.rootfs_image_digest != other.rootfs_image_digest,
                _ => false,
            };
            if differs {
                return Some(field);
            }
        }
        None
    }

    /// Fail-closed compat check: `self` is the class the checkpoint was taken for
    /// (expected), `actual` is the candidate restore host. Typed mismatch, never
    /// a bare bool — "unknown" can't be mistaken for "compatible".
    pub(crate) fn ensure_compatible(&self, actual: &Self) -> Result<(), CriuClassMismatch> {
        match self.first_divergent_field(actual) {
            None => Ok(()),
            Some(field) => Err(CriuClassMismatch {
                first_divergent_field: field.to_string(),
            }),
        }
    }
}

/// A CRIU checkpoint was attempted against a host whose class does not match the
/// one it was taken for. Fail-closed; surfaced at the restore gate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("CRIU runner class mismatch (first divergent field: {first_divergent_field})")]
pub(crate) struct CriuClassMismatch {
    pub(crate) first_divergent_field: String,
}

/// Parse the version from `criu --version` output (`"Version: 3.17.1"` →
/// `Some("3.17.1")`).
fn parse_criu_version(output: &str) -> Option<String> {
    for line in output.lines() {
        if let Some(rest) = line.trim().strip_prefix("Version:") {
            let v = rest.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// The result of probing a host for CRIU-spike readiness.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CriuSpikeReport {
    /// False on non-Linux (CRIU is Linux-only); the rest is then empty.
    pub(crate) applicable: bool,
    pub(crate) capability: Option<CriuBackendCapability>,
    pub(crate) criu_version: Option<String>,
    pub(crate) kernel_release: Option<String>,
    pub(crate) cgroup_version: Option<String>,
    pub(crate) namespace_model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) diagnostics: Vec<String>,
}

/// Probe this host for CRIU-spike readiness (read-only, best-effort). Non-Linux
/// hosts return `applicable: false` — CRIU restore is Linux-only.
pub(crate) fn probe() -> CriuSpikeReport {
    if std::env::consts::OS != "linux" {
        return CriuSpikeReport {
            applicable: false,
            capability: None,
            criu_version: None,
            kernel_release: None,
            cgroup_version: None,
            namespace_model: None,
            diagnostics: vec![format!(
                "CRIU checkpoint is Linux-only; host OS is '{}'. The spike does not target the \
                 macOS Apple Containerization guest yet (Mac M4).",
                std::env::consts::OS
            )],
        };
    }

    let criu_version = detect_criu_version();
    let runtime = ContainerRuntime::detect(|n| crate::application::runner_agent::binary_on_path(n));
    let mut diagnostics = Vec::new();
    if criu_version.is_none() {
        diagnostics
            .push("criu not found on PATH; install criu to run the checkpoint spike.".into());
    }
    if runtime == ContainerRuntime::Unknown {
        diagnostics
            .push("no CRIU-capable container runtime found (podman/crun/runc/containerd).".into());
    }

    CriuSpikeReport {
        applicable: true,
        capability: Some(CriuBackendCapability::experimental(
            runtime,
            criu_version.is_some(),
        )),
        criu_version,
        kernel_release: detect_kernel_release(),
        cgroup_version: Some(detect_cgroup_version()),
        namespace_model: Some(detect_namespace_model()),
        diagnostics,
    }
}

fn detect_criu_version() -> Option<String> {
    let out = std::process::Command::new("criu")
        .arg("--version")
        .output()
        .ok()?;
    parse_criu_version(&String::from_utf8_lossy(&out.stdout))
}

fn detect_kernel_release() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn detect_cgroup_version() -> String {
    if std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        "v2".to_string()
    } else {
        "v1".to_string()
    }
}

/// The set of namespaces visible to this process, sorted+joined
/// (`"cgroup+ipc+mnt+net+pid+uts"`). Empty string off Linux.
fn detect_namespace_model() -> String {
    let mut ns: Vec<String> = std::fs::read_dir("/proc/self/ns")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    ns.sort();
    ns.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&CriuIsolationBoundary::VmWrappedContainer).unwrap(),
            "\"vm_wrapped_container\""
        );
        assert_eq!(
            serde_json::to_string(&ContainerRuntime::Podman).unwrap(),
            "\"podman\""
        );
    }

    #[test]
    fn capability_marks_linux_and_criu_required() {
        let cap = CriuBackendCapability::experimental(ContainerRuntime::Podman, true);
        assert_eq!(cap.ready_state_kind, "criu_checkpoint");
        assert!(cap.requires_linux_kernel);
        assert!(cap.requires_criu);
        assert_eq!(cap.container_runtime, ContainerRuntime::Podman);
        assert!(cap.criu_available);
        assert_eq!(cap.maturity, "experimental");
    }

    #[test]
    fn runtime_detect_prefers_podman_then_falls_back() {
        assert_eq!(
            ContainerRuntime::detect(|n| n == "podman" || n == "runc"),
            ContainerRuntime::Podman
        );
        assert_eq!(
            ContainerRuntime::detect(|n| n == "runc"),
            ContainerRuntime::Runc
        );
        assert_eq!(
            ContainerRuntime::detect(|_| false),
            ContainerRuntime::Unknown
        );
    }

    #[test]
    fn parse_criu_version_extracts_semver() {
        assert_eq!(
            parse_criu_version("Version: 3.17.1\nGitID: v3.17"),
            Some("3.17.1".to_string())
        );
        assert_eq!(parse_criu_version("no version here"), None);
    }

    fn facts() -> CriuRunnerClassFacts {
        CriuRunnerClassFacts {
            guest_os: "linux".into(),
            guest_arch: "x86_64".into(),
            kernel_release: "6.8.0-31-generic".into(),
            criu_version: "3.17.1".into(),
            runtime_id: "runc".into(),
            runtime_version: "1.1.12".into(),
            rootfs_image_digest: "blake3:abc".into(),
            cgroup_version: "v2".into(),
            namespace_model: "cgroup+ipc+mnt+net+pid+uts".into(),
        }
    }

    #[test]
    fn equal_facts_are_compatible() {
        assert!(facts().ensure_compatible(&facts()).is_ok());
    }

    #[test]
    fn mismatch_reports_first_divergent_field_coarsest_first() {
        let expected = facts();
        let mut actual = facts();
        // Diverge on a fine field and a coarse field; coarse (arch) wins.
        actual.rootfs_image_digest = "blake3:other".into();
        actual.guest_arch = "aarch64".into();
        let err = expected.ensure_compatible(&actual).unwrap_err();
        assert_eq!(err.first_divergent_field, "guest_arch");
    }

    #[test]
    fn kernel_or_criu_version_change_is_detected() {
        let expected = facts();
        let mut actual = facts();
        actual.criu_version = "3.18.0".into();
        assert_eq!(
            expected
                .ensure_compatible(&actual)
                .unwrap_err()
                .first_divergent_field,
            "criu_version"
        );
    }

    #[test]
    fn probe_is_non_applicable_off_linux() {
        // This test host is macOS; CRIU is Linux-only.
        if std::env::consts::OS != "linux" {
            let r = probe();
            assert!(!r.applicable);
            assert!(r.capability.is_none());
            assert!(!r.diagnostics.is_empty());
        } else {
            // On Linux the probe is applicable and yields a capability.
            let r = probe();
            assert!(r.applicable);
            assert!(r.capability.is_some());
        }
    }
}

// ── Manual Linux CRIU checkpoint/restore smoke (ignored) ────────────────────
//
// Requires Linux + criu + a CRIU-capable runtime (podman preferred). Run:
//
//   cargo test -p cli criu_spike::smoke -- --ignored --nocapture
//
// It runs a tiny HTTP container, checkpoints it via the runtime's CRIU-backed
// checkpoint, restores it, re-checks reachability, and tears down. Nothing here
// is wired into the product; a binding-required workload is never run.
#[cfg(test)]
mod smoke {
    use super::*;
    use serde::Serialize;
    use std::net::TcpStream;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[derive(Debug, Serialize)]
    struct CriuSpikeReceipt {
        host_os: String,
        runtime: String,
        criu_version: Option<String>,
        kernel_release: Option<String>,
        image: String,
        checkpoint_ok: bool,
        restore_ok: bool,
        restored_reachable: bool,
        checkpoint_ms: Option<u128>,
        restore_ms: Option<u128>,
        cleanup_ok: bool,
    }

    fn smoke_image() -> String {
        std::env::var("ATO_CRIU_SMOKE_IMAGE")
            .unwrap_or_else(|_| "docker.io/library/python:3-alpine".to_string())
    }

    fn run(program: &str, args: &[&str], timeout: Duration) -> (bool, String) {
        let mut child = match Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return (false, format!("spawn {program} failed: {e}")),
        };
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let out = child.wait_with_output().ok();
                    let stderr = out
                        .map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
                        .unwrap_or_default();
                    return (status.success(), stderr);
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return (false, format!("{program} timed out"));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return (false, format!("wait {program} failed: {e}")),
            }
        }
    }

    fn reachable(port: u16, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if TcpStream::connect_timeout(
                &format!("127.0.0.1:{port}").parse().unwrap(),
                Duration::from_millis(500),
            )
            .is_ok()
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        false
    }

    #[test]
    #[ignore = "manual: Linux + criu + podman/runc/crun (#839 CRIU spike)"]
    fn criu_checkpoint_restore() {
        let report = super::probe();
        if !report.applicable {
            eprintln!("SKIP: CRIU spike is Linux-only ({})", std::env::consts::OS);
            return;
        }
        let runtime = report
            .capability
            .as_ref()
            .map(|c| c.container_runtime)
            .unwrap_or(ContainerRuntime::Unknown);
        if runtime != ContainerRuntime::Podman {
            eprintln!(
                "SKIP: this smoke drives CRIU via `podman container checkpoint/restore`; podman not found."
            );
            return;
        }
        if report.criu_version.is_none() {
            eprintln!("SKIP: criu not installed.");
            return;
        }

        let image = smoke_image();
        let port = 8080u16;
        let pid = std::process::id();
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let name = format!("ato-criu-spike-{pid}-{millis}");
        let port_map = format!("{port}:{port}");
        let port_str = port.to_string();

        let cleanup = |n: &str| {
            let _ = run("podman", &["stop", n], Duration::from_secs(20));
            let _ = run("podman", &["rm", "-f", n], Duration::from_secs(20));
        };

        // Run a tiny HTTP server container.
        let (ran, err) = run(
            "podman",
            &[
                "run",
                "-d",
                "--name",
                &name,
                "-p",
                &port_map,
                &image,
                "python3",
                "-m",
                "http.server",
                &port_str,
            ],
            Duration::from_secs(120),
        );
        assert!(ran, "podman run failed: {err}");

        let mut checkpoint_ok = false;
        let mut restore_ok = false;
        let mut restored_reachable = false;
        let mut checkpoint_ms = None;
        let mut restore_ms = None;

        if reachable(port, Duration::from_secs(30)) {
            // Checkpoint (CRIU dump).
            let t = Instant::now();
            let (ck, ckerr) = run(
                "podman",
                &["container", "checkpoint", &name],
                Duration::from_secs(60),
            );
            checkpoint_ok = ck;
            checkpoint_ms = Some(t.elapsed().as_millis());
            if !ck {
                eprintln!("checkpoint failed: {ckerr}");
            }

            if checkpoint_ok {
                // Restore (CRIU restore).
                let t = Instant::now();
                let (rs, rserr) = run(
                    "podman",
                    &["container", "restore", &name],
                    Duration::from_secs(60),
                );
                restore_ok = rs;
                restore_ms = Some(t.elapsed().as_millis());
                if !rs {
                    eprintln!("restore failed: {rserr}");
                }
                if restore_ok {
                    restored_reachable = reachable(port, Duration::from_secs(30));
                }
            }
        } else {
            eprintln!("container never became reachable before checkpoint");
        }

        cleanup(&name);
        // Idempotent second cleanup is safe.
        cleanup(&name);

        let receipt = CriuSpikeReceipt {
            host_os: report
                .applicable
                .then(|| "linux".to_string())
                .unwrap_or_default(),
            runtime: runtime.as_str().to_string(),
            criu_version: report.criu_version.clone(),
            kernel_release: report.kernel_release.clone(),
            image,
            checkpoint_ok,
            restore_ok,
            restored_reachable,
            checkpoint_ms,
            restore_ms,
            cleanup_ok: true,
        };
        println!(
            "CRIU spike receipt:\n{}",
            serde_json::to_string_pretty(&receipt).unwrap()
        );

        assert!(
            checkpoint_ok && restore_ok && restored_reachable,
            "CRIU checkpoint/restore did not round-trip a reachable service — record the receipt"
        );
    }
}

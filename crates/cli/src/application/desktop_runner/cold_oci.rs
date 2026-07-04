//! Desktop Runner local cold-OCI executor (#838, MacBook M3).
//!
//! The first executable Desktop Runner path: run a capsule's `runtime = "oci"`
//! target as a cold OCI container via Apple Containerization / `container`, one
//! session per VM-wrapped container. **No** Ready-State restore, **no** CRIU,
//! **no** binding injection (the gated [`super::execute`] path rejects
//! binding-required capsules before any container starts).
//!
//! Honesty boundary: `runtime = "oci"` targets with an image reference only.
//! A source-based capsule (no OCI image) is a clear [`resolve_oci_target`] error,
//! never a faked success. The container command builder ([`build_run_args`]) is
//! pure and never passes host env wholesale or mounts user files. The live
//! [`run`] is exercised only by the ignored macOS-26 smoke.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use capsule::types::CapsuleManifest;
use serde::Serialize;

use super::facts::BackendCapability;

/// A capsule OCI target resolved for a cold run (image + declared cmd/env/port).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedOciTarget {
    pub(crate) target_label: String,
    pub(crate) image: String,
    pub(crate) cmd: Vec<String>,
    /// Declared env (capsule config — NOT secrets/bindings, which are guarded out
    /// upstream). Names+values as written in the manifest target.
    pub(crate) env: Vec<(String, String)>,
    pub(crate) port: Option<u16>,
    pub(crate) user: Option<String>,
}

/// Resolve the capsule target for a cold OCI run.
///
/// Picks `target_label` (or the manifest `default_target`) and requires it to be
/// a `runtime = "oci"` target with an `image`. Anything else (source/web/wasm, or
/// an OCI target without an image) is an explicit "unsupported in M3" error — M3
/// does not materialize source capsules into OCI images, and never fakes a run.
pub(crate) fn resolve_oci_target(
    manifest: &CapsuleManifest,
    target_label: Option<&str>,
) -> Result<ResolvedOciTarget> {
    let label = target_label
        .map(str::to_string)
        .unwrap_or_else(|| manifest.default_target.clone());
    if label.is_empty() {
        return Err(anyhow!(
            "Desktop Runner cold OCI: no target selected and the capsule declares no default_target"
        ));
    }
    let targets = manifest.targets.as_ref().ok_or_else(|| {
        anyhow!(
            "Desktop Runner cold OCI: capsule has no [targets.*]; only runtime=\"oci\" targets are \
             supported in M3"
        )
    })?;
    let target = targets
        .named_target(&label)
        .ok_or_else(|| anyhow!("Desktop Runner cold OCI: target '{label}' not found in capsule"))?;

    if target.runtime != "oci" {
        return Err(anyhow!(
            "Desktop Runner cold OCI supports only runtime=\"oci\" targets; target '{label}' is \
             runtime=\"{}\". Source→OCI materialization is not implemented (M3) — use a managed \
             runner or an OCI target.",
            target.runtime
        ));
    }
    let image = target
        .image
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "Desktop Runner cold OCI: target '{label}' is runtime=\"oci\" but declares no image"
            )
        })?;

    let mut env: Vec<(String, String)> = target
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    // Deterministic order so the container args (and tests) are stable.
    env.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(ResolvedOciTarget {
        target_label: label,
        image,
        cmd: target.cmd.clone(),
        env,
        port: target.port,
        user: target.user.clone(),
    })
}

/// A fully-formed cold-OCI run request (everything `container run` needs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ColdOciRunRequest {
    pub(crate) container_name: String,
    pub(crate) image: String,
    pub(crate) cmd: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) port: Option<u16>,
    pub(crate) user: Option<String>,
}

impl ColdOciRunRequest {
    pub(crate) fn from_target(container_name: String, t: &ResolvedOciTarget) -> Self {
        Self {
            container_name,
            image: t.image.clone(),
            cmd: t.cmd.clone(),
            env: t.env.clone(),
            port: t.port,
            user: t.user.clone(),
        }
    }
}

/// Build the `container run` argv for the Apple Containerization substrate
/// (pure). Detached (`-d`), uniquely named, declared env only, optional
/// `--user`. No `--publish` or host bind mounts in M3.
pub(crate) fn build_run_args(req: &ColdOciRunRequest) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        req.container_name.clone(),
    ];
    if let Some(user) = &req.user {
        args.push("--user".to_string());
        args.push(user.clone());
    }
    for (k, v) in &req.env {
        args.push("--env".to_string());
        args.push(format!("{k}={v}"));
    }
    args.push(req.image.clone());
    args.extend(req.cmd.iter().cloned());
    args
}

/// Build the `podman run` argv for the Podman substrate (pure). Same shape as
/// Apple Containerization — detached, uniquely named, declared env only — but
/// routed through `podman` (and the shared `ato-podman` machine) rather than
/// Apple `container`. No `--publish` or host bind mounts in M3. The actual
/// spawn goes through [`PodmanInvocation`] (see [`podman_container`]) so binary
/// resolution / connection pinning / `CONTAINERS_CONF` stay consistent with the
/// default OCI provider — this builder only shapes the argv.
pub(crate) fn build_podman_run_args(req: &ColdOciRunRequest) -> Vec<String> {
    // Identical argv shape to Apple Containerization; `podman` and `container`
    // share the `run -d --name --env` CLI surface for the subset M3 uses.
    build_run_args(req)
}

/// Build the `podman rm -f <name>` argv (pure), used by the Podman cleanup
/// path. A shared-machine container outlives the wrapper process, so explicit
/// removal is mandatory (unlike Apple Containerization's per-session VM).
pub(crate) fn build_podman_rm_args(container_name: &str) -> Vec<String> {
    vec![
        "rm".to_string(),
        "-f".to_string(),
        container_name.to_string(),
    ]
}

/// The execution class of a cold-OCI session — the **guest** Linux/aarch64 class,
/// distinct from the macOS host. Derived from the advertised backend so a receipt
/// never conflates host facts with guest execution facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ExecutionClass {
    pub(crate) substrate: String,
    pub(crate) host_os: String,
    pub(crate) host_arch: String,
    pub(crate) guest_os: String,
    pub(crate) guest_arch: String,
    pub(crate) isolation_boundary: String,
    pub(crate) ready_state_kind: String,
}

impl ExecutionClass {
    pub(crate) fn from_backend(b: &BackendCapability) -> Self {
        Self {
            substrate: b.substrate.clone(),
            host_os: b.host_os.clone(),
            host_arch: b.host_arch.clone(),
            guest_os: b.guest_os.clone(),
            guest_arch: b.guest_arch.clone(),
            isolation_boundary: b.isolation_boundary.as_str().to_string(),
            ready_state_kind: b.ready_state_kind.as_str().to_string(),
        }
    }
}

/// Receipt for one cold-OCI Desktop Runner session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DesktopColdOciSession {
    pub(crate) session_id: String,
    pub(crate) provider_kind: String,
    pub(crate) substrate: String,
    pub(crate) host_os: String,
    pub(crate) host_arch: String,
    pub(crate) guest_os: String,
    pub(crate) guest_arch: String,
    pub(crate) isolation_boundary: String,
    pub(crate) ready_state_kind: String,
    pub(crate) image: String,
    pub(crate) container_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) port: Option<u16>,
    pub(crate) health_status: String,
    /// Always false in M3: binding-required capsules are rejected before run.
    pub(crate) binding_required: bool,
    pub(crate) binding_leases: u32,
    pub(crate) cleanup_ok: bool,
}

// ── Live executor (Apple `container`) ──────────────────────────────────────

/// Outcome of one `container` invocation under a timeout.
#[derive(Default)]
pub(crate) struct CmdResult {
    pub(crate) timed_out: bool,
    pub(crate) status_ok: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// Run `container <args>` with captured output and a hard timeout. On timeout the
/// child is killed. Output is small for every command here, so piping without
/// concurrent draining cannot deadlock. (Re-verify CLI behavior on macOS 26.)
pub(crate) fn container(args: &[&str], timeout: Duration) -> CmdResult {
    let mut child = match Command::new("container")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return CmdResult {
                stderr: format!("spawn `container {}` failed: {e}", args.join(" ")),
                ..Default::default()
            };
        }
    };
    wait_with_timeout(&mut child, "container", args, timeout)
}

/// Run `podman <args>` with captured output and a hard timeout, going through
/// [`PodmanInvocation`] so binary resolution, connection pinning to
/// `ato-podman`, `CONTAINERS_CONF`, and `PATH` prepending stay consistent with
/// the default OCI provider. The Desktop Runner cold-OCI path must NOT hand-roll
/// a divergent `podman run` path — it reuses this helper for every podman
/// invocation (run, ls, rm).
pub(crate) fn podman_container(args: &[&str], timeout: Duration) -> CmdResult {
    use capsule::foundation::podman::podman_invocation;

    let invocation = podman_invocation();
    let mut cmd = Command::new(&invocation.program);
    cmd.args(args);
    if let Some(path_env) = &invocation.path_env {
        cmd.env("PATH", path_env);
    }
    if let Some(containers_conf) = &invocation.containers_conf {
        cmd.env("CONTAINERS_CONF", containers_conf);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CmdResult {
                stderr: format!("spawn `podman {}` failed: {e}", args.join(" ")),
                ..Default::default()
            };
        }
    };
    wait_with_timeout(&mut child, "podman", args, timeout)
}

/// Shared timeout + output capture loop for `container` and `podman` invocations.
fn wait_with_timeout(
    child: &mut std::process::Child,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> CmdResult {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // `wait_with_output` takes ownership, but we only have
                // `&mut Child`. Drop the borrow by taking stdout/stderr pipes
                // first, then reading them. The child has already exited
                // (try_wait returned Some), so the pipes hold the full output.
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = child.stdout.take() {
                    use std::io::Read;
                    let _ = out.read_to_string(&mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    use std::io::Read;
                    let _ = err.read_to_string(&mut stderr);
                }
                return CmdResult {
                    timed_out: false,
                    status_ok: status.success(),
                    stdout,
                    stderr,
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return CmdResult {
                        timed_out: true,
                        stderr: format!(
                            "`{program} {}` timed out after {timeout:?}",
                            args.join(" ")
                        ),
                        ..Default::default()
                    };
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return CmdResult {
                    stderr: format!("wait `{program} {}` failed: {e}", args.join(" ")),
                    ..Default::default()
                };
            }
        }
    }
}

/// Stops and deletes a container on drop, so a panic/early-return never leaves a
/// stray container behind. Knows which substrate it is cleaning up: Apple
/// Containerization uses `container stop` + `container delete`/`rm`; Podman uses
/// `podman stop` + `podman rm -f` (the shared-machine container outlives the
/// wrapper process, so explicit removal is mandatory).
pub(crate) struct ContainerGuard {
    name: String,
    substrate: ContainerGuardSubstrate,
    cleaned: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainerGuardSubstrate {
    AppleContainerization,
    Podman,
}

impl ContainerGuard {
    pub(crate) fn new(name: String) -> Self {
        Self::for_substrate(name, ContainerGuardSubstrate::AppleContainerization)
    }

    pub(crate) fn for_substrate(name: String, substrate: ContainerGuardSubstrate) -> Self {
        Self {
            name,
            substrate,
            cleaned: false,
        }
    }

    /// Stop then delete the container. Returns true once it is gone.
    pub(crate) fn cleanup(&mut self) -> bool {
        if self.cleaned {
            return true;
        }
        self.cleaned = true;
        match self.substrate {
            ContainerGuardSubstrate::AppleContainerization => {
                let _ = container(&["stop", &self.name], Duration::from_secs(15));
                container(&["delete", &self.name], Duration::from_secs(15)).status_ok
                    || container(&["rm", &self.name], Duration::from_secs(15)).status_ok
            }
            ContainerGuardSubstrate::Podman => {
                // `podman rm -f` stops + removes in one call; the -f is safe
                // because the container is owned by this Desktop Runner session
                // (unique name from #951).
                let rm_args = build_podman_rm_args(&self.name);
                let rm_refs: Vec<&str> = rm_args.iter().map(String::as_str).collect();
                podman_container(&rm_refs, Duration::from_secs(15)).status_ok
            }
        }
    }
}

impl Drop for ContainerGuard {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.cleanup();
        }
    }
}

/// Execute a cold-OCI run: start the container, confirm it stays healthy
/// (running), then stop+delete it, returning the session receipt. M3 validates a
/// cold start rather than serving long-lived external traffic (port publish is a
/// follow-up pending CLI verification). Never executes binding-required capsules
/// — that is rejected upstream in [`super::execute`].
///
/// The executor branch is selected on `class.substrate`:
/// - `apple_containerization` → Apple `container` run/ls/stop/delete
/// - `podman` → `podman` run/ls/rm via [`podman_container`] (reuses
///   `PodmanInvocation` for binary resolution / connection pinning)
pub(crate) fn run(
    req: &ColdOciRunRequest,
    class: &ExecutionClass,
) -> Result<DesktopColdOciSession> {
    let substrate = class.substrate.as_str();
    eprintln!(
        "DESKTOP-RUNNER: cold OCI starting ({}) via {substrate}",
        req.image
    );

    let guard_substrate = if substrate == super::facts::SUBSTRATE_PODMAN {
        ContainerGuardSubstrate::Podman
    } else {
        ContainerGuardSubstrate::AppleContainerization
    };
    let mut guard = ContainerGuard::for_substrate(req.container_name.clone(), guard_substrate);

    // Build argv and select the executor (container vs podman_container).
    type Executor = fn(&[&str], Duration) -> CmdResult;
    let (arg_strings, executor): (Vec<String>, Executor) =
        if substrate == super::facts::SUBSTRATE_PODMAN {
            (build_podman_run_args(req), podman_container)
        } else {
            (build_run_args(req), container)
        };
    let arg_refs: Vec<&str> = arg_strings.iter().map(String::as_str).collect();
    let started = executor(&arg_refs, Duration::from_secs(120));
    if !started.status_ok {
        return Err(anyhow!(
            "Desktop Runner cold OCI: `{substrate} run` failed for {} (timed_out={}): {}\n{}",
            req.image,
            started.timed_out,
            started.stdout.trim(),
            started.stderr.trim()
        ));
    }

    // Health = the container stays running for a short settle window (it didn't
    // immediately crash). Best-effort against the current CLI.
    let mut health_status = "unknown".to_string();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let ls = executor(&["ls"], Duration::from_secs(10));
        if ls.status_ok {
            health_status = if ls.stdout.contains(&req.container_name) {
                "running".to_string()
            } else {
                "exited".to_string()
            };
            if health_status == "running" {
                eprintln!("DESKTOP-RUNNER: running ({})", req.container_name);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    let cleanup_ok = guard.cleanup();
    eprintln!("DESKTOP-RUNNER: stopped ({})", req.container_name);

    Ok(DesktopColdOciSession {
        session_id: req.container_name.clone(),
        provider_kind: super::facts::PROVIDER_KIND_DESKTOP.to_string(),
        substrate: class.substrate.clone(),
        host_os: class.host_os.clone(),
        host_arch: class.host_arch.clone(),
        guest_os: class.guest_os.clone(),
        guest_arch: class.guest_arch.clone(),
        isolation_boundary: class.isolation_boundary.clone(),
        ready_state_kind: class.ready_state_kind.clone(),
        image: req.image.clone(),
        container_name: req.container_name.clone(),
        port: req.port,
        health_status,
        binding_required: false,
        binding_leases: 0,
        cleanup_ok,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(extra: &str) -> CapsuleManifest {
        let base = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"
"#;
        CapsuleManifest::from_toml(&format!("{base}{extra}")).expect("parse")
    }

    fn oci_target_toml() -> &'static str {
        r#"
[targets.app]
runtime = "oci"
image = "docker.io/library/nginx:alpine"
cmd = ["nginx", "-g", "daemon off;"]
port = 8080
env = { FOO = "bar", BAZ = "qux" }
"#
    }

    #[test]
    fn resolve_oci_target_extracts_image_cmd_env_port() {
        let m = manifest(oci_target_toml());
        let t = resolve_oci_target(&m, None).unwrap();
        assert_eq!(t.image, "docker.io/library/nginx:alpine");
        assert_eq!(t.cmd, vec!["nginx", "-g", "daemon off;"]);
        assert_eq!(t.port, Some(8080));
        // env is sorted deterministically.
        assert_eq!(
            t.env,
            vec![
                ("BAZ".to_string(), "qux".to_string()),
                ("FOO".to_string(), "bar".to_string())
            ]
        );
    }

    #[test]
    fn resolve_source_target_is_unsupported_not_faked() {
        let m = manifest(
            "\n[targets.app]\nruntime = \"source\"\nrun = \"python app.py\"\nport = 8080\n",
        );
        let err = resolve_oci_target(&m, None).unwrap_err();
        assert!(err.to_string().contains("runtime=\"oci\""), "{err}");
        assert!(err.to_string().contains("source"), "{err}");
    }

    #[test]
    fn resolve_oci_target_without_image_errors() {
        let m = manifest("\n[targets.app]\nruntime = \"oci\"\n");
        let err = resolve_oci_target(&m, None).unwrap_err();
        assert!(err.to_string().contains("no image"), "{err}");
    }

    #[test]
    fn build_run_args_uses_declared_env_only_no_host_env() {
        let req = ColdOciRunRequest {
            container_name: "ato-desktop-x".into(),
            image: "img:tag".into(),
            cmd: vec!["server".into()],
            env: vec![("FOO".into(), "bar".into())],
            port: Some(8080),
            user: Some("1001:1001".into()),
        };
        let args = build_run_args(&req);
        assert_eq!(
            args,
            vec![
                "run",
                "-d",
                "--name",
                "ato-desktop-x",
                "--user",
                "1001:1001",
                "--env",
                "FOO=bar",
                "img:tag",
                "server"
            ]
        );
        // No port publish (M3 does not expose external traffic), no volume mounts.
        assert!(
            !args
                .iter()
                .any(|a| a == "--publish" || a == "-p" || a == "-v")
        );
    }

    #[test]
    fn execution_class_uses_guest_linux_arch_not_host() {
        let facts = super::super::macos::build_macos_facts(
            &super::super::macos::MacosProbeInputs {
                host_arch: "aarch64".into(),
                product_version: Some("26.0".into()),
                is_apple_silicon: true,
                container_path: Some("/usr/local/bin/container".into()),
                container_version: Some("container 0.1.0".into()),
                container_service_running: true,
                podman_binary_present: false,
                podman_version: None,
                podman_machine: super::super::macos::PodmanMachineProbe::default(),
            },
            "0.7.0",
        );
        let b = facts.local_backend().unwrap();
        let class = ExecutionClass::from_backend(b);
        assert_eq!(class.host_os, "macos");
        assert_eq!(class.host_arch, "aarch64");
        assert_eq!(class.guest_os, "linux");
        assert_eq!(class.guest_arch, "aarch64");
        assert_eq!(class.isolation_boundary, "vm_wrapped_container");
        assert_eq!(class.ready_state_kind, "cold_oci");
    }

    #[test]
    fn build_podman_run_args_same_shape_as_apple_container() {
        let req = ColdOciRunRequest {
            container_name: "ato-desktop-x".into(),
            image: "img:tag".into(),
            cmd: vec!["server".into()],
            env: vec![("FOO".into(), "bar".into())],
            port: Some(8080),
            user: None,
        };
        let podman_args = build_podman_run_args(&req);
        let apple_args = build_run_args(&req);
        // Same argv shape — podman and container share the run CLI surface.
        assert_eq!(podman_args, apple_args);
        // No --publish, no -v (no host bind mounts, no external traffic in M3).
        assert!(
            !podman_args
                .iter()
                .any(|a| a == "--publish" || a == "-p" || a == "-v")
        );
    }

    #[test]
    fn build_podman_rm_args_includes_force_flag() {
        let args = build_podman_rm_args("ato-desktop-test-123");
        assert_eq!(args, vec!["rm", "-f", "ato-desktop-test-123"]);
        // -f is mandatory: a shared-machine container outlives the wrapper,
        // so cleanup must stop+remove in one call.
    }
}

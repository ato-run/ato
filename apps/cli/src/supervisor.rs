use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_adapter_api::AdapterContext;
use ato_adapter_http::{HTTP_ADAPTER_ID, serve_proxy};
use ato_adapter_process::{ProcessAdapter, ProcessSpec, terminate_process_tree};
use ato_computation::ComputationRef;
use ato_objects::{ActiveRun, LocalCapsuleRepository, ObjectStore};

use crate::{
    adapter_registry,
    authoring::{AdapterConfig, load_state},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SupervisorState {
    Preparing,
    Starting,
    Active,
    Stopping,
    Sealed,
    Failed,
}

fn enter(state: SupervisorState) {
    let _ = state;
}

pub(crate) fn start_durable(
    repository: &LocalCapsuleRepository,
    branch: &str,
    head: &ComputationRef,
    bindings: &BTreeMap<String, String>,
) -> Result<()> {
    if repository.active_run()?.is_some() {
        bail!("capsule already has an active Run; stop it before resuming");
    }
    let log_path = repository.root().join("runs/output.log");
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("__worker")
        .arg(repository.project())
        .arg(branch)
        .arg(head.to_string())
        .env("ATO_RUNTIME_BINDINGS", serde_json::to_string(bindings)?)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stdout);
    configure_detached_process(&mut command);
    let child = command.spawn()?;
    for _ in 0..100 {
        if repository.active_run()?.is_some() {
            return Ok(());
        }
        if process_start_time(child.id()).is_none() {
            bail!(
                "capsule worker exited before becoming active; see {}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    bail!(
        "capsule worker did not become active; see {}",
        log_path.display()
    )
}

pub(crate) fn worker(project: &Path, branch: &str, head: &ComputationRef) -> Result<()> {
    let repository = LocalCapsuleRepository::open(project)?;
    enter(SupervisorState::Preparing);
    let config = load_state(head, repository.objects())?.config;
    let registry = adapter_registry()?;
    let context = AdapterContext {
        workspace: repository.project(),
        objects: repository.objects(),
    };
    for configured in &config.adapter {
        registry.get(&configured.use_adapter)?.preflight(&context)?;
    }
    enter(SupervisorState::Starting);
    let active = ActiveRun {
        branch: branch.to_owned(),
        head: head.clone(),
        pid: std::process::id(),
        process_start_time: process_start_time(std::process::id())
            .context("worker process start time is unavailable")?,
        process_group: current_process_group()?,
        boot_session: boot_session_identity()?,
        status: "active".to_owned(),
    };
    repository.set_active_run(&active)?;
    enter(SupervisorState::Active);

    let bindings: BTreeMap<String, String> = std::env::var("ATO_RUNTIME_BINDINGS")
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default();
    let environment: BTreeMap<_, _> = config
        .binding
        .iter()
        .filter_map(|binding| {
            bindings
                .get(&binding.id)
                .map(|value| (binding.environment.clone(), value.clone()))
        })
        .collect();
    let mut processes = Vec::new();
    for process in config.process {
        let adapter = ProcessAdapter::new(ProcessSpec {
            id: process.id,
            command: process.command,
            cwd: process.cwd,
            environment: environment.clone(),
        })?;
        processes.push(adapter.spawn_attached(repository.project())?);
    }
    for configured in &config.adapter {
        registry.get(&configured.use_adapter)?.attach(&context)?;
    }
    spawn_http_adapters(&repository, &config.adapter, true)?;
    if processes.is_empty() {
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    for process in &mut processes {
        let _ = process.wait()?;
    }
    // A failed or exited child is still a valid handoff point. Keep the Run
    // active until an explicit `ato stop` seals it.
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

pub(crate) fn stop_active(repository: &LocalCapsuleRepository) -> Result<Option<ActiveRun>> {
    enter(SupervisorState::Stopping);
    let Some(run) = repository.active_run()? else {
        return Ok(None);
    };
    if boot_session_identity()? != run.boot_session
        || process_start_time(run.pid).as_deref() != Some(run.process_start_time.as_str())
    {
        enter(SupervisorState::Failed);
        bail!(
            "active Run process identity no longer matches; refusing to stop PID {}",
            run.pid
        );
    }
    terminate_process_tree(run.pid, run.process_group)?;
    for _ in 0..100 {
        if process_start_time(run.pid).is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    enter(SupervisorState::Sealed);
    Ok(Some(run))
}

pub(crate) fn run_foreground(
    project: &Path,
    head: &ComputationRef,
    bindings: &BTreeMap<String, String>,
) -> Result<()> {
    let repository = LocalCapsuleRepository::open(project)?;
    let config = load_state(head, repository.objects())?.config;
    spawn_http_adapters(&repository, &config.adapter, false)?;
    let environment: BTreeMap<_, _> = config
        .binding
        .iter()
        .filter_map(|binding| {
            bindings
                .get(&binding.id)
                .map(|value| (binding.environment.clone(), value.clone()))
        })
        .collect();
    let mut processes = Vec::new();
    for process in config.process {
        let adapter = ProcessAdapter::new(ProcessSpec {
            id: process.id,
            command: process.command,
            cwd: process.cwd,
            environment: environment.clone(),
        })?;
        processes.push(adapter.spawn(project)?);
    }
    for process in &mut processes {
        let _ = process.wait()?;
    }
    Ok(())
}

fn spawn_http_adapters(
    repository: &LocalCapsuleRepository,
    configured: &[AdapterConfig],
    record: bool,
) -> Result<()> {
    for adapter in configured {
        if adapter.use_adapter != HTTP_ADAPTER_ID {
            continue;
        }
        let listen = adapter
            .listen
            .as_deref()
            .context("ato.http@1 adapter requires listen")?
            .parse()?;
        let upstream = adapter
            .upstream
            .as_deref()
            .context("ato.http@1 adapter requires upstream")?
            .parse()?;
        let port = ato_computation::PortId::parse(
            adapter
                .port
                .as_deref()
                .context("ato.http@1 adapter requires port")?,
        )?;
        let project = repository.project().to_path_buf();
        let branch = repository
            .active_run()?
            .map_or_else(|| "ephemeral".to_owned(), |run| run.branch);
        std::thread::spawn(move || {
            let _ = serve_proxy(listen, upstream, port, move |observation| {
                if !record {
                    return;
                }
                let Ok(repository) = LocalCapsuleRepository::open(&project) else {
                    return;
                };
                let Ok(Some(active)) = repository.active_run() else {
                    return;
                };
                let Ok(payload_ref) = repository.objects().put(&observation.payload) else {
                    return;
                };
                let previous = repository
                    .records_for_stream(&branch, None)
                    .ok()
                    .and_then(|records| records.last().map(|record| record.seq));
                let _ = repository.append_record(ato_objects::RecordEnvelope {
                    seq: 0,
                    stream: branch.clone(),
                    adapter_id: HTTP_ADAPTER_ID.to_owned(),
                    protocol_id: observation.protocol_id,
                    port_id: observation.port_id,
                    direction: observation.direction,
                    payload_ref,
                    head_before: active.head.clone(),
                    head_after: active.head,
                    caused_by: previous.into_iter().collect(),
                    observed_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or_else(|_| "0".to_owned(), |value| value.as_secs().to_string()),
                });
            });
        });
    }
    Ok(())
}

#[cfg(unix)]
fn configure_detached_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_detached_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(target_os = "linux")]
fn process_start_time(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    fields.get(19).map(|value| (*value).to_owned())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_start_time(pid: u32) -> Option<String> {
    command_output("ps", &["-o", "lstart=", "-p", &pid.to_string()])
}

#[cfg(windows)]
fn process_start_time(pid: u32) -> Option<String> {
    command_output(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            &format!("(Get-Process -Id {pid} -ErrorAction Stop).StartTime.ToUniversalTime().Ticks"),
        ],
    )
}

#[cfg(target_os = "linux")]
fn boot_session_identity() -> Result<String> {
    Ok(std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}

#[cfg(target_os = "macos")]
fn boot_session_identity() -> Result<String> {
    command_output("sysctl", &["-n", "kern.boottime"])
        .context("kernel boot identity is unavailable")
}

#[cfg(windows)]
fn boot_session_identity() -> Result<String> {
    command_output(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "(Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToUniversalTime().Ticks",
        ],
    )
    .context("Windows boot identity is unavailable")
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn boot_session_identity() -> Result<String> {
    bail!("boot/session identity is unavailable on this platform")
}

#[cfg(unix)]
fn current_process_group() -> Result<u32> {
    command_output(
        "ps",
        &["-o", "pgid=", "-p", &std::process::id().to_string()],
    )
    .and_then(|value| value.parse().ok())
    .context("current process group is unavailable")
}

#[cfg(windows)]
fn current_process_group() -> Result<u32> {
    Ok(std::process::id())
}

#[cfg(any(unix, windows))]
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

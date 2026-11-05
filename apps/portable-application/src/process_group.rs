//! Supervisor for one native process service group.
//!
//! Every service of a selected multi-process Derivation is started, made
//! ready, watched, stopped, and reaped here, in that order, by one owner.
//! Services are one Application trust boundary: there is no independent
//! restart, and a single service exit fails the whole group. The supervisor
//! knows nothing about K, D, sandboxes, or peer wiring; the caller hands it
//! fully-built commands and the addresses that prove readiness.

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ato_adapter_process::{
    ProcessAdapter, ProcessHandle, ProcessSpec, force_kill_process_tree, process_group_is_alive,
};

/// How long one service may take to accept TCP on its first Port.
pub const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a SIGTERM may take before the group is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// A runtime-owned helper process (one peer shim) that belongs to a service.
pub struct HelperProcess {
    pub name: String,
    pub handle: ProcessHandle,
}

/// One service as the caller built it: a command, its environment, the
/// address readiness is observed on, and the helpers it depends on.
pub struct ServiceSpec {
    pub name: String,
    pub command: Vec<String>,
    /// Workspace-relative working directory.
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub readiness: SocketAddr,
    pub helpers: Vec<HelperProcess>,
}

struct RunningService {
    name: String,
    handle: ProcessHandle,
    helpers: Vec<HelperProcess>,
}

/// The live processes of one selected route.
pub struct ProcessServiceGroup {
    services: Vec<RunningService>,
}

impl ProcessServiceGroup {
    /// Start every service in order. A service starts only after every earlier
    /// service accepts TCP on its readiness address; any failure stops and
    /// reaps what already started before returning.
    pub fn start(workspace: &std::path::Path, services: Vec<ServiceSpec>) -> Result<Self> {
        if services.is_empty() {
            bail!("a native process service group declares no service");
        }
        let mut group = Self {
            services: Vec::with_capacity(services.len()),
        };
        for mut spec in services {
            for service in &mut group.services {
                for helper in &mut service.helpers {
                    if let Some(status) = helper.handle.try_wait()? {
                        let error = anyhow::anyhow!(
                            "peer shim `{}` exited before the group was ready: {status}",
                            helper.name
                        );
                        return Err(group.fail(error));
                    }
                }
            }
            for helper in &mut spec.helpers {
                if let Some(status) = helper.handle.try_wait()? {
                    let error = anyhow::anyhow!(
                        "peer shim `{}` exited before its service started: {status}",
                        helper.name
                    );
                    return Err(group.fail(error));
                }
            }
            let adapter = ProcessAdapter::new(ProcessSpec {
                id: spec.name.clone(),
                command: spec.command,
                cwd: spec.cwd,
                environment: spec.environment,
                isolated_group: true,
            })
            .with_context(|| format!("prepare service `{}`", spec.name))?;
            let handle = match adapter.spawn(workspace) {
                Ok(handle) => handle,
                Err(error) => {
                    return Err(group.fail(anyhow::Error::new(error).context(format!(
                        "start service `{}`",
                        spec.name
                    ))));
                }
            };
            group.services.push(RunningService {
                name: spec.name,
                handle,
                helpers: spec.helpers,
            });
            if let Err(error) = group.wait_until_ready(spec.readiness) {
                return Err(group.fail(error));
            }
        }
        Ok(group)
    }

    /// The first service or helper that exited, if any. Once one component of
    /// the group is gone the group is no longer the route it claims to be.
    pub fn exited(&mut self) -> Result<Option<String>> {
        for service in &mut self.services {
            if let Some(status) = service.handle.try_wait()? {
                return Ok(Some(format!(
                    "service `{}` exited: {status}",
                    service.name
                )));
            }
            for helper in &mut service.helpers {
                if let Some(status) = helper.handle.try_wait()? {
                    return Ok(Some(format!(
                        "peer shim `{}` for service `{}` exited: {status}",
                        helper.name, service.name
                    )));
                }
            }
        }
        Ok(None)
    }

    /// The live pid of every service, in start order. Run evidence only; it
    /// never enters K, D, or a receipt identity.
    pub fn pids(&self) -> Vec<(String, u32)> {
        self.services
            .iter()
            .map(|service| (service.name.clone(), service.handle.pid()))
            .collect()
    }

    /// Stop every component in reverse start order and prove it is gone.
    pub fn stop(mut self) -> Result<()> {
        let mut failures = Vec::new();
        for service in self.services.iter_mut().rev() {
            if let Err(error) = stop_handle(&service.name, &mut service.handle) {
                failures.push(format!("{error:#}"));
            }
            for helper in service.helpers.iter_mut().rev() {
                if let Err(error) = stop_handle(&helper.name, &mut helper.handle) {
                    failures.push(format!("{error:#}"));
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            bail!("process group was not fully stopped: {}", failures.join("; "))
        }
    }

    fn wait_until_ready(&mut self, readiness: SocketAddr) -> Result<()> {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            for service in &mut self.services {
                if let Some(status) = service.handle.try_wait()? {
                    bail!(
                        "service `{}` exited before the group was ready: {status}",
                        service.name
                    );
                }
                for helper in &mut service.helpers {
                    if let Some(status) = helper.handle.try_wait()? {
                        bail!(
                            "peer shim `{}` for service `{}` exited before readiness: {status}",
                            helper.name,
                            service.name
                        );
                    }
                }
            }
            if TcpStream::connect_timeout(&readiness, Duration::from_millis(250)).is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "service `{}` did not accept TCP on {readiness} within {}s",
                    self.services
                        .last()
                        .map(|service| service.name.as_str())
                        .unwrap_or("?"),
                    READY_TIMEOUT.as_secs()
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Stop and reap after a failed start; the reported error stays primary.
    fn fail(mut self, error: anyhow::Error) -> anyhow::Error {
        if let Err(stop) = self.stop_owned() {
            return error.context(format!("additionally, {stop:#}"));
        }
        error
    }

    fn stop_owned(&mut self) -> Result<()> {
        let mut failures = Vec::new();
        for service in self.services.iter_mut().rev() {
            if let Err(error) = stop_handle(&service.name, &mut service.handle) {
                failures.push(format!("{error:#}"));
            }
            for helper in service.helpers.iter_mut().rev() {
                if let Err(error) = stop_handle(&helper.name, &mut helper.handle) {
                    failures.push(format!("{error:#}"));
                }
            }
        }
        self.services.clear();
        if failures.is_empty() {
            Ok(())
        } else {
            bail!("process group was not fully stopped: {}", failures.join("; "))
        }
    }
}

/// Stop one process group, escalate once, and require that it is gone.
fn stop_handle(name: &str, handle: &mut ProcessHandle) -> Result<()> {
    if handle.try_wait()?.is_some() {
        return Ok(());
    }
    let pid = handle.pid();
    let process_group = handle.process_group();
    handle.terminate()?;
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline {
        if handle.try_wait()?.is_some() {
            return confirm_gone(name, process_group);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    force_kill_process_tree(pid, process_group)?;
    for _ in 0..200 {
        if handle.try_wait()?.is_some() {
            return confirm_gone(name, process_group);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    bail!("process `{name}` (pid {pid}) was not reaped after SIGKILL")
}

fn confirm_gone(name: &str, process_group: u32) -> Result<()> {
    if process_group != 0 && process_group_is_alive(process_group) {
        bail!("process group of `{name}` is still alive after stop");
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn free_port() -> u16 {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    fn service(name: &str, port: u16, seconds: u64) -> ServiceSpec {
        ServiceSpec {
            name: name.to_owned(),
            command: vec![
                "python3".to_owned(),
                "-c".to_owned(),
                format!(
                    "import socket,time\n\
                     s=socket.socket()\n\
                     s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)\n\
                     s.bind(('127.0.0.1',{port}))\n\
                     s.listen()\n\
                     time.sleep({seconds})\n"
                ),
            ],
            cwd: PathBuf::from("."),
            environment: BTreeMap::new(),
            readiness: (std::net::Ipv4Addr::LOCALHOST, port).into(),
            helpers: Vec::new(),
        }
    }

    fn assert_reaped(pids: &[(String, u32)]) {
        for (name, pid) in pids {
            assert!(
                !process_group_is_alive(*pid),
                "process `{name}` (pid {pid}) survived stop"
            );
        }
    }

    #[test]
    fn starts_every_service_in_order_and_stops_every_process() {
        let workspace = tempfile::tempdir().unwrap();
        let first = free_port();
        let second = free_port();
        let group = ProcessServiceGroup::start(
            workspace.path(),
            vec![service("first", first, 30), service("second", second, 30)],
        )
        .expect("the group starts");
        assert_eq!(group.pids().len(), 2);
        assert!(TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, first)).is_ok());
        assert!(TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, second)).is_ok());
        let pids = group.pids();
        group.stop().expect("the group stops");
        assert_reaped(&pids);
        // The ports are released, which only happens once both are gone.
        assert!(TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, first)).is_ok());
        assert!(TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, second)).is_ok());
    }

    #[test]
    fn one_service_exit_is_reported_for_the_whole_group() {
        let workspace = tempfile::tempdir().unwrap();
        let first = free_port();
        let second = free_port();
        let group = ProcessServiceGroup::start(
            workspace.path(),
            vec![service("short", first, 1), service("long", second, 30)],
        )
        .expect("the group starts");
        let mut group = group;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exit = None;
        while Instant::now() < deadline {
            if let Some(reason) = group.exited().unwrap() {
                exit = Some(reason);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let reason = exit.expect("a short service exits within 10s");
        assert!(reason.contains("short"), "{reason}");
        let pids = group.pids();
        group.stop().expect("the group stops");
        assert_reaped(&pids);
    }

    #[test]
    fn a_failed_start_reaps_what_already_started() {
        let workspace = tempfile::tempdir().unwrap();
        let first = free_port();
        let failing = ServiceSpec {
            name: "fails".to_owned(),
            command: vec!["python3".to_owned(), "-c".to_owned(), "raise SystemExit(7)".to_owned()],
            cwd: PathBuf::from("."),
            environment: BTreeMap::new(),
            readiness: (std::net::Ipv4Addr::LOCALHOST, free_port()).into(),
            helpers: Vec::new(),
        };
        let error = ProcessServiceGroup::start(
            workspace.path(),
            vec![service("first", first, 30), failing],
        )
        .err()
        .expect("a service that exits before readiness fails the start");
        assert!(format!("{error:#}").contains("fails"), "{error:#}");
        // The already-started service released its Port: it was stopped and
        // reaped, not abandoned.
        assert!(TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, first)).is_ok());
    }
}

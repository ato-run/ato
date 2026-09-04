//! Process spawning and process-tree ownership without runtime inference.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

use ato_adapter_api::{
    AdapterAttachContext, AdapterCapabilities, AdapterContext, AdapterError, AdapterFactory,
    AdapterInstance, AttachedAdapter,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROCESS_ADAPTER_ID: &str = "ato.process@1";

#[derive(Default)]
pub struct ProcessLifecycleAdapter;

impl AdapterFactory for ProcessLifecycleAdapter {
    fn id(&self) -> &str {
        PROCESS_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            observe: true,
            verify: true,
            quiesce: true,
            ..AdapterCapabilities::default()
        }
    }

    fn preflight(
        &self,
        instance: &AdapterInstance,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        let spec = parse_spec(instance)?;
        ProcessAdapter::new(spec)
            .map_err(operation_error)?
            .preflight(context)
    }

    fn attach(
        &self,
        instance: &AdapterInstance,
        context: &AdapterAttachContext<'_>,
    ) -> Result<Box<dyn AttachedAdapter>, AdapterError> {
        let spec = parse_spec(instance)?;
        let isolated_group = spec.isolated_group;
        let handle = ProcessAdapter::new(spec)
            .map_err(operation_error)?
            .spawn_with_group(context.runtime.workspace, isolated_group)
            .map_err(operation_error)?;
        Ok(Box::new(ProcessSession {
            instance_id: instance.instance_id.clone(),
            handle,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSpec {
    pub id: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub isolated_group: bool,
}

pub struct ProcessHandle {
    child: Child,
    process_group: u32,
}

impl ProcessHandle {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn process_group(&self) -> u32 {
        self.process_group
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        Ok(self.child.try_wait()?)
    }

    pub fn wait(&mut self) -> Result<ExitStatus, ProcessError> {
        Ok(self.child.wait()?)
    }

    pub fn terminate(&mut self) -> Result<(), ProcessError> {
        if self.process_group == 0 {
            self.child.kill()?;
            Ok(())
        } else {
            terminate_process_tree(self.pid(), self.process_group)
        }
    }

    fn terminate_and_reap(&mut self) -> Result<(), ProcessError> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        self.terminate()?;
        for _ in 0..100 {
            if self.try_wait()?.is_some() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // Adapter attachment starts the application process. Any later error
        // (for example replay verification before Run activation) must not
        // orphan that process merely because normal detach was never reached.
        let _ = self.terminate_and_reap();
    }
}

#[derive(Debug, Clone)]
pub struct ProcessAdapter {
    spec: ProcessSpec,
}

impl ProcessAdapter {
    pub fn new(spec: ProcessSpec) -> Result<Self, ProcessError> {
        if spec.id.is_empty() || spec.command.is_empty() {
            return Err(ProcessError::InvalidSpec);
        }
        Ok(Self { spec })
    }

    pub fn spec(&self) -> &ProcessSpec {
        &self.spec
    }

    pub fn spawn(&self, workspace: &std::path::Path) -> Result<ProcessHandle, ProcessError> {
        self.spawn_with_group(workspace, true)
    }

    pub fn spawn_attached(
        &self,
        workspace: &std::path::Path,
    ) -> Result<ProcessHandle, ProcessError> {
        self.spawn_with_group(workspace, false)
    }

    fn spawn_with_group(
        &self,
        workspace: &std::path::Path,
        isolated_group: bool,
    ) -> Result<ProcessHandle, ProcessError> {
        let program = &self.spec.command[0];
        let cwd = workspace.join(&self.spec.cwd);
        let mut command = Command::new(program);
        command
            .args(&self.spec.command[1..])
            .current_dir(cwd)
            .env_clear()
            .envs(explicit_base_environment())
            .envs(&self.spec.environment)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if isolated_group {
            configure_process_group(&mut command);
        }
        let child = command.spawn()?;
        let pid = child.id();
        Ok(ProcessHandle {
            child,
            process_group: if isolated_group { pid } else { 0 },
        })
    }
}

impl ProcessAdapter {
    fn preflight(&self, context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        let cwd = context.workspace.join(&self.spec.cwd);
        if !cwd.is_dir() {
            return Err(AdapterError::Operation(format!(
                "process `{}` cwd does not exist: {}",
                self.spec.id,
                cwd.display()
            )));
        }
        Ok(())
    }
}

struct ProcessSession {
    instance_id: String,
    handle: ProcessHandle,
}

impl AttachedAdapter for ProcessSession {
    fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn adapter_id(&self) -> &str {
        PROCESS_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterFactory::capabilities(&ProcessLifecycleAdapter)
    }

    fn quiesce(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Ok(())
    }

    fn detach(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        self.handle.terminate_and_reap().map_err(operation_error)
    }

    fn wait(&mut self) -> Result<(), AdapterError> {
        let status = self.handle.wait().map_err(operation_error)?;
        if status.success() {
            Ok(())
        } else {
            Err(AdapterError::Operation(format!(
                "process `{}` exited with {status}",
                self.instance_id
            )))
        }
    }
}

fn parse_spec(instance: &AdapterInstance) -> Result<ProcessSpec, AdapterError> {
    if instance.adapter_id != PROCESS_ADAPTER_ID {
        return Err(AdapterError::InvalidConfig(format!(
            "process factory cannot attach `{}`",
            instance.adapter_id
        )));
    }
    serde_json::from_value(instance.config.clone()).map_err(AdapterError::from)
}

fn operation_error(error: ProcessError) -> AdapterError {
    AdapterError::Operation(error.to_string())
}

fn explicit_base_environment() -> BTreeMap<String, String> {
    ["PATH", "SYSTEMROOT", "WINDIR"]
        .into_iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| (name.to_owned(), value))
        })
        .collect()
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
pub fn terminate_process_tree(pid: u32, process_group: u32) -> Result<(), ProcessError> {
    if pid != process_group {
        return Err(ProcessError::UnownedProcessGroup);
    }
    let status = Command::new("kill")
        .args(["-TERM", "--", &format!("-{process_group}")])
        .status()?;
    if !status.success() {
        return Err(ProcessError::TerminationFailed);
    }
    Ok(())
}

/// SIGKILL the whole process group.
///
/// Separate from [`terminate_process_tree`] because the two are different
/// steps of one lifecycle, not alternatives: a graceful stop asks, and this
/// one does not. A supervisor that only ever asked would leave a workload
/// ignoring SIGTERM holding its state directory open forever.
#[cfg(unix)]
pub fn force_kill_process_tree(pid: u32, process_group: u32) -> Result<(), ProcessError> {
    if pid != process_group {
        return Err(ProcessError::UnownedProcessGroup);
    }
    let status = Command::new("kill")
        .args(["-KILL", "--", &format!("-{process_group}")])
        .status()?;
    // A group that has already exited is the outcome this call wanted, so a
    // non-zero status is not on its own a failure. The caller verifies the
    // subtree is gone rather than trusting either result.
    let _ = status;
    Ok(())
}

/// Whether any process in the group is still alive.
///
/// `kill(-pgid, 0)` performs the permission and existence check without
/// delivering a signal, which is exactly the "did the subtree actually
/// disappear" question a supervisor must answer before packing state.
#[cfg(unix)]
pub fn process_group_is_alive(process_group: u32) -> bool {
    Command::new("kill")
        .args(["-0", "--", &format!("-{process_group}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn force_kill_process_tree(pid: u32, process_group: u32) -> Result<(), ProcessError> {
    terminate_process_tree(pid, process_group)
}

#[cfg(windows)]
pub fn process_group_is_alive(_process_group: u32) -> bool {
    false
}

#[cfg(windows)]
pub fn terminate_process_tree(pid: u32, process_group: u32) -> Result<(), ProcessError> {
    if pid != process_group {
        return Err(ProcessError::UnownedProcessGroup);
    }
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status()?;
    if !status.success() {
        return Err(ProcessError::TerminationFailed);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("process spec requires an id and non-empty argv")]
    InvalidSpec,
    #[error("process adapter I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("process group does not belong to the recorded process")]
    UnownedProcessGroup,
    #[error("process tree termination failed")]
    TerminationFailed,
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn host_environment_does_not_cross_the_process_boundary() {
        assert!(std::env::var_os("HOME").is_some());
        let adapter = ProcessAdapter::new(ProcessSpec {
            id: "isolated-env".to_owned(),
            command: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "test -z \"$HOME\"".to_owned(),
            ],
            cwd: PathBuf::from("."),
            environment: BTreeMap::new(),
            isolated_group: false,
        })
        .unwrap();
        let mut handle = adapter
            .spawn_attached(PathBuf::from(".").as_path())
            .unwrap();
        assert!(handle.wait().unwrap().success());
    }

    #[test]
    fn dropping_an_attached_process_terminates_and_reaps_it() {
        let adapter = ProcessAdapter::new(ProcessSpec {
            id: "drop-cleanup".to_owned(),
            command: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "while true; do sleep 1; done".to_owned(),
            ],
            cwd: PathBuf::from("."),
            environment: BTreeMap::new(),
            isolated_group: false,
        })
        .unwrap();
        let handle = adapter
            .spawn_attached(PathBuf::from(".").as_path())
            .unwrap();
        let pid = handle.pid();
        assert!(
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );

        drop(handle);

        assert!(
            !Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
    }
}

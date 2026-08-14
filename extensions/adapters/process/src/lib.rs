//! Process spawning and process-tree ownership without runtime inference.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

use ato_adapter_api::{Adapter, AdapterCapabilities, AdapterContext, AdapterError};
use thiserror::Error;

pub const PROCESS_ADAPTER_ID: &str = "ato.process@1";

#[derive(Default)]
pub struct ProcessLifecycleAdapter;

impl Adapter for ProcessLifecycleAdapter {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    pub id: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
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
        terminate_process_tree(self.pid(), self.process_group)
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

impl Adapter for ProcessAdapter {
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

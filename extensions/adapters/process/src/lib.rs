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
    /// Where stdout and stderr go. `None` inherits the caller's streams.
    output: Option<BoundedOutput>,
}

impl ProcessAdapter {
    pub fn new(spec: ProcessSpec) -> Result<Self, ProcessError> {
        if spec.id.is_empty() || spec.command.is_empty() {
            return Err(ProcessError::InvalidSpec);
        }
        Ok(Self { spec, output: None })
    }

    /// Keep at most `max_bytes` of the workload's stdout and stderr at
    /// `path`, instead of inheriting the caller's streams.
    ///
    /// For a caller whose own stdout is a result, or a workload nobody
    /// trusts: the streams are drained continuously (a pipe never fills) and
    /// the kept output is bounded (a chatty workload cannot fill the disk).
    /// The newest output is kept; [`read_output_tail`] reads it back.
    pub fn with_output_file(mut self, path: impl Into<PathBuf>, max_bytes: u64) -> Self {
        self.output = Some(BoundedOutput {
            path: path.into(),
            max_bytes,
        });
        self
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
            .stdin(Stdio::inherit());
        match &self.output {
            Some(_) => {
                command
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
            }
            None => {
                command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
            }
        }
        if isolated_group {
            configure_process_group(&mut command);
        }
        let mut child = command.spawn()?;
        if let Some(output) = &self.output {
            let sink =
                std::sync::Arc::new(std::sync::Mutex::new(BoundedOutputSink::create(output)?));
            if let Some(stdout) = child.stdout.take() {
                drain_into(stdout, sink.clone());
            }
            if let Some(stderr) = child.stderr.take() {
                drain_into(stderr, sink);
            }
        }
        let pid = child.id();
        Ok(ProcessHandle {
            child,
            process_group: if isolated_group { pid } else { 0 },
        })
    }
}

/// A size-bounded destination for a workload's output.
#[derive(Debug, Clone)]
struct BoundedOutput {
    path: PathBuf,
    max_bytes: u64,
}

/// Two segments, `<path>.1` (older) and `<path>` (newer), each at most half
/// the bound. When the newer one is full it replaces the older, so the total
/// kept never exceeds the bound and the most recent output always survives.
struct BoundedOutputSink {
    path: PathBuf,
    previous: PathBuf,
    segment_limit: u64,
    current: std::fs::File,
    written: u64,
}

impl BoundedOutputSink {
    fn create(output: &BoundedOutput) -> std::io::Result<Self> {
        let previous = previous_segment(&output.path);
        let _ = std::fs::remove_file(&previous);
        Ok(Self {
            current: std::fs::File::create(&output.path)?,
            path: output.path.clone(),
            previous,
            segment_limit: (output.max_bytes / 2).max(1),
            written: 0,
        })
    }

    fn write(&mut self, mut bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write as _;
        while !bytes.is_empty() {
            if self.written >= self.segment_limit {
                std::fs::rename(&self.path, &self.previous)?;
                self.current = std::fs::File::create(&self.path)?;
                self.written = 0;
            }
            let room = (self.segment_limit - self.written) as usize;
            let (now, later) = bytes.split_at(room.min(bytes.len()));
            self.current.write_all(now)?;
            self.written += now.len() as u64;
            bytes = later;
        }
        Ok(())
    }
}

fn previous_segment(path: &std::path::Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// Read the stream until it closes. A write failure stops KEEPING output,
/// never reading it: the workload must not block on a full pipe.
fn drain_into(
    mut stream: impl std::io::Read + Send + 'static,
    sink: std::sync::Arc<std::sync::Mutex<BoundedOutputSink>>,
) {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        let mut keeping = true;
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) if keeping => {
                    let written = sink
                        .lock()
                        .map(|mut sink| sink.write(&buffer[..read]))
                        .unwrap_or_else(|_| Err(std::io::Error::other("poisoned")));
                    keeping = written.is_ok();
                }
                Ok(_) => {}
            }
        }
    });
}

/// The last `max_bytes` of output kept by [`ProcessAdapter::with_output_file`].
pub fn read_output_tail(path: &std::path::Path, max_bytes: usize) -> String {
    let mut kept = std::fs::read(previous_segment(path)).unwrap_or_default();
    kept.extend(std::fs::read(path).unwrap_or_default());
    let start = kept.len().saturating_sub(max_bytes);
    String::from_utf8_lossy(&kept[start..]).into_owned()
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
    use std::path::Path;

    fn process_group_alive(process_group: u32) -> bool {
        Command::new("kill")
            .args(["-0", "--", &format!("-{process_group}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn wait_for_process_group_exit(process_group: u32) {
        for _ in 0..100 {
            if !process_group_alive(process_group) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("process group {process_group} remained orphaned");
    }

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

    #[test]
    fn stopping_one_of_two_isolated_runs_does_not_stop_the_other() {
        let adapter = |id: &str| {
            ProcessAdapter::new(ProcessSpec {
                id: id.to_owned(),
                command: vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 30".to_owned()],
                cwd: PathBuf::from("."),
                environment: BTreeMap::new(),
                isolated_group: true,
            })
            .unwrap()
        };
        let first = adapter("run-a").spawn(Path::new(".")).unwrap();
        let second = adapter("run-b").spawn(Path::new(".")).unwrap();
        let second_pid = second.pid();

        drop(first);

        assert!(
            Command::new("kill")
                .args(["-0", &second_pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success(),
            "stopping run-a affected run-b"
        );
        drop(second);
    }

    #[test]
    fn run_owned_process_groups_collect_grandchildren_without_cross_run_kill() {
        let adapter = |id: &str| {
            ProcessAdapter::new(ProcessSpec {
                id: id.to_owned(),
                command: vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "sleep 60 & wait".to_owned(),
                ],
                cwd: PathBuf::from("."),
                environment: BTreeMap::new(),
                isolated_group: true,
            })
            .unwrap()
        };
        let first = adapter("run-first")
            .spawn(PathBuf::from(".").as_path())
            .unwrap();
        let second = adapter("run-second")
            .spawn(PathBuf::from(".").as_path())
            .unwrap();
        let first_group = first.process_group();
        let second_group = second.process_group();
        assert!(process_group_alive(first_group));
        assert!(process_group_alive(second_group));

        drop(first);
        wait_for_process_group_exit(first_group);
        assert!(
            process_group_alive(second_group),
            "cleaning one Run must not signal another Run's process group"
        );

        drop(second);
        wait_for_process_group_exit(second_group);
        let orphan_process_count = [first_group, second_group]
            .into_iter()
            .filter(|group| process_group_alive(*group))
            .count();
        assert_eq!(orphan_process_count, 0);
    }
}

#[cfg(all(test, unix))]
mod bounded_output_tests {
    use super::*;

    fn spawn_writer(script: &str, path: &std::path::Path, max: u64) -> ProcessHandle {
        let workspace = std::env::temp_dir();
        ProcessAdapter::new(ProcessSpec {
            id: "chatty".to_owned(),
            command: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd: PathBuf::new(),
            environment: BTreeMap::new(),
            isolated_group: true,
        })
        .unwrap()
        .with_output_file(path, max)
        .spawn(&workspace)
        .unwrap()
    }

    fn settle(path: &std::path::Path) {
        // The drain threads finish once the pipes close.
        let mut last = u64::MAX;
        for _ in 0..100 {
            let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
                + std::fs::metadata(previous_segment(path))
                    .map(|meta| meta.len())
                    .unwrap_or(0);
            if size == last {
                return;
            }
            last = size;
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn a_chatty_workload_cannot_exceed_the_output_bound() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.log");
        // ~4 MiB of output against a 64 KiB bound, then a final marker.
        let mut handle = spawn_writer(
            "head -c 4194304 /dev/zero | tr '\\0' x; echo; echo LAST-LINE",
            &path,
            64 * 1024,
        );
        handle.wait().unwrap();
        settle(&path);
        let kept = std::fs::metadata(&path).unwrap().len()
            + std::fs::metadata(previous_segment(&path))
                .map(|meta| meta.len())
                .unwrap_or(0);
        assert!(kept <= 64 * 1024, "kept {kept} bytes");
        // The newest output survives.
        assert!(read_output_tail(&path, 64).contains("LAST-LINE"));
    }

    #[test]
    fn stderr_is_kept_too_and_small_output_is_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.log");
        let mut handle = spawn_writer("echo to-stdout; echo to-stderr >&2", &path, 1024 * 1024);
        handle.wait().unwrap();
        settle(&path);
        let tail = read_output_tail(&path, 4096);
        assert!(
            tail.contains("to-stdout") && tail.contains("to-stderr"),
            "{tail}"
        );
    }
}

//! Native OS process host.
//!
//! This is the concrete [`RunnerHost`](crate::backend::RunnerHost) for a real
//! operating system. It consolidates the OS-specific execution primitives —
//! process-group spawn (`process_group(0)`), the whole-group teardown
//! (`kill(-pid, SIGKILL)` on Unix, `taskkill /T /F` on Windows), and
//! console-window suppression on Windows — behind the host-agnostic seam. A
//! future host either reuses this or provides its own `RunnerHost` without
//! touching the supervision logic above it.
//!
//! Binary resolution is *policy*, not an OS primitive: where `ato` lives
//! depends on how the host was packaged. So [`NativeHost`] takes an injected
//! resolver rather than baking a lookup strategy in — [`resolve_on_path`] is
//! provided as the obvious default for hosts that ship the binary on `PATH`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::backend::{
    ChildId, CommandSpec, CompletedCommand, HostError, ManagedChild, OutputSink, RunnerHost,
    SpawnSpec,
};

/// Extension trait suppressing the console window a GUI-subsystem process would
/// otherwise pop when it spawns a *console* child on Windows. No-op elsewhere.
pub trait CommandNoWindowExt {
    /// Spawn the child without allocating a console window on Windows.
    fn no_console_window(&mut self) -> &mut Self;
}

impl CommandNoWindowExt for Command {
    #[cfg(target_os = "windows")]
    fn no_console_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW (0x0800_0000): run the console child without a
        // console window. Replaces the whole creation-flag set, but no spawn
        // site here sets other flags, so this is safe.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(target_os = "windows"))]
    fn no_console_window(&mut self) -> &mut Self {
        self
    }
}

/// Terminate the process **group** led by `pid` — the wrapper and every process
/// it started as one unit. Idempotent and safe on an already-dead group.
///
/// The spawned child is its own group leader ([`NativeHost::spawn`] sets
/// `process_group(0)` on Unix), so a single negative-pid `SIGKILL` reaps the
/// whole tree — including a detached runtime that outlived the wrapper. On
/// Windows, which has no process groups, `taskkill /T` walks the process tree
/// to the same effect.
///
/// `pid <= 1` is refused: never signal the whole session (group 0) or init.
pub fn terminate_process_group(pid: u32) -> Result<(), HostError> {
    if pid <= 1 {
        return Ok(());
    }
    #[cfg(unix)]
    {
        // SAFETY: kill(2) with a negative pid signals the process group led by
        // `pid`. SIGKILL is an uncatchable hard teardown.
        let rc = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            // ESRCH: the group is already gone — the desired end state.
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(HostError::Teardown(format!(
                    "kill process group {pid}: {err}"
                )));
            }
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        // /T = terminate the tree (process + children), /F = force. taskkill
        // returns non-zero when the pid is already gone; that is our success
        // state, so only a failure to *spawn* taskkill is an error.
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .no_console_window()
            .status()
            .map_err(|e| HostError::Teardown(format!("taskkill {pid}: {e}")))?;
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        Ok(())
    }
}

/// Configure `cmd` so its spawned child becomes its own **process-group
/// leader** (`pgid == its pid`). A child detached by that child inherits the
/// group, so a single [`terminate_process_group`] on the child's pid later
/// reaps the whole tree — the wrapper and anything it started — even after the
/// wrapper itself has exited.
///
/// No-op on non-Unix: Windows has no process groups, and its teardown walks the
/// Is any process still alive in the group led by `pid`?
///
/// Signal 0 performs the permission and existence checks without delivering
/// anything, which is the only portable way to ask "is this group still there"
/// without racing a wait.
#[cfg(unix)]
fn process_group_alive(pid: u32) -> bool {
    // SAFETY: kill(2) with signal 0 delivers nothing; it only reports whether
    // the target group exists and is signalable.
    let rc = unsafe { libc::kill(-(pid as libc::pid_t), 0) };
    rc == 0
}

/// Ask a process group to exit, and give it a bounded chance to do so before
/// taking it down.
///
/// The difference from [`terminate_process_group`] is the chance itself. A
/// process that owns work — an execution, a database write, a child tree of its
/// own — needs to hear "stop" and finish, and SIGKILL never gives it that. But
/// waiting forever is its own failure, so the wait is bounded and the fallback
/// is the same hard teardown as before.
///
/// On Windows there is no signal to ask with, so this is the hard teardown
/// immediately — documented rather than pretended otherwise.
pub fn terminate_process_group_gracefully(
    pid: u32,
    grace: std::time::Duration,
) -> Result<(), HostError> {
    if pid <= 1 {
        return Ok(());
    }
    #[cfg(unix)]
    {
        // SAFETY: as above — a negative pid addresses the process group.
        let rc = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                return Ok(()); // already gone: the desired end state
            }
        }
        let deadline = std::time::Instant::now() + grace;
        while std::time::Instant::now() < deadline {
            if !process_group_alive(pid) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    // Outlived the grace, or a platform with nothing to ask with.
    terminate_process_group(pid)
}

/// process tree via `taskkill /T` instead. This is the spawn-side pair of
/// [`terminate_process_group`]; [`NativeHost::spawn`] applies it for you, and it
/// is exposed for callers that build their own `Command` yet want the same
/// group-reap guarantee.
pub fn mark_process_group_leader(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = cmd;
    }
}

/// Resolve `name` against the `PATH` environment variable, honoring the
/// platform executable extension search on Windows (`PATHEXT`). The default
/// [`NativeHost`] resolver for hosts that ship ato binaries on `PATH`.
pub fn resolve_on_path(name: &str) -> Result<PathBuf, HostError> {
    // An explicit path (absolute or containing a separator) is used as-is.
    let as_path = Path::new(name);
    if as_path.is_absolute() || name.contains(std::path::MAIN_SEPARATOR) {
        return if as_path.is_file() {
            Ok(as_path.to_path_buf())
        } else {
            Err(HostError::BinaryNotFound(name.to_string()))
        };
    }

    let path_var =
        std::env::var_os("PATH").ok_or_else(|| HostError::BinaryNotFound(name.to_string()))?;
    #[cfg(windows)]
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_string())
        .split(';')
        .filter(|e| !e.is_empty())
        .map(|e| e.to_ascii_lowercase())
        .collect();

    for dir in std::env::split_paths(&path_var) {
        let direct = dir.join(name);
        if direct.is_file() {
            return Ok(direct);
        }
        #[cfg(windows)]
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(HostError::BinaryNotFound(name.to_string()))
}

/// A child spawned on the native OS, tracked as a process-group leader.
pub struct NativeChild {
    /// Group-leader pid — stored separately from the handle so it survives
    /// reaping and stays usable for [`terminate_process_group`].
    pid: u32,
    /// The wrapper handle, behind a mutex so liveness can be polled through a
    /// shared reference. `None` once the process has been reaped.
    handle: Mutex<Option<std::process::Child>>,
    exit_code: Mutex<Option<i32>>,
}

impl ManagedChild for NativeChild {
    fn id(&self) -> ChildId {
        ChildId(self.pid as u64)
    }

    fn is_alive(&self) -> bool {
        let mut guard = self
            .handle
            .lock()
            .expect("native child handle mutex poisoned");
        match guard.as_mut() {
            Some(child) => match child.try_wait() {
                // Exited — drop the reaped handle so we do not wait on it twice.
                Ok(Some(status)) => {
                    *self
                        .exit_code
                        .lock()
                        .expect("native child exit-code mutex poisoned") =
                        Some(status.code().unwrap_or(-1));
                    *guard = None;
                    false
                }
                Ok(None) => true,
                // Can't determine — assume alive so teardown still runs.
                Err(_) => true,
            },
            None => false,
        }
    }

    fn exit_code(&self) -> Option<i32> {
        *self
            .exit_code
            .lock()
            .expect("native child exit-code mutex poisoned")
    }

    fn terminate_group_gracefully(&mut self, grace: std::time::Duration) -> Result<(), HostError> {
        let result = terminate_process_group_gracefully(self.pid, grace);
        if let Ok(mut guard) = self.handle.lock()
            && let Some(mut child) = guard.take()
        {
            // No kill() here: the group was asked to leave and given time, so
            // the handle is waited on rather than shot. `wait` returns at once
            // for a process that already exited.
            let _ = child.wait();
        }
        result
    }

    fn terminate_group(&mut self) -> Result<(), HostError> {
        let result = terminate_process_group(self.pid);
        // Reap the wrapper handle regardless of the group-kill result so we do
        // not leak a zombie; the group signal above already tore down the tree.
        if let Ok(mut guard) = self.handle.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        result
    }
}

/// Resolver policy injected into a [`NativeHost`]: maps an ato-family binary
/// name to an absolute path on this host.
type BinaryResolver = Box<dyn Fn(&str) -> Result<PathBuf, HostError> + Send + Sync>;

/// The native OS execution host. Spawns process-group leaders with the correct
/// per-platform flags and tears them down as whole groups.
pub struct NativeHost {
    resolve: BinaryResolver,
}

impl NativeHost {
    /// A host that resolves ato-family binaries with `resolver`.
    pub fn new<F>(resolver: F) -> Self
    where
        F: Fn(&str) -> Result<PathBuf, HostError> + Send + Sync + 'static,
    {
        Self {
            resolve: Box::new(resolver),
        }
    }

    /// A host that resolves binaries on `PATH` (see [`resolve_on_path`]).
    pub fn with_path_lookup() -> Self {
        Self::new(resolve_on_path)
    }
}

impl RunnerHost for NativeHost {
    type Child = NativeChild;

    fn resolve_binary(&self, name: &str) -> Result<PathBuf, HostError> {
        (self.resolve)(name)
    }

    fn spawn(&self, spec: &SpawnSpec) -> Result<NativeChild, HostError> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args);
        for (key, value) in &spec.env {
            cmd.env(key, value);
        }
        cmd.no_console_window();
        // Make the child its own process-group leader so terminate_group can
        // reap the whole tree with one negative-pid signal.
        mark_process_group_leader(&mut cmd);
        match &spec.output {
            OutputSink::LogFile(path) => {
                // Redirect to a file, not a pipe: a long-running foreground
                // child stalls once an undrained pipe buffer fills.
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(|e| HostError::Spawn(format!("open log {}: {e}", path.display())))?;
                let err_handle = file
                    .try_clone()
                    .map_err(|e| HostError::Spawn(format!("clone log handle: {e}")))?;
                cmd.stdin(Stdio::null())
                    .stdout(Stdio::from(file))
                    .stderr(Stdio::from(err_handle));
            }
            OutputSink::Inherit => {}
            OutputSink::Null => {
                cmd.stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
            }
        }
        let child = cmd
            .spawn()
            .map_err(|e| HostError::Spawn(format!("spawn {}: {e}", spec.program.display())))?;
        let pid = child.id();
        Ok(NativeChild {
            pid,
            handle: Mutex::new(Some(child)),
            exit_code: Mutex::new(None),
        })
    }

    fn run_to_completion(&self, spec: &CommandSpec) -> Result<CompletedCommand, HostError> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args).stdin(Stdio::null());
        for (key, value) in &spec.env {
            cmd.env(key, value);
        }
        cmd.no_console_window();
        let output = cmd.output().map_err(|e| {
            HostError::Run(format!("run {} to completion: {e}", spec.program.display()))
        })?;
        Ok(CompletedCommand {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_signal_session_and_init() {
        // pid 0 (whole session / process group of the caller) and pid 1 (init)
        // must never be signalled. Both are no-op successes.
        terminate_process_group(0).unwrap();
        terminate_process_group(1).unwrap();
    }

    #[test]
    fn resolve_on_path_rejects_missing_binary() {
        let err = resolve_on_path("definitely-not-a-real-binary-xyzzy").unwrap_err();
        assert!(matches!(err, HostError::BinaryNotFound(_)));
    }

    #[test]
    fn injected_resolver_is_used() {
        let host = NativeHost::new(|name| Ok(PathBuf::from(format!("/opt/ato/{name}"))));
        assert_eq!(
            host.resolve_binary("ato").unwrap(),
            PathBuf::from("/opt/ato/ato")
        );
    }

    #[cfg(unix)]
    #[test]
    fn runs_short_command_and_captures_stdout_stderr_and_exit_code() {
        let host = NativeHost::with_path_lookup();
        let shell = host.resolve_binary("sh").expect("sh on PATH for the test");
        let completed = host
            .run_to_completion(&CommandSpec {
                program: shell,
                args: vec!["-c".into(), "printf 'out'; printf 'err' >&2; exit 7".into()],
                env: vec![],
            })
            .expect("command completes");

        assert_eq!(completed.exit_code, 7);
        assert_eq!(completed.stdout, b"out");
        assert_eq!(completed.stderr, b"err");
        assert!(!completed.success());
    }

    #[cfg(unix)]
    #[test]
    fn short_command_receives_extra_environment() {
        let host = NativeHost::with_path_lookup();
        let shell = host.resolve_binary("sh").expect("sh on PATH for the test");
        let completed = host
            .run_to_completion(&CommandSpec {
                program: shell,
                args: vec!["-c".into(), "printf '%s' \"$ATO_HOST_TEST_VALUE\"".into()],
                env: vec![("ATO_HOST_TEST_VALUE".into(), "present".into())],
            })
            .expect("command completes");

        assert!(completed.success());
        assert_eq!(completed.stdout, b"present");
        assert!(completed.stderr.is_empty());
    }
}

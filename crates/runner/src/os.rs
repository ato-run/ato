//! Native OS process host — the `(d)` OS receptacle.
//!
//! This is the concrete [`RunnerHost`](crate::backend::RunnerHost) for a real
//! operating system. It consolidates the OS-specific execution primitives that
//! were previously triplicated across the desktop shell — process-group spawn
//! (`process_group(0)`), the whole-group teardown (`kill(-pid, SIGKILL)` on
//! Unix, `taskkill /T /F` on Windows), and console-window suppression on
//! Windows — behind the host-agnostic seam. A future host (IoT / mobile / EV)
//! either reuses this or provides its own `RunnerHost` without touching the
//! supervision logic above it.
//!
//! Binary resolution is *policy*, not an OS primitive: where `ato` / `nacelle`
//! live depends on how the host was packaged (the desktop resolves them from
//! its app bundle). So [`NativeHost`] takes an injected resolver rather than
//! baking a lookup strategy in — [`resolve_on_path`] is provided as the
//! obvious default for hosts that ship the binaries on `PATH`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::backend::{ChildId, HostError, ManagedChild, OutputSink, RunnerHost, SpawnSpec};

/// Extension trait suppressing the console window a GUI-subsystem process would
/// otherwise pop when it spawns a *console* child on Windows. No-op elsewhere.
///
/// Owned here (rather than in the desktop shell) because it is an OS execution
/// primitive every host's process spawns want; the desktop re-exports it so its
/// existing `crate::proc_util::CommandNoWindowExt` call sites are unchanged.
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

    let path_var = std::env::var_os("PATH").ok_or_else(|| HostError::BinaryNotFound(name.to_string()))?;
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
}

impl ManagedChild for NativeChild {
    fn id(&self) -> ChildId {
        ChildId(self.pid as u64)
    }

    fn is_alive(&self) -> bool {
        let mut guard = self.handle.lock().expect("native child handle mutex poisoned");
        match guard.as_mut() {
            Some(child) => match child.try_wait() {
                // Exited — drop the reaped handle so we do not wait on it twice.
                Ok(Some(_status)) => {
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
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Make the child its own process-group leader so terminate_group can
            // reap the whole tree with one negative-pid signal.
            cmd.process_group(0);
        }
        match &spec.output {
            OutputSink::LogFile(path) => {
                // Redirect to a file, not a pipe: a long-running foreground
                // child stalls once an undrained pipe buffer fills. The
                // supervisor tails the file instead.
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
            host.resolve_binary("nacelle").unwrap(),
            PathBuf::from("/opt/ato/nacelle")
        );
    }

    #[cfg(unix)]
    #[test]
    fn spawns_a_group_and_tears_it_down() {
        use crate::supervisor::ProcessSupervisor;

        // `sleep` is its own group leader after process_group(0); terminate_group
        // reaps it. Resolve via PATH so the test is host-portable.
        let host = NativeHost::with_path_lookup();
        let sleep = host
            .resolve_binary("sleep")
            .expect("sleep on PATH for the test");
        let mut sup = ProcessSupervisor::new(host);
        let spec = SpawnSpec {
            program: sleep,
            args: vec!["30".into()],
            env: vec![],
            output: OutputSink::Null,
        };
        let _id = sup.spawn(&spec).unwrap();
        assert_eq!(sup.supervised_count(), 1);
        // Alive immediately after spawn — nothing reaped.
        assert_eq!(sup.reap(), 0);
        // Tear the group down; the child must be gone afterwards.
        sup.shutdown().unwrap();
        assert_eq!(sup.supervised_count(), 0);
    }
}

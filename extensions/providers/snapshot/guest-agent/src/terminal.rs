//! PTY-backed workload used by `SessionSurface(kind=terminal)`.
//!
//! The PTY master never leaves guest-agent. A host relay may attach through the
//! dedicated terminal vsock listener, while process creation and teardown stay
//! behind the existing supervisor `Workload` contract.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ato_ipc::terminal_surface::{MAX_TERMINAL_COLS, MAX_TERMINAL_ROWS};

use crate::supervisor::{ChildWorkload, SpawnPlan, Workload, spawn_script_for_workload};

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const STOP_GRACE: Duration = Duration::from_millis(2_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

struct TerminalState {
    master: Option<File>,
    generation: u64,
    cols: u16,
    rows: u16,
    pgid: Option<i32>,
    running: bool,
    exit: Option<TerminalExit>,
}

/// Shared attachment point for the terminal broker and PTY workload.
pub struct TerminalBrokerState {
    inner: Mutex<TerminalState>,
    viewer_attached: AtomicBool,
}

impl Default for TerminalBrokerState {
    fn default() -> Self {
        Self {
            inner: Mutex::new(TerminalState {
                master: None,
                generation: 0,
                cols: DEFAULT_COLS,
                rows: DEFAULT_ROWS,
                pgid: None,
                running: false,
                exit: None,
            }),
            viewer_attached: AtomicBool::new(false),
        }
    }
}

impl TerminalBrokerState {
    pub fn attach(&self) -> io::Result<TerminalAttachment<'_>> {
        self.viewer_attached
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "terminal viewer already attached",
                )
            })?;
        let state = self.inner.lock().map_err(poisoned)?;
        let master = match state.master.as_ref().map(File::try_clone) {
            Some(Ok(master)) => master,
            Some(Err(error)) => {
                self.viewer_attached.store(false, Ordering::Release);
                return Err(error);
            }
            None => {
                self.viewer_attached.store(false, Ordering::Release);
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "terminal workload is not running",
                ));
            }
        };
        Ok(TerminalAttachment {
            master,
            generation: state.generation,
            cols: state.cols,
            rows: state.rows,
            owner: self,
        })
    }

    pub fn resize(&self, generation: u64, cols: u16, rows: u16) -> io::Result<()> {
        if cols > MAX_TERMINAL_COLS || rows > MAX_TERMINAL_ROWS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal size exceeds v1 bounds",
            ));
        }
        ato_ipc::terminal_surface::validate_terminal_size(cols, rows)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let mut state = self.inner.lock().map_err(poisoned)?;
        if state.generation != generation || !state.running {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "stale terminal generation",
            ));
        }
        let master = state.master.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "terminal master is unavailable",
            )
        })?;
        set_winsize(master, cols, rows)?;
        if let Some(pgid) = state.pgid {
            unsafe {
                libc::killpg(pgid, libc::SIGWINCH);
            }
        }
        state.cols = cols;
        state.rows = rows;
        Ok(())
    }

    pub fn exit_for(&self, generation: u64) -> Option<TerminalExit> {
        self.inner
            .lock()
            .ok()
            .filter(|state| state.generation == generation)
            .and_then(|state| state.exit)
    }

    fn begin(&self, master: File, pgid: i32) -> io::Result<u64> {
        let mut state = self.inner.lock().map_err(poisoned)?;
        state.generation = state.generation.saturating_add(1);
        state.master = Some(master);
        state.pgid = Some(pgid);
        state.running = true;
        state.exit = None;
        Ok(state.generation)
    }

    fn finish(&self, generation: u64, status: io::Result<ExitStatus>) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        if state.generation != generation {
            return;
        }
        state.running = false;
        state.pgid = None;
        state.master.take();
        state.exit = Some(match status {
            Ok(status) => exit_from_status(status),
            Err(_) => TerminalExit {
                code: None,
                signal: None,
            },
        });
    }

    fn running_pgid(&self, generation: u64) -> Option<i32> {
        self.inner.lock().ok().and_then(|state| {
            (state.generation == generation && state.running)
                .then_some(state.pgid)
                .flatten()
        })
    }

    pub fn is_running(&self) -> bool {
        self.inner.lock().is_ok_and(|state| state.running)
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> io::Error {
    io::Error::other("terminal state lock poisoned")
}

pub struct TerminalAttachment<'a> {
    pub master: File,
    pub generation: u64,
    pub cols: u16,
    pub rows: u16,
    owner: &'a TerminalBrokerState,
}

impl Drop for TerminalAttachment<'_> {
    fn drop(&mut self) {
        self.owner.viewer_attached.store(false, Ordering::Release);
    }
}

pub struct PtyChildWorkload {
    broker: Arc<TerminalBrokerState>,
    generation: Option<u64>,
}

impl PtyChildWorkload {
    pub fn new(broker: Arc<TerminalBrokerState>) -> Self {
        Self {
            broker,
            generation: None,
        }
    }
}

impl Workload for PtyChildWorkload {
    fn start(&mut self, plan: &SpawnPlan) -> io::Result<()> {
        if plan.cmd.is_empty() {
            return Err(io::Error::other("supervisor cmd is empty"));
        }
        let (master, slave) = open_pty(DEFAULT_COLS, DEFAULT_ROWS)?;
        let mut command = Command::new("/bin/sh");
        let outer_cwd = if plan.rootfs.is_some() {
            "/"
        } else {
            &plan.cwd
        };
        let slave_fd = slave.as_raw_fd();
        command
            .arg("-c")
            .arg(spawn_script_for_workload(plan))
            .current_dir(outer_cwd)
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .env("TERM", "xterm-256color");
        for (key, value) in &plan.base_env {
            command.env(key, value);
        }
        #[cfg(unix)]
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let pgid = child.id() as i32;
        let generation = self.broker.begin(master, pgid)?;
        self.generation = Some(generation);
        let broker = Arc::clone(&self.broker);
        std::thread::spawn(move || broker.finish(generation, child.wait()));
        Ok(())
    }

    fn stop(&mut self) -> io::Result<bool> {
        let Some(generation) = self.generation.take() else {
            return Ok(false);
        };
        let Some(pgid) = self.broker.running_pgid(generation) else {
            return Ok(false);
        };
        unsafe {
            libc::killpg(pgid, libc::SIGTERM);
        }
        let deadline = Instant::now() + STOP_GRACE;
        while self.broker.running_pgid(generation).is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if self.broker.running_pgid(generation).is_some() {
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
        while self.broker.running_pgid(generation).is_some() {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(true)
    }

    fn is_running(&self) -> bool {
        self.generation.is_some() && self.broker.is_running()
    }

    fn run_once(&mut self, _plan: &SpawnPlan) -> io::Result<i32> {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "run_once cannot use terminal PTY mode",
        ))
    }
}

pub enum GuestWorkload {
    Log(ChildWorkload),
    Pty(PtyChildWorkload),
}

impl Workload for GuestWorkload {
    fn start(&mut self, plan: &SpawnPlan) -> io::Result<()> {
        match self {
            Self::Log(workload) => workload.start(plan),
            Self::Pty(workload) => workload.start(plan),
        }
    }
    fn stop(&mut self) -> io::Result<bool> {
        match self {
            Self::Log(workload) => workload.stop(),
            Self::Pty(workload) => workload.stop(),
        }
    }
    fn is_running(&self) -> bool {
        match self {
            Self::Log(workload) => workload.is_running(),
            Self::Pty(workload) => workload.is_running(),
        }
    }
    fn run_once(&mut self, plan: &SpawnPlan) -> io::Result<i32> {
        match self {
            Self::Log(workload) => workload.run_once(plan),
            Self::Pty(workload) => workload.run_once(plan),
        }
    }
}

fn open_pty(cols: u16, rows: u16) -> io::Result<(File, File)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let size_ptr = std::ptr::from_mut(&mut size);
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            size_ptr,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) })
}

fn set_winsize(master: &File, cols: u16, rows: u16) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn exit_from_status(status: ExitStatus) -> TerminalExit {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        TerminalExit {
            code: status.code(),
            signal: status.signal(),
        }
    }
    #[cfg(not(unix))]
    TerminalExit {
        code: status.code(),
        signal: None,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::path::PathBuf;

    fn plan(script: &str) -> SpawnPlan {
        SpawnPlan {
            cmd: vec!["/bin/sh".into(), "-c".into(), script.into()],
            cwd: "/".into(),
            base_env: BTreeMap::new(),
            secret_env: Vec::new(),
            stdout_log: PathBuf::from("/dev/null"),
            stderr_log: PathBuf::from("/dev/null"),
            rootfs: None,
        }
    }

    fn read_until(master: &mut File, needle: &str) -> String {
        let fd = master.as_raw_fd();
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut output = Vec::new();
        while Instant::now() < deadline {
            let mut buffer = [0u8; 256];
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    output.extend_from_slice(&buffer[..count]);
                    if String::from_utf8_lossy(&output).contains(needle) {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&output).into_owned()
    }

    #[test]
    fn pty_round_trips_input_and_combined_output() {
        let broker = Arc::new(TerminalBrokerState::default());
        let mut workload = PtyChildWorkload::new(Arc::clone(&broker));
        workload
            .start(&plan(
                "IFS= read -r line; printf 'got:%s' \"$line\"; printf ':err' >&2",
            ))
            .expect("start PTY workload");
        let mut attachment = broker.attach().expect("attach terminal");
        attachment
            .master
            .write_all(b"hello\n")
            .expect("write input");
        let output = read_until(&mut attachment.master, ":err");
        assert!(output.contains("got:hello"), "output was {output:?}");
        assert!(output.contains(":err"), "stderr was {output:?}");
        let _ = workload.stop();
    }

    #[test]
    fn resize_updates_the_pty_and_stale_generation_is_rejected() {
        let broker = Arc::new(TerminalBrokerState::default());
        let mut workload = PtyChildWorkload::new(Arc::clone(&broker));
        workload
            .start(&plan("sleep 30"))
            .expect("start PTY workload");
        let attachment = broker.attach().expect("attach terminal");
        broker
            .resize(attachment.generation, 120, 40)
            .expect("resize");
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::ioctl(attachment.master.as_raw_fd(), libc::TIOCGWINSZ, &mut size) },
            0
        );
        assert_eq!((size.ws_col, size.ws_row), (120, 40));
        let stale = attachment.generation;
        drop(attachment);
        workload.stop().expect("stop first generation");
        workload
            .start(&plan("sleep 30"))
            .expect("restart PTY workload");
        assert!(broker.resize(stale, 80, 24).is_err());
        workload.stop().expect("stop second generation");
    }

    #[test]
    fn ctrl_c_and_stop_target_the_workload_process_group() {
        let broker = Arc::new(TerminalBrokerState::default());
        let mut workload = PtyChildWorkload::new(Arc::clone(&broker));
        workload
            .start(&plan(
                "trap 'printf INTERRUPTED; exit 0' INT; printf READY; while :; do sleep 1; done",
            ))
            .expect("start PTY workload");
        let mut attachment = broker.attach().expect("attach terminal");
        assert!(read_until(&mut attachment.master, "READY").contains("READY"));
        attachment.master.write_all(&[3]).expect("send Ctrl+C");
        let output = read_until(&mut attachment.master, "INTERRUPTED");
        assert!(output.contains("INTERRUPTED"), "output was {output:?}");
        let deadline = Instant::now() + Duration::from_secs(2);
        while workload.is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!workload.is_running());
    }

    #[test]
    fn only_one_terminal_viewer_can_attach() {
        let broker = Arc::new(TerminalBrokerState::default());
        let mut workload = PtyChildWorkload::new(Arc::clone(&broker));
        workload
            .start(&plan("sleep 30"))
            .expect("start PTY workload");
        let first = broker.attach().expect("first viewer");
        let second = broker.attach();
        assert!(matches!(second, Err(ref error) if error.kind() == io::ErrorKind::WouldBlock));
        drop(first);
        broker.attach().expect("viewer slot released");
        workload.stop().expect("stop workload");
    }
}

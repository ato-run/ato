//! Running a build plan's steps under isolation, with a fence that decides
//! whether the result may be published.
//!
//! ## Attempts, and why a fence is not optional
//!
//! A job is a request; an attempt is one execution of it. A slow attempt that
//! finishes after its retry has already published is the ordinary case, not the
//! exotic one — a network stall is enough. Without a fence the late attempt
//! would overwrite a newer result with older bytes, and nothing downstream
//! could tell.

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
#[cfg(unix)]
use ato_formation::failure::{FailureStage, FormationFailure};
use ato_formation::intent::{BuildStepV1, EffectiveBuildPlanV1};

use crate::build_sandbox::{
    BuildSandbox, GUEST_WORKSPACE_ROOT, NetworkPolicy, sandboxed_build_step_command,
};

/// One execution of a job.
#[derive(Debug, Clone)]
pub struct BuildAttempt {
    pub job_id: String,
    pub attempt_id: String,
    /// Monotonic per job. A result carrying a fence lower than the job's
    /// current one is refused at publication.
    pub attempt_fence: u64,
}

/// What a finished attempt produced.
#[derive(Debug)]
pub struct BuildOutcome {
    pub attempt: BuildAttempt,
    /// The assembled workspace, ready to be packed.
    pub workspace_root: PathBuf,
    /// Bounded and redacted. A build log can contain a token the source itself
    /// printed, and diagnostics are published.
    pub diagnostics: Vec<String>,
}

/// The largest diagnostic this will carry out of a build.
const MAX_DIAGNOSTIC_BYTES: usize = 8 * 1024;

/// How a step's output is reduced to something publishable.
///
/// Truncated from the END, because a failing command says why it failed on its
/// last lines, and the first 8KB of a compiler's output is almost never the
/// reason.
fn bounded_diagnostic(name: &str, stream: &[u8]) -> String {
    let text = String::from_utf8_lossy(stream);
    let trimmed = text.trim_end();
    let tail = if trimmed.len() > MAX_DIAGNOSTIC_BYTES {
        let start = trimmed.len() - MAX_DIAGNOSTIC_BYTES;
        // Land on a char boundary; a split code point would render as garbage.
        let start = (start..trimmed.len())
            .find(|index| trimmed.is_char_boundary(*index))
            .unwrap_or(trimmed.len());
        format!("…{}", &trimmed[start..])
    } else {
        trimmed.to_owned()
    };
    // Redact URLs INSIDE the text rather than passing the whole diagnostic
    // through a URL redactor: `redact_url` returns "<redacted>" for anything
    // that is not a URL, which turned every build failure into a message with
    // no content at all.
    format!("[{name}] {}", redact_urls_in(&tail))
}

/// Replace any URL in free text with its scheme, host and path.
///
/// A pre-signed grant's query string IS the credential, and a build log can
/// carry one the source itself printed. Everything around the URL is kept,
/// because that is where the reason usually is.
fn redact_urls_in(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("http") {
        let (before, tail) = rest.split_at(start);
        out.push_str(before);
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(tail.len());
        let (url, remainder) = tail.split_at(end);
        out.push_str(&ato_formation::source::redact_url(url));
        rest = remainder;
    }
    out.push_str(rest);
    out
}

/// Execute a plan's steps, in order, each under isolation.
pub fn run_build(
    plan: &EffectiveBuildPlanV1,
    attempt: BuildAttempt,
    sandbox: &BuildSandbox<'_>,
) -> Result<BuildOutcome> {
    let (source_root, workspace_root, cache_root, shim, network, limits) = (
        sandbox.source_root,
        sandbox.workspace_root,
        sandbox.cache_root,
        sandbox.shim,
        sandbox.network,
        sandbox.limits,
    );
    std::fs::create_dir_all(workspace_root)
        .with_context(|| format!("cannot create {}", workspace_root.display()))?;

    // The host writes the policy before every step, so it must live where no
    // step can reach: a link planted there by one step would carry the next
    // write outside the sandbox.
    let policy_path = sandbox.policy_host_path;
    ensure!(
        !path_is_within(policy_path, workspace_root)?
            && cache_root.is_none_or(|cache| !path_is_within(policy_path, cache).unwrap_or(true))
            && !path_is_within(policy_path, source_root)?,
        "the build sandbox policy must live outside every path the build can reach"
    );
    let mut diagnostics = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(limits.wall_clock_seconds);

    // A step that needs more network than the policy allows is refused before
    // ANY step runs: a build that fails at its third step has already spent
    // the first two, and left their output behind.
    if let Some(step) = plan
        .steps
        .iter()
        .find(|step| step.needs_network && network == NetworkPolicy::Denied)
    {
        bail!(
            "build step {:?} needs the network and this job's policy denies it",
            step.name
        );
    }

    for step in &plan.steps {
        // A step that declared no network must not get one, even when the job's
        // policy would have allowed it. The narrower of the two wins.
        let step_network = if step.needs_network {
            network
        } else {
            NetworkPolicy::Denied
        };

        // Resolved now, against the workspace as the previous steps left it:
        // one of them may have created the directory, or a link in its place.
        let guest_cwd = resolve_step_cwd(workspace_root, step)?;

        let command = sandboxed_build_step_command(
            &step.argv,
            guest_cwd.as_deref(),
            &step.env,
            &plan.toolchain_path,
            &BuildSandbox {
                source_root,
                workspace_root,
                cache_root,
                shim,
                policy_host_path: policy_path,
                network: step_network,
                limits,
                toolchain: step.toolchain_access,
            },
        )?;
        std::fs::write(
            policy_path,
            serde_json::to_vec_pretty(&command.policy)
                .context("cannot serialize the build sandbox policy")?,
        )
        .context("cannot write the build sandbox policy")?;

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!(
                "the build exceeded its {}s budget",
                limits.wall_clock_seconds
            );
        }
        let output = run_step(step, &command.argv, remaining)?;
        diagnostics.push(bounded_diagnostic(&step.name, &output));
    }

    Ok(BuildOutcome {
        attempt,
        workspace_root: workspace_root.to_path_buf(),
        diagnostics,
    })
}

/// Where an attempt's build policy lives: a control directory beside the
/// workspace, never inside anything the build binds.
pub fn control_policy_path(attempt_root: &Path) -> Result<PathBuf> {
    let control = attempt_root.join("control");
    std::fs::create_dir_all(&control)
        .with_context(|| format!("cannot create {}", control.display()))?;
    Ok(control.join("build-policy.json"))
}

/// Is `path` at or below `root`, comparing the real locations? A path that
/// does not exist yet is judged by its nearest existing ancestor.
fn path_is_within(path: &Path, root: &Path) -> Result<bool> {
    let root = std::fs::canonicalize(root)
        .with_context(|| format!("cannot resolve {}", root.display()))?;
    let mut probe = std::path::absolute(path).context("cannot resolve the policy path")?;
    let mut rest = Vec::new();
    let resolved = loop {
        match std::fs::canonicalize(&probe) {
            Ok(real) => break real,
            Err(_) => {
                let Some(name) = probe.file_name().map(ToOwned::to_owned) else {
                    return Ok(false);
                };
                rest.push(name);
                if !probe.pop() {
                    return Ok(false);
                }
            }
        }
    };
    let full = rest.iter().rev().fold(resolved, |at, name| at.join(name));
    Ok(full.starts_with(&root))
}

/// The guest directory a step runs in, or `None` for the workspace root.
///
/// The authored path was checked when the route was projected (plain names
/// only). Here the REAL directory is checked: every link on the way is
/// followed on the host, and the result must be a directory inside the
/// workspace. The guest path handed to the sandbox is that resolved one, so
/// the step runs exactly where this check looked — nothing falls back to the
/// workspace root or to anywhere else.
fn resolve_step_cwd(workspace_root: &Path, step: &BuildStepV1) -> Result<Option<String>> {
    if step.cwd_relative.is_empty() {
        return Ok(None);
    }
    let outside = || {
        #[cfg(unix)]
        {
            anyhow::Error::new(FormationFailure::new(
                "build_cwd_outside_workspace",
                FailureStage::Build,
                format!(
                    "build step {:?}: cwd {:?} is not a directory inside the workspace",
                    step.name, step.cwd_relative
                ),
            ))
        }
        #[cfg(not(unix))]
        {
            anyhow::anyhow!(
                "build step {:?}: cwd {:?} is not a directory inside the workspace",
                step.name,
                step.cwd_relative
            )
        }
    };
    let relative = ato_formation::projection::workspace_relative_cwd(&step.cwd_relative)
        .map_err(|_| outside())?;
    if relative.is_empty() {
        return Ok(None);
    }
    let root =
        std::fs::canonicalize(workspace_root).context("cannot resolve the build workspace")?;
    let resolved = std::fs::canonicalize(root.join(&relative)).map_err(|_| outside())?;
    let inside = resolved.strip_prefix(&root).map_err(|_| outside())?;
    if !resolved.is_dir() {
        return Err(outside());
    }
    let inside = inside.to_str().ok_or_else(outside)?;
    Ok(Some(if inside.is_empty() {
        GUEST_WORKSPACE_ROOT.to_owned()
    } else {
        format!("{GUEST_WORKSPACE_ROOT}/{inside}")
    }))
}

/// How much of each stream a step keeps while it runs: its tail, for the
/// diagnostic (itself bounded to `MAX_DIAGNOSTIC_BYTES`) and the typed failure
/// marker a shipped step prints last. The rest is read and dropped.
const STREAM_TAIL_BYTES: usize = 256 * 1024;

/// After the step's own process exits, how long its output pipes may stay
/// open (a descendant holding them) before the rest of its process group is
/// stopped. Only a descendant pays this; a step whose pipes close with it
/// returns at once.
const PIPE_CLOSE_GRACE: Duration = Duration::from_secs(2);

/// The last `STREAM_TAIL_BYTES` of a stream.
struct Tail {
    bytes: Vec<u8>,
}

impl Tail {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        // Trimmed in bulk, so memory stays within twice the bound and the copy
        // is amortized.
        if self.bytes.len() > 2 * STREAM_TAIL_BYTES {
            let excess = self.bytes.len() - STREAM_TAIL_BYTES;
            self.bytes.drain(..excess);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bytes.len() > STREAM_TAIL_BYTES {
            let excess = self.bytes.len() - STREAM_TAIL_BYTES;
            self.bytes.drain(..excess);
        }
        self.bytes
    }
}

/// One output pipe, read without ever blocking.
#[cfg(unix)]
struct Drain<R: std::io::Read + std::os::fd::AsRawFd> {
    pipe: Option<R>,
    tail: Tail,
}

#[cfg(unix)]
impl<R: std::io::Read + std::os::fd::AsRawFd> Drain<R> {
    /// Refuses rather than falling back to blocking reads, which would hang
    /// the worker on the first quiet pipe.
    fn new(pipe: R) -> std::io::Result<Self> {
        let fd = pipe.as_raw_fd();
        // SAFETY: flags on a descriptor this function owns.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: as above.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            pipe: Some(pipe),
            tail: Tail::new(),
        })
    }

    fn open(&self) -> bool {
        self.pipe.is_some()
    }

    fn poll_fd(&self) -> Option<libc::pollfd> {
        self.pipe.as_ref().map(|pipe| libc::pollfd {
            fd: pipe.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        })
    }

    /// Read everything available now. Closes on end of file or error.
    fn read_available(&mut self) {
        let mut buffer = [0_u8; 64 * 1024];
        while let Some(pipe) = self.pipe.as_mut() {
            match pipe.read(&mut buffer) {
                Ok(0) => self.pipe = None,
                Ok(read) => self.tail.push(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => self.pipe = None,
            }
        }
    }
}

/// Run one step, and prove its process group is gone before returning.
///
/// Returning while a descendant survives would let it keep writing to the
/// workspace after the build believed it had finished — and the workspace is
/// about to be packed and content-addressed, so that corruption becomes
/// permanent. So a build step may not leave anything running: whatever of its
/// process group outlives the step's own process is stopped (TERM, then KILL)
/// before this returns — on success, on failure and on timeout alike. A build
/// that relies on a background daemon surviving it is not a build.
///
/// (In the sandboxed build the step is bubblewrap with its own PID namespace,
/// whose descendants die with it; this is the invariant for the process group
/// this function itself starts.)
///
/// Both output streams are drained for as long as the step runs. A step's
/// output is not bounded — `pip install`, a compiler, a bundler can print
/// megabytes — and a pipe that nobody reads fills at 64 KiB and blocks its
/// writer, which then never exits: the step would hang until its budget and
/// fail as a timeout. Only each stream's tail is kept.
#[cfg(unix)]
fn run_step(step: &BuildStepV1, argv: &[String], budget: Duration) -> Result<Vec<u8>> {
    use std::os::unix::process::CommandExt as _;

    let (program, arguments) = argv.split_first().expect("argv is non-empty");
    let mut command = std::process::Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so termination reaches everything it spawned.
    command.process_group(0);

    let mut child = command
        .spawn()
        .with_context(|| format!("cannot start build step {:?}", step.name))?;
    let pid = child.id();
    // From here on, every way out stops the whole group first.
    let abort = |child: &mut std::process::Child, reason: String| -> Result<Vec<u8>> {
        terminate_group(pid);
        let _ = child.wait();
        if let Err(error) = ensure_group_gone(pid) {
            bail!("{reason}; {error}")
        }
        bail!(reason)
    };
    let drains = Drain::new(child.stdout.take().expect("piped")).and_then(|stdout| {
        Drain::new(child.stderr.take().expect("piped")).map(|stderr| (stdout, stderr))
    });
    let (mut stdout, mut stderr) = match drains {
        Ok(drains) => drains,
        Err(error) => {
            return abort(
                &mut child,
                format!(
                    "build step {:?}: cannot read its output without blocking ({error})",
                    step.name
                ),
            );
        }
    };

    let deadline = Instant::now() + budget;
    let mut exited_at: Option<Instant> = None;
    loop {
        let mut fds: Vec<libc::pollfd> = [stdout.poll_fd(), stderr.poll_fd()]
            .into_iter()
            .flatten()
            .collect();
        if !fds.is_empty() {
            // SAFETY: `fds` is a valid array of `fds.len()` pollfd entries.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 50) };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return abort(
                        &mut child,
                        format!(
                            "build step {:?}: cannot wait for its output ({error})",
                            step.name
                        ),
                    );
                }
            }
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
        stdout.read_available();
        stderr.read_available();

        // Seen without reaping: the step's process stays a zombie, so its pid
        // — the process group's id — cannot be reused while the rest of the
        // group is dealt with.
        if exited_at.is_none() {
            match leader_exited(pid) {
                Ok(true) => exited_at = Some(Instant::now()),
                Ok(false) => {}
                // Not knowing whether the step is still running is not
                // taken to mean it is.
                Err(error) => {
                    return abort(
                        &mut child,
                        format!(
                            "build step {:?}: cannot tell whether it has exited ({error})",
                            step.name
                        ),
                    );
                }
            }
        }
        match exited_at {
            Some(at) if (!stdout.open() && !stderr.open()) || at.elapsed() >= PIPE_CLOSE_GRACE => {
                let status = match child.wait() {
                    Ok(status) => status,
                    Err(error) => {
                        return abort(
                            &mut child,
                            format!("build step {:?}: cannot reap it ({error})", step.name),
                        );
                    }
                };
                // Anything of the step still running is stopped now, before
                // the workspace is handed on.
                if group_alive(pid) {
                    terminate_group(pid);
                    ensure_group_gone(pid)?;
                }
                // What the stopped descendants had written is still read, and
                // still bounded.
                let drained_until = Instant::now() + Duration::from_millis(500);
                while (stdout.open() || stderr.open()) && Instant::now() < drained_until {
                    stdout.read_available();
                    stderr.read_available();
                    std::thread::sleep(Duration::from_millis(20));
                }
                let mut combined = stdout.tail.finish();
                combined.extend_from_slice(&stderr.tail.finish());
                if !status.success() {
                    // A step that KNOWS why it refused says so in a line
                    // written for the uploader. Without this the typed
                    // reason is flattened into build output, and build
                    // output is exactly what must not reach the uploader —
                    // so a person who imported lodash would be told "the
                    // build failed".
                    if let Some(failure) = typed_step_failure(&combined) {
                        return Err(anyhow::Error::new(failure));
                    }
                    bail!(
                        "build step {:?} failed ({status}): {}",
                        step.name,
                        bounded_diagnostic(&step.name, &combined)
                    );
                }
                return Ok(combined);
            }
            None if Instant::now() >= deadline => {
                return abort(
                    &mut child,
                    format!("build step {:?} exceeded its time budget", step.name),
                );
            }
            _ => {}
        }
    }
}

/// Has the step's own process exited? Checked without reaping it.
#[cfg(unix)]
fn leader_exited(pid: u32) -> std::io::Result<bool> {
    // SAFETY: `info` is a zeroed siginfo the kernel fills; WNOWAIT leaves the
    // child waitable for `Child::wait`.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let result = libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        );
        if result == 0 {
            return Ok(info.si_pid() != 0);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        Err(error)
    }
}

/// Does any process of the group still exist?
///
/// `EPERM` means a process exists that this worker may not signal: alive.
/// Only `ESRCH` means gone; any other answer is not taken as gone either.
#[cfg(unix)]
fn group_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks existence.
    let result = unsafe { libc::kill(-(pid as libc::pid_t), 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn run_step(_step: &BuildStepV1, _argv: &[String], _budget: Duration) -> Result<Vec<u8>> {
    bail!("Formation build execution requires Unix process-group isolation")
}

/// The typed failure a build step reported about itself, if it reported one.
///
/// The marker is a whole line so it cannot be produced by accident in the
/// middle of other output, and the payload is JSON so a message may contain
/// anything — including the newline a line-oriented format could not carry.
///
/// Only steps Ato itself ships emit this. A step running somebody's `npm run
/// build` is not trusted to name its own failure code: it could mint any code
/// a client branches on, and the uploader's own build script is the last thing
/// that should decide what the platform says about it.
#[cfg(unix)]
fn typed_step_failure(output: &[u8]) -> Option<FormationFailure> {
    const MARKER: &str = "ATO_FORMATION_FAILURE ";
    #[derive(serde::Deserialize)]
    struct Reported {
        code: String,
        message: String,
    }
    let text = String::from_utf8_lossy(output);
    let payload = text
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(MARKER))?;
    let reported: Reported = serde_json::from_str(payload).ok()?;
    // The stage is NOT taken from the payload. Where a failure happened is the
    // worker's own knowledge, and a step naming its own stage could claim to
    // be an authoring refusal it is not.
    Some(FormationFailure::new(
        reported.code,
        FailureStage::Build,
        reported.message,
    ))
}

#[cfg(unix)]
fn terminate_group(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .status();
    std::thread::sleep(Duration::from_millis(500));
    let _ = std::process::Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .status();
}

#[cfg(unix)]
fn ensure_group_gone(pid: u32) -> Result<()> {
    for _ in 0..50 {
        if !group_alive(pid) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    bail!("build process group {pid} survived termination")
}

/// Whether an attempt is still the one allowed to publish.
///
/// Checked immediately before registration, not at the start: an attempt that
/// was current when it began may not be current when it finishes, and the
/// window between is exactly where a stale publish happens.
pub fn may_publish(attempt: &BuildAttempt, current_fence: u64) -> bool {
    attempt.attempt_fence >= current_fence
}

/// The workspace subtree that becomes the materialization.
pub fn output_root(outcome: &BuildOutcome, plan: &EffectiveBuildPlanV1) -> Result<PathBuf> {
    if plan.output_root.is_empty() {
        return Ok(outcome.workspace_root.clone());
    }
    let candidate = outcome.workspace_root.join(&plan.output_root);
    if !candidate.is_dir() {
        // The plan DECLARED this directory. Its absence is a disagreement
        // between declaration and execution, which is a build failure — never a
        // reason to fall back to the whole workspace.
        bail!(
            "the build plan declares output root {:?}, which the build did not produce",
            plan.output_root
        );
    }
    let root = outcome
        .workspace_root
        .canonicalize()
        .context("cannot resolve the workspace root")?;
    let output = candidate
        .canonicalize()
        .context("cannot resolve the declared output root")?;
    if !output.starts_with(&root) {
        bail!("the declared output root resolves outside the workspace");
    }
    Ok(output)
}

/// The guest path the workspace occupies during a build.
pub fn guest_workspace_root() -> &'static str {
    GUEST_WORKSPACE_ROOT
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn step(script: &str) -> (BuildStepV1, Vec<String>) {
        let argv = vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()];
        (
            BuildStepV1 {
                name: "test".to_owned(),
                argv: argv.clone(),
                needs_network: false,
                cwd_relative: String::new(),
                env: std::collections::BTreeMap::new(),
                toolchain_access: ato_formation::intent::ToolchainAccess::ReadOnly,
            },
            argv,
        )
    }

    fn run(script: &str, budget: Duration) -> Result<Vec<u8>> {
        let (step, argv) = step(script);
        run_step(&step, &argv, budget)
    }

    /// Four MiB and more to one stream: far past any pipe buffer.
    const CHATTY: &str = "head -c 4500000 /dev/zero | tr '\\0' a";

    #[test]
    fn megabytes_on_stdout_do_not_block_the_step() {
        let started = Instant::now();
        let output = run(CHATTY, Duration::from_secs(60)).expect("the step succeeds");
        assert!(started.elapsed() < Duration::from_secs(30));
        assert!(output.len() <= STREAM_TAIL_BYTES);
        assert!(output.iter().all(|byte| *byte == b'a'));
    }

    #[test]
    fn megabytes_on_stderr_do_not_block_the_step() {
        let output = run(&format!("{CHATTY} >&2"), Duration::from_secs(60)).expect("success");
        assert!(output.len() <= STREAM_TAIL_BYTES);
    }

    #[test]
    fn both_streams_at_once_do_not_block_the_step() {
        run(
            &format!("({CHATTY}) & ({CHATTY}) >&2; wait"),
            Duration::from_secs(60),
        )
        .expect("success");
    }

    #[test]
    fn a_failure_after_megabytes_keeps_its_last_words() {
        let error = run(
            &format!("{CHATTY}; echo; echo 'the actual reason' >&2; exit 3"),
            Duration::from_secs(60),
        )
        .expect_err("the step fails");
        let message = format!("{error:#}");
        assert!(message.contains("the actual reason"), "{message}");
        assert!(
            message.len() < MAX_DIAGNOSTIC_BYTES + 512,
            "{}",
            message.len()
        );
    }

    #[test]
    fn a_typed_failure_after_megabytes_is_still_typed() {
        let error = run(
            &format!(
                "{CHATTY}; echo; echo 'ATO_FORMATION_FAILURE {{\"code\":\"dependency_unavailable\",\"message\":\"no wheel\"}}'; exit 2"
            ),
            Duration::from_secs(60),
        )
        .expect_err("the step fails");
        let failure = error
            .downcast_ref::<FormationFailure>()
            .expect("a typed failure");
        assert_eq!(failure.code, "dependency_unavailable");
    }

    #[test]
    fn a_step_that_never_ends_is_stopped_with_everything_it_started() {
        let marker = format!("ato-build-drain-test-{}", std::process::id());
        let started = Instant::now();
        let error = run(
            &format!("sh -c 'sleep 600; :' {marker} & yes {marker}"),
            Duration::from_secs(2),
        )
        .expect_err("the step times out");
        assert!(format!("{error:#}").contains("exceeded its time budget"));
        assert!(started.elapsed() < Duration::from_secs(15));
        assert!(survivors(&marker).is_empty(), "processes outlived the step");
    }

    /// Pids whose command line contains `marker`. A failing pgrep fails the
    /// test: an empty answer must mean "none".
    fn survivors(marker: &str) -> Vec<String> {
        let output = std::process::Command::new("pgrep")
            .args(["-f", "--", marker])
            .output()
            .expect("pgrep");
        assert!(matches!(output.status.code(), Some(0 | 1)), "pgrep failed");
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_descendant_holding_the_pipes_is_stopped_before_the_step_returns() {
        let marker = format!("ato-build-holder-{}-{}", std::process::id(), line!());
        let started = Instant::now();
        run(
            &format!("(sh -c 'sleep 30; :' {marker} &); echo done"),
            Duration::from_secs(60),
        )
        .expect("the step succeeds");
        // Returned after the pipe grace and the group's termination, not
        // after the descendant's own 30 s — and with nothing left behind.
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(
            survivors(&marker).is_empty(),
            "a descendant outlived the step"
        );
    }

    #[test]
    fn a_descendant_that_lets_go_of_the_pipes_but_keeps_writing_is_stopped() {
        let marker = format!("ato-build-writer-{}-{}", std::process::id(), line!());
        let scratch = tempfile::tempdir().expect("tempdir");
        let file = scratch.path().join("workspace-file");
        // Detached from the step's output, it would be invisible to the pipe
        // check — and still rewriting the workspace.
        run(
            &format!(
                "(exec >/dev/null 2>&1 </dev/null; while true; do echo {marker} >> {}; sleep 0.05; done) & echo started",
                file.display()
            ),
            Duration::from_secs(60),
        )
        .expect("the step succeeds");
        assert!(
            survivors(&marker).is_empty(),
            "the writer outlived the step"
        );
        let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0),
            size,
            "the workspace changed after the step returned"
        );
    }

    #[test]
    fn a_step_that_leaves_nothing_behind_returns_at_once() {
        let started = Instant::now();
        let output = run("echo hello", Duration::from_secs(60)).expect("success");
        assert_eq!(output, b"hello\n");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_failing_step_with_a_descendant_still_fails_with_its_reason_and_leaves_nothing() {
        let marker = format!(
            "ato-build-failing-holder-{}-{}",
            std::process::id(),
            line!()
        );
        let error = run(
            &format!("(sh -c 'sleep 30; :' {marker} &); echo 'the reason' >&2; exit 4"),
            Duration::from_secs(60),
        )
        .expect_err("the step fails");
        assert!(format!("{error:#}").contains("the reason"), "{error:#}");
        assert!(
            survivors(&marker).is_empty(),
            "a descendant outlived the step"
        );
    }
}

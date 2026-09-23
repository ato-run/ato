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

use std::path::PathBuf;
#[cfg(unix)]
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
#[cfg(unix)]
use ato_formation::failure::{FailureStage, FormationFailure};
use ato_formation::intent::{BuildStepV1, EffectiveBuildPlanV1};

use crate::sandbox::{BuildSandbox, GUEST_WORKSPACE_ROOT, NetworkPolicy, sandboxed_build_command};

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

    let policy_path = workspace_root.join(".ato-build-policy.json");
    let mut diagnostics = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(limits.wall_clock_seconds);

    for step in &plan.steps {
        // A step that declared no network must not get one, even when the job's
        // policy would have allowed it. The narrower of the two wins.
        let step_network = if step.needs_network {
            network
        } else {
            NetworkPolicy::Denied
        };
        if step.needs_network && network == NetworkPolicy::Denied {
            bail!(
                "build step {:?} needs the network and this job's policy denies it",
                step.name
            );
        }

        let command = sandboxed_build_command(
            &step.argv,
            &BuildSandbox {
                source_root,
                workspace_root,
                cache_root,
                shim,
                policy_host_path: &policy_path,
                network: step_network,
                limits,
            },
        )?;
        std::fs::write(
            &policy_path,
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

    // The policy file is scaffolding, not output. Leaving it would put it in
    // the materialization and change its digest.
    let _ = std::fs::remove_file(&policy_path);

    Ok(BuildOutcome {
        attempt,
        workspace_root: workspace_root.to_path_buf(),
        diagnostics,
    })
}

/// How much of each stream a step keeps while it runs: its tail, for the
/// diagnostic (itself bounded to `MAX_DIAGNOSTIC_BYTES`) and the typed failure
/// marker a shipped step prints last. The rest is read and dropped.
const STREAM_TAIL_BYTES: usize = 256 * 1024;

/// After the step's own process exits, how long a descendant that still
/// holds its output pipes may keep them before this stops reading.
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
    fn new(pipe: R) -> Self {
        // SAFETY: flags on a descriptor this function owns.
        unsafe {
            let fd = pipe.as_raw_fd();
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Self {
            pipe: Some(pipe),
            tail: Tail::new(),
        }
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
/// permanent.
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
    let mut stdout = Drain::new(child.stdout.take().expect("piped"));
    let mut stderr = Drain::new(child.stderr.take().expect("piped"));

    let deadline = Instant::now() + budget;
    let mut exited: Option<(std::process::ExitStatus, Instant)> = None;
    loop {
        let mut fds: Vec<libc::pollfd> = [stdout.poll_fd(), stderr.poll_fd()]
            .into_iter()
            .flatten()
            .collect();
        if !fds.is_empty() {
            // SAFETY: `fds` is a valid array of `fds.len()` pollfd entries.
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 50) };
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
        stdout.read_available();
        stderr.read_available();

        if exited.is_none()
            && let Some(status) = child.try_wait().context("cannot poll the build step")?
        {
            exited = Some((status, Instant::now()));
        }
        match exited {
            Some((status, at)) => {
                // Done once both pipes are closed — or, if a descendant still
                // holds them, after a short grace: its output is not the
                // step's result.
                if (!stdout.open() && !stderr.open()) || at.elapsed() >= PIPE_CLOSE_GRACE {
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
            }
            None if Instant::now() >= deadline => {
                terminate_group(pid);
                let _ = child.wait();
                ensure_group_gone(pid)?;
                bail!("build step {:?} exceeded its time budget", step.name);
            }
            None => {}
        }
    }
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
        let alive = std::process::Command::new("kill")
            .args(["-0", "--", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !alive {
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
            &format!("sleep 600 {marker} & yes {marker}"),
            Duration::from_secs(2),
        )
        .expect_err("the step times out");
        assert!(format!("{error:#}").contains("exceeded its time budget"));
        assert!(started.elapsed() < Duration::from_secs(15));
        let survivors = std::process::Command::new("pgrep")
            .args(["-f", "--", &marker])
            .output()
            .expect("pgrep");
        assert!(
            String::from_utf8_lossy(&survivors.stdout).trim().is_empty(),
            "processes outlived the step"
        );
    }

    #[test]
    fn a_descendant_holding_the_pipes_does_not_hold_the_step() {
        // The step exits at once; something it left behind keeps the pipes.
        let marker = format!("ato-build-holder-test-{}", std::process::id());
        let started = Instant::now();
        run(
            &format!("(sleep 20 {marker} &); echo done"),
            Duration::from_secs(60),
        )
        .expect("success");
        assert!(started.elapsed() < Duration::from_secs(10));
        let _ = std::process::Command::new("pkill")
            .args(["-f", "--", &marker])
            .status();
    }
}

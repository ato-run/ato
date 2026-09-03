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
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
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

/// Run one step, and prove its process group is gone before returning.
///
/// Returning while a descendant survives would let it keep writing to the
/// workspace after the build believed it had finished — and the workspace is
/// about to be packed and content-addressed, so that corruption becomes
/// permanent.
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

    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait().context("cannot poll the build step")? {
            Some(status) => {
                let output = child
                    .wait_with_output()
                    .unwrap_or_else(|_| std::process::Output {
                        status,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    });
                let mut combined = output.stdout;
                combined.extend_from_slice(&output.stderr);
                if !status.success() {
                    bail!(
                        "build step {:?} failed ({status}): {}",
                        step.name,
                        bounded_diagnostic(&step.name, &combined)
                    );
                }
                return Ok(combined);
            }
            None if Instant::now() >= deadline => {
                terminate_group(pid);
                let _ = child.wait();
                ensure_group_gone(pid)?;
                bail!("build step {:?} exceeded its time budget", step.name);
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn terminate_group(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .status();
    std::thread::sleep(Duration::from_millis(500));
    let _ = std::process::Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .status();
}

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

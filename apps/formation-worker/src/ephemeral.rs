//! Ephemeral candidate execution: run the thing just long enough to observe
//! it, then take it apart (ADR-019).
//!
//! A Formation is not done until the candidate is observed satisfying the
//! Contract. For a process lane that means a real process on a real port —
//! so a local attempt launches the workspace it just built, performs the
//! HTTP observations the Contract names, and destroys the process group
//! before returning.
//!
//! This is NOT a Run. No lease, no durable state, no stable endpoint, no
//! record pipeline: the candidate exists only to be measured, inside the
//! attempt, and nothing it did outlives the attempt.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ato_formation::intent::ProgramIntentV1;
use ato_formation::verify::RuntimeHttpObservation;

/// How long a candidate gets to come up before the attempt is failed.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-request ceiling once it is up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Grace between SIGTERM and SIGKILL on the candidate's process group.
const TERM_GRACE: Duration = Duration::from_millis(800);
/// Bodies larger than this are not hashed into evidence.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// An HTTP observation the Contract needs, resolved to a concrete target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredObservation {
    /// The Contract's port id (`app.http`), kept so the verdict can be
    /// attached to the requirement that asked.
    pub port_id: String,
    /// The loopback port the candidate must answer on.
    pub port: u16,
    pub path: String,
}

/// Launch the candidate, observe every required path, and tear it down.
///
/// Returns one [RuntimeHttpObservation] per required observation. A candidate that
/// never comes up, or dies mid-observation, is an error — the caller records
/// the attempt as failed at `candidate-execution` rather than guessing
/// verdicts for observations that never happened.
pub fn observe_process_candidate(
    workspace_root: &Path,
    workspace_guest_root: &str,
    intent: &ProgramIntentV1,
    required: &[RequiredObservation],
) -> Result<Vec<RuntimeHttpObservation>> {
    if required.is_empty() {
        return Ok(Vec::new());
    }
    let (program, arguments) = intent
        .launch_argv
        .split_first()
        .context("the intent declares no launch argv")?;
    let cwd = workspace_root.join(&intent.cwd_relative);

    // Guest-absolute paths are rewritten onto the workspace that was actually
    // built: inside the build sandbox the workspace was `/app`, here it is a
    // real directory. A candidate that embeds the guest root — PYTHONPATH does
    // — is pointed at the tree it will actually run against.
    let rewrite =
        |value: &str| -> String { rewrite_guest_path(value, workspace_guest_root, workspace_root) };
    let argv: Vec<String> = std::iter::once(rewrite(program))
        .chain(arguments.iter().map(|arg| rewrite(arg)))
        .collect();

    let mut env: BTreeMap<String, String> = intent
        .public_env
        .iter()
        .map(|(key, value)| (key.clone(), rewrite(value)))
        .collect();

    // The runner convention: a workload learns its port from the environment.
    // An authored env.PORT wins — the author stated it.
    let ports: BTreeMap<String, u16> = intent.exported_ports.iter().cloned().collect();
    for observation in required {
        if let Some(port) = ports.get(&observation.port_id) {
            env.entry("PORT".to_owned())
                .or_insert_with(|| port.to_string());
        }
    }

    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&cwd)
        .envs(&env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // Piped so a crash reports something; drained after teardown.
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Its own process group, so teardown reaches everything it spawned.
        command.process_group(0);
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "cannot launch the candidate ({}): is the toolchain on this host?",
            argv[0]
        )
    })?;
    let launched = child.id();

    // Everything from here must end in teardown, so the body is a closure the
    // caller runs before cleaning up — a bail cannot strand the process.
    let outcome = run_observations(&mut child, required);
    teardown(&mut child);
    let observed = outcome.with_context(|| {
        let stderr = child
            .stderr
            .take()
            .map(|mut pipe| {
                let mut buf = Vec::new();
                let _ = pipe.read_to_end(&mut buf);
                String::from_utf8_lossy(&buf)
                    .trim()
                    .chars()
                    .take(400)
                    .collect::<String>()
            })
            .unwrap_or_default();
        if stderr.is_empty() {
            format!("candidate pid {launched} did not become observable")
        } else {
            format!("candidate pid {launched} was not observable: {stderr}")
        }
    })?;

    Ok(observed)
}

/// Wait for every required port, then GET every required path.
fn run_observations(
    child: &mut Child,
    required: &[RequiredObservation],
) -> Result<Vec<RuntimeHttpObservation>> {
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    let ports: std::collections::BTreeSet<u16> = required.iter().map(|o| o.port).collect();
    for port in &ports {
        wait_for_port(child, *port, deadline)?;
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        // A candidate has no business redirecting its Contract observation
        // somewhere else; what it answers directly is what is measured.
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    let mut observed = Vec::with_capacity(required.len());
    for observation in required {
        let url = format!("http://127.0.0.1:{}{}", observation.port, observation.path);
        let response = client
            .get(&url)
            .send()
            .with_context(|| format!("GET {} failed", observation.path))?;
        let status = response.status().as_u16();
        let body = response.bytes().context("cannot read the response body")?;
        if body.len() > MAX_BODY_BYTES {
            bail!(
                "GET {} returned more than {MAX_BODY_BYTES} bytes",
                observation.path
            );
        }
        observed.push(RuntimeHttpObservation::from_response(
            observation.port_id.clone(),
            "GET",
            observation.path.clone(),
            status,
            &body,
        ));
    }
    Ok(observed)
}

/// Poll the loopback port until the candidate accepts a connection.
fn wait_for_port(child: &mut Child, port: u16, deadline: Instant) -> Result<()> {
    loop {
        match child.try_wait()? {
            Some(status) => bail!("the candidate exited ({status}) before it was observable"),
            None => {}
        }
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "the candidate did not answer on 127.0.0.1:{port} within {}s",
                LAUNCH_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The process group is gone before this returns. A candidate that keeps
/// running after verification would be a Run the Formation never owned.
fn teardown(child: &mut Child) {
    let pid = child.id();
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", "--", &format!("-{pid}")])
            .status();
        let deadline = Instant::now() + TERM_GRACE;
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .status();
    }
    let _ = child.wait();
}

/// Point one guest-absolute value at the real workspace.
///
/// Only the workspace root is rewritten: anything else absolute — a toolchain
/// path like `/opt/ato/toolchains/...` — means the same thing on this host.
fn rewrite_guest_path(value: &str, guest_root: &str, workspace_root: &Path) -> String {
    let guest_root = guest_root.trim_end_matches('/');
    if value == guest_root {
        return workspace_root.to_string_lossy().into_owned();
    }
    if let Some(rest) = value.strip_prefix(&format!("{guest_root}/")) {
        return workspace_root.join(rest).to_string_lossy().into_owned();
    }
    value.to_owned()
}

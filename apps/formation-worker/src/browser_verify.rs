//! Running the browser verifier helper against a realized candidate.
//!
//! The verifier is a separate process (a Node program driving a local browser
//! and a judge model). The boundary is one JSON request on stdin and one JSON
//! result on stdout:
//!
//! ```text
//! BrowserVerificationRequest --stdin--> helper --stdout--> BrowserVerificationResult
//! ```
//!
//! Whatever the helper does, this side decides what it is worth: the result
//! is validated against the Contract it was asked about, the overall verdict
//! is recomputed, and every way the helper can fail — missing, crashed, slow,
//! out of contract — becomes an `inconclusive` receipt. Nothing here turns an
//! absent answer into a pass.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ato_formation::browser::{
    BROWSER_VERIFIER_PROTOCOL, BrowserBudget, BrowserContractV0, BrowserTarget,
    BrowserVerificationReceipt, BrowserVerificationRequest, BrowserVerificationResult,
};

use crate::browser_sandbox::{
    BrowserVerifierSandboxSpec, GUEST_SCRATCH, SECRETS_FD, contained_evidence, uncontained_evidence,
};

/// Non-secret settings the helper may see: model names and, for an
/// uncontained run, which browser to start.
const FORWARDED_SETTINGS: &[&str] = &["ATO_JEV_MODEL", "ATO_BROWSER_AGENT_MODEL"];

/// The helper's model keys. Never placed in any process environment: they
/// are written to a pipe the helper inherits as file descriptor 3, so neither
/// the helper's `/proc/<pid>/environ` nor anything it starts carries them.
const SECRET_ENV: &[&str] = &["JEV_API_KEY", "DEEPSEEK_API_KEY"];

/// Past the verification's own wall clock, how long the helper gets to shut
/// its browser down and answer before it is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(15);
/// The largest answer read from the helper.
const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;
/// Where verification scratch directories live, one per verification.
const SCRATCH_PARENT: &str = "ato-browser-verify";

/// How to start the verifier helper.
#[derive(Debug, Clone)]
pub enum BrowserVerifierCommand {
    /// The helper inside the verifier sandbox (`browser_sandbox`). The only
    /// form a Runtime Network worker uses or advertises.
    Contained(BrowserVerifierSandboxSpec),
    /// A process on the host, as it is: the host filesystem is visible to it
    /// and to its browser. Development and tests only — never advertised as
    /// a capability, and recorded as `containment: none` in every receipt.
    Uncontained {
        argv: Vec<String>,
        cwd: Option<PathBuf>,
    },
}

impl BrowserVerifierCommand {
    /// The helper shipped in `apps/formation-browser-verifier`, run on the
    /// host with the given Node binary. Development only.
    pub fn uncontained_node_helper(node: impl Into<String>, dir: PathBuf) -> Self {
        Self::Uncontained {
            argv: vec![
                node.into(),
                "--import".to_owned(),
                "tsx".to_owned(),
                "src/main.ts".to_owned(),
            ],
            cwd: Some(dir),
        }
    }

    pub fn is_contained(&self) -> bool {
        matches!(self, Self::Contained(_))
    }

    /// Can this verifier run here? A contained one only when its sandbox
    /// starts; an uncontained one was chosen explicitly.
    pub fn usable(&self) -> bool {
        match self {
            Self::Contained(spec) => spec.usable(),
            Self::Uncontained { .. } => true,
        }
    }

    /// What a receipt calls this verifier. No host path.
    fn describe(&self) -> &'static str {
        match self {
            Self::Contained(_) => "contained-node-helper",
            Self::Uncontained { .. } => "uncontained-helper",
        }
    }
}

/// Everything a browser verification needs besides the target.
#[derive(Debug, Clone)]
pub struct BrowserVerification {
    pub contract: BrowserContractV0,
    /// `None` when no verifier is installed: the verification is recorded as
    /// unavailable rather than skipped.
    pub verifier: Option<BrowserVerifierCommand>,
    pub budget: BrowserBudget,
}

/// Verify `target` against the Contract. Always returns a receipt.
pub fn verify_in_browser(
    verification: &BrowserVerification,
    target: BrowserTarget,
) -> BrowserVerificationReceipt {
    let contract = &verification.contract;
    let Some(command) = &verification.verifier else {
        return BrowserVerificationReceipt::unavailable(
            contract,
            target,
            "none",
            "browser_verifier_unavailable: no browser verifier is configured on this Runtime",
        );
    };
    let evidence = match command {
        BrowserVerifierCommand::Contained(_) => contained_evidence(),
        BrowserVerifierCommand::Uncontained { .. } => uncontained_evidence(),
    };
    let unavailable = |target: BrowserTarget, reason: String| {
        let mut receipt = BrowserVerificationReceipt::unavailable(
            contract,
            target,
            command.describe(),
            &scrub(&reason),
        );
        receipt.containment = Some(evidence.clone());
        receipt
    };
    if !command.usable() {
        return unavailable(
            target,
            "browser_verifier_containment_unavailable: the verifier sandbox cannot run on this \
             Runtime; refusing to verify outside it"
                .to_owned(),
        );
    }
    sweep_orphaned_scratch();
    let scratch = match new_scratch() {
        Ok(scratch) => scratch,
        Err(error) => {
            return unavailable(
                target,
                format!("browser_verifier_unavailable: no scratch directory ({error})"),
            );
        }
    };
    let scratch_seen_by_helper = match command {
        BrowserVerifierCommand::Contained(_) => GUEST_SCRATCH.to_owned(),
        BrowserVerifierCommand::Uncontained { .. } => scratch.path().to_string_lossy().into_owned(),
    };
    let request = BrowserVerificationRequest {
        protocol: BROWSER_VERIFIER_PROTOCOL.to_owned(),
        contract: contract.clone(),
        url: target.endpoint.clone(),
        budget: verification.budget,
        scratch_dir: scratch_seen_by_helper,
    };
    let timeout = Duration::from_millis(verification.budget.wall_clock_ms) + SHUTDOWN_GRACE;
    let output = run_helper(command, &request, scratch.path(), timeout);
    // An uncontained browser launched detached is not in the helper's process
    // group; its profile path in the scratch directory is how it is found. A
    // contained one went down with the sandbox's PID namespace already.
    stop_processes_using(scratch.path());
    drop(scratch);
    let output = match output {
        Ok(output) => output,
        Err(reason) => return unavailable(target, reason),
    };
    let result: BrowserVerificationResult = match serde_json::from_slice(&output) {
        Ok(result) => result,
        Err(error) => {
            return unavailable(
                target,
                format!(
                    "browser_verifier_result_invalid: the answer is not a verification result ({error})"
                ),
            );
        }
    };
    let mut receipt = match BrowserVerificationReceipt::new(contract, target.clone(), result) {
        Ok(receipt) => receipt,
        Err(error) => {
            return unavailable(target, format!("browser_verifier_result_invalid: {error}"));
        }
    };
    if let Some(name) = leaked_secret(&receipt) {
        return unavailable(
            target,
            format!("browser_verifier_result_invalid: the answer contained the value of {name}"),
        );
    }
    receipt.containment = Some(evidence);
    receipt
}

/// A fresh scratch directory, marked with this process's id so a later
/// verification can tell an orphan (its worker died) from a live one.
fn new_scratch() -> std::io::Result<tempfile::TempDir> {
    let parent = std::env::temp_dir().join(SCRATCH_PARENT);
    std::fs::create_dir_all(&parent)?;
    let scratch = tempfile::Builder::new().prefix("v-").tempdir_in(&parent)?;
    std::fs::write(
        scratch.path().join(".owner"),
        std::process::id().to_string(),
    )?;
    for sub in ["home", "tmp"] {
        std::fs::create_dir_all(scratch.path().join(sub))?;
    }
    Ok(scratch)
}

/// Remove scratch left behind by a worker that was killed mid-verification.
/// Its sandbox died with it (`--die-with-parent`); its directory did not.
fn sweep_orphaned_scratch() {
    let parent = std::env::temp_dir().join(SCRATCH_PARENT);
    let Ok(entries) = std::fs::read_dir(&parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let owner = std::fs::read_to_string(path.join(".owner"))
            .ok()
            .and_then(|pid| pid.trim().parse::<i32>().ok());
        let orphaned = match owner {
            Some(pid) => !process_alive(pid),
            // Unmarked: either being created right now, or left by a worker
            // killed before it marked it. Only the second is ever old.
            None => entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(600)),
        };
        if orphaned {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: i32) -> bool {
    true
}

/// The helper's model keys, as the JSON it reads from its secrets pipe.
fn secrets_payload() -> Vec<u8> {
    let secrets: serde_json::Map<String, serde_json::Value> = SECRET_ENV
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).to_owned(), serde_json::Value::String(value)))
        })
        .collect();
    serde_json::to_vec(&secrets).expect("a string map serializes")
}

/// A pipe whose read end carries `payload` and then end-of-file. Both ends
/// are created close-on-exec atomically, so no other process this worker
/// starts concurrently inherits either of them.
#[cfg(unix)]
fn secrets_pipe(payload: &[u8]) -> std::io::Result<std::os::fd::OwnedFd> {
    let (read, mut write) = std::io::pipe()?;
    // A few hundred bytes: far below any pipe buffer, so this never blocks.
    write.write_all(payload)?;
    drop(write);
    Ok(read.into())
}

/// Run the helper once: request in, answer out, bounded in time and size.
fn run_helper(
    command: &BrowserVerifierCommand,
    request: &BrowserVerificationRequest,
    scratch: &Path,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let settings: Vec<(String, String)> = FORWARDED_SETTINGS
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).to_owned(), value))
        })
        .collect();
    let mut process = match command {
        BrowserVerifierCommand::Contained(spec) => {
            let argv = spec.argv(scratch, &settings).map_err(|error| {
                format!("browser_verifier_unavailable: cannot build the verifier sandbox ({error})")
            })?;
            let mut process = Command::new(&argv[0]);
            process
                .args(&argv[1..])
                .env_clear()
                .env("PATH", "/usr/local/bin:/usr/bin:/bin");
            process
        }
        BrowserVerifierCommand::Uncontained { argv, cwd } => {
            let (program, arguments) = argv.split_first().ok_or_else(|| {
                "browser_verifier_unavailable: the verifier command is empty".to_owned()
            })?;
            let mut process = Command::new(program);
            // Only what a helper needs; nothing ambient crosses, and no key.
            process.args(arguments).env_clear();
            for name in ["PATH", "LANG", "LC_ALL", "ATO_BROWSER_CHROME_PATH"] {
                if let Some(value) = std::env::var_os(name) {
                    process.env(name, value);
                }
            }
            for (name, value) in &settings {
                process.env(name, value);
            }
            process
                .env("HOME", scratch.join("home"))
                .env("TMPDIR", scratch.join("tmp"))
                .env("ATO_VERIFIER_SECRETS_FD", SECRETS_FD.to_string());
            if let Some(cwd) = cwd {
                process.current_dir(cwd);
            }
            process
        }
    };
    process
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    let secrets = secrets_pipe(&secrets_payload()).map_err(|error| {
        format!("browser_verifier_unavailable: cannot hand the verifier its keys ({error})")
    })?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::process::CommandExt as _;
        // Its own process group: what it starts goes down with it.
        process.process_group(0);
        let fd = secrets.as_raw_fd();
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            process.pre_exec(move || {
                if fd == SECRETS_FD {
                    libc::fcntl(fd, libc::F_SETFD, 0);
                } else if libc::dup2(fd, SECRETS_FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let spawned = process.spawn();
    #[cfg(unix)]
    drop(secrets);
    let mut child = spawned.map_err(|error| {
        format!("browser_verifier_unavailable: cannot start the verifier ({error})")
    })?;
    let group = child.id();

    let body = serde_json::to_vec(request).expect("a verification request always serializes");
    if let Some(mut stdin) = child.stdin.take() {
        // A helper that exits without reading is reported by its status.
        let _ = stdin.write_all(&body);
    }
    let stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut kept = Vec::new();
        let _ = stdout
            .take(MAX_RESULT_BYTES as u64 + 1)
            .read_to_end(&mut kept);
        kept
    });
    let stderr = child.stderr.take().expect("piped");
    let diagnostics = std::thread::spawn(move || {
        // Drained so the helper never blocks; only a short tail is kept, and
        // only for the failure message.
        let mut tail = Vec::new();
        let mut buffer = [0_u8; 8192];
        let mut stderr = stderr;
        while let Ok(read) = stderr.read(&mut buffer) {
            if read == 0 {
                break;
            }
            tail.extend_from_slice(&buffer[..read]);
            if tail.len() > 2048 {
                tail.drain(..tail.len() - 2048);
            }
        }
        String::from_utf8_lossy(&tail).trim().to_owned()
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => {
                kill_group(group);
                let _ = child.wait();
                return Err(format!(
                    "browser_verifier_unavailable: cannot wait for the verifier ({error})"
                ));
            }
        }
    };
    // Whatever happened, nothing the helper started outlives this call. For
    // a contained helper, killing bubblewrap tears down its PID namespace —
    // the helper, the guard and the browser's own namespace with it.
    kill_group(group);
    let Some(status) = status else {
        let _ = child.wait();
        return Err(format!(
            "browser_verifier_timeout: no answer within {}s; the verifier and its browser were stopped",
            timeout.as_secs()
        ));
    };
    let output = reader.join().unwrap_or_default();
    let tail = scrub(&diagnostics.join().unwrap_or_default());
    if output.len() > MAX_RESULT_BYTES {
        return Err(
            "browser_verifier_result_invalid: the answer exceeds its size bound".to_owned(),
        );
    }
    if output.is_empty() {
        return Err(format!(
            "browser_verifier_failed: the verifier exited ({status}) without an answer: {tail}"
        ));
    }
    Ok(output)
}

/// `text` with every model key's value replaced by its name.
fn scrub(text: &str) -> String {
    let mut text = text.to_owned();
    for name in SECRET_ENV {
        if let Ok(value) = std::env::var(name)
            && value.len() >= 8
        {
            text = text.replace(&value, &format!("<{name}>"));
        }
    }
    text
}

#[cfg(unix)]
fn kill_group(group: u32) {
    // SAFETY: signalling a process group this call created; a group that is
    // already gone makes `kill` fail harmlessly.
    unsafe {
        libc::kill(-(group as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_group: u32) {}

/// Stop every process whose command line names `path`.
fn stop_processes_using(path: &std::path::Path) {
    #[cfg(unix)]
    {
        let _ = Command::new("pkill")
            .args(["-KILL", "-f"])
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// The name of a secret whose value appears in the receipt, if any.
fn leaked_secret(receipt: &BrowserVerificationReceipt) -> Option<&'static str> {
    let text = serde_json::to_string(receipt).ok()?;
    SECRET_ENV.iter().copied().find(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| value.len() >= 8)
            .is_some_and(|value| text.contains(&value))
    })
}

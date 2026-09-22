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
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ato_formation::browser::{
    BROWSER_VERIFIER_PROTOCOL, BrowserBudget, BrowserContractV0, BrowserTarget,
    BrowserVerificationReceipt, BrowserVerificationRequest, BrowserVerificationResult,
};

/// Environment the helper may see. Everything else is cleared: the helper
/// drives a browser at untrusted content and must not inherit the caller's
/// ambient credentials.
const FORWARDED_ENV: &[&str] = &[
    "PATH",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    // The judge and the browser agent's model. Read by the helper only.
    "JEV_API_KEY",
    "DEEPSEEK_API_KEY",
    // Explicit overrides for pinned versions and binaries.
    "ATO_JEV_MODEL",
    "ATO_BROWSER_AGENT_MODEL",
    "ATO_BROWSER_CHROME_PATH",
];

/// Secret-bearing variables whose values must never appear in a receipt.
const SECRET_ENV: &[&str] = &["JEV_API_KEY", "DEEPSEEK_API_KEY"];

/// Past the verification's own wall clock, how long the helper gets to shut
/// its browser down and answer before it is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(15);
/// The largest answer read from the helper.
const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;

/// How to start the verifier helper.
#[derive(Debug, Clone)]
pub struct BrowserVerifierCommand {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
}

impl BrowserVerifierCommand {
    /// The helper shipped in `apps/formation-browser-verifier`, run with the
    /// given Node binary.
    pub fn node_helper(node: impl Into<String>, dir: PathBuf) -> Self {
        Self {
            argv: vec![
                node.into(),
                "--import".to_owned(),
                "tsx".to_owned(),
                "src/main.ts".to_owned(),
            ],
            cwd: Some(dir),
        }
    }

    fn describe(&self) -> String {
        self.argv.first().cloned().unwrap_or_default()
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
    let scratch = match tempfile::Builder::new()
        .prefix("ato-browser-verify-")
        .tempdir()
    {
        Ok(scratch) => scratch,
        Err(error) => {
            return BrowserVerificationReceipt::unavailable(
                contract,
                target,
                &command.describe(),
                &format!("browser_verifier_unavailable: no scratch directory ({error})"),
            );
        }
    };
    let request = BrowserVerificationRequest {
        protocol: BROWSER_VERIFIER_PROTOCOL.to_owned(),
        contract: contract.clone(),
        url: target.endpoint.clone(),
        budget: verification.budget,
        scratch_dir: scratch.path().to_string_lossy().into_owned(),
    };
    let timeout = Duration::from_millis(verification.budget.wall_clock_ms) + SHUTDOWN_GRACE;
    let unavailable = |target: BrowserTarget, reason: String| {
        BrowserVerificationReceipt::unavailable(contract, target, &command.describe(), &reason)
    };
    let output = run_helper(command, &request, timeout);
    // A browser launched detached is not in the helper's process group; its
    // profile path in the scratch directory is how it is found.
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
    let receipt = match BrowserVerificationReceipt::new(contract, target.clone(), result) {
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
    receipt
}

/// Run the helper once: request in, answer out, bounded in time and size.
fn run_helper(
    command: &BrowserVerifierCommand,
    request: &BrowserVerificationRequest,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let (program, arguments) = command
        .argv
        .split_first()
        .ok_or_else(|| "browser_verifier_unavailable: the verifier command is empty".to_owned())?;
    let mut process = Command::new(program);
    process
        .args(arguments)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in FORWARDED_ENV {
        if let Some(value) = std::env::var_os(name) {
            process.env(name, value);
        }
    }
    if let Some(cwd) = &command.cwd {
        process.current_dir(cwd);
    }
    // The helper and its browser get a home inside the scratch directory:
    // nothing under the user's HOME is theirs to read or write.
    let home = std::path::Path::new(&request.scratch_dir).join("home");
    if std::fs::create_dir_all(&home).is_ok() {
        process.env("HOME", &home);
    }
    // Its own process group: the browser it starts goes down with it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        process.process_group(0);
    }
    let mut child = process.spawn().map_err(|error| {
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
    // Whatever happened, nothing the helper started outlives this call.
    kill_group(group);
    let Some(status) = status else {
        let _ = child.wait();
        return Err(format!(
            "browser_verifier_timeout: no answer within {}s; the verifier and its browser were stopped",
            timeout.as_secs()
        ));
    };
    let output = reader.join().unwrap_or_default();
    let tail = diagnostics.join().unwrap_or_default();
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

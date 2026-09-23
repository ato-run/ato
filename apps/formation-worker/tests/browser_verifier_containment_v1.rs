//! The browser verifier sandbox, measured from the inside and the outside.
//!
//! Stand-in helpers run in the real verifier sandbox with the real Node and
//! the real browser, and try what a compromised helper or browser would: read
//! the host (its home, a canary, the repository, the worker's key file),
//! write the helper's own code and runtimes, find the model keys in the
//! browser's environment or through `/proc`, and outlive the verification.
//!
//! Linux with bubblewrap only, and only when `ATO_TEST_BROWSER_NODE` and
//! `ATO_TEST_BROWSER_CHROME` name a Node binary and a Chrome binary; anywhere
//! else every test says it skipped.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ato_formation::browser::{
    BrowserBudget, BrowserContractV0, BrowserTarget, BrowserVerdict, BrowserVerificationReceipt,
};
use ato_formation_worker::browser_sandbox::BrowserVerifierSandboxSpec;
use ato_formation_worker::browser_verify::{
    BrowserVerification, BrowserVerifierCommand, verify_in_browser,
};
use ato_formation_worker::sandbox::containment_available;

/// These tests set the model keys in this process's environment.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const JEV_KEY: &str = "jev-containment-test-key-0123456789abcdef";
const DEEPSEEK_KEY: &str = "deepseek-containment-test-key-fedcba9876543210";

/// Answers the verifier protocol from a stand-in: `answer(ok, facts)`.
const ANSWER_JS: &str = r#"
import { readFileSync, closeSync } from "node:fs";
const request = JSON.parse(readFileSync(0, "utf8"));
const fd = Number(process.env.ATO_VERIFIER_SECRETS_FD);
const keys = JSON.parse(readFileSync(fd, "utf8") || "{}");
closeSync(fd);
function answer(ok, facts) {
  const verdict = ok ? "pass" : "fail";
  const choice = ok ? "complete" : "incomplete";
  process.stdout.write(JSON.stringify({
    protocol: "ato.browser-verifier/0",
    verdict,
    criteria: [{ id: "primary", verdict, rounds: 1, evidence_refs: ["e1"], reason: null,
      decision: { choice, confidence: 0.9, model: "jev-1.13.0",
        probabilities: { complete: 0.05, verify_more: 0.05, incomplete: 0.05, [choice]: 0.85 } } }],
    evidence: [{ id: "e1", kind: "browser_snapshot", sequence: 1, url: request.url, title: "stand-in",
      facts: facts.slice(0, 60).map((f) => String(f).slice(0, 400)), text_excerpt: "stand-in" }],
    observed_events: [{ sequence: 1, kind: "navigation", url: request.url }],
    action_trace: [{ kind: "navigate", description: "stand-in", url: request.url }],
    verifier: { verifier: "stand-in", stagehand_version: null, browser_version: null,
      agent_model: null, judge_model: "jev-1.13.0" },
    reason: null,
  }) + "\n");
}
"#;

struct Setup {
    node: String,
    chrome: PathBuf,
    /// This repository's browser verifier, for its launcher.
    launcher: PathBuf,
}

fn setup() -> Option<Setup> {
    let (Ok(node), Ok(chrome)) = (
        std::env::var("ATO_TEST_BROWSER_NODE"),
        std::env::var("ATO_TEST_BROWSER_CHROME"),
    ) else {
        eprintln!("skipping: ATO_TEST_BROWSER_NODE / ATO_TEST_BROWSER_CHROME are not set");
        return None;
    };
    if !containment_available() {
        eprintln!("skipping: bubblewrap is unavailable");
        return None;
    }
    Some(Setup {
        node,
        chrome: PathBuf::from(chrome),
        launcher: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../formation-browser-verifier/bin/chrome-contained"),
    })
}

/// A helper root holding the launcher and one stand-in program.
fn stand_in(setup: &Setup, body: &str) -> (tempfile::TempDir, BrowserVerifierCommand) {
    let root = tempfile::tempdir().expect("helper root");
    std::fs::create_dir_all(root.path().join("bin")).expect("bin");
    std::fs::copy(&setup.launcher, root.path().join("bin/chrome-contained")).expect("launcher");
    std::fs::write(
        root.path().join("standin.mjs"),
        format!("{ANSWER_JS}\n{body}\n"),
    )
    .expect("stand-in");
    let spec = BrowserVerifierSandboxSpec::resolve(
        root.path(),
        &setup.node,
        &setup.chrome,
        vec!["standin.mjs".to_owned()],
    )
    .expect("the sandbox resolves");
    (root, BrowserVerifierCommand::Contained(spec))
}

fn verify(command: BrowserVerifierCommand, wall_clock_ms: u64) -> BrowserVerificationReceipt {
    verify_in_browser(
        &BrowserVerification {
            contract: BrowserContractV0::from_prompt("Stand-in check.").unwrap(),
            verifier: Some(command),
            budget: BrowserBudget {
                max_browser_steps: 1,
                max_jev_rounds: 1,
                wall_clock_ms,
            },
        },
        BrowserTarget {
            runtime_id: "local".to_owned(),
            endpoint: "http://127.0.0.1:9/".to_owned(),
        },
    )
}

fn facts(receipt: &BrowserVerificationReceipt) -> Vec<String> {
    receipt
        .evidence
        .iter()
        .flat_map(|e| e.facts.clone())
        .collect()
}

fn with_keys<T>(run: impl FnOnce() -> T) -> T {
    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: serialized by ENV_LOCK; the values are test-only.
    unsafe {
        std::env::set_var("JEV_API_KEY", JEV_KEY);
        std::env::set_var("DEEPSEEK_API_KEY", DEEPSEEK_KEY);
    }
    let result = run();
    unsafe {
        std::env::remove_var("JEV_API_KEY");
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
    result
}

/// A canary in the real home directory, removed when dropped.
struct Canary {
    path: PathBuf,
    secret: String,
}

impl Canary {
    fn new() -> Self {
        let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
        let secret = format!(
            "ATO_BROWSER_HOST_SECRET_{}_{}",
            std::process::id(),
            rand_token()
        );
        let path = home.join(format!(
            "ato-browser-containment-canary-{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, &secret).expect("canary");
        Self { path, secret }
    }
}

impl Drop for Canary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn rand_token() -> String {
    format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

fn js_string(value: &str) -> String {
    serde_json::to_string(value).unwrap()
}

#[test]
fn the_helper_cannot_read_the_host() {
    let Some(setup) = setup() else { return };
    let canary = Canary::new();
    let home = std::env::var("HOME").unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let targets = [
        canary.path.display().to_string(),
        home.clone(),
        format!("{home}/.config/ato/formation-browser.env"),
        format!("{home}/.ssh"),
        repo.join("Cargo.toml").display().to_string(),
        "/etc/passwd".to_owned(),
        "/etc/shadow".to_owned(),
        "/root".to_owned(),
    ];
    let body = format!(
        r#"
import {{ readFileSync, readdirSync }} from "node:fs";
const out = [];
let leaked = false;
for (const p of {targets}) {{
  try {{
    const st = readdirSync(p, {{ withFileTypes: false }});
    out.push(p + " LISTED " + st.length); leaked = true;
  }} catch (e1) {{
    try {{ readFileSync(p, "utf8"); out.push(p + " READ"); leaked = true; }}
    catch (e2) {{ out.push(p + " " + e2.code); }}
  }}
}}
answer(!leaked, out);
"#,
        targets = serde_json::to_string(&targets).unwrap()
    );
    let (_root, command) = stand_in(&setup, &body);
    let receipt = with_keys(|| verify(command, 15_000));
    let text = serde_json::to_string(&receipt).unwrap();
    assert!(!text.contains(&canary.secret));
    assert_eq!(
        receipt.overall,
        BrowserVerdict::Pass,
        "{:?}",
        facts(&receipt)
    );
    for fact in facts(&receipt) {
        assert!(fact.ends_with("ENOENT"), "{fact}");
    }
    let containment = receipt.containment.expect("containment evidence");
    assert_eq!(containment.containment, "bwrap");
    assert_eq!(containment.filesystem, "allowlisted");
}

#[test]
fn the_helper_cannot_write_its_code_or_runtimes() {
    let Some(setup) = setup() else { return };
    let body = r#"
import { writeFileSync } from "node:fs";
const out = [];
let ok = true;
for (const [p, allowed] of [
  ["/verifier/standin.mjs", false], ["/verifier/injected.js", false],
  ["/runtime/node/bin/injected", false], ["/runtime/chrome/injected", false],
  ["/usr/injected", false], ["/scratch/ok", true], ["/tmp/ok", true],
]) {
  let wrote = true;
  try { writeFileSync(p, "x"); } catch (e) { wrote = false; out.push(p + " " + e.code); continue; }
  out.push(p + " WROTE");
  if (wrote !== allowed) ok = false;
}
answer(ok, out);
"#;
    let (_root, command) = stand_in(&setup, body);
    let receipt = with_keys(|| verify(command, 15_000));
    assert_eq!(
        receipt.overall,
        BrowserVerdict::Pass,
        "{:?}",
        facts(&receipt)
    );
}

#[test]
fn the_browser_has_no_key_no_view_of_the_helper_and_no_host() {
    let Some(setup) = setup() else { return };
    let canary = Canary::new();
    let marker = format!("--ato-containment-marker={}", rand_token());
    // Inside the sandbox: the launcher starts a shell in the browser's place
    // (same namespace, same environment policy) and reports what it sees;
    // then it starts the real browser, which reads the canary as a file URL
    // and stays up long enough to be measured from the host.
    let body = format!(
        r#"
import {{ spawnSync, spawn }} from "node:child_process";
const out = [];
process.env.ATO_TEST_AMBIENT = "must-not-cross";
const probe = spawnSync("/verifier/bin/chrome-contained",
  ["-c", "env; echo ===; for d in /proc/[0-9]*; do tr '\\0' ' ' < $d/cmdline; echo; done; echo ===; cat {canary} 2>&1"],
  {{ env: {{ ...process.env, ATO_BROWSER_CHROME_REAL: "/bin/sh" }}, encoding: "utf8" }});
const [env, procs, cat] = (probe.stdout || "").split("===\n");
out.push("env-has-keys " + /JEV_API_KEY|DEEPSEEK_API_KEY/.test(env));
out.push("env-has-ambient " + env.includes("ATO_TEST_AMBIENT"));
out.push("sees-helper " + procs.includes("standin.mjs"));
out.push("canary " + (cat || "").trim());
const dump = spawnSync("/verifier/bin/chrome-contained",
  ["--headless=new", "--no-first-run", "--disable-gpu", "--user-data-dir=/scratch/dump",
   "--dump-dom", "file://{canary}"], {{ encoding: "utf8", timeout: 60000 }});
out.push("dump-has-canary " + (dump.stdout || "").includes({secret}));
const chrome = spawn("/verifier/bin/chrome-contained",
  ["--headless=new", "--no-first-run", "--disable-gpu", "--user-data-dir=/scratch/profile",
   "--remote-debugging-port=0", {marker}, "about:blank"], {{ stdio: "ignore" }});
await new Promise((r) => setTimeout(r, 8000));
chrome.kill("SIGKILL");
const ok = out.every((f) => /false$|No such file or directory$/.test(f));
answer(ok, out);
"#,
        canary = canary.path.display(),
        secret = js_string(&canary.secret),
        marker = js_string(&marker),
    );
    let (_root, command) = stand_in(&setup, &body);
    // From the host, while the browser is up: every process carrying the
    // marker is read through /proc for the keys.
    let watcher_marker = marker.clone();
    let watcher = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut seen = 0;
        let mut leaked = Vec::new();
        while Instant::now() < deadline {
            let pids = Command::new("pgrep")
                .args(["-f", &watcher_marker])
                .output()
                .unwrap();
            for pid in String::from_utf8_lossy(&pids.stdout).split_whitespace() {
                if let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) {
                    seen += 1;
                    let environ = String::from_utf8_lossy(&environ);
                    for needle in [JEV_KEY, DEEPSEEK_KEY, "JEV_API_KEY", "DEEPSEEK_API_KEY"] {
                        if environ.contains(needle) {
                            leaked.push(format!("{pid}: {needle}"));
                        }
                    }
                }
            }
            if seen > 5 {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        (seen, leaked)
    });
    let receipt = with_keys(|| verify(command, 90_000));
    let (seen, leaked) = watcher.join().unwrap();
    assert!(seen > 0, "the browser was never observed from the host");
    assert!(leaked.is_empty(), "{leaked:?}");
    assert_eq!(
        receipt.overall,
        BrowserVerdict::Pass,
        "{:?}",
        facts(&receipt)
    );
    let text = serde_json::to_string(&receipt).unwrap();
    for needle in [JEV_KEY, DEEPSEEK_KEY, canary.secret.as_str()] {
        assert!(!text.contains(needle));
    }
}

/// Every way a verification can end leaves no helper, no browser and no
/// scratch behind.
#[test]
fn timeout_crash_and_kill_leave_nothing_behind() {
    let Some(setup) = setup() else { return };
    for (case, tail, wall_clock_ms) in [
        ("timeout", "await new Promise(() => {});", 1_000),
        ("crash", "process.exit(1);", 30_000),
        ("sigkill", "process.kill(process.pid, 'SIGKILL');", 30_000),
    ] {
        let marker = format!("--ato-cleanup-marker={case}-{}", rand_token());
        let body = format!(
            r#"
import {{ spawn }} from "node:child_process";
spawn("/verifier/bin/chrome-contained",
  ["--headless=new", "--no-first-run", "--disable-gpu", "--user-data-dir=/scratch/profile",
   "--remote-debugging-port=0", {marker}, "about:blank"], {{ stdio: "ignore", detached: true }});
spawn("/runtime/node/bin/node", ["-e", "setTimeout(() => {{}}, 600000)", {marker}],
  {{ stdio: "ignore", detached: true }});
await new Promise((r) => setTimeout(r, 3000));
{tail}
"#,
            marker = js_string(&marker),
        );
        let (_root, command) = stand_in(&setup, &body);
        let before = scratch_entries();
        let receipt = with_keys(|| verify(command, wall_clock_ms));
        assert_eq!(receipt.overall, BrowserVerdict::Inconclusive, "{case}");
        std::thread::sleep(Duration::from_millis(500));
        let survivors = Command::new("pgrep")
            .args(["-f", &marker])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&survivors.stdout).trim().is_empty(),
            "{case}: processes outlived the verification"
        );
        let after = scratch_entries();
        assert!(
            after.iter().all(|entry| before.contains(entry)),
            "{case}: scratch left behind: {after:?}"
        );
    }
}

fn scratch_entries() -> Vec<PathBuf> {
    std::fs::read_dir(std::env::temp_dir().join("ato-browser-verify"))
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

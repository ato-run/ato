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
use std::time::Duration;

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
            .join("../formation-browser-verifier/bin/chrome-contained.cjs"),
    })
}

/// A helper root holding the launcher and one stand-in program.
fn stand_in(setup: &Setup, body: &str) -> (tempfile::TempDir, BrowserVerifierCommand) {
    let root = tempfile::tempdir().expect("helper root");
    std::fs::create_dir_all(root.path().join("bin")).expect("bin");
    std::fs::copy(
        &setup.launcher,
        root.path().join("bin/chrome-contained.cjs"),
    )
    .expect("launcher");
    std::fs::write(
        root.path().join("standin.mjs"),
        format!("{ANSWER_JS}\n{body}\n"),
    )
    .expect("stand-in");
    let spec = BrowserVerifierSandboxSpec::resolve(
        root.path(),
        &setup.node,
        &setup.chrome,
        // Node 20 has WebSocket behind this flag; Node 22 accepts it as is.
        vec![
            "--experimental-websocket".to_owned(),
            "standin.mjs".to_owned(),
        ],
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
import {{ readdirSync }} from "node:fs";
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
        "{:?} {:?}",
        receipt.reason,
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
        "{:?} {:?}",
        receipt.reason,
        facts(&receipt)
    );
}

#[test]
fn the_browser_has_no_key_no_view_of_the_helper_and_no_host() {
    let Some(setup) = setup() else { return };
    let canary = Canary::new();
    let marker = format!("--ato-containment-marker={}", rand_token());
    // Inside the helper's sandbox, with an ambient variable set, the stand-in
    // asks for the browser as Stagehand does; the browser is asked over CDP to
    // open the host canary as a file URL, and stays up long enough to be
    // measured from the host: its environment, and its PID namespace against
    // the helper's.
    let body = format!(
        r#"
import {{ spawn }} from "node:child_process";
const out = [];
process.env.ATO_TEST_AMBIENT = "must-not-cross";
const started = Date.now();
const chrome = spawn("/verifier/bin/chrome-contained.cjs",
  ["--headless=new", "--no-first-run", "--disable-gpu", "--user-data-dir=/scratch/profile",
   "--remote-debugging-port=0", {marker}, "about:blank"], {{ stdio: ["ignore", "ignore", "pipe"] }});
let stderr = "";
chrome.stderr.on("data", (d) => {{ stderr = (stderr + d).slice(-300); }});
// The running browser is asked, over CDP, to open the host canary as a file
// URL — and, as a control, a page it can render.
let port = null;
for (let i = 0; i < 150 && port === null; i++) {{
  try {{ port = Number(readFileSync("/scratch/profile/DevToolsActivePort", "utf8").split("\n")[0]) || null; }}
  catch {{ await new Promise((r) => setTimeout(r, 100)); }}
}}
async function pageHtml(url) {{
  const target = await (await fetch(`http://127.0.0.1:${{port}}/json/new?${{encodeURIComponent(url)}}`, {{ method: "PUT" }})).json();
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((r) => (ws.onopen = r));
  await new Promise((r) => setTimeout(r, 2000));
  const reply = await new Promise((r) => {{
    ws.onmessage = (m) => {{ const d = JSON.parse(m.data); if (d.id === 1) r(d); }};
    ws.send(JSON.stringify({{ id: 1, method: "Runtime.evaluate",
      params: {{ expression: "location.href + ' ' + document.documentElement.outerHTML", returnByValue: true }} }}));
  }});
  ws.close();
  return String(reply.result?.result?.value ?? JSON.stringify(reply.error ?? reply.result));
}}
let control = "", canaryPage = "";
try {{
  control = await pageHtml("data:text/html,<p>containment-control</p>");
  canaryPage = await pageHtml("file://{canary}");
}} catch (e) {{ control = "cdp-error " + e; }}
const controlOk = control.includes("containment-control");
out.push("file-url-has-canary " + canaryPage.includes({secret}));
await new Promise((r) => setTimeout(r, Math.max(0, 8000 - (Date.now() - started))));
const alive = chrome.exitCode === null && chrome.signalCode === null;
chrome.kill("SIGKILL");
const ok = out.every((f) => /false$|No such file or directory$/.test(f)) && alive && controlOk;
out.push("control-rendered " + controlOk + (controlOk ? "" : " " + control.slice(0, 200)));
out.push("file-url-page " + JSON.stringify(canaryPage.replace(/\s+/g, " ").slice(0, 200)));
out.push("browser-alive " + alive + " " + (alive ? "" : stderr));
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
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watching = done.clone();
    let pid_ns = |pid: &str| std::fs::read_link(format!("/proc/{pid}/ns/pid")).ok();
    let watcher = std::thread::spawn(move || {
        let mut seen = 0;
        let mut leaked = Vec::new();
        let mut shared_namespace = Vec::new();
        while !watching.load(std::sync::atomic::Ordering::Relaxed) {
            let helpers: Vec<_> = processes_matching("standin.mjs")
                .iter()
                .filter_map(|pid| pid_ns(pid))
                .collect();
            for pid in processes_matching(&watcher_marker) {
                // The launcher client carries the marker in its argv too;
                // only the real browser runs from /runtime/chrome.
                let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                if !String::from_utf8_lossy(&cmdline).starts_with("/runtime/chrome/") {
                    continue;
                }
                if let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) {
                    seen += 1;
                    let environ = String::from_utf8_lossy(&environ);
                    for needle in [
                        JEV_KEY,
                        DEEPSEEK_KEY,
                        "JEV_API_KEY",
                        "DEEPSEEK_API_KEY",
                        "ATO_TEST_AMBIENT",
                        "ATO_VERIFIER_SECRETS_FD",
                    ] {
                        if environ.contains(needle) {
                            leaked.push(format!("{pid}: {needle}"));
                        }
                    }
                }
                if let Some(ns) = pid_ns(&pid)
                    && helpers.contains(&ns)
                {
                    shared_namespace.push(pid.clone());
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        (seen, leaked, shared_namespace)
    });
    let receipt = with_keys(|| verify(command, 150_000));
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let (seen, leaked, shared_namespace) = watcher.join().unwrap();
    eprintln!(
        "browser containment facts: {:?}; host observed {seen} browser processes",
        facts(&receipt)
    );
    assert!(
        seen > 0,
        "the browser was never observed from the host: {:?} {:?}",
        receipt.reason,
        facts(&receipt)
    );
    assert!(leaked.is_empty(), "{leaked:?}");
    assert!(
        shared_namespace.is_empty(),
        "browser processes in the helper's PID namespace: {shared_namespace:?}"
    );
    assert_eq!(
        receipt.overall,
        BrowserVerdict::Pass,
        "{:?} {:?}",
        receipt.reason,
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
spawn("/verifier/bin/chrome-contained.cjs",
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
        // Snapshots and verification under the same lock the other tests
        // verify under: a scratch directory another test is using at that
        // moment is not this verification's leftover.
        let (before, receipt, survivors, after) = with_keys(|| {
            let before = scratch_entries();
            let receipt = verify(command, wall_clock_ms);
            std::thread::sleep(Duration::from_millis(500));
            (
                before,
                receipt,
                processes_matching(&marker),
                scratch_entries(),
            )
        });
        assert_eq!(receipt.overall, BrowserVerdict::Inconclusive, "{case}");
        assert!(
            survivors.is_empty(),
            "{case}: processes outlived the verification: {survivors:?}"
        );
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

/// Pids whose command line contains `pattern`. A pgrep that fails (rather
/// than finding nothing) fails the test: an empty answer must mean "none".
fn processes_matching(pattern: &str) -> Vec<String> {
    let output = Command::new("pgrep")
        .args(["-f", "--", pattern])
        .output()
        .expect("pgrep");
    assert!(
        matches!(output.status.code(), Some(0 | 1)),
        "pgrep failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

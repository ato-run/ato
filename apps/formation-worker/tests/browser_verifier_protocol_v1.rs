//! The browser verifier boundary, against stand-in helpers.
//!
//! The real helper drives a browser and a judge model; these stand-ins answer
//! the same protocol in every way a helper can — correctly, wrongly, late, not
//! at all — so what this side does with each answer is pinned down without a
//! browser. A helper's word is only accepted when it is well-formed, about the
//! Contract that was asked, and supported by its own judgment; everything else
//! is `inconclusive`, never a pass.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ato_formation::browser::{
    BrowserBudget, BrowserContractV0, BrowserTarget, BrowserVerdict, BrowserVerificationReceipt,
};
use ato_formation_worker::browser_verify::{
    BrowserVerification, BrowserVerifierCommand, verify_in_browser,
};

const PROMPT: &str = "Create a note named 'formation-check'. Reload the page and verify that the note is still present.";

/// A stand-in helper: reads the request, then runs `body` (Python) with
/// `request` bound, printing whatever it prints.
fn helper(dir: &tempfile::TempDir, body: &str) -> BrowserVerifierCommand {
    let path = dir.path().join("helper.py");
    std::fs::write(
        &path,
        format!("import json, os, sys, time\nrequest = json.load(sys.stdin)\n{body}\n",),
    )
    .expect("helper");
    BrowserVerifierCommand {
        argv: vec!["python3".to_owned(), path.to_string_lossy().into_owned()],
        cwd: None,
    }
}

/// A well-formed answer for the primary criterion.
const ANSWER: &str = r#"
def answer(verdict, choice, reason=None, extra=None):
    decision = None if choice is None else {
        "choice": choice, "confidence": 0.9,
        "probabilities": {"complete": 0.05, "verify_more": 0.05, "incomplete": 0.05, choice: 0.85},
        "model": "jev-1.13.0",
    }
    result = {
        "protocol": "ato.browser-verifier/0",
        "verdict": verdict,
        "criteria": [{
            "id": "primary", "verdict": verdict, "decision": decision,
            "rounds": 1, "evidence_refs": ["e1"], "reason": reason,
        }],
        "evidence": [{
            "id": "e1", "kind": "page_state", "url": request["url"], "title": "Notes",
            "facts": ["a note named formation-check is listed"], "text_excerpt": None,
        }],
        "action_trace": [{"kind": "navigate", "description": "open the candidate", "url": request["url"]}],
        "verifier": {
            "verifier": "stand-in", "stagehand_version": None, "browser_version": None,
            "agent_model": None, "judge_model": "jev-1.13.0",
        },
        "reason": reason,
    }
    if extra:
        extra(result)
    print(json.dumps(result))
"#;

fn verification(command: BrowserVerifierCommand, wall_clock_ms: u64) -> BrowserVerification {
    BrowserVerification {
        contract: BrowserContractV0::from_prompt(PROMPT).expect("contract"),
        verifier: Some(command),
        budget: BrowserBudget {
            max_browser_steps: 5,
            max_jev_rounds: 2,
            wall_clock_ms,
        },
    }
}

fn target() -> BrowserTarget {
    BrowserTarget {
        runtime_id: "local".to_owned(),
        endpoint: "http://127.0.0.1:41234/".to_owned(),
    }
}

fn run(body: &str) -> BrowserVerificationReceipt {
    let dir = tempfile::tempdir().expect("tempdir");
    verify_in_browser(
        &verification(helper(&dir, &format!("{ANSWER}\n{body}")), 20_000),
        target(),
    )
}

#[test]
fn a_supported_pass_is_a_pass_bound_to_the_contract_and_target() {
    let receipt = run(r#"answer("pass", "complete")"#);
    assert_eq!(receipt.overall, BrowserVerdict::Pass, "{receipt:?}");
    let contract = BrowserContractV0::from_prompt(PROMPT).unwrap();
    assert_eq!(receipt.contract_ref, contract.contract_ref());
    assert_eq!(
        receipt.original_prompt_digest,
        contract.original_prompt_digest()
    );
    assert_eq!(receipt.target, target());
    assert_eq!(
        receipt.criteria[0].decision.as_ref().unwrap().model,
        "jev-1.13.0"
    );
}

#[test]
fn the_helper_is_asked_about_the_contract_and_the_realized_url() {
    // The stand-in fails the criterion unless the request is exactly right.
    let receipt = run(r#"
ok = (request["protocol"] == "ato.browser-verifier/0"
      and request["url"] == "http://127.0.0.1:41234/"
      and request["contract"]["original_prompt"] == request["contract"]["criteria"][0]["instruction"]
      and request["budget"]["max_jev_rounds"] == 2)
answer("pass" if ok else "fail", "complete" if ok else "incomplete")
"#);
    assert_eq!(receipt.overall, BrowserVerdict::Pass, "{receipt:?}");
}

#[test]
fn a_supported_fail_is_a_fail() {
    let receipt = run(r#"answer("fail", "incomplete")"#);
    assert_eq!(receipt.overall, BrowserVerdict::Fail);
}

#[test]
fn verify_more_at_the_end_of_the_budget_is_inconclusive() {
    let receipt = run(r#"answer("inconclusive", "verify_more", reason="budget_exhausted")"#);
    assert_eq!(receipt.overall, BrowserVerdict::Inconclusive);
    assert_eq!(receipt.reason.as_deref(), Some("budget_exhausted"));
}

#[test]
fn a_pass_without_a_complete_judgment_is_not_accepted() {
    // The helper says pass; its own judge said verify_more. Refused.
    let receipt = run(r#"answer("pass", "verify_more")"#);
    assert_eq!(receipt.overall, BrowserVerdict::Inconclusive);
    assert!(
        receipt
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("result_invalid")
    );
    assert!(
        receipt
            .criteria
            .iter()
            .all(|c| c.verdict == BrowserVerdict::Inconclusive)
    );
}

#[test]
fn an_unavailable_judge_is_inconclusive() {
    let receipt = run(r#"answer("inconclusive", None, reason="judge_unavailable")"#);
    assert_eq!(receipt.overall, BrowserVerdict::Inconclusive);
    assert_eq!(receipt.reason.as_deref(), Some("judge_unavailable"));
}

#[test]
fn an_answer_with_extra_fields_is_refused() {
    let receipt = run(
        r#"answer("pass", "complete", extra=lambda r: r.update({"cookies": [{"name": "session"}]}))"#,
    );
    assert_eq!(receipt.overall, BrowserVerdict::Inconclusive);
}

#[test]
fn a_non_json_answer_is_inconclusive() {
    let receipt = run(r#"print("PASS")"#);
    assert_eq!(receipt.overall, BrowserVerdict::Inconclusive);
    assert!(
        receipt
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("result_invalid")
    );
}

#[test]
fn a_crashing_helper_is_inconclusive_and_reports_its_stderr() {
    let receipt = run(r#"sys.stderr.write("chrome exited unexpectedly\n"); sys.exit(4)"#);
    assert_eq!(receipt.overall, BrowserVerdict::Inconclusive);
    let reason = receipt.reason.unwrap_or_default();
    assert!(reason.contains("browser_verifier_failed"), "{reason}");
    assert!(reason.contains("chrome exited unexpectedly"), "{reason}");
}

#[test]
fn a_helper_that_leaks_a_secret_into_its_answer_is_refused() {
    // Only this test sets the variable; the value is distinctive.
    let secret = "jev-test-secret-value-0123456789";
    // SAFETY: tests in this file do not read JEV_API_KEY concurrently except
    // through this helper, and the value is test-only.
    unsafe { std::env::set_var("JEV_API_KEY", secret) };
    let receipt = run(
        r#"answer("pass", "complete", extra=lambda r: r["evidence"][0]["facts"].append(os.environ.get("JEV_API_KEY", "")))"#,
    );
    unsafe { std::env::remove_var("JEV_API_KEY") };
    assert_eq!(receipt.overall, BrowserVerdict::Inconclusive);
    let text = serde_json::to_string(&receipt).unwrap();
    assert!(!text.contains(secret));
}

#[test]
fn only_allowlisted_environment_reaches_the_helper() {
    // SAFETY: a test-only variable no other test reads.
    unsafe { std::env::set_var("ATO_TEST_AMBIENT_CREDENTIAL", "should-not-cross") };
    let receipt = run(r#"
leaked = "ATO_TEST_AMBIENT_CREDENTIAL" in os.environ
answer("fail" if leaked else "pass", "incomplete" if leaked else "complete")
"#);
    unsafe { std::env::remove_var("ATO_TEST_AMBIENT_CREDENTIAL") };
    assert_eq!(receipt.overall, BrowserVerdict::Pass);
}

#[cfg(unix)]
#[test]
fn a_helper_that_never_answers_is_stopped_with_everything_it_started() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = format!("ato-browser-verifier-hang-{}", std::process::id());
    let command = helper(
        &dir,
        &format!(
            "import subprocess\n\
             subprocess.Popen(['python3', '-c', 'import time; time.sleep(600)', '{marker}'])\n\
             # Detached, the way a browser launcher starts Chrome: its own\n\
             # session, found only by the scratch path it was given.\n\
             subprocess.Popen(['python3', '-c', 'import time; time.sleep(600)', \n\
                 request['scratch_dir'] + '/profile', '{marker}'], start_new_session=True)\n\
             time.sleep(600)"
        ),
    );
    let started = Instant::now();
    // 1 s of wall clock + the fixed shutdown grace.
    let receipt = verify_in_browser(&verification(command, 1_000), target());
    assert_eq!(receipt.overall, BrowserVerdict::Inconclusive);
    assert!(
        receipt
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("timeout")
    );
    assert!(started.elapsed() < Duration::from_secs(40));
    // Its children — a browser, in the real helper — went with it.
    std::thread::sleep(Duration::from_millis(300));
    let survivors = std::process::Command::new("pgrep")
        .args(["-f", &marker])
        .output()
        .expect("pgrep");
    assert!(
        String::from_utf8_lossy(&survivors.stdout).trim().is_empty(),
        "helper children outlived the verification"
    );
}

#[test]
fn no_verifier_means_unavailable_not_skipped() {
    let receipt = verify_in_browser(
        &BrowserVerification {
            contract: BrowserContractV0::from_prompt(PROMPT).unwrap(),
            verifier: None,
            budget: BrowserBudget::default(),
        },
        target(),
    );
    assert_eq!(receipt.overall, BrowserVerdict::Inconclusive);
    assert!(
        receipt
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("browser_verifier_unavailable")
    );
}

#[test]
fn a_missing_helper_binary_is_unavailable() {
    let receipt = verify_in_browser(
        &verification(
            BrowserVerifierCommand {
                argv: vec![
                    PathBuf::from("/nonexistent/node")
                        .to_string_lossy()
                        .into_owned(),
                ],
                cwd: None,
            },
            5_000,
        ),
        target(),
    );
    assert_eq!(receipt.overall, BrowserVerdict::Inconclusive);
}

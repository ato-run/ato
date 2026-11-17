//! End-to-end Kev-0.8B inference against a locally installed Model Set.
//!
//! Ignored by default: it needs the 1.8 GB pinned payload and a Python runtime
//! with the Kev package, neither of which belongs in CI. It is the reproducible
//! form of the acceptance measurement, so a release gate can re-run it:
//!
//!   ATO_KEV_TEST_PYTHON=.tmp/kev-venv/bin/python \
//!   ATO_KEV_TEST_HOME=.tmp/ato-home \
//!     cargo test -p ato-cli --test model_inference -- --ignored --nocapture
//!
//! Without both variables the tests report why they cannot run and pass, so a
//! plain cargo test never fails for a missing 1.8 GB download.
use std::process::Command;

fn python() -> Option<String> {
    std::env::var("ATO_KEV_TEST_PYTHON").ok()
}

fn home() -> Option<String> {
    std::env::var("ATO_KEV_TEST_HOME").ok()
}

fn score(request: &str) -> serde_json::Value {
    let (Some(python), Some(home)) = (python(), home()) else {
        eprintln!("skipping: set ATO_KEV_TEST_PYTHON and ATO_KEV_TEST_HOME to run this test");
        return serde_json::Value::Null;
    };
    let output = Command::new(env!("CARGO_BIN_EXE_ato"))
        .args([
            "model",
            "score",
            "--home",
            &home,
            "--python",
            &python,
            "--request",
            request,
            "--json",
        ])
        .output()
        .expect("run ato model score");
    assert!(
        output.status.success(),
        "ato model score failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("score report is JSON")
}

const REQUEST: &str = r#"{
  "state": "I was charged twice for order 1182. Please refund one of the charges. Our refund policy returns card charges within five business days.",
  "questions": {
    "team": {"type": "choice", "instructions": "Which team should handle this?",
             "criteria": {"billing": "Charges and refunds",
                          "shipping": "Deliveries and tracking",
                          "returns": "Exchanges and returns"}},
    "urgent": {"type": "noul", "instructions": "Does this need a reply today?"}
  }
}"#;

#[test]
#[ignore = "needs the installed Kev Model Set and a Python runtime"]
fn the_installed_model_set_scores_a_state() {
    let report = score(REQUEST);
    if report.is_null() {
        return;
    }
    assert_eq!(report["schema"], "ato.model-score/1");
    assert_eq!(report["model"], "kev-0.8b");
    // The model is a decision model: it returns a distribution over the offered
    // labels, not text.
    assert_eq!(report["answers"]["team"]["type"], "choice");
    assert_eq!(report["answers"]["team"]["choice"], "billing");
    let probabilities = report["answers"]["team"]["probabilities"]
        .as_object()
        .expect("probabilities");
    assert_eq!(probabilities.len(), 3);
    let total: f64 = probabilities
        .values()
        .map(|value| value.as_f64().expect("probability"))
        .sum();
    assert!((total - 1.0).abs() < 1e-6, "probabilities sum to {total}");
    assert_eq!(report["answers"]["urgent"]["type"], "noul");
    // Provenance: the local runtime and the pinned checkpoint, not a remote call.
    assert_eq!(report["runtime"]["backend"], "mlx");
    assert_eq!(report["checkpoint"]["weights"], "lora");
    assert_eq!(
        report["checkpoint"]["base_revision"],
        "dc7cdfe2ee4154fa7e30f5b51ca41bfa40174e68"
    );
    assert!(
        (report["checkpoint"]["temperature"].as_f64().expect("temperature") - 2.3511).abs() < 0.001,
        "the calibrated temperature comes from head.pt"
    );
}

#[test]
#[ignore = "needs the installed Kev Model Set and a Python runtime"]
fn the_same_state_scores_identically_twice() {
    let first = score(REQUEST);
    if first.is_null() {
        return;
    }
    let second = score(REQUEST);
    assert_eq!(
        first["raw"], second["raw"],
        "a decision model must be deterministic for the same state"
    );
}

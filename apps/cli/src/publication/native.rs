//! Publication evidence from one owned macOS window, separate from the Capsule's K.

use anyhow::{Result, ensure};
use ato_runtime_attempt::browser_verify::OllamaJudgeConfig;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{bundle_sha256, limited_text};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Task {
    criterion: String,
    press: Vec<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativePlan {
    task: Task,
    judge: OllamaJudgeConfig,
}

impl NativePlan {
    pub(super) fn freeze(task: Task) -> Result<Self> {
        ensure!(
            limited_text(&task.criterion, 2048)
                && (1..=4).contains(&task.press.len())
                && task.press.iter().all(|label| limited_text(label, 128)),
            "A native window task needs a criterion and one to four exact button labels"
        );
        let judge = crate::publication::browser_models(true)?
            .ollama_judge
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Configure a digest-pinned local Ollama judge for native window verification"
                )
            })?;
        Ok(Self { task, judge })
    }

    pub(super) fn reference(&self) -> Result<String> {
        Ok(bundle_sha256(&serde_jcs::to_vec(&json!({
            "schema": "ato.publication.native-window/1", "task": self.task,
        }))?))
    }

    pub(super) fn effective_contract(&self, base: &str) -> Result<String> {
        Ok(bundle_sha256(&serde_jcs::to_vec(&json!({
            "schema": "ato.publication.native-window-contract/1", "base_contract_ref": base,
            "native_task_ref": self.reference()?,
        }))?))
    }
}

pub(super) use ato_portable_application::portable_run::supports_bounded_native_window as supports;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Proof {
    task_ref: String,
    attempt_id: String,
    judge: String,
    decision: Decision,
    frame_digests: Vec<String>,
    signature: String,
    pub(super) local_override: bool,
    containment: String,
}

impl Proof {
    pub(super) fn passed(&self, plan: &NativePlan, attempt_id: &str) -> bool {
        plan.reference()
            .is_ok_and(|reference| reference == self.task_ref)
            && self.attempt_id == attempt_id
            && self.containment == "seatbelt"
            && self.judge == plan.judge.identity()
            && self.decision.verdict == DecisionKind::Complete
            && self.frame_digests.len() == plan.task.press.len() + 1
            && self.decision.cites(self.frame_digests.len())
    }

    pub(super) fn failed(&self) -> bool {
        self.decision.verdict == DecisionKind::Incomplete
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    verdict: DecisionKind,
    frames: Vec<usize>,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DecisionKind {
    Complete,
    Incomplete,
    VerifyMore,
}

impl Decision {
    fn cites(&self, count: usize) -> bool {
        count >= 2
            && self.frames.len() <= count
            && self.frames.first() == Some(&0)
            && self.frames.last() == Some(&(count - 1))
            && self.frames.windows(2).all(|pair| pair[0] < pair[1])
    }
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(super) use macos::{available, verify};

#[cfg(not(target_os = "macos"))]
pub(super) fn available() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub(super) fn verify(
    _: &NativePlan,
    _: &str,
    _: &mut ato_runtime_attempt::realize::LiveCandidate,
    _: &ato_formation::verify::VerificationExecutionEvidence,
    _: &ato_runtime_attempt::control::ExecutionControl,
    _: &std::sync::atomic::AtomicBool,
) -> Result<Proof> {
    anyhow::bail!("Native window verification requires a macOS graphical session")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_publication_binds_actions_to_the_base_contract_and_evidence_to_the_attempt() {
        let mut plan = NativePlan {
            task: Task {
                criterion: "Count increases".into(),
                press: vec!["Increment".into()],
            },
            judge: OllamaJudgeConfig::new(
                "http://127.0.0.1:11438",
                "judge:local".into(),
                "a".repeat(64),
            )
            .unwrap(),
        };
        let effective = plan.effective_contract("base").unwrap();
        let mut proof = Proof {
            task_ref: plan.reference().unwrap(),
            attempt_id: "attempt-one".into(),
            judge: plan.judge.identity(),
            decision: Decision {
                verdict: DecisionKind::Complete,
                frames: vec![0, 1],
            },
            frame_digests: vec![bundle_sha256(b"before"), bundle_sha256(b"after")],
            signature: "adhoc".into(),
            local_override: true,
            containment: "seatbelt".into(),
        };
        assert!(proof.passed(&plan, "attempt-one"));
        assert!(!proof.passed(&plan, "attempt-two"));
        proof.decision.frames = vec![1];
        assert!(!proof.passed(&plan, "attempt-one"));
        plan.task.press[0] = "Reset".into();
        assert_ne!(effective, plan.effective_contract("base").unwrap());
        assert_ne!(
            effective,
            plan.effective_contract("other-contract").unwrap()
        );
    }
}

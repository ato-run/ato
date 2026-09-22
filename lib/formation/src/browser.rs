//! Browser Contract v0: a natural-language acceptance prompt, kept verbatim
//! inside a typed envelope, and the evidence-backed verdict a browser verifier
//! returns for it.
//!
//! ```text
//! human intent ("create a note, reload, it is still there")
//!     -> BrowserContractV0 { original_prompt, criteria[], normalization }
//!     -> browser verifier (drives the realized candidate, collects evidence)
//!     -> BrowserVerificationResult (per-criterion verdicts + evidence)
//!     -> BrowserVerificationReceipt (bound to this Contract and this target)
//! ```
//!
//! v0 does not decompose the prompt: the whole prompt is one required
//! `browser.task` criterion. What v0 fixes is the shape — the prompt is never
//! lost, every verdict names the evidence it rests on, and the overall result
//! is recomputed here rather than taken from the verifier's word.
//!
//! Terminal verdicts are `pass`, `fail` and `inconclusive`. "Verify more" is a
//! state inside the verifier's loop and never leaves it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub const BROWSER_CONTRACT_V0_SCHEMA: &str = "ato.browser-contract/0";
pub const BROWSER_TASK_KIND: &str = "browser.task";
/// v0 normalization: the prompt becomes one criterion, word for word.
pub const NORMALIZATION_V0_IDENTITY: &str = "v0.identity";
/// The one criterion v0 derives from a prompt.
pub const PRIMARY_CRITERION_ID: &str = "primary";

/// Longest acceptance prompt accepted, in bytes.
pub const MAX_PROMPT_BYTES: usize = 4096;

// Bounds a verifier result must respect. Evidence is kept small on purpose: a
// receipt is a record of what was seen, not a copy of the page.
pub const MAX_ACTIONS: usize = 200;
pub const MAX_EVIDENCE: usize = 64;
pub const MAX_FACTS_PER_EVIDENCE: usize = 64;
pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_REASON_BYTES: usize = 2048;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BrowserContractError {
    #[error("the acceptance prompt is empty")]
    EmptyPrompt,
    #[error("the acceptance prompt is longer than {MAX_PROMPT_BYTES} bytes")]
    PromptTooLong,
}

/// One thing the candidate must be observed doing, in a browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserCriterion {
    pub id: String,
    /// Always `browser.task` in v0.
    pub kind: String,
    pub instruction: String,
    pub required: bool,
}

/// The typed envelope around a human acceptance prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserContractV0 {
    pub schema: String,
    /// Exactly what the person wrote. Never rewritten, never summarized.
    pub original_prompt: String,
    pub criteria: Vec<BrowserCriterion>,
    /// How `criteria` were derived from the prompt.
    pub normalization: String,
}

impl BrowserContractV0 {
    /// The v0 Contract for a prompt: the prompt, verbatim, as one required
    /// criterion.
    pub fn from_prompt(prompt: &str) -> Result<Self, BrowserContractError> {
        if prompt.trim().is_empty() {
            return Err(BrowserContractError::EmptyPrompt);
        }
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(BrowserContractError::PromptTooLong);
        }
        Ok(Self {
            schema: BROWSER_CONTRACT_V0_SCHEMA.to_owned(),
            original_prompt: prompt.to_owned(),
            criteria: vec![BrowserCriterion {
                id: PRIMARY_CRITERION_ID.to_owned(),
                kind: BROWSER_TASK_KIND.to_owned(),
                instruction: prompt.to_owned(),
                required: true,
            }],
            normalization: NORMALIZATION_V0_IDENTITY.to_owned(),
        })
    }

    /// Content address of the canonical (JCS) Contract.
    pub fn contract_ref(&self) -> String {
        let canonical = serde_jcs::to_vec(self).expect("a Browser Contract always serializes");
        format!("sha256:{:x}", Sha256::digest(canonical))
    }

    pub fn original_prompt_digest(&self) -> String {
        format!(
            "sha256:{:x}",
            Sha256::digest(self.original_prompt.as_bytes())
        )
    }
}

/// How much a verifier may spend before it must answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserBudget {
    /// Browser actions the verifier may take across the whole run.
    pub max_browser_steps: u32,
    /// Judgment rounds; the last `verify_more` becomes `inconclusive`.
    pub max_jev_rounds: u32,
    /// Wall-clock bound for the whole verification.
    pub wall_clock_ms: u64,
}

impl Default for BrowserBudget {
    fn default() -> Self {
        Self {
            max_browser_steps: 20,
            max_jev_rounds: 3,
            wall_clock_ms: 180_000,
        }
    }
}

/// What the verifier helper is asked, on stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserVerificationRequest {
    pub protocol: String,
    pub contract: BrowserContractV0,
    /// The realized endpoint. The verifier navigates here and nowhere else.
    pub url: String,
    pub budget: BrowserBudget,
    /// A directory the caller owns and removes afterwards. The browser's
    /// profile lives here, so anything the verifier leaves running can be
    /// found by it and stopped.
    pub scratch_dir: String,
}

pub const BROWSER_VERIFIER_PROTOCOL: &str = "ato.browser-verifier/0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserVerdict {
    Pass,
    Fail,
    Inconclusive,
}

/// A judge's bounded decision for one criterion, as returned.
///
/// Evidence, not authority: the probabilities are kept for the record and
/// never used to grant anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeDecision {
    /// `complete`, `verify_more` or `incomplete`.
    pub choice: String,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
    /// The versioned model that answered.
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionResult {
    pub id: String,
    pub verdict: BrowserVerdict,
    /// The final round's decision; absent when no judgment was obtained.
    pub decision: Option<JudgeDecision>,
    pub rounds: u32,
    pub evidence_refs: Vec<String>,
    pub reason: Option<String>,
}

/// One bounded observation of the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserEvidence {
    pub id: String,
    /// e.g. `page_state`, `extracted_facts`, `agent_report`.
    pub kind: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub facts: Vec<String>,
    pub text_excerpt: Option<String>,
}

/// One step the verifier took, bounded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserAction {
    /// e.g. `navigate`, `agent_step`, `observe`, `extract`.
    pub kind: String,
    pub description: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifierIdentity {
    pub verifier: String,
    pub stagehand_version: Option<String>,
    pub browser_version: Option<String>,
    pub agent_model: Option<String>,
    pub judge_model: Option<String>,
}

/// What the verifier helper answers, on stdout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserVerificationResult {
    pub protocol: String,
    pub verdict: BrowserVerdict,
    pub criteria: Vec<CriterionResult>,
    pub evidence: Vec<BrowserEvidence>,
    pub action_trace: Vec<BrowserAction>,
    pub verifier: VerifierIdentity,
    /// Why the run ended where it did, when that is not a criterion's fault:
    /// `judge_unavailable`, `browser_failed`, `timeout`.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrowserTarget {
    pub runtime_id: String,
    pub endpoint: String,
}

/// The record of one browser verification, bound to its Contract and target.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BrowserVerificationReceipt {
    pub contract_ref: String,
    pub original_prompt_digest: String,
    pub verifier: VerifierIdentity,
    pub target: BrowserTarget,
    pub actions: Vec<BrowserAction>,
    pub evidence: Vec<BrowserEvidence>,
    pub criteria: Vec<CriterionResult>,
    /// Recomputed from `criteria` against the Contract — not the verifier's
    /// own overall claim.
    pub overall: BrowserVerdict,
    pub reason: Option<String>,
}

/// The overall verdict a Contract's criteria results add up to.
///
/// - `pass`: every required criterion passed
/// - `fail`: some required criterion failed
/// - `inconclusive`: otherwise — including a required criterion with no
///   result at all
pub fn overall_verdict(
    contract: &BrowserContractV0,
    results: &[CriterionResult],
) -> BrowserVerdict {
    let mut all_passed = true;
    for criterion in contract
        .criteria
        .iter()
        .filter(|criterion| criterion.required)
    {
        match results.iter().find(|result| result.id == criterion.id) {
            Some(result) if result.verdict == BrowserVerdict::Fail => {
                return BrowserVerdict::Fail;
            }
            Some(result) if result.verdict == BrowserVerdict::Pass => {}
            _ => all_passed = false,
        }
    }
    if all_passed {
        BrowserVerdict::Pass
    } else {
        BrowserVerdict::Inconclusive
    }
}

/// Why a verifier's answer cannot be used as evidence.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BrowserResultError {
    #[error("the verifier spoke protocol {0:?}")]
    Protocol(String),
    #[error("the verifier returned a result for criterion {0:?}, which the Contract does not have")]
    UnknownCriterion(String),
    #[error("the verifier returned criterion {0:?} twice")]
    DuplicateCriterion(String),
    #[error("criterion {id:?} is {verdict:?}, which its judgment {choice:?} does not support")]
    UnsupportedVerdict {
        id: String,
        verdict: BrowserVerdict,
        choice: Option<String>,
    },
    #[error("criterion {id:?} cites evidence {evidence:?}, which the result does not contain")]
    UnknownEvidence { id: String, evidence: String },
    #[error("the verifier's result exceeds the evidence bounds ({0})")]
    Unbounded(&'static str),
    #[error("the verifier claimed {claimed:?}, but its criteria add up to {actual:?}")]
    OverallMismatch {
        claimed: BrowserVerdict,
        actual: BrowserVerdict,
    },
}

/// Check a verifier's answer against the Contract it was asked about.
///
/// A verdict is accepted only when its judgment supports it: `pass` needs a
/// `complete` decision, `fail` an `incomplete` one. Anything else — a missing
/// judgment, `verify_more` at the end of the budget — can only be
/// `inconclusive`. The overall verdict must equal what the criteria add up
/// to.
pub fn validate_result(
    contract: &BrowserContractV0,
    result: &BrowserVerificationResult,
) -> Result<BrowserVerdict, BrowserResultError> {
    if result.protocol != BROWSER_VERIFIER_PROTOCOL {
        return Err(BrowserResultError::Protocol(result.protocol.clone()));
    }
    if result.action_trace.len() > MAX_ACTIONS {
        return Err(BrowserResultError::Unbounded("action_trace"));
    }
    if result.evidence.len() > MAX_EVIDENCE {
        return Err(BrowserResultError::Unbounded("evidence"));
    }
    let long = |text: &Option<String>, limit: usize| text.as_ref().is_some_and(|t| t.len() > limit);
    for evidence in &result.evidence {
        if evidence.facts.len() > MAX_FACTS_PER_EVIDENCE
            || evidence
                .facts
                .iter()
                .any(|fact| fact.len() > MAX_TEXT_BYTES)
            || long(&evidence.text_excerpt, MAX_TEXT_BYTES)
            || long(&evidence.title, MAX_TEXT_BYTES)
            || long(&evidence.url, MAX_TEXT_BYTES)
        {
            return Err(BrowserResultError::Unbounded("evidence entry"));
        }
    }
    if result.action_trace.iter().any(|action| {
        action.description.len() > MAX_TEXT_BYTES || long(&action.url, MAX_TEXT_BYTES)
    }) {
        return Err(BrowserResultError::Unbounded("action entry"));
    }
    if long(&result.reason, MAX_REASON_BYTES) {
        return Err(BrowserResultError::Unbounded("reason"));
    }

    let mut seen = std::collections::BTreeSet::new();
    for criterion in &result.criteria {
        if !contract
            .criteria
            .iter()
            .any(|known| known.id == criterion.id)
        {
            return Err(BrowserResultError::UnknownCriterion(criterion.id.clone()));
        }
        if !seen.insert(criterion.id.as_str()) {
            return Err(BrowserResultError::DuplicateCriterion(criterion.id.clone()));
        }
        if long(&criterion.reason, MAX_REASON_BYTES) {
            return Err(BrowserResultError::Unbounded("criterion reason"));
        }
        let choice = criterion
            .decision
            .as_ref()
            .map(|decision| decision.choice.as_str());
        let supported = match criterion.verdict {
            BrowserVerdict::Pass => choice == Some("complete"),
            BrowserVerdict::Fail => choice == Some("incomplete"),
            BrowserVerdict::Inconclusive => {
                choice != Some("complete") && choice != Some("incomplete")
            }
        };
        if !supported {
            return Err(BrowserResultError::UnsupportedVerdict {
                id: criterion.id.clone(),
                verdict: criterion.verdict,
                choice: choice.map(str::to_owned),
            });
        }
        for evidence in &criterion.evidence_refs {
            if !result.evidence.iter().any(|known| &known.id == evidence) {
                return Err(BrowserResultError::UnknownEvidence {
                    id: criterion.id.clone(),
                    evidence: evidence.clone(),
                });
            }
        }
    }

    let actual = overall_verdict(contract, &result.criteria);
    if actual != result.verdict {
        return Err(BrowserResultError::OverallMismatch {
            claimed: result.verdict,
            actual,
        });
    }
    Ok(actual)
}

impl BrowserVerificationReceipt {
    /// Bind a validated result to its Contract and target.
    pub fn new(
        contract: &BrowserContractV0,
        target: BrowserTarget,
        result: BrowserVerificationResult,
    ) -> Result<Self, BrowserResultError> {
        let overall = validate_result(contract, &result)?;
        Ok(Self {
            contract_ref: contract.contract_ref(),
            original_prompt_digest: contract.original_prompt_digest(),
            verifier: result.verifier,
            target,
            actions: result.action_trace,
            evidence: result.evidence,
            criteria: result.criteria,
            overall,
            reason: result.reason,
        })
    }

    /// A receipt for a verification that produced no usable answer: the
    /// verifier was missing, crashed, timed out or answered out of contract.
    /// Always `inconclusive` — the absence of evidence is never a pass.
    pub fn unavailable(
        contract: &BrowserContractV0,
        target: BrowserTarget,
        verifier: &str,
        reason: &str,
    ) -> Self {
        let mut reason = reason.to_owned();
        if reason.len() > MAX_REASON_BYTES {
            let mut cut = MAX_REASON_BYTES;
            while !reason.is_char_boundary(cut) {
                cut -= 1;
            }
            reason.truncate(cut);
        }
        Self {
            contract_ref: contract.contract_ref(),
            original_prompt_digest: contract.original_prompt_digest(),
            verifier: VerifierIdentity {
                verifier: verifier.to_owned(),
                stagehand_version: None,
                browser_version: None,
                agent_model: None,
                judge_model: None,
            },
            target,
            actions: Vec::new(),
            evidence: Vec::new(),
            criteria: contract
                .criteria
                .iter()
                .map(|criterion| CriterionResult {
                    id: criterion.id.clone(),
                    verdict: BrowserVerdict::Inconclusive,
                    decision: None,
                    rounds: 0,
                    evidence_refs: Vec::new(),
                    reason: None,
                })
                .collect(),
            overall: BrowserVerdict::Inconclusive,
            reason: Some(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROMPT: &str = "Create a note named 'formation-check'. Reload the page and verify that the note is still present.";

    fn decision(choice: &str) -> Option<JudgeDecision> {
        Some(JudgeDecision {
            choice: choice.to_owned(),
            confidence: 0.9,
            probabilities: [(choice.to_owned(), 0.95)].into(),
            model: "jev-1.13.0".to_owned(),
        })
    }

    fn result(verdict: BrowserVerdict, choice: Option<&str>) -> BrowserVerificationResult {
        BrowserVerificationResult {
            protocol: BROWSER_VERIFIER_PROTOCOL.to_owned(),
            verdict,
            criteria: vec![CriterionResult {
                id: PRIMARY_CRITERION_ID.to_owned(),
                verdict,
                decision: choice.and_then(decision),
                rounds: 1,
                evidence_refs: vec!["e1".to_owned()],
                reason: None,
            }],
            evidence: vec![BrowserEvidence {
                id: "e1".to_owned(),
                kind: "page_state".to_owned(),
                url: Some("http://127.0.0.1:41234/".to_owned()),
                title: Some("Notes".to_owned()),
                facts: vec!["a note 'formation-check' is listed".to_owned()],
                text_excerpt: None,
            }],
            action_trace: vec![BrowserAction {
                kind: "navigate".to_owned(),
                description: "open the candidate".to_owned(),
                url: Some("http://127.0.0.1:41234/".to_owned()),
            }],
            verifier: VerifierIdentity {
                verifier: "formation-browser-verifier/0".to_owned(),
                stagehand_version: Some("3.7.3".to_owned()),
                browser_version: None,
                agent_model: None,
                judge_model: Some("jev-1.13.0".to_owned()),
            },
            reason: None,
        }
    }

    #[test]
    fn the_prompt_is_kept_verbatim_as_one_required_criterion() {
        let contract = BrowserContractV0::from_prompt(PROMPT).unwrap();
        assert_eq!(contract.original_prompt, PROMPT);
        assert_eq!(contract.criteria.len(), 1);
        assert_eq!(contract.criteria[0].instruction, PROMPT);
        assert_eq!(contract.criteria[0].kind, BROWSER_TASK_KIND);
        assert!(contract.criteria[0].required);
        assert_eq!(contract.normalization, NORMALIZATION_V0_IDENTITY);
    }

    #[test]
    fn a_non_ascii_prompt_is_kept_byte_for_byte() {
        let prompt = "新しいメモを1件作成し、リロード後もそのメモが残っていることを確認する";
        let contract = BrowserContractV0::from_prompt(prompt).unwrap();
        assert_eq!(contract.original_prompt.as_bytes(), prompt.as_bytes());
    }

    #[test]
    fn the_contract_ref_is_stable_and_depends_on_the_prompt() {
        let a = BrowserContractV0::from_prompt(PROMPT).unwrap();
        let b = BrowserContractV0::from_prompt(PROMPT).unwrap();
        let c = BrowserContractV0::from_prompt("something else").unwrap();
        assert_eq!(a.contract_ref(), b.contract_ref());
        assert_ne!(a.contract_ref(), c.contract_ref());
        assert!(a.contract_ref().starts_with("sha256:"));
    }

    #[test]
    fn empty_and_oversized_prompts_are_refused() {
        assert_eq!(
            BrowserContractV0::from_prompt("  \n").unwrap_err(),
            BrowserContractError::EmptyPrompt
        );
        assert_eq!(
            BrowserContractV0::from_prompt(&"x".repeat(MAX_PROMPT_BYTES + 1)).unwrap_err(),
            BrowserContractError::PromptTooLong
        );
    }

    #[test]
    fn verdicts_must_be_supported_by_their_judgment() {
        let contract = BrowserContractV0::from_prompt(PROMPT).unwrap();
        assert_eq!(
            validate_result(&contract, &result(BrowserVerdict::Pass, Some("complete"))),
            Ok(BrowserVerdict::Pass)
        );
        assert_eq!(
            validate_result(&contract, &result(BrowserVerdict::Fail, Some("incomplete"))),
            Ok(BrowserVerdict::Fail)
        );
        assert_eq!(
            validate_result(
                &contract,
                &result(BrowserVerdict::Inconclusive, Some("verify_more"))
            ),
            Ok(BrowserVerdict::Inconclusive)
        );
        assert_eq!(
            validate_result(&contract, &result(BrowserVerdict::Inconclusive, None)),
            Ok(BrowserVerdict::Inconclusive)
        );
        // A pass nobody judged complete is not a pass.
        for choice in [None, Some("verify_more"), Some("incomplete")] {
            assert!(matches!(
                validate_result(&contract, &result(BrowserVerdict::Pass, choice)),
                Err(BrowserResultError::UnsupportedVerdict { .. })
            ));
        }
        // And a judged-complete criterion cannot be downgraded to "unsure".
        assert!(
            validate_result(
                &contract,
                &result(BrowserVerdict::Inconclusive, Some("complete"))
            )
            .is_err()
        );
    }

    #[test]
    fn a_missing_required_criterion_is_inconclusive_not_a_pass() {
        let contract = BrowserContractV0::from_prompt(PROMPT).unwrap();
        assert_eq!(
            overall_verdict(&contract, &[]),
            BrowserVerdict::Inconclusive
        );
        let mut claimed_pass = result(BrowserVerdict::Pass, Some("complete"));
        claimed_pass.criteria.clear();
        assert_eq!(
            validate_result(&contract, &claimed_pass),
            Err(BrowserResultError::OverallMismatch {
                claimed: BrowserVerdict::Pass,
                actual: BrowserVerdict::Inconclusive
            })
        );
    }

    #[test]
    fn a_result_about_another_contract_or_citing_nothing_is_refused() {
        let contract = BrowserContractV0::from_prompt(PROMPT).unwrap();
        let mut unknown = result(BrowserVerdict::Pass, Some("complete"));
        unknown.criteria[0].id = "invented".to_owned();
        assert!(matches!(
            validate_result(&contract, &unknown),
            Err(BrowserResultError::UnknownCriterion(_))
        ));
        let mut dangling = result(BrowserVerdict::Pass, Some("complete"));
        dangling.criteria[0].evidence_refs = vec!["e404".to_owned()];
        assert!(matches!(
            validate_result(&contract, &dangling),
            Err(BrowserResultError::UnknownEvidence { .. })
        ));
    }

    #[test]
    fn unbounded_evidence_is_refused() {
        let contract = BrowserContractV0::from_prompt(PROMPT).unwrap();
        let mut long = result(BrowserVerdict::Pass, Some("complete"));
        long.evidence[0].text_excerpt = Some("x".repeat(MAX_TEXT_BYTES + 1));
        assert!(matches!(
            validate_result(&contract, &long),
            Err(BrowserResultError::Unbounded(_))
        ));
    }

    #[test]
    fn an_unavailable_verifier_yields_an_inconclusive_receipt() {
        let contract = BrowserContractV0::from_prompt(PROMPT).unwrap();
        let receipt = BrowserVerificationReceipt::unavailable(
            &contract,
            BrowserTarget {
                runtime_id: "local".to_owned(),
                endpoint: "http://127.0.0.1:1/".to_owned(),
            },
            "none",
            "browser_verifier_unavailable",
        );
        assert_eq!(receipt.overall, BrowserVerdict::Inconclusive);
        assert_eq!(receipt.criteria[0].verdict, BrowserVerdict::Inconclusive);
        assert_eq!(receipt.contract_ref, contract.contract_ref());
    }

    #[test]
    fn the_wire_result_parses_strictly() {
        let text = serde_json::to_string(&result(BrowserVerdict::Pass, Some("complete"))).unwrap();
        let parsed: BrowserVerificationResult = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.verdict, BrowserVerdict::Pass);
        let extra = text.replacen('{', "{\"cookies\":[],", 1);
        assert!(serde_json::from_str::<BrowserVerificationResult>(&extra).is_err());
    }
}

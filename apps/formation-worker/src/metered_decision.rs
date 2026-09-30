//! Requester-side finite-choice provider with durable, pre-send accounting.
use crate::{
    decision_provider::{
        DECISION_INSTRUCTIONS, DecisionPoint, DecisionProvider, JEV_BASE_URL, JevDecisionProvider,
        MAX_REQUEST_BYTES, ProviderAnswer, decision_request, validate_response,
    },
    runtime_network::proposal::budget::{
        BudgetPlan, CallBudget, FinishReason, RequestEvidence, ResponseEvidence,
    },
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeteredDecisionConfig {
    pub model: String,
    pub timeout_ms: u64,
    pub budget: BudgetPlan,
    pub credential_environment: String,
}
impl MeteredDecisionConfig {
    pub fn validate(&self) -> Result<()> {
        self.budget.validate()?;
        ensure!(
            self.model.starts_with("jev-")
                && self.model.len() <= 64
                && (1..=30_000).contains(&self.timeout_ms)
                && self.budget.output_price == 0
                && !self.credential_environment.is_empty()
                && self
                    .credential_environment
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'),
            "invalid metered decision config"
        );
        Ok(())
    }
    pub fn configuration_ref(&self) -> Result<String> {
        self.validate()?;
        Ok(hash(&serde_jcs::to_vec(&(
            self,
            JEV_BASE_URL,
            DECISION_INSTRUCTIONS,
        ))?))
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub struct MeteredDecisionProvider {
    config: MeteredDecisionConfig,
    budget: Arc<CallBudget>,
    search_id: String,
    answers: PathBuf,
}
impl MeteredDecisionProvider {
    pub fn new(
        config: MeteredDecisionConfig,
        budget: Arc<CallBudget>,
        search_id: &str,
        answers: &Path,
    ) -> Result<Self> {
        config.validate()?;
        ensure!(
            &config.budget == budget.plan(),
            "decision journal config mismatch"
        );
        std::fs::create_dir_all(answers)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(answers, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            config,
            budget,
            search_id: search_id.into(),
            answers: answers.into(),
        })
    }
    pub fn accounting(&self) -> Result<Value> {
        let journal = self.budget.snapshot()?;
        let mut responses = Vec::new();
        for cell in journal.cells.keys() {
            let seq = cell
                .strip_prefix(&format!("{}_d", self.search_id))
                .context("decision journal assignment mismatch")?;
            ensure!(
                seq.bytes().all(|b| b.is_ascii_digit()),
                "invalid decision sequence"
            );
            let path = self.answers.join(format!("decision-{seq}.json"));
            if path.is_file() {
                let bytes = std::fs::read(path)?;
                ensure!(
                    bytes.len() <= MAX_REQUEST_BYTES,
                    "cached decision too large"
                );
                responses.push(serde_json::from_slice::<Value>(&bytes)?);
            }
        }
        Ok(
            json!({"model":self.config.model,"configuration_ref":self.config.configuration_ref()?,
            "calls":journal.cells.len(),"journal":journal,"responses":responses,
            "reservation_per_call_usd_micros":self.config.budget.validate()?}),
        )
    }
    pub fn all_settled(&self) -> Result<bool> {
        Ok(self
            .budget
            .snapshot()?
            .cells
            .values()
            .all(|c| c.is_settled()))
    }
    fn attach_accounting(answer: &mut ProviderAnswer, value: &Value) {
        if let ProviderAnswer::Choice { evidence, .. } = answer {
            for field in [
                "latency_ms",
                "reserved_usd_micros",
                "estimated_cost_usd_micros",
            ] {
                evidence[field] = value[field].clone();
            }
        }
    }
    fn decide_metered(&self, point: &DecisionPoint) -> Result<ProviderAnswer> {
        if let [only] = point.choices.as_slice() {
            return Ok(ProviderAnswer::Choice {
                choice_id: only.choice_id.clone(),
                evidence: json!({"provider":"deterministic_singleton","calls":0,"no_call_reason":"single_choice"}),
            });
        }
        ensure!(point.choices.len() > 1, "no finite choice available");
        let request = decision_request(&self.config.model, point);
        let bytes = serde_json::to_vec(&request)?;
        ensure!(
            bytes.len() <= MAX_REQUEST_BYTES
                && (bytes.len() + 1024) as u64 <= self.config.budget.input_token_cap,
            "decision input exceeds reservation"
        );
        let cell = format!("{}_d{}", self.search_id, point.seq);
        let request_hash = hash(&bytes);
        let answer_path = self.answers.join(format!("decision-{}.json", point.seq));
        let snapshot = self.budget.snapshot()?;
        if let Some(saved) = snapshot.cells.get(&cell) {
            ensure!(
                saved
                    .request
                    .as_ref()
                    .is_some_and(|r| r.provider_body_sha256 == request_hash),
                "decision point changed after reservation"
            );
            if let Some(response) = &saved.response {
                if !response.within(&self.config.budget) {
                    return Ok(ProviderAnswer::Fallback {
                        reason: "provider_error",
                    });
                }
                let answer: Value = serde_json::from_slice(&std::fs::read(answer_path)?)?;
                ensure!(
                    answer["request_sha256"] == request_hash,
                    "cached decision assignment mismatch"
                );
                let raw = &answer["raw_response"];
                ensure!(raw["model"] == self.config.model, "cached model mismatch");
                let mut result = validate_response(point, raw);
                Self::attach_accounting(&mut result, &answer);
                return Ok(result);
            }
            if !saved.charged_unknown {
                self.budget.charge_unknown(&cell)?;
            }
            return Ok(ProviderAnswer::Fallback {
                reason: "provider_error",
            });
        }
        self.budget.reserve_request(RequestEvidence {
            cell: cell.clone(),
            proposal_request_sha256: request_hash.clone(),
            provider_body_sha256: request_hash.clone(),
            timeout_ms: self.config.timeout_ms,
            proposal_request_bytes: bytes.len() as u64,
            provider_body_bytes: bytes.len() as u64,
        })?;
        let started = Instant::now();
        let response = (|| {
            let key = std::env::var(&self.config.credential_environment)
                .context("decision credential unavailable")?;
            let provider = JevDecisionProvider::new(
                JEV_BASE_URL,
                &key,
                &self.config.model,
                Duration::from_millis(self.config.timeout_ms),
            )?;
            provider
                .evaluate(&request)
                .map_err(|code| anyhow::anyhow!(code))
        })();
        let raw = match response {
            Ok(raw) => raw,
            Err(_) => {
                self.budget.charge_unknown(&cell)?;
                return Ok(ProviderAnswer::Fallback {
                    reason: "provider_error",
                });
            }
        };
        let input = raw["usage"]["input_tokens"].as_u64();
        let output = raw["usage"]["output_tokens"].as_u64();
        let matches = raw["model"] == self.config.model;
        let mut answer = validate_response(point, &raw);
        let public = match &answer {
            ProviderAnswer::Choice {
                choice_id,
                evidence,
            } => json!({"model":raw["model"],"usage":{"input_tokens":input,"output_tokens":output},
                "answers":{"decision":{"type":"choice","choice":choice_id,"confidence":evidence["confidence"],"probabilities":evidence["probabilities"]}}}),
            _ => {
                json!({"model":raw["model"],"usage":{"input_tokens":input,"output_tokens":output}})
            }
        };
        let value = json!({"request_sha256":request_hash,"raw_response":public,
            "latency_ms":started.elapsed().as_millis(),"reserved_usd_micros":self.config.budget.validate()?,
            "estimated_cost_usd_micros":input.and_then(|i|self.config.budget.cost(i,0).ok())});
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(answer_path)?;
        writeln!(file, "{}", serde_json::to_string(&value)?)?;
        file.sync_all()?;
        match (input, output) {
            (Some(i), Some(o)) => {
                let response = ResponseEvidence {
                    cell,
                    finish_reason: FinishReason::Stop,
                    model_matches: matches,
                    input_tokens: i,
                    output_tokens: o,
                };
                if !response.within(&self.config.budget) {
                    answer = ProviderAnswer::Fallback {
                        reason: "provider_error",
                    };
                }
                self.budget.record_response(response)?;
            }
            _ => {
                self.budget.charge_unknown(&cell)?;
                answer = ProviderAnswer::Fallback {
                    reason: "provider_error",
                };
            }
        }
        Self::attach_accounting(&mut answer, &value);
        Ok(answer)
    }
}
impl DecisionProvider for MeteredDecisionProvider {
    fn decide(&self, point: &DecisionPoint) -> ProviderAnswer {
        match self.decide_metered(point) {
            Ok(answer) => answer,
            Err(_) => {
                // A crash between durable reservation and response accounting
                // never becomes a second send, even after requester restart.
                let cell = format!("{}_d{}", self.search_id, point.seq);
                if self
                    .budget
                    .snapshot()
                    .ok()
                    .is_some_and(|s| s.cells.get(&cell).is_some_and(|c| !c.is_settled()))
                {
                    let _ = self.budget.charge_unknown(&cell);
                }
                ProviderAnswer::Fallback {
                    reason: "provider_error",
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, MeteredDecisionProvider) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp");
        std::fs::create_dir_all(&root).unwrap();
        let dir = tempfile::tempdir_in(root).unwrap();
        let config = MeteredDecisionConfig {
            model: "jev-1.13.0".into(),
            timeout_ms: 1000,
            budget: BudgetPlan {
                max_calls: 1,
                input_token_cap: 65536,
                output_token_cap: 65536,
                input_price: 42000,
                output_price: 0,
                ceiling_usd_micros: 2753,
            },
            credential_environment: "ATO_TEST_ABSENT_DECISION_KEY".into(),
        };
        let budget = Arc::new(
            CallBudget::create(&dir.path().join("journal"), config.budget.clone()).unwrap(),
        );
        let provider =
            MeteredDecisionProvider::new(config, budget, "search", &dir.path().join("answers"))
                .unwrap();
        (dir, provider)
    }
    fn point() -> DecisionPoint {
        serde_json::from_value(
            json!({"seq":1,"default_choice_id":"a","expires_at":"2099-01-01T00:00:00Z",
            "choices":[{"choice_id":"a","action":{"kind":"stop","reason_class":"test"}}]}),
        )
        .unwrap()
    }
    #[test]
    fn singleton_bypasses_credential_and_reservation() {
        let (_dir, provider) = fixture();
        assert!(matches!(
            provider.decide(&point()),
            ProviderAnswer::Choice { .. }
        ));
        assert_eq!(provider.accounting().unwrap()["calls"], 0);
    }
    #[test]
    fn crash_after_reservation_is_charged_and_never_sent_again() {
        let (_dir, provider) = fixture();
        let mut p = point();
        let mut other = p.choices[0].clone();
        other.choice_id = "b".into();
        p.choices.push(other);
        let bytes = serde_json::to_vec(&decision_request(&provider.config.model, &p)).unwrap();
        provider
            .budget
            .reserve_request(RequestEvidence {
                cell: "search_d1".into(),
                proposal_request_sha256: hash(&bytes),
                provider_body_sha256: hash(&bytes),
                timeout_ms: 1000,
                proposal_request_bytes: bytes.len() as u64,
                provider_body_bytes: bytes.len() as u64,
            })
            .unwrap();
        assert!(matches!(
            provider.decide(&p),
            ProviderAnswer::Fallback { .. }
        ));
        assert!(provider.all_settled().unwrap());
        assert!(provider.budget.snapshot().unwrap().cells["search_d1"].charged_unknown);
        assert!(provider.budget.snapshot().unwrap().stopped);
        assert!(matches!(
            provider.decide(&p),
            ProviderAnswer::Fallback { .. }
        ));
        assert_eq!(provider.accounting().unwrap()["calls"], 1);
    }
}

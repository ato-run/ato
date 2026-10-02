//! Bounded JSON ABI for the common Rust receipt authority. No executor,
//! sandbox, source decoder or runtime is linked into this module.
use std::collections::BTreeMap;

use ato_formation::{
    authoring::{BOUND_CONTRACT_SCHEMA, BoundContract},
    browser::{BrowserContractV0, effective_contract_ref},
    receipt::{ReceiptRejection, VerifiedRouteAssignment, accept_verified_route},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ABI_VERSION: u32 = 1;
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;

/// These values come from the authenticated request at creation time, never
/// from a Runtime's receipt. Persist them with the request before issuing work.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenContract {
    pub base_contract: BoundContract,
    pub base_contract_ref: String,
    pub effective_contract_ref: String,
    pub browser_contract: Option<BrowserContractV0>,
}

impl FrozenContract {
    fn validate(&self) -> Result<(), ReceiptRejection> {
        let reject = |code, detail: &str| ReceiptRejection {
            code,
            detail: detail.into(),
        };
        if self.base_contract.schema != BOUND_CONTRACT_SCHEMA {
            return Err(reject(
                "contract_schema_unknown",
                "unsupported frozen Contract schema",
            ));
        }
        let actual = self
            .base_contract
            .contract_ref()
            .map_err(|e| ReceiptRejection {
                code: "contract_malformed",
                detail: e.to_string(),
            })?;
        if actual != self.base_contract_ref {
            return Err(reject(
                "contract_digest_mismatch",
                "frozen K does not match its content address",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        if self
            .base_contract
            .requirements
            .iter()
            .any(|r| !ids.insert(&r.id))
        {
            return Err(reject(
                "contract_duplicate_requirement",
                "frozen K repeats a requirement",
            ));
        }
        if let Some(browser) = &self.browser_contract {
            let expected =
                BrowserContractV0::from_prompt(&browser.original_prompt).map_err(|e| {
                    ReceiptRejection {
                        code: "browser_contract_malformed",
                        detail: e.to_string(),
                    }
                })?;
            if browser != &expected {
                return Err(reject(
                    "browser_contract_malformed",
                    "unsupported browser v0 normalization",
                ));
            }
        }
        if effective_contract_ref(&self.base_contract_ref, self.browser_contract.as_ref())
            != self.effective_contract_ref
        {
            return Err(reject(
                "request_effective_contract_inconsistent",
                "effective K differs from the frozen base and browser Contracts",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthorityRequest {
    ValidateContract {
        frozen: FrozenContract,
    },
    ValidateRetained {
        frozen: FrozenContract,
        derivation_ref: String,
        retained_ref: String,
        descriptor_json: String,
    },
    ValidateBuildRecord {
        frozen: FrozenContract,
        capsule_toml: String,
        source_closure_ref: String,
        derivation_ref: String,
        attempt_id: String,
        record_ref: String,
        manifest_json: String,
    },
    AcceptRoute {
        frozen: FrozenContract,
        request_id: String,
        authorized_derivations: Vec<String>,
        route: Value,
        attempt: Value,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Decision {
    Accepted,
    Rejected { code: String, detail: String },
}

fn check(request: AuthorityRequest) -> Result<(), ReceiptRejection> {
    match request {
        AuthorityRequest::ValidateContract { frozen } => frozen.validate(),
        AuthorityRequest::ValidateBuildRecord {
            frozen,
            capsule_toml,
            source_closure_ref,
            derivation_ref,
            attempt_id,
            record_ref,
            manifest_json,
        } => {
            frozen.validate()?;
            use ato_formation::{
                authoring::{BindingContext, bind},
                capsule_toml::parse_capsule_toml,
            };
            let invalid = || ReceiptRejection {
                code: "build_record_derivation",
                detail: "registered authoring does not match the assignment".into(),
            };
            let draft = parse_capsule_toml(&capsule_toml).map_err(|_| invalid())?;
            let (k, d) = bind(
                &draft,
                &BindingContext {
                    source_closure_ref: &source_closure_ref,
                },
            )
            .map_err(|_| invalid())?;
            if k != frozen.base_contract
                || d.derivation_ref().ok().as_ref() != Some(&derivation_ref)
            {
                return Err(invalid());
            }
            let manifest = ato_formation::build_record::BuildRecordManifest::parse(
                manifest_json.as_bytes(),
                &record_ref,
            )
            .map_err(|e| ReceiptRejection {
                code: e.0,
                detail: "build Record manifest rejected".into(),
            })?;
            manifest
                .match_assignment(
                    &frozen.effective_contract_ref,
                    &d,
                    &source_closure_ref,
                    &attempt_id,
                )
                .map_err(|e| ReceiptRejection {
                    code: e.0,
                    detail: "build Record assignment rejected".into(),
                })
        }
        AuthorityRequest::ValidateRetained {
            frozen,
            derivation_ref,
            retained_ref,
            descriptor_json,
        } => {
            frozen.validate()?;
            let descriptor = ato_formation::retained::RetainedCandidateV1::parse(
                descriptor_json.as_bytes(),
                &retained_ref,
            )
            .map_err(|e| ReceiptRejection {
                code: "retained_descriptor_invalid",
                detail: e.to_string(),
            })?;
            descriptor
                .match_assignment(&frozen.effective_contract_ref, &derivation_ref)
                .map_err(|e| ReceiptRejection {
                    code: "retained_assignment_mismatch",
                    detail: e.to_string(),
                })?;
            if descriptor.base_contract != frozen.base_contract
                || descriptor.browser_contract != frozen.browser_contract
            {
                return Err(ReceiptRejection {
                    code: "retained_contract_mismatch",
                    detail: "retained K differs from the frozen request".into(),
                });
            }
            Ok(())
        }
        AuthorityRequest::AcceptRoute {
            frozen,
            request_id,
            authorized_derivations,
            route,
            attempt,
        } => {
            frozen.validate()?;
            // The Coordinator always supplies one exact assignment, with no
            // legacy placement-only lookup. The common acceptance still checks
            // that all D/runtime/environment/attempt identities agree.
            if !route["attempt_id"].is_string() {
                return Err(ReceiptRejection {
                    code: "route_attempt_missing",
                    detail: "exact attempt id required".into(),
                });
            }
            let contracts: BTreeMap<_, _> = authorized_derivations
                .into_iter()
                .map(|d| (d, frozen.base_contract.clone()))
                .collect();
            accept_verified_route(
                &VerifiedRouteAssignment {
                    request_id: &request_id,
                    effective_contract_ref: &frozen.effective_contract_ref,
                    base_contract_ref: &frozen.base_contract_ref,
                    contracts: &contracts,
                    browser_contract: frozen.browser_contract.as_ref(),
                },
                &route,
                &[attempt],
            )
        }
    }
}

pub fn evaluate(bytes: &[u8]) -> Decision {
    if bytes.len() > MAX_INPUT_BYTES {
        return Decision::Rejected {
            code: "authority_input_too_large".into(),
            detail: "receipt authority input exceeds 1 MiB".into(),
        };
    }
    let request = match serde_json::from_slice(bytes) {
        Ok(request) => request,
        Err(error) => {
            return Decision::Rejected {
                code: "authority_input_malformed".into(),
                detail: error.to_string(),
            };
        }
    };
    match check(request) {
        Ok(()) => Decision::Accepted,
        Err(error) => Decision::Rejected {
            code: error.code.into(),
            detail: error.detail,
        },
    }
}

/// Search uses the same bounded memory transport, not the receipt decision ABI.
/// In particular a search decision can never be mistaken for an accepted K.
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum SearchRequest {
    AssembleExplorationSubmission {
        state: Box<ato_formation::search::SearchStateV1>,
        capsule_toml: String,
        attempt_id: String,
        receipt: Box<ato_formation::verify::ContractVerificationReceipt>,
    },
    SelectExplorationSubmission {
        state: Box<ato_formation::search::SearchStateV1>,
        submissions: Vec<ato_formation::exploration::ExplorationSubmission>,
    },
    /// Strict batch bytes are validated by Rust, never canonicalized by TS.
    CompileProposals {
        frozen: Box<ato_formation::search::FrozenSearchV1>,
        base_recipes: std::collections::BTreeMap<String, String>,
        batch_json: String,
        #[serde(default)]
        inspection_remaining: Option<u32>,
    },
    CompileGeneration {
        policy: ato_formation::generation::GenerationPolicy,
        base_capsule_toml: String,
        closure_ref: String,
        base_contract_ref: String,
        draft: ato_formation::generation::GenerationDraft,
    },
    FreezeSearch {
        frozen: Box<ato_formation::search::FrozenSearchV1>,
    },
    DecideSearch {
        state: Box<ato_formation::search::SearchStateV1>,
        placements: Vec<ato_formation::search::Placement>,
        now_ms: u64,
    },
    /// Stage 5a: judge a requester's answer against the point as opened.
    ValidateDecision {
        state: Box<ato_formation::search::SearchStateV1>,
        submission: ato_formation::decision::DecisionSubmission,
        /// The Coordinator's trusted time: a point past its deadline is closed.
        now_ms: u64,
    },
}
pub fn evaluate_search(bytes: &[u8]) -> Value {
    use ato_formation::search::*;
    let evaluate = || -> Result<Value, String> {
        if bytes.len() > MAX_INPUT_BYTES {
            return Err("search_input_too_large".into());
        }
        // A malformed generation draft can carry source or secret text. Never
        // echo serde's input-bearing diagnostics through the search ABI.
        let request: SearchRequest =
            serde_json::from_slice(bytes).map_err(|_| "search_input_invalid".to_owned())?;
        match request {
            SearchRequest::AssembleExplorationSubmission {
                mut state,
                capsule_toml,
                attempt_id,
                receipt,
            } => {
                use ato_formation::{
                    authoring::{BindingContext, bind},
                    capsule_toml::parse_capsule_toml,
                    exploration::{ExplorationSubmission, SubmissionStatus},
                };
                state.validate().map_err(|e| e.to_string())?;
                let source = state
                    .frozen
                    .initial_source
                    .as_ref()
                    .ok_or("exploration_source_required")?;
                let (contract, derivation) = bind(
                    &parse_capsule_toml(&capsule_toml).map_err(|e| e.to_string())?,
                    &BindingContext {
                        source_closure_ref: &source.closure_ref,
                    },
                )
                .map_err(|e| e.to_string())?;
                let derivation_ref = derivation.derivation_ref().map_err(|e| e.to_string())?;
                let matched = state
                    .attempts
                    .iter_mut()
                    .find(|a| {
                        a.attempt_id == attempt_id
                            && a.derivation_ref == derivation_ref
                            && a.status == DurableAttemptStatus::Pass
                            && a.record == Some(ExecutionRecord::Finished)
                    })
                    .ok_or("submission_attempt_mismatch")?;
                // Local validation only. Persisted acceptance comes ONLY from a
                // saved, revalidated submission; this does not mutate a row.
                matched.route_accepted = true;
                let submission = ExplorationSubmission {
                    status: SubmissionStatus::KReachedAwaitingAssessment,
                    contract,
                    contract_ref: state.frozen.contract_ref.clone(),
                    derivation,
                    derivation_ref,
                    attempt_id,
                    receipt: *receipt,
                };
                submission.validate(&state).map_err(|e| e.to_string())?;
                Ok(
                    serde_json::json!({"status":"exploration_submission_ready","submission_json":serde_jcs::to_string(&submission).map_err(|e| e.to_string())?}),
                )
            }
            SearchRequest::SelectExplorationSubmission { state, submissions } => {
                state.validate().map_err(|e| e.to_string())?;
                if submissions.len() > 256 {
                    return Err("exploration_submission_bounds".into());
                }
                let mut best: Option<ato_formation::exploration::ExplorationSubmission> = None;
                for submission in submissions {
                    submission.validate(&state).map_err(|e| e.to_string())?;
                    if best
                        .as_ref()
                        .is_none_or(|old| old.admits_reduction(&submission.derivation).is_ok())
                    {
                        best = Some(submission);
                    }
                }
                Ok(
                    serde_json::json!({"status":"exploration_submission_selected","submission":best}),
                )
            }
            SearchRequest::CompileProposals {
                frozen,
                base_recipes,
                batch_json,
                inspection_remaining,
            } => {
                use ato_formation::proposal::{
                    CandidateRegistry, ProducerOutput, ProducerProvenance, ProposalOutcome,
                };
                let output = ProducerOutput::new(
                    batch_json.into_bytes(),
                    ProducerProvenance {
                        provider: "requester".into(),
                        model: None,
                    },
                )
                .map_err(|e| e.0.to_owned())?;
                let mut registry = CandidateRegistry::new(&frozen).map_err(|e| e.0.to_owned())?;
                if let Some(remaining) = inspection_remaining {
                    registry = registry
                        .with_inspection_budget(remaining)
                        .map_err(|e| e.0.to_owned())?;
                }
                let outcomes = registry
                    .validate_batch(&base_recipes, &output)
                    .map_err(|e| e.0.to_owned())?;
                let outcomes: Vec<_> = outcomes
                    .into_iter()
                    .map(|outcome| match outcome {
                        ProposalOutcome::Admitted(candidate) => serde_json::json!({
                            "status": "admitted", "proposal_id": candidate.proposal_id(),
                            "derivation_ref": candidate.compiled().derivation_ref,
                            "capsule_toml": candidate.compiled().capsule_toml,
                            "derivation": candidate.compiled().derivation,
                            "candidate": candidate.candidate(),
                        }),
                        ProposalOutcome::Unsupported => {
                            serde_json::json!({"status": "unsupported"})
                        }
                        ProposalOutcome::Rejected(error) => {
                            serde_json::json!({"status": "rejected", "code": error.0})
                        }
                        ProposalOutcome::InspectionRequested(sources) => serde_json::json!({"status":"rejected","code":"source_inspection_requested","inspection_refs":sources}),
                    })
                    .collect();
                Ok(
                    serde_json::json!({"status": "proposals_validated", "outcomes": outcomes,
                    "decline_reasons": registry.decline_reasons()}),
                )
            }
            SearchRequest::CompileGeneration {
                policy,
                base_capsule_toml,
                closure_ref,
                base_contract_ref,
                draft,
            } => {
                let compiled = match ato_formation::generation::compile_generation(
                    &policy,
                    &base_capsule_toml,
                    &closure_ref,
                    &base_contract_ref,
                    &draft,
                ) {
                    Ok(compiled) => compiled,
                    Err(error) => {
                        return Ok(serde_json::json!({
                            "status": "rejected", "code": error.code(), "detail": error.code(),
                        }));
                    }
                };
                Ok(serde_json::json!({
                    "status": "generation_compiled",
                    "derivation_ref": compiled.derivation_ref,
                    "capsule_toml": compiled.capsule_toml,
                    "derivation": compiled.derivation,
                }))
            }
            SearchRequest::FreezeSearch { frozen } => {
                FrozenContract {
                    base_contract: frozen.base_contract.clone(),
                    base_contract_ref: frozen.base_contract_ref.clone(),
                    effective_contract_ref: frozen.contract_ref.clone(),
                    browser_contract: frozen.browser_contract.clone(),
                }
                .validate()
                .map_err(|e| e.detail)?;
                let canonical = frozen.canonical_bytes().map_err(|e| e.to_string())?;
                Ok(
                    serde_json::json!({"status":"frozen","canonical_json":String::from_utf8(canonical).unwrap()}),
                )
            }
            SearchRequest::DecideSearch {
                state,
                placements,
                now_ms,
            } => {
                let action = decide_next(&state, &placements, now_ms).map_err(|e| e.to_string())?;
                let canonical = state.canonical_bytes().map_err(|e| e.to_string())?;
                Ok(
                    serde_json::json!({"status":"search_decision","state_json":String::from_utf8(canonical).unwrap(),"events":events(&state,&action),"action":action}),
                )
            }
            SearchRequest::ValidateDecision {
                state,
                submission,
                now_ms,
            } => Ok(
                match ato_formation::decision::validate_decision(&state, &submission, now_ms) {
                    Ok(verdict) => {
                        serde_json::json!({"status":"decision_verdict","verdict":verdict})
                    }
                    Err(refusal) => {
                        serde_json::json!({"status":"decision_refused","code":refusal.code})
                    }
                },
            ),
        }
    };
    evaluate().unwrap_or_else(|detail|serde_json::json!({"status":"rejected","code":"search_state_invalid","detail":detail}))
}

#[cfg(test)]
mod generation_tests {
    use super::evaluate_search;
    use ato_formation::{
        authoring::{BindingContext, bind},
        capsule_toml::parse_capsule_toml,
    };
    use serde_json::{Value, json};

    fn request() -> Value {
        let text = include_str!(
            "../../../apps/formation-worker/fixtures/runtime-network/notes/capsule.toml"
        );
        let closure = format!("sha256:{}", "a".repeat(64));
        let (k, d) = bind(
            &parse_capsule_toml(text).unwrap(),
            &BindingContext {
                source_closure_ref: &closure,
            },
        )
        .unwrap();
        json!({
            "operation": "compile_generation",
            "policy": {
                "schema": "ato.formation-generation-policy/1",
                "base_derivation_ref": d.derivation_ref().unwrap(),
                "entrypoints": { "entry_a": "alternate.py" },
                "max_generations": 1, "timeout_ms": 5000
            },
            "base_capsule_toml": text,
            "closure_ref": closure,
            "base_contract_ref": k.contract_ref().unwrap(),
            "draft": {
                "schema": "ato.formation-derivation-draft/1",
                "operation": "python_script", "entrypoint_id": "entry_a"
            }
        })
    }

    #[test]
    fn compile_abi_returns_canonical_d_without_accepting_a_route() {
        let request = request();
        let response = evaluate_search(&serde_json::to_vec(&request).unwrap());
        assert_eq!(response["status"], "generation_compiled");
        let d: ato_formation::authoring::BoundDerivation =
            serde_json::from_value(response["derivation"].clone()).unwrap();
        assert_eq!(response["derivation_ref"], d.derivation_ref().unwrap());
        assert_ne!(
            response["derivation_ref"],
            request["policy"]["base_derivation_ref"]
        );
        let (k, rebound) = bind(
            &parse_capsule_toml(response["capsule_toml"].as_str().unwrap()).unwrap(),
            &BindingContext {
                source_closure_ref: request["closure_ref"].as_str().unwrap(),
            },
        )
        .unwrap();
        assert_eq!(rebound, d);
        assert_eq!(k.contract_ref().unwrap(), request["base_contract_ref"]);
        assert!(response.get("fully_satisfied").is_none());
    }

    #[test]
    fn compile_abi_reports_duplicates_and_rejects_arbitrary_drafts_without_leaking_them() {
        let mut request = request();
        request["policy"]["entrypoints"]["entry_a"] = "app.py".into();
        assert_eq!(
            evaluate_search(&serde_json::to_vec(&request).unwrap())["code"],
            "generation_duplicate"
        );
        request["draft"]["argv"] = json!(["SECRET_CANARY"]);
        let response = evaluate_search(&serde_json::to_vec(&request).unwrap());
        assert_eq!(response["status"], "rejected");
        assert!(!response.to_string().contains("SECRET_CANARY"));
    }
}

// Each JS call creates a fresh instance. Buffers remain owned by Rust for the
// lifetime of that instance. The host writes only the allocated input range,
// synchronously between prepare and evaluate; no Rust reference spans that write.
#[cfg(target_arch = "wasm32")]
mod abi {
    use std::cell::RefCell;
    thread_local! {
        static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn authority_abi_version() -> u32 {
        super::ABI_VERSION
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn authority_prepare(length: usize) -> usize {
        if length > super::MAX_INPUT_BYTES {
            return 0;
        }
        INPUT.with(|input| {
            let mut input = input.borrow_mut();
            input.resize(length, 0);
            input.as_mut_ptr() as usize
        })
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn authority_search() -> u64 {
        let decision = INPUT.with(|input| super::evaluate_search(&input.borrow()));
        let Ok(bytes) = serde_json::to_vec(&decision) else {
            return 0;
        };
        if bytes.len() > super::MAX_INPUT_BYTES {
            return 0;
        }
        OUTPUT.with(|output| {
            let mut output = output.borrow_mut();
            *output = bytes;
            ((output.len() as u64) << 32) | output.as_ptr() as u64
        })
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn authority_evaluate() -> u64 {
        let decision = INPUT.with(|input| super::evaluate(&input.borrow()));
        let Ok(bytes) = serde_json::to_vec(&decision) else {
            return 0;
        };
        OUTPUT.with(|output| {
            let mut output = output.borrow_mut();
            *output = bytes;
            ((output.len() as u64) << 32) | output.as_ptr() as u64
        })
    }
}

#[cfg(test)]
mod proposal_tests {
    use super::*;
    use ato_formation::{
        authoring::{BindingContext, bind},
        capsule_toml::parse_capsule_toml,
        search::SearchStateV1,
    };
    const BASE: &str = include_str!("../../../lib/formation/tests/fixtures/proposal-python.toml");
    fn request() -> Value {
        let mut state: SearchStateV1 = serde_json::from_str(include_str!(
            "../../../lib/formation/tests/fixtures/search-state/d1-failed.json"
        ))
        .unwrap();
        let closure = format!("sha256:{}", "a".repeat(64));
        let (k, d) = bind(
            &parse_capsule_toml(BASE).unwrap(),
            &BindingContext {
                source_closure_ref: &closure,
            },
        )
        .unwrap();
        state.frozen.base_contract_ref = k.contract_ref().unwrap();
        state.frozen.contract_ref = state.frozen.base_contract_ref.clone();
        state.frozen.base_contract = k;
        state.frozen.candidates.truncate(1);
        state.frozen.candidates[0].derivation_ref = d.derivation_ref().unwrap();
        state.frozen.candidates[0].materialization =
            ato_formation::search::CandidateInput::Source {
                closure_ref: closure.clone(),
                archive_digest: closure.clone(),
            };
        state.frozen.initial_source = Some(ato_formation::search::InitialSource {
            closure_ref: closure.clone(),
            archive_digest: closure,
        });
        state.frozen.policy.proposal=Some(serde_json::from_value(serde_json::json!({
            "modifiable_derivation_refs":[d.derivation_ref().unwrap()],"source_domain":{"entrypoints":{"e":"working.py"},"modules":{"m":"pkg.server"}}, "python_http_process":null,
            "policy":{"max_proposal_rounds":1,"max_proposals":4,"timeout_ms":5000,"allow_source_text":false,"max_source_bytes":0}
        })).unwrap());
        serde_json::json!({"operation":"compile_proposals","frozen":state.frozen,"base_recipes":{(d.derivation_ref().unwrap()):BASE},
            "batch_json":serde_json::json!({"schema":"ato.formation-proposal/1","proposals":[
                {"kind":"modify_derivation","base_derivation_ref":d.derivation_ref().unwrap(),"operations":[{"operation":"python_script@1","entrypoint_id":"e"}]},
                {"kind":"modify_derivation","base_derivation_ref":d.derivation_ref().unwrap(),"operations":[{"operation":"python_module@1","module_id":"m"}]},
                {"kind":"modify_derivation","base_derivation_ref":d.derivation_ref().unwrap(),"operations":[{"operation":"python_script@1","entrypoint_id":"e"}]},
                {"kind":"modify_derivation","base_derivation_ref":d.derivation_ref().unwrap(),"operations":[{"operation":"shell@1","secret":"do-not-echo"}]}
            ]}).to_string()})
    }
    #[test]
    fn shared_rust_abi_validates_mixed_batch_without_becoming_receipt_authority() {
        let request = request();
        let result = evaluate_search(&serde_json::to_vec(&request).unwrap());
        assert_eq!(result["status"], "proposals_validated");
        assert_eq!(result["outcomes"][0]["status"], "admitted");
        assert_eq!(result["outcomes"][1]["status"], "admitted");
        assert_eq!(result["outcomes"][2]["code"], "proposal_duplicate");
        assert_eq!(result["outcomes"][3]["status"], "rejected");
        assert!(!result.to_string().contains("do-not-echo"));
        assert!(result.get("fully_satisfied").is_none());
        // Compiler output is not accepted by the independent receipt ABI.
        assert!(matches!(
            evaluate(&serde_json::to_vec(&result).unwrap()),
            Decision::Rejected { .. }
        ));
        for outcome in result["outcomes"].as_array().unwrap().iter().take(2) {
            let (k, d) = bind(
                &parse_capsule_toml(outcome["capsule_toml"].as_str().unwrap()).unwrap(),
                &BindingContext {
                    source_closure_ref:
                        request["frozen"]["candidates"][0]["materialization"]["closure_ref"]
                            .as_str()
                            .unwrap(),
                },
            )
            .unwrap();
            assert_eq!(
                k.contract_ref().unwrap(),
                request["frozen"]["base_contract_ref"]
            );
            assert_eq!(d.derivation_ref().unwrap(), outcome["derivation_ref"]);
        }
    }
    #[test]
    fn base_free_abi_needs_initial_source_but_no_known_d_or_base_recipe() {
        let mut request = request();
        request["frozen"]["candidates"] = serde_json::json!([]);
        request["frozen"]["policy"]["proposal"]["modifiable_derivation_refs"] =
            serde_json::json!([]);
        request["frozen"]["policy"]["proposal"]["python_http_process"] =
            serde_json::json!({"python_version":"3.12.7","http_port":"app.http","guest_port":8000});
        request["base_recipes"] = serde_json::json!({});
        request["batch_json"] = serde_json::json!({"schema":"ato.formation-proposal/1","proposals":[{"kind":"propose_derivation","operations":[{"operation":"python_http_process@1","entrypoint_id":"e"}]}]}).to_string().into();
        let result = evaluate_search(&serde_json::to_vec(&request).unwrap());
        assert_eq!(result["outcomes"][0]["status"], "admitted", "{result}");
        let compiled = &result["outcomes"][0];
        let (k, d) = bind(
            &parse_capsule_toml(compiled["capsule_toml"].as_str().unwrap()).unwrap(),
            &BindingContext {
                source_closure_ref: request["frozen"]["initial_source"]["closure_ref"]
                    .as_str()
                    .unwrap(),
            },
        )
        .unwrap();
        assert_eq!(k.contract_ref().unwrap(), request["frozen"]["contract_ref"]);
        assert_eq!(d.derivation_ref().unwrap(), compiled["derivation_ref"]);
        assert!(matches!(
            evaluate(&serde_json::to_vec(&result).unwrap()),
            Decision::Rejected { .. }
        ));
        request["frozen"]
            .as_object_mut()
            .unwrap()
            .remove("initial_source");
        assert_eq!(
            evaluate_search(&serde_json::to_vec(&request).unwrap())["status"],
            "rejected"
        );
    }
    #[test]
    fn missing_policy_or_unknown_abi_field_rejects_without_echoing_input() {
        for remove_policy in [true, false] {
            let mut r = request();
            if remove_policy {
                r["frozen"]["policy"]
                    .as_object_mut()
                    .unwrap()
                    .remove("proposal");
            } else {
                r["secret"] = "do-not-echo".into();
            }
            let result = evaluate_search(&serde_json::to_vec(&r).unwrap());
            assert_eq!(result["status"], "rejected");
            assert!(!result.to_string().contains("do-not-echo"));
        }
    }
}

#[cfg(test)]
mod exploration_submission_tests {
    use super::*;
    use ato_formation::{
        authoring::{BindingContext, bind},
        capsule_toml::parse_capsule_toml,
        exploration::{ExplorationPolicy, FormationConfig},
        requirements::ExecutionRequirements,
        search::{DurableAttemptStatus, ExecutionRecord, SearchStateV1},
        verify::{
            ContractVerificationReceipt, RuntimeHttpObservation, RuntimeObservation, verify_runtime,
        },
    };
    use serde_json::json;
    const BASE: &str = include_str!("../../../lib/formation/tests/fixtures/proposal-python.toml");
    fn request() -> Value {
        // Synthetic evidence is only for ABI rejection/selection tests.
        let mut state: SearchStateV1 = serde_json::from_str(include_str!(
            "../../../lib/formation/tests/fixtures/search-state/d1-failed.json"
        ))
        .unwrap();
        let closure = format!("sha256:{}", "a".repeat(64));
        let (k, d) = bind(
            &parse_capsule_toml(BASE).unwrap(),
            &BindingContext {
                source_closure_ref: &closure,
            },
        )
        .unwrap();
        state.frozen.base_contract_ref = k.contract_ref().unwrap();
        state.frozen.contract_ref = state.frozen.base_contract_ref.clone();
        state.frozen.base_contract = k.clone();
        state.frozen.candidates.truncate(1);
        state.frozen.candidates[0].derivation_ref = d.derivation_ref().unwrap();
        state.frozen.candidates[0].materialization =
            ato_formation::search::CandidateInput::Source {
                closure_ref: closure.clone(),
                archive_digest: closure.clone(),
            };
        state.frozen.initial_source = Some(ato_formation::search::InitialSource {
            closure_ref: closure.clone(),
            archive_digest: closure.clone(),
        });
        state.frozen.policy.proposal=Some(serde_json::from_value(json!({"modifiable_derivation_refs":[],"source_domain":{"entrypoints":{"entry":"broken.py"},"modules":{}},"policy":{"max_proposal_rounds":1,"max_proposals":1,"timeout_ms":5000,"allow_source_text":false,"max_source_bytes":0}})).unwrap());
        state.frozen.policy.exploration = Some(ExplorationPolicy {
            formation: FormationConfig::default(),
            ceiling: ExecutionRequirements::default(),
            max_provider_calls: 3,
            max_inspections: 3,
            reasoning: None,
            max_provider_cost_usd_micros: 100000,
            max_provider_input_tokens: 90000,
            max_provider_output_tokens: 6144,
            max_network_transfer_bytes: 1048576,
            max_network_transfer_bytes_per_attempt: 524288,
            provider_configuration_ref: None,
            decision_provider_configuration_ref: None,
            provider_budget_binding_ref: None,
        });
        state.source_archive_bytes = Some(64);
        let a = &mut state.attempts[0];
        a.derivation_ref = d.derivation_ref().unwrap();
        a.status = DurableAttemptStatus::Pass;
        a.record = Some(ExecutionRecord::Finished);
        a.failure_code = None;
        let observations = RuntimeObservation {
            input_refs: [("workspace".into(), closure)].into(),
            http: vec![RuntimeHttpObservation::from_response(
                "app.http", "GET", "/", 200, b"ok",
            )],
            instance_snapshot_ref: None,
        };
        let mut receipt = ContractVerificationReceipt::from_attempt(
            &state.frozen.contract_ref,
            &a.derivation_ref,
            &k,
            &observations,
            verify_runtime(&k, &observations),
        );
        receipt.execution = Some(
            serde_json::from_value(json!({"realization":"process","attempt_id":a.attempt_id}))
                .unwrap(),
        );
        json!({"operation":"assemble_exploration_submission","state":state,"capsule_toml":BASE,"attempt_id":"attempt-d1","receipt":receipt})
    }
    #[test]
    fn same_k_fresh_receipt_submission_has_no_normal_authorization() {
        let r = request();
        let out = evaluate_search(&serde_json::to_vec(&r).unwrap());
        assert_eq!(out["status"], "exploration_submission_ready", "{out}");
        let submitted: Value =
            serde_json::from_str(out["submission_json"].as_str().unwrap()).unwrap();
        assert_eq!(submitted["status"], "k_reached_awaiting_assessment");
        assert_eq!(submitted["contract"], r["state"]["frozen"]["base_contract"]);
        assert_eq!(submitted["receipt"], r["receipt"]);
        for key in ["approved", "grant", "ceiling", "published", "deployed"] {
            assert!(submitted.get(key).is_none());
        }
        let mut state = r["state"].clone();
        state["attempts"][0]["route_accepted"] = json!(true);
        let selected=evaluate_search(&serde_json::to_vec(&json!({"operation":"select_exploration_submission","state":state,"submissions":[submitted.clone()]})).unwrap());
        assert_eq!(selected["submission"], submitted);
    }
    #[test]
    fn unverified_reduced_or_stale_receipt_is_never_submitted() {
        for mode in 0..4 {
            let mut r = request();
            match mode {
                0 => r["receipt"]["execution"]["attempt_id"] = json!("previous-attempt"),
                1 => r["receipt"]["fully_satisfied"] = json!(false),
                2 => r["state"]["attempts"][0]["status"] = json!("fail"),
                _ => r["receipt"]["derivation_ref"] = json!(format!("sha256:{}", "c".repeat(64))),
            }
            assert_eq!(
                evaluate_search(&serde_json::to_vec(&r).unwrap())["status"],
                "rejected"
            );
        }
    }
}

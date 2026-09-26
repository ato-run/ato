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
        let request: SearchRequest = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        match request {
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

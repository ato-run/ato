//! Run the existing portable verifier. Only Ato can create this evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Result, ensure};
use ato_formation::browser::{BrowserBudget, BrowserVerdict, BrowserVerificationReceipt};
use ato_formation::request::{AttemptStatus, OutcomeState};
use ato_formation::verify::{ReceiptOutcome, VerificationTargetKind};
use ato_portable_application::portable_run::{
    NativeSourceConsent, PortableBundleExecutor, PortableRunContext, run_authorization,
};
use ato_portable_application::{
    PortableRealizationKind, bundle_sha256, resolve_application_bindings,
};
use ato_runtime_attempt::attempt::{AttemptRequest, Continuation, ReceiptContext, run_attempt};
use ato_runtime_attempt::browser_verify::{BrowserVerification, BrowserVerifierCommand};
use ato_runtime_attempt::build_sandbox::NetworkPolicy;
use ato_runtime_attempt::control::ExecutionControl;
use ato_runtime_attempt::journal::{AttemptJournal, AttemptRecordState};
use ato_runtime_attempt::realize::StopClass;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Entry, Phase, Plan, Store, browser_models};
use crate::{ato_home, browser_verifier_command, hub, unix_millis};

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Verdict {
    Pass,
    Fail,
    Inconclusive,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Attempt {
    attempt_id: String,
    derivation_ref: String,
    base_contract_ref: String,
    effective_contract_ref: String,
    artifact_sha256: String,
    host: String,
    runtime_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_version: Option<String>,
    verdict: Verdict,
    pub(super) cleanup_confirmed: bool,
    execution: AttemptRecordState,
    issue: Option<String>,
    mechanical_receipt_ref: Option<String>,
    mechanical: Vec<Condition>,
    browser: Option<BrowserProof>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Condition {
    id: String,
    verdict: Verdict,
    evidence_ref: Option<String>,
    observed_http: Option<ObservedHttp>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservedHttp {
    status: u16,
    body_sha256: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserProof {
    browser_contract_ref: String,
    attempt_id: String,
    receipt_ref: String,
    verdict: Verdict,
    verifier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    browser_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    issue: Option<String>,
    agent_model: Option<String>,
    judge_model: Option<String>,
    containment: Value,
    actions: Vec<ActionEvidence>,
    events: Vec<Event>,
    criteria: Vec<BrowserCriterion>,
    evidence: Vec<BrowserEvidence>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    kind: String,
    sequence: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionEvidence {
    kind: String,
    position: usize,
    digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserCriterion {
    id: String,
    verdict: Verdict,
    model: Option<String>,
    decision: Option<String>,
    evidence: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserEvidence {
    id: String,
    kind: String,
    sequence: u32,
    digest: String,
}

impl Attempt {
    pub(super) fn derivation_ref(&self) -> &str {
        &self.derivation_ref
    }

    pub(super) fn valid_pass(&self, plan: &Plan) -> bool {
        if self.verdict != Verdict::Pass
            || self.execution != AttemptRecordState::Finished
            || !self.cleanup_confirmed
            || self.base_contract_ref != plan.base_contract_ref
            || self.effective_contract_ref != plan.effective_contract_ref
            || self.artifact_sha256 != plan.artifact_sha256
            || self.host != plan.target_host
            || !plan.derivations.contains(&self.derivation_ref)
            || self.mechanical_receipt_ref.is_none()
        {
            return false;
        }
        let expected = plan
            .checks
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect::<BTreeSet<_>>();
        let observed = self
            .mechanical
            .iter()
            .filter(|item| item.verdict == Verdict::Pass)
            .map(|item| item.id.as_str())
            .collect::<BTreeSet<_>>();
        if expected != observed {
            return false;
        }
        match (&plan.browser_contract, &self.browser) {
            (None, None) => true,
            (Some(contract), Some(browser)) => {
                browser.browser_contract_ref == contract.contract_ref()
                    && browser.attempt_id == self.attempt_id
                    && browser.verdict == Verdict::Pass
                    && browser.containment["containment"] == "bwrap"
                    && browser.agent_model.is_some()
                    && browser.judge_model.is_some()
                    && browser.agent_model != browser.judge_model
                    && plan.browser_models.as_ref().is_some_and(|models| {
                        browser.agent_model.as_deref() == Some(models.browser_driver.as_str())
                            && browser.judge_model.as_deref() == Some(models.judge.as_str())
                    })
                    && ordered_browser_evidence(browser)
                    && contract
                        .criteria
                        .iter()
                        .filter(|item| item.required)
                        .all(|required| {
                            browser.criteria.iter().any(|result| {
                                result.id == required.id
                                    && result.verdict == Verdict::Pass
                                    && result.decision.as_deref() == Some("complete")
                                    && result.model == browser.judge_model
                                    && result.evidence.iter().any(|id| {
                                        browser.evidence.iter().any(|item| {
                                            item.id == *id && item.kind == "browser_snapshot"
                                        })
                                    })
                            })
                        })
            }
            _ => false,
        }
    }

    fn unavailable(entry: &Entry, id: String, route: &str, host: &str, issue: &str) -> Self {
        Self {
            attempt_id: id,
            derivation_ref: route.to_owned(),
            base_contract_ref: entry.plan.base_contract_ref.clone(),
            effective_contract_ref: entry.plan.effective_contract_ref.clone(),
            artifact_sha256: entry.plan.artifact_sha256.clone(),
            host: host.to_owned(),
            runtime_id: "local".into(),
            runtime_version: None,
            verdict: Verdict::Inconclusive,
            cleanup_confirmed: true,
            execution: AttemptRecordState::NotStarted,
            issue: Some(issue.into()),
            mechanical_receipt_ref: None,
            mechanical: Vec::new(),
            browser: None,
        }
    }
}

fn ordered_browser_evidence(browser: &BrowserProof) -> bool {
    let sequences = browser
        .evidence
        .iter()
        .map(|item| item.sequence)
        .chain(browser.events.iter().map(|event| event.sequence))
        .collect::<BTreeSet<_>>();
    let ids = browser
        .evidence
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    sequences.len() == browser.evidence.len() + browser.events.len()
        && ids.len() == browser.evidence.len()
        && browser.criteria.iter().all(|criterion| {
            let mut previous = 0;
            criterion.evidence.iter().all(|reference| {
                if let Some(item) = browser.evidence.iter().find(|item| &item.id == reference)
                    && item.sequence > previous
                {
                    previous = item.sequence;
                    true
                } else {
                    false
                }
            })
        })
}

pub(crate) fn worker(id: &str) -> Result<()> {
    let store = Store::open()?;
    let _owner = store.job(id)?;
    {
        let _lock = store.edit(id)?;
        let mut entry = store.read(id)?;
        if entry.phase != Phase::Queued {
            return Ok(());
        }
        entry.phase = Phase::Verifying;
        entry.changed_at_ms = unix_millis();
        store.save(&entry)?;
    }
    let directory = store.directory(id)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let cancellation = Arc::clone(&cancelled);
    let done_reading = Arc::clone(&done);
    let marker = directory.join("cancel.json");
    let polling = std::thread::Builder::new().spawn(move || {
        while !done_reading.load(Ordering::Relaxed) {
            if marker.exists() {
                cancellation.store(true, Ordering::Relaxed);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })?;
    let verified = verify_job(&store, id, &directory, &cancelled);
    done.store(true, Ordering::Relaxed);
    let _ = polling.join();
    let _lock = store.edit(id)?;
    let mut entry = store.read(id)?;
    if verified.is_err() {
        entry.phase = Phase::Inconclusive;
        entry.issue = Some("verification_worker_failed".into());
    } else if entry
        .attempts
        .iter()
        .any(|attempt| !attempt.cleanup_confirmed)
    {
        entry.phase = Phase::Inconclusive;
        entry.issue = Some("verification_cleanup_unconfirmed".into());
    } else if cancelled.load(Ordering::Relaxed) || directory.join("cancel.json").exists() {
        entry.phase = Phase::Cancelled;
        entry.issue = Some("verification_cancelled".into());
    } else if entry
        .attempts
        .iter()
        .any(|attempt| attempt.valid_pass(&entry.plan))
    {
        entry.phase = Phase::Ready;
    } else if entry
        .attempts
        .iter()
        .any(|attempt| attempt.verdict == Verdict::Fail)
    {
        entry.phase = Phase::Unmet;
        entry.issue = Some("no_route_met_the_frozen_conditions".into());
    } else {
        entry.phase = Phase::Inconclusive;
        entry.issue = Some("no_confirmed_verification_within_budget".into());
    }
    entry.changed_at_ms = unix_millis();
    store.save(&entry)
}

fn verify_job(store: &Store, id: &str, directory: &Path, cancelled: &AtomicBool) -> Result<()> {
    let entry = store.read(id)?;
    let bytes = store.artifact(&entry)?;
    let formed = hub::validate_formed_bundle_document(&bytes)?;
    ensure!(
        formed.contract_ref == entry.plan.base_contract_ref
            && formed.bundle.index.derivations == entry.plan.derivations,
        "The plan no longer names the validated bundle"
    );
    let profile = ato_formation_worker::local::probe_local_runtime();
    let host = ato_formation_worker::local::host_triple();
    let bound = unix_millis().saturating_add(entry.plan.budget_ms);
    let control = ExecutionControl::new(bound);
    let journal = AttemptJournal::new(directory.join("attempt-records"));
    let shim = std::env::current_exe()?.canonicalize()?;
    let home = ato_home()?;
    let app_cache = directory.join("verification-cache");
    let context = PortableRunContext {
        cache_root: &app_cache,
        sandbox_shim: &shim,
        sandbox_subcommand: "__portable-sandbox-exec",
        host: None,
        native_local_override: false,
        ato_home: &home,
        native_source_consent: NativeSourceConsent::Unavailable,
    };
    for (index, route) in formed
        .derivations
        .iter()
        .take(entry.plan.max_attempts)
        .enumerate()
    {
        if cancelled.load(Ordering::Relaxed)
            || directory.join("cancel.json").exists()
            || unix_millis() >= bound
        {
            break;
        }
        let attempt_id = format!("publication-{id}-{}", index + 1);
        let route_id = route.derivation_ref.as_str();
        let mut recorded = Attempt::unavailable(
            &entry,
            attempt_id.clone(),
            route_id,
            &host,
            "verification_not_started",
        );
        let target_matches = host == entry.plan.target_host;
        let browser = if target_matches {
            make_browser(&entry.plan, directory, &attempt_id)
        } else {
            None
        };
        let bindings = resolve_application_bindings(&route.application, &BTreeMap::new());
        if !target_matches {
            recorded.issue = Some("requested_verification_host_is_unavailable".into());
        } else if bindings.is_err() {
            recorded.issue =
                Some("verification_requires_binding_values_no_private_inputs_are_accepted".into());
        } else if !route.model_sets.is_empty() {
            recorded.issue = Some("verification_model_set_delivery_is_unavailable".into());
        } else if route
            .derivation
            .requirements
            .network
            .iter()
            .any(|need| need.phase == ato_formation::requirements::ExecutionPhase::Runtime)
        {
            recorded.issue =
                Some("external_service_access_is_not_in_this_verification_scope".into());
        } else if matches!(
            route.realization,
            PortableRealizationKind::OciContainer | PortableRealizationKind::OciServiceGroup
        ) && !formed.bundle.portability.as_ref().is_some_and(|transport| {
            matches!(
                transport.profile,
                ato_objects::PortableDependencyProfile::Offline
            )
        }) {
            recorded.issue = Some("oci_verification_requires_an_offline_bundle".into());
        } else if entry.plan.browser_contract.is_some()
            && browser
                .as_ref()
                .is_none_or(|configuration| configuration.verifier.is_none())
        {
            recorded.issue = Some("contained_browser_verification_is_not_configured".into());
        } else {
            let bindings = bindings?;
            let runtime = directory.join("scratch").join(&attempt_id);
            fs::create_dir_all(&runtime)?;
            let spec = route.attempt_spec(None);
            let outcome = run_attempt(
                &AttemptRequest {
                    request_id: id,
                    attempt_id: &attempt_id,
                    label: "publication",
                    spec: &spec,
                    contract_ref: &entry.plan.effective_contract_ref,
                    runtime_id: "local",
                    profile: &profile,
                    authorization: run_authorization(route, NativeSourceConsent::Unavailable),
                    network: NetworkPolicy::Denied,
                    browser: browser.as_ref(),
                    attempt_root: &runtime,
                    continuation: Continuation::Stop,
                    receipt: ReceiptContext {
                        target: VerificationTargetKind::CliLocal,
                        bundle_sha256: Some(&entry.plan.artifact_sha256),
                        portability_profile: None,
                        run_id: Some(&attempt_id),
                    },
                    interrupt: Some(cancelled),
                    control: Some(&control),
                },
                &PortableBundleExecutor {
                    bundle: &formed.bundle,
                    validated: route,
                    runtime_root: &runtime,
                    static_state: None,
                    filesystem_state: None,
                    binding_environment: &bindings,
                    context: &context,
                },
                &journal,
            );
            recorded.execution = outcome.attempt_record;
            recorded.cleanup_confirmed = match outcome.stop {
                Some(StopClass::Confirmed) => recorded.execution == AttemptRecordState::Finished,
                None => {
                    recorded.execution == AttemptRecordState::NotStarted
                        || recorded.execution == AttemptRecordState::Finished
                            && outcome.attempt.outcomes.cleanup.state == OutcomeState::NotApplicable
                }
                _ => false,
            };
            recorded.issue = outcome.attempt.failure.as_ref().map(|error| {
                format!(
                    "{}:{}",
                    safe_identifier(&error.stage),
                    safe_identifier(&error.code)
                )
            });
            if let Some(receipt) = &outcome.attempt.receipt {
                ensure!(
                    receipt.bundle_sha256.as_deref() == Some(entry.plan.artifact_sha256.as_str())
                        && receipt.contract_ref == entry.plan.base_contract_ref
                        && receipt.derivation_ref == route_id
                        && receipt
                            .execution
                            .as_ref()
                            .and_then(|execution| execution.attempt_id.as_deref())
                            == Some(attempt_id.as_str()),
                    "Runtime receipt does not match the fixed attempt"
                );
                recorded.mechanical_receipt_ref = Some(bundle_sha256(&receipt.canonical_bytes()?));
                recorded.runtime_version = receipt
                    .execution
                    .as_ref()
                    .and_then(|execution| execution.runtime_version.as_ref())
                    .filter(|value| super::limited_text(value, 128))
                    .cloned();
                recorded.mechanical = receipt
                    .observations
                    .iter()
                    .map(|observation| {
                        let verdict = match observation.outcome {
                            ReceiptOutcome::Satisfied => Verdict::Pass,
                            ReceiptOutcome::Failed if observation.evidence.is_some() => {
                                Verdict::Fail
                            }
                            _ => Verdict::Inconclusive,
                        };
                        Ok(Condition {
                            id: observation.id.clone(),
                            verdict,
                            evidence_ref: observation.evidence.as_ref().map(digest).transpose()?,
                            observed_http: observation.evidence.as_ref().and_then(|evidence| {
                                Some(ObservedHttp {
                                    status: evidence.status?,
                                    body_sha256: evidence.body_sha256.clone(),
                                })
                            }),
                        })
                    })
                    .collect::<Result<_>>()?;
            }
            if let Some(receipt) = &outcome.attempt.browser_verification {
                ensure!(
                    receipt.target.attempt_id.as_deref() == Some(attempt_id.as_str())
                        && receipt.target.runtime_id == "local"
                        && entry
                            .plan
                            .browser_contract
                            .as_ref()
                            .is_some_and(
                                |contract| contract.contract_ref() == receipt.browser_contract_ref
                            ),
                    "Browser evidence does not match the fixed attempt"
                );
                recorded.browser = Some(browser_proof(receipt, &attempt_id)?);
            }
            let has_failure = recorded
                .mechanical
                .iter()
                .any(|condition| condition.verdict == Verdict::Fail)
                || recorded
                    .browser
                    .as_ref()
                    .is_some_and(|proof| proof.verdict == Verdict::Fail);
            let verified = outcome.attempt.status == AttemptStatus::Verified
                && recorded.cleanup_confirmed
                && !cancelled.load(Ordering::Relaxed);
            recorded.verdict = if verified {
                Verdict::Pass
            } else if has_failure {
                Verdict::Fail
            } else {
                Verdict::Inconclusive
            };
            if recorded.verdict == Verdict::Pass && !recorded.valid_pass(&entry.plan) {
                recorded.verdict = Verdict::Inconclusive;
                recorded.issue = Some("mandatory_evidence_unavailable".into());
            }
            if recorded.cleanup_confirmed {
                // The common executor already confirmed cessation. Remove only
                // this preparation's now-unused attempt scratch.
                if let Err(error) = fs::remove_dir_all(&runtime)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    recorded.cleanup_confirmed = false;
                    recorded.verdict = Verdict::Inconclusive;
                    recorded.issue = Some("verification_scratch_retained".into());
                }
            }
        }
        let keep_searching = recorded.cleanup_confirmed;
        let _lock = store.edit(id)?;
        let mut latest = store.read(id)?;
        latest.attempts.push(recorded);
        store.save(&latest)?;
        if !keep_searching {
            break;
        }
    }
    Ok(())
}

fn digest(data: &impl Serialize) -> Result<String> {
    Ok(bundle_sha256(&serde_jcs::to_vec(data)?))
}

fn safe_identifier(value: &str) -> String {
    if value.len() <= 128
        && !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
    {
        value.to_owned()
    } else {
        "unavailable".into()
    }
}

fn browser_verdict(value: BrowserVerdict) -> Verdict {
    match value {
        BrowserVerdict::Pass => Verdict::Pass,
        BrowserVerdict::Fail => Verdict::Fail,
        BrowserVerdict::Inconclusive => Verdict::Inconclusive,
    }
}

fn browser_proof(receipt: &BrowserVerificationReceipt, id: &str) -> Result<BrowserProof> {
    let containment = serde_json::to_value(&receipt.containment)?;
    let boundary = receipt
        .observed_events
        .iter()
        .any(|event| matches!(event.kind.as_str(), "origin_violation" | "blocked_request"));
    let verdict = if boundary || containment["containment"] != "bwrap" {
        Verdict::Inconclusive
    } else {
        browser_verdict(receipt.overall)
    };
    Ok(BrowserProof {
        browser_contract_ref: receipt.browser_contract_ref.clone(),
        attempt_id: id.to_owned(),
        receipt_ref: digest(receipt)?,
        verdict,
        verifier: safe_identifier(&receipt.verifier.verifier),
        browser_version: receipt
            .verifier
            .browser_version
            .as_deref()
            .map(safe_identifier),
        issue: receipt.reason.as_deref().and_then(|reason| {
            let code = reason.split(':').next()?;
            let code = safe_identifier(code);
            (code != "unavailable").then_some(code)
        }),
        agent_model: receipt.verifier.agent_model.as_deref().map(safe_identifier),
        judge_model: receipt.verifier.judge_model.as_deref().map(safe_identifier),
        containment: json!({"containment": if containment["containment"] == "bwrap" {"bwrap"} else {"unavailable"}}),
        actions: receipt
            .actions
            .iter()
            .enumerate()
            .map(|(position, action)| {
                Ok(ActionEvidence {
                    kind: safe_identifier(&action.kind),
                    position,
                    digest: digest(action)?,
                })
            })
            .collect::<Result<_>>()?,
        events: receipt
            .observed_events
            .iter()
            .map(|event| Event {
                kind: safe_identifier(&event.kind),
                sequence: event.sequence,
            })
            .collect(),
        criteria: receipt
            .criteria
            .iter()
            .map(|result| BrowserCriterion {
                id: result.id.clone(),
                verdict: browser_verdict(result.verdict),
                model: result
                    .decision
                    .as_ref()
                    .map(|value| safe_identifier(&value.model)),
                decision: result
                    .decision
                    .as_ref()
                    .map(|value| safe_identifier(&value.choice)),
                evidence: result.evidence_refs.clone(),
            })
            .collect(),
        evidence: receipt
            .evidence
            .iter()
            .map(|item| {
                Ok(BrowserEvidence {
                    id: item.id.clone(),
                    kind: item.kind.clone(),
                    sequence: item.sequence,
                    digest: digest(item)?,
                })
            })
            .collect::<Result<_>>()?,
    })
}

fn make_browser(plan: &Plan, directory: &Path, attempt_id: &str) -> Option<BrowserVerification> {
    let contract = plan.browser_contract.clone()?;
    let models = browser_models(plan.browser_interactive).ok()?;
    if plan.browser_models.as_ref() != Some(&models) {
        return None;
    }
    let dir = std::env::var_os("ATO_PUBLICATION_BROWSER_VERIFIER").map(PathBuf::from);
    let chrome = std::env::var_os("ATO_BROWSER_CHROME_PATH").map(PathBuf::from);
    let node = std::env::var("ATO_PUBLICATION_NODE").unwrap_or_else(|_| "node".into());
    let verifier = browser_verifier_command(dir, &node, chrome, false)
        .ok()
        .flatten()
        .filter(|command| command.is_contained() && command.usable())
        .map(|command| match command {
            BrowserVerifierCommand::Contained(spec) if plan.browser_interactive => {
                BrowserVerifierCommand::Interactive {
                    spec,
                    directory: directory.join("browser").join(attempt_id),
                    cancel_file: directory.join("cancel.json"),
                    ollama_judge: models.ollama_judge,
                }
            }
            configured => configured,
        });
    Some(BrowserVerification {
        contract,
        verifier,
        budget: BrowserBudget::default(),
    })
}

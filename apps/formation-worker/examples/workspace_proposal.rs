//! Formation 6b-B local acceptance driver: bounded workspace inventory →
//! owner authorization → FixedCandidateProducer (preregistered opaque ID) →
//! production ProposalValidator/CandidateRegistry → canonical D → the existing
//! local Formation path (real Runtime + Verifier). No model, no Coordinator.
//!
//! The producer sees only the production ProposalRequest projection. The
//! admitted recipe is executed as an authored D under the explicitly given
//! local network policy; the proposal carries no network or Runtime choice.
use anyhow::{Context, Result, bail, ensure};
use ato_formation::{
    authoring::{BindingContext, bind},
    failure::FormationFailure,
    preset::{AppPreset, synthesize_authoring},
    proposal::*,
    request::*,
    search::FrozenSearchV1,
    source::{DownloadedArchive, SourceError, SourceLimits},
    workspace::inventory,
};
use ato_formation_worker::{
    local::{self, LocalFormation},
    sandbox::BuildLimits,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 8,
        "archive expected_digest work_root shim denied|dependency_resolution selection|none result.json"
    );
    let (archive, digest, root) = (&args[1], &args[2], PathBuf::from(&args[3]));
    let network = match args[5].as_str() {
        "denied" => FormationNetworkPolicy::Denied,
        "dependency_resolution" => FormationNetworkPolicy::DependencyResolution,
        other => bail!("unknown network policy {other}"),
    };
    let selection = (args[6] != "none").then(|| args[6].clone());
    let bytes = std::fs::read(archive)?;
    let limits = SourceLimits::default();
    // Independent verified extraction for the inventory (Ato-side facts only).
    let verified = DownloadedArchive::new(bytes.clone())
        .verify_archive_digest(digest)?
        .verify_tree_digest(None, limits)?;
    let closure = verified.closure_ref("")?.as_str().to_owned();
    let tree = verified.materialize(&root.join("inventory-tree"), "", limits)?;
    let mut record = json!({"schema":"ato.formation-workspace-acceptance/1","model_calls":0,
        "candidate_producer":"fixed","closure_ref":closure,"network":args[5],"selection":selection});
    let facts = match inventory(&tree) {
        Ok(facts) => facts,
        Err(error) => {
            record["inventory"] = json!({"refused":error.code(),"detail":error.to_string()});
            record["terminal"] = json!(error.code());
            return write(&args[7], &record);
        }
    };
    record["inventory"] = serde_json::to_value(&facts)?;
    let Some(domain) = facts.authorization() else {
        // No authorized ID exists, so the operation is not published and the
        // producer is not invoked.
        record["operation_published"] = json!(false);
        record["terminal"] = json!(match &facts.installation {
            ato_formation::workspace::Installation::Refused { code } => code.clone(),
            _ => "workspace_no_static_candidate".into(),
        });
        return write(&args[7], &record);
    };
    record["operation_published"] = json!(true);
    // K: the unchanged static-surface template, frozen before any proposal.
    let (k, _) = bind(
        &synthesize_authoring(AppPreset::NodeStatic),
        &BindingContext {
            source_closure_ref: &closure,
        },
    )?;
    let contract_ref = k.contract_ref()?;
    let authorization = ProposalAuthorization {
        execution_plan: None,
        modifiable_derivation_refs: vec![],
        source_domain: SourceDomain {
            entrypoints: BTreeMap::new(),
            modules: BTreeMap::new(),
        },
        python_http_process: None,
        node_static_workspace: Some(domain),
        policy: CandidateProducerPolicy {
            max_proposal_rounds: 1,
            max_proposals: 1,
            timeout_ms: 30_000,
            allow_source_text: false,
            max_source_bytes: 0,
        },
    };
    let frozen: FrozenSearchV1 = serde_json::from_value(json!({
        "contract_ref":contract_ref,"base_contract_ref":contract_ref,"base_contract":k,
        "browser_contract":null,"candidates":[],
        "initial_source":{"closure_ref":closure,"archive_digest":digest},
        "policy":{"mode":"first_pass","runtime_constraint":{"kind":"exact","runtime_id":"local"},
            "network":"denied","allow_managed":false,"bindings":{},
            "budget":{"max_attempts":4,"deadline_seconds":3600,"max_transfer_bytes":1,
                "max_expanded_bytes":1,"max_stored_bytes":1},
            "proposal":authorization}}))?;
    let request = ProposalRequest {
        schema: PROPOSAL_REQUEST_SCHEMA.into(),
        search_id: "workspace-acceptance".into(),
        frozen_contract: frozen.base_contract.clone(),
        runtime_constraint: frozen.policy.runtime_constraint.clone(),
        known_derivations: vec![],
        failure_evidence: vec![],
        inspection_evidence: vec![],
        operation_catalog: authorization.catalog()?,
        remaining_budget: ProposalBudget {
            rounds_remaining: 1,
            max_proposals: 1,
            timeout_ms: 30_000,
            attempts_remaining: 4,
        },
    };
    record["provider_request"] = serde_json::to_value(&request)?;
    let Some(selection) = selection else {
        record["terminal"] = json!("no_preregistered_selection");
        return write(&args[7], &record);
    };
    let batch = json!({"schema":PROPOSAL_SCHEMA,"proposals":[{"kind":"propose_derivation",
        "operations":[{"operation":"node_static_workspace@1","workspace_id":selection}]}]});
    let producer = FixedCandidateProducer {
        output: ProducerOutput::new(
            serde_json::to_vec(&batch)?,
            ProducerProvenance {
                provider: "fixed".into(),
                model: None,
            },
        )?,
    };
    let output = producer
        .propose(&request)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    record["producer_output"] = serde_json::from_slice::<Value>(output.raw())?;
    let mut registry = CandidateRegistry::new(&frozen)?;
    let outcomes = registry.validate_batch(&BTreeMap::new(), &output)?;
    let [ProposalOutcome::Admitted(candidate)] = outcomes.as_slice() else {
        record["proposal_outcomes"] = json!(format!("{outcomes:?}"));
        record["terminal"] = json!("proposal_not_admitted");
        return write(&args[7], &record);
    };
    record["proposal"] = json!({"proposal_id":candidate.proposal_id(),
        "derivation_ref":candidate.compiled().derivation_ref,
        "base_contract_ref":candidate.compiled().base_contract_ref,
        "capsule_toml":candidate.compiled().capsule_toml,
        "search_candidate":candidate.candidate()});
    // W11: compilation alone is not K. Only the Runtime + Verifier path below decides.
    let formation = FormationRequest {
        initial_condition: InitialCondition::Archive {
            bytes,
            expected_digest: digest.clone(),
        },
        contract: ContractSource::Authored {
            toml: candidate.compiled().capsule_toml.clone(),
        },
        runtime: RuntimeConstraint::Exact {
            runtime_id: "local".into(),
        },
        policy: FormationPolicy { network },
        budget: SearchBudget { max_attempts: 1 },
        browser_contract: None,
    };
    let env = LocalFormation {
        work_root: root.join("work"),
        out_dir: root.join("out"),
        shim: PathBuf::from(&args[4]),
        limits: BuildLimits::default(),
        source_limits: limits,
        browser_verifier: None,
        browser_budget: Default::default(),
    };
    let profile = local::probe_local_runtime();
    record["runtime_facts"] = serde_json::to_value(&profile.capabilities)?;
    record["result"] = match local::run(&formation, &env) {
        Ok(result) => {
            let value = serde_json::to_value(&result)?;
            for attempt in value["attempts"].as_array().into_iter().flatten() {
                if let Some(seen) = attempt["contract_ref"].as_str() {
                    ensure!(seen == contract_ref, "executed K differs from frozen K");
                }
                if let Some(seen) = attempt["derivation_ref"].as_str() {
                    ensure!(
                        seen == candidate.compiled().derivation_ref,
                        "executed D differs from admitted D"
                    );
                }
            }
            json!({"status":"formation_result","result":value})
        }
        Err(error) => {
            if let Some(f) = error.downcast_ref::<FormationFailure>() {
                json!({"status":"typed_terminal_error","stage":f.stage.as_str(),"code":f.code})
            } else if let Some(f) = error.downcast_ref::<SourceError>() {
                json!({"status":"typed_terminal_error","stage":"source","code":f.code()})
            } else {
                json!({"status":"unclassified_error","detail":format!("{error:#}")})
            }
        }
    };
    write(&args[7], &record)
}

fn write(path: &str, record: &Value) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(record)?).context("write record")
}

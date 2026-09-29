//! Read-only input, existing local Formation path. No producer/provider APIs.
use anyhow::{Context, Result};
use ato_formation::{
    failure::FormationFailure,
    request::*,
    source::{SourceError, SourceLimits},
};
use ato_formation_worker::{
    local::{self, LocalFormation},
    sandbox::BuildLimits,
};
use serde_json::json;
use std::path::PathBuf;
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 6,
        "archive expected_digest work_root shim result.json"
    );
    let root = PathBuf::from(&args[3]);
    let request = FormationRequest {
        initial_condition: InitialCondition::Archive {
            bytes: std::fs::read(&args[1])?,
            expected_digest: args[2].clone(),
        },
        contract: ContractSource::Infer,
        runtime: RuntimeConstraint::Exact {
            runtime_id: "local".into(),
        },
        policy: FormationPolicy {
            network: FormationNetworkPolicy::Denied,
        },
        budget: SearchBudget { max_attempts: 4 },
        browser_contract: None,
    };
    let env = LocalFormation {
        work_root: root.join("work"),
        out_dir: root.join("out"),
        shim: PathBuf::from(&args[4]),
        limits: BuildLimits::default(),
        source_limits: SourceLimits::default(),
        browser_verifier: None,
        browser_budget: Default::default(),
    };
    let profile = local::probe_local_runtime();
    let result = match local::run(&request, &env) {
        Ok(result) => json!({"status":"formation_result", "result":result}),
        Err(error) => {
            if let Some(f) = error.downcast_ref::<FormationFailure>() {
                json!({"status":"typed_terminal_error", "stage":f.stage.as_str(), "code":f.code})
            } else if let Some(f) = error.downcast_ref::<SourceError>() {
                json!({"status":"typed_terminal_error", "stage":"source", "code":f.code()})
            } else {
                // Untyped infrastructure errors are NOT measured terminal apps.
                json!({"status":"unclassified_error", "measured":false})
            }
        }
    };
    let output = json!({"schema":"ato.formation-coverage-observation/1", "runtime_id":profile.runtime_id,"runtime_facts":profile.capabilities,"candidate_producer":"off","decision_provider":"off","model_calls":0,"result":result});
    std::fs::write(&args[5], serde_json::to_vec_pretty(&output)?).context("write observation")?;
    Ok(())
}

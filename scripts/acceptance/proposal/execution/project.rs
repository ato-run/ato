//! Independently reproduce pinned requester inputs. Not the live producer.
//! This pre-claim view is explicitly NOT an exact post-claim provider wire hash:
//! the pinned requester reduces timeout_ms using the clock after claim.
use anyhow::{Context, Result, ensure};
use ato_formation_worker::runtime_network::{
    RuntimeConstraintWire, Submission, prepare_submission, proposal::prepare_proposal_submission,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

fn submission(config: &Value) -> Result<Submission> {
    let source: PathBuf = serde_json::from_value(config["source"].clone())?;
    let work: PathBuf = serde_json::from_value(config["work"].clone())?;
    let routes: Vec<PathBuf> = serde_json::from_value(config["routes"].clone())?;
    let contract = serde_json::from_value(config["contract"].clone())?;
    let auth = serde_json::from_value(config["authorization"].clone())?;
    let policy = serde_json::from_value(config["policy"].clone())?;
    let budget = serde_json::from_value(config["budget"].clone())?;
    let id = config["search_id"].as_str().context("search id")?;
    if routes.is_empty() {
        prepare_proposal_submission(
            &source,
            contract,
            &work,
            RuntimeConstraintWire::Any,
            policy,
            budget,
            id,
            auth,
        )
    } else {
        let mut s = prepare_submission(
            &source,
            &routes,
            None,
            &work,
            RuntimeConstraintWire::Any,
            policy,
            budget,
            id,
        )?;
        ensure!(s.request.base_contract == contract, "contract drift");
        s.enable_candidate_producer(auth)?;
        Ok(s)
    }
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(args.len() == 4, "usage: proposal-project CONFIG STATUS OUT");
    let config: Value = serde_json::from_slice(&fs::read(&args[1])?)?;
    let status: Value = serde_json::from_slice(&fs::read(&args[2])?)?;
    let s = submission(&config)?;
    ensure!(
        config["expected_preregistration"] == s.proposal_preregistration()?,
        "immutable input drift"
    );
    let request = s.proposal_request_v2(&status)?;
    let auth = s
        .request
        .policy
        .proposal
        .as_ref()
        .context("authorization")?;
    let evidence = json!({"kind":"independently_reproduced_preclaim_view",
        "failure_evidence":request.failure_evidence,"inspection_evidence":request.inspection_evidence,
        "source_context_sha256":request.source_context_sha256(auth)?,
        "known_derivations":request.known_derivations,
        "projection_matches_registered":true});
    fs::write(&args[3], serde_json::to_vec_pretty(&evidence)?)?;
    Ok(())
}

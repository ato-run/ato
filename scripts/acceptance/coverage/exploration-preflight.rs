//! No-call preparation for the frozen upstream cohort and product entry.
//! Never executes source programs or alters source, K, Runtime or permissions.
use anyhow::{Result, ensure};
use ato_formation::{
    authoring::{BindingContext, bind},
    preset::{AppPreset, synthesize_authoring},
    source::{DownloadedArchive, SourceError, SourceLimits},
};
use ato_formation_worker::runtime_network::{
    RuntimeConstraintWire, SatisfyBudget, SatisfyPolicy,
    proposal::prepare_exploration_submission_auto,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Serialize, Deserialize)]
struct Config {
    exploration: ato_formation::exploration::ExplorationPolicy,
    toolchains: BTreeMap<String, String>,
    policy: SatisfyPolicy,
    budget: SatisfyBudget,
}
fn prepare() -> Result<()> {
    let a: Vec<_> = std::env::args().collect();
    if a.len() == 3 && matches!(a[1].as_str(), "--journal-snapshot" | "--journal-accounting") {
        use ato_formation_worker::runtime_network::proposal::budget::{BudgetPlan, CallBudget};
        let bytes = std::fs::read_to_string(&a[2])?;
        let plan: BudgetPlan = serde_json::from_str(
            bytes
                .lines()
                .next()
                .ok_or_else(|| anyhow::anyhow!("journal plan missing"))?,
        )?;
        let reservation = plan.validate()?;
        let budget = CallBudget::reopen(std::path::Path::new(&a[2]), plan.clone())?;
        if a[1] == "--journal-accounting" {
            println!("{}", serde_json::to_string(&budget.accounting()?)?);
            return Ok(());
        }
        let snapshot = budget.snapshot()?;
        ensure!(
            snapshot.cells.values().all(|c| c.is_settled()),
            "unresolved provider reservation"
        );
        println!(
            "{}",
            serde_json::to_string(
                &json!({"plan":plan,"per_call_reservation":reservation,"snapshot":snapshot})
            )?
        );
        return Ok(());
    }
    ensure!(
        a.len() == 7,
        "archive digest scratch search-id policy.json result.json"
    );
    let verified = DownloadedArchive::new(std::fs::read(&a[1])?)
        .verify_archive_digest(&a[2])?
        .verify_tree_digest(None, SourceLimits::default())?;
    let closure = verified.closure_ref("")?.as_str().to_owned();
    let root = PathBuf::from(&a[3]);
    std::fs::create_dir_all(&root)?;
    let tree = verified.materialize(&root.join("source"), "", SourceLimits::default())?;
    let (k, _) = bind(
        &synthesize_authoring(AppPreset::StaticFiles),
        &BindingContext {
            source_closure_ref: &closure,
        },
    )?;
    let cfg: Config = serde_json::from_slice(&std::fs::read(&a[5])?)?;
    let mut policy = cfg.policy;
    policy.exploration = Some(cfg.exploration);
    let submission = prepare_exploration_submission_auto(
        &tree,
        &[],
        k.clone(),
        &root.join("requester-preflight"),
        RuntimeConstraintWire::Exact {
            runtime_id: "local".into(),
            environment_id: None,
        },
        policy,
        cfg.budget,
        &a[4],
        None,
        cfg.toolchains,
    )?;
    ensure!(submission.request.base_contract == k, "K drift");
    ensure!(
        submission.request.source.closure_ref == closure,
        "source closure drift"
    );
    let projection = json!({"schema":"ato.formation-exploration-preflight/1","source":tree,"source_closure_ref":closure,"contract":k,"contract_ref":k.contract_ref()?,
        "request":submission.request,"preregistration":submission.proposal_preregistration()?,"model_calls":0,"source_programs_executed":0});
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&a[6])?;
    use std::io::Write;
    writeln!(file, "{}", serde_json::to_string_pretty(&projection)?)?;
    file.sync_all()?;
    Ok(())
}

fn main() -> Result<()> {
    match prepare() {
        Ok(()) => Ok(()),
        Err(error) => {
            // Pinned source rejection is an application entry terminal. Digest
            // mismatch, I/O and unrecognized failures remain infrastructure stops.
            let typed = error.downcast_ref::<SourceError>().filter(|e| {
                matches!(
                    e,
                    SourceError::LimitExceeded { .. }
                        | SourceError::PathEscape { .. }
                        | SourceError::UnsupportedEntry { .. }
                        | SourceError::SymlinkEscape { .. }
                        | SourceError::SubdirectoryEscape { .. }
                        | SourceError::SubdirectoryMissing { .. }
                )
            });
            if let Some(source) = typed {
                let args: Vec<_> = std::env::args().collect();
                let result = json!({"schema":"ato.formation-exploration-source-terminal/1",
                    "preflight_terminal":{"code":source.code(),"message":source.to_string()},
                    "model_calls":0,"source_programs_executed":0,"K_formed":false});
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&args[6])?;
                writeln!(file, "{}", serde_json::to_string_pretty(&result)?)?;
                file.sync_all()?;
            }
            Err(error)
        }
    }
}

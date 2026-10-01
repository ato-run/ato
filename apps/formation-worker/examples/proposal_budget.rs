//! Credential-free controller helper. Journal semantics belong to CallBudget.
use anyhow::{Result, bail, ensure};
use ato_formation_worker::runtime_network::proposal::budget::{BudgetPlan, CallBudget};
use std::{env, fs, path::Path};

fn main() -> Result<()> {
    let args: Vec<_> = env::args().collect();
    ensure!(
        (4..=5).contains(&args.len()),
        "usage: proposal_budget <check|create|snapshot|inspect-request> <budget.json> <journal> [cell]"
    );
    let plan: BudgetPlan = serde_json::from_slice(&fs::read(&args[2])?)?;
    let per_call = plan.validate()?;
    let journal = Path::new(&args[3]);
    match args[1].as_str() {
        "check" => println!(
            "{}",
            serde_json::json!({"per_call":per_call,"total":per_call * u64::from(plan.max_calls)})
        ),
        "create" => {
            CallBudget::create(journal, plan)?;
        }
        "snapshot" => println!(
            "{}",
            serde_json::to_string(&CallBudget::reopen(journal, plan)?.snapshot()?)?
        ),
        "inspect-request" => {
            let cell = args
                .get(4)
                .ok_or_else(|| anyhow::anyhow!("cell required"))?;
            println!(
                "{}",
                serde_json::to_string(&CallBudget::reopen(journal, plan)?.inspect_request(cell)?)?
            );
        }
        _ => bail!("unknown budget command"),
    }
    Ok(())
}

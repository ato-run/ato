//! Operational control only; links the pristine execution pin's CallBudget.
//! No provider, credential, source, K or HTTP handling.
use anyhow::{Context, Result, ensure};
use ato_formation_worker::runtime_network::proposal::budget::{BudgetPlan, CallBudget};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 4,
        "usage: proposal-budget <check|create|reopen> PLAN JOURNAL"
    );
    let bytes = fs::read(&args[2])?;
    let document: Value = serde_json::from_slice(&bytes)?;
    let plan: BudgetPlan = serde_json::from_value(document["budget"].clone())?;
    let per_call = plan.validate()?;
    let total = per_call
        .checked_mul(u64::from(plan.max_calls))
        .context("reservation overflow")?;
    let path = Path::new(&args[3]);
    match args[1].as_str() {
        "check" => {}
        "create" => {
            CallBudget::create(path, plan)?;
        }
        "reopen" => {
            CallBudget::reopen(path, plan)?;
        }
        _ => anyhow::bail!("unsupported budget operation"),
    }
    println!(
        "{}",
        json!({"per_call_usd_micros":per_call,"total_usd_micros":total})
    );
    Ok(())
}

//! Compile a preregistered typed gate plan through the product Rust validator.
//! This supplies test authoring, never a Runtime/Verifier verdict.
use anyhow::{Context, Result};
use ato_formation::{proposal::*, search::FrozenSearchV1};
use serde_json::{Value, json};
fn main() -> Result<()> {
    let a: Vec<_> = std::env::args().collect();
    let projected: Value = serde_json::from_slice(&std::fs::read(&a[1])?)?;
    let frozen: FrozenSearchV1 =
        serde_json::from_value(projected["preregistration"]["frozen"].clone())?;
    let plan: Value = serde_json::from_slice(&std::fs::read(&a[2])?)?;
    let raw = serde_json::to_vec(&json!({"schema":PROPOSAL_SCHEMA,"proposals":[
        {"kind":"propose_derivation","operations":[{"operation":"execution_plan@1","plan":plan}]}]}))?;
    let output = ProducerOutput::new(
        raw,
        ProducerProvenance {
            provider: "fixed".into(),
            model: None,
        },
    )?;
    let outcomes = CandidateRegistry::new(&frozen)?.validate_batch(&Default::default(), &output)?;
    let ProposalOutcome::Admitted(c) = outcomes.first().context("missing outcome")? else {
        anyhow::bail!("gate authoring rejected: {outcomes:?}");
    };
    std::fs::write(&a[3], &c.compiled().capsule_toml)?;
    println!("{}", serde_json::to_string(c.compiled())?);
    Ok(())
}

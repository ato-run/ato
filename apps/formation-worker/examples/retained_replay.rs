//! Actual requester acceptance driver: no source argument, no old receipt input.
use anyhow::{Context, Result, bail};
use ato_formation_worker::runtime_network::{
    Client, RuntimeConstraintWire, SatisfyBudget, Settlement,
};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 {
        bail!("usage: retained_replay API TOKEN_FILE RETAINED_REF SEARCH_ID");
    }
    let token = std::fs::read_to_string(&args[2])?;
    let client = Client::new(&args[1], &token)?;
    let submission = client.prepare_retained(
        &args[3],
        &args[4],
        RuntimeConstraintWire::Any,
        SatisfyBudget::ceilings(2, "first_pass"),
    )?;
    let created = client.submit_retained(&submission)?;
    let id = created["satisfy_id"]
        .as_str()
        .context("missing satisfy id")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    loop {
        let status = client.satisfy_status(id)?;
        let settlement = Settlement::of(&status)?;
        if settlement != Settlement::Running {
            println!("{}", serde_json::to_string_pretty(&status)?);
            let (accepted, refused) = submission.accept_routes(id, &status);
            anyhow::ensure!(
                settlement == Settlement::Satisfied && !accepted.is_empty(),
                "fresh replay not accepted: {settlement}: {refused:?}"
            );
            return Ok(());
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "replay did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

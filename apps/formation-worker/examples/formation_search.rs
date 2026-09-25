//! Actual known-D search acceptance driver. Frozen source and routes are planned
//! once, then every accepted route is checked by the shared Rust authority.
use anyhow::{Context, Result, bail};
use ato_formation_worker::runtime_network::{
    Client, RuntimeConstraintWire, SatisfyBudget, SatisfyPolicy, accept_verified_routes,
    prepare_submission,
};
use std::path::Path;
fn main() -> Result<()> {
    let a: Vec<_> = std::env::args().collect();
    if a.len() < 9 {
        bail!(
            "usage: formation_search API TOKEN_FILE SOURCE WORK SEARCH_ID MAX_ATTEMPTS CREATED_JSON ROUTE..."
        );
    }
    let client = Client::new(&a[1], &std::fs::read_to_string(&a[2])?)?;
    let submission = prepare_submission(
        Path::new(&a[3]),
        &a[8..].iter().map(Into::into).collect::<Vec<_>>(),
        None,
        Path::new(&a[4]),
        RuntimeConstraintWire::Any,
        SatisfyPolicy {
            network: "dependency-resolution".into(),
            allow_managed: false,
        },
        SatisfyBudget::ceilings(a[6].parse()?, "first_pass"),
        &a[5],
    )?;
    let created = client.submit(&submission)?;
    std::fs::write(&a[7], serde_json::to_vec_pretty(&created)?)?;
    let id = created["satisfy_id"].as_str().context("satisfy id")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    loop {
        // Coordinator may be restarting; a transport failure is not D failure.
        if let Ok(status) = client.satisfy_status(id) {
            let state = status["status"].as_str().unwrap_or("running");
            if !matches!(state, "running" | "unknown") {
                println!("{}", serde_json::to_string_pretty(&status)?);
                if state == "satisfied" {
                    let (accepted, refused) = accept_verified_routes(&submission, id, &status);
                    anyhow::ensure!(
                        !accepted.is_empty(),
                        "requester refused fresh route: {refused:?}"
                    );
                }
                return Ok(());
            }
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "search did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

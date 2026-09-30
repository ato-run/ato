//! Scripted infrastructure gates with actual product Coordinator and Runtime.
//! Fixed proposal schedules are distinguished from live-provider acceptance.
use anyhow::{Context, Result, ensure};
use ato_formation::{authoring::BoundContract, proposal::*};
use ato_formation_worker::runtime_network::{
    Client, RuntimeConstraintWire, SatisfyBudget, SatisfyPolicy, Settlement,
    proposal::{prepare_exploration_submission_auto, serve_proposal},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[derive(Deserialize)]
struct Config {
    source: PathBuf,
    contract: BoundContract,
    work: PathBuf,
    routes: Vec<PathBuf>,
    policy: SatisfyPolicy,
    budget: SatisfyBudget,
    toolchains: std::collections::BTreeMap<String, String>,
    search_id: String,
    api: String,
    token_file: PathBuf,
    schedule: Vec<Value>,
    result: PathBuf,
    created: PathBuf,
    #[serde(default)]
    resume: bool,
    #[serde(default)]
    submit_only: bool,
}
struct Script {
    schedule: Vec<Value>,
    seq: Mutex<usize>,
    calls: Mutex<Vec<Value>>,
}
impl CandidateProducer for Script {
    fn propose(
        &self,
        request: &ProposalRequest,
    ) -> std::result::Result<ProducerOutput, ProducerError> {
        let mut seq = self.seq.lock().unwrap();
        self.calls
            .lock()
            .unwrap()
            .push(serde_json::to_value(request).unwrap());
        let output = self.schedule.get(*seq).cloned();
        *seq += 1;
        let value = output
            .unwrap_or(json!({"schema":PROPOSAL_SCHEMA,"proposals":[{"kind":"unsupported"}]}));
        ProducerOutput::new(
            serde_json::to_vec(&value).unwrap(),
            ProducerProvenance {
                provider: "fixed".into(),
                model: None,
            },
        )
        .map_err(|_| ProducerError::ProviderError)
    }
}
fn main() -> Result<()> {
    let a = std::env::args().nth(1).context("gate config")?;
    let c: Config = serde_json::from_slice(&std::fs::read(a)?)?;
    let mut submission = prepare_exploration_submission_auto(
        &c.source,
        &c.routes,
        c.contract,
        &c.work,
        RuntimeConstraintWire::Exact {
            runtime_id: "local".into(),
            environment_id: None,
        },
        c.policy,
        c.budget,
        &c.search_id,
        None,
        c.toolchains,
    )?;
    let client = Client::new(&c.api, &std::fs::read_to_string(c.token_file)?)?;
    let created = if c.resume {
        client.resume_exploration(&submission)?
    } else {
        client.submit(&submission)?
    };
    if !c.resume {
        std::fs::write(
            &c.created,
            serde_json::to_vec_pretty(&json!({"created":created,"request":submission.request}))?,
        )?;
    }
    if c.submit_only {
        ensure!(!c.resume, "submit-only cannot replace an existing search");
        return Ok(());
    }
    let id = created["satisfy_id"].as_str().context("request id")?;
    let scripted = Arc::new(Script {
        schedule: c.schedule,
        seq: Mutex::new(0),
        calls: Mutex::new(vec![]),
    });
    let end = Instant::now() + Duration::from_secs(210);
    loop {
        let status = client.satisfy_status(id)?;
        serve_proposal(
            &client,
            id,
            &status,
            &mut submission,
            "10000000-0000-4000-8000-000000000001",
            scripted.clone(),
        )?;
        if Settlement::of(&status)? != Settlement::Running {
            let result = submission.exploration_result(&status)?;
            ensure!(
                result["approval"] == "not_assessed" && result["deployed"] == false,
                "submission promoted to approval"
            );
            std::fs::write(
                &c.result,
                serde_json::to_vec_pretty(&json!({"result":result,"status":status,
                "fixed_proposal_calls":*scripted.calls.lock().unwrap(),"live_provider_calls":0}))?,
            )?;
            return Ok(());
        }
        ensure!(
            Instant::now() < end,
            "gate deadline: durable search unchanged"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

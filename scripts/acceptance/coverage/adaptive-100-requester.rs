//! Measurement-only requester around the unchanged bounded production APIs.
//! No compiler, Runtime, proposal vocabulary, prompt or provider transport change.
use anyhow::{Context, Result, ensure};
use ato_formation::{authoring::BoundContract, proposal::ProposalAuthorization};
use ato_formation_worker::{
    decision_provider::{DecisionPoint, DecisionProvider, JevDecisionProvider, ProviderAnswer, decision_request, serve_decision},
    runtime_network::{Client, RuntimeConstraintWire, SatisfyBudget, SatisfyPolicy, accept_verified_routes,
        proposal::{budget::{BudgetPlan,CallBudget}, deepseek::{DeepSeekConfig,DeepSeekCandidateProducer},
            prepare_proposal_submission,serve_general_proposal}},
};
use serde::Deserialize;
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use std::{collections::BTreeSet,io::Write,path::PathBuf,sync::Arc,time::{Duration,Instant}};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    preregister_only:bool, api:String,token_file:PathBuf,source:PathBuf,work:PathBuf,
    search_id:String,claimant_id:String,contract:BoundContract,authorization:ProposalAuthorization,
    policy:SatisfyPolicy,budget:SatisfyBudget,runtime_constraint:RuntimeConstraintWire,
    expected_preregistration:Option<Value>, live_llm:Option<LiveLlm>,
    decision:Option<DecisionConfig>,created:PathBuf,result:PathBuf,settle_seconds:u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveLlm { config:DeepSeekConfig,budget:BudgetPlan,journal:PathBuf }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionConfig { model:String,input_token_cap:u64,input_price:u64,
    reservation_usd_micros:u64,request_record:PathBuf,response_record:PathBuf }
fn durable_new(path:&PathBuf,value:&Value)->Result<()> {
    let mut f=std::fs::OpenOptions::new().create_new(true).write(true).open(path)?;
    f.write_all(&serde_json::to_vec_pretty(value)?)?; f.write_all(b"\n")?;f.sync_all()?; Ok(())
}
struct MeteredDecision<'a> { config:&'a DecisionConfig,log:&'a std::cell::RefCell<Vec<Value>> }
impl DecisionProvider for MeteredDecision<'_> {
    fn decide(&self,point:&DecisionPoint)->ProviderAnswer {
        // A singleton is deterministic: no provider or credential access.
        if point.choices.len()==1 {
            self.log.borrow_mut().push(json!({"seq":point.seq,"calls":0,"reason":"singleton"}));
            return ProviderAnswer::Choice {choice_id:point.choices[0].choice_id.clone(),evidence:json!({"provider":"deterministic_singleton"})};
        }
        let cfg=self.config;
        let reservation=(cfg.input_token_cap*cfg.input_price).div_ceil(1_000_000);
        let request=decision_request(&cfg.model,point);
        let bytes=serde_json::to_vec(&request).expect("serializable projection");
        let ready=cfg.model=="jev-1.13.0" && cfg.input_token_cap==65536 && cfg.input_price==42000
            && cfg.reservation_usd_micros==reservation && bytes.len()<=48*1024;
        // One create-new + fsync before from_env. Existing reservation prevents a second send.
        if !ready || durable_new(&cfg.request_record,&json!({"schema":"ato.formation-adaptive-decision-reservation/1",
            "seq":point.seq,"request_sha256":format!("sha256:{:x}",Sha256::digest(&bytes)),
            "request_bytes":bytes.len(),"request":request,"model":cfg.model,"input_token_cap":cfg.input_token_cap,
            "reserved_usd_micros":reservation,"status":"reserved_before_credential_read"})).is_err() {
            return ProviderAnswer::Fallback {reason:"budget_exhausted"};
        }
        let start=Instant::now();
        let mut answer=match JevDecisionProvider::from_env(Duration::from_secs(20)) {
            Ok(provider)=>provider.decide(point),Err(_)=>ProviderAnswer::Fallback {reason:"credential_unavailable"},
        };
        let submitted=answer.submission(point.seq);
        let usage=submitted.get("evidence").and_then(|e|e.get("usage")).cloned();
        let input=usage.as_ref().and_then(|u|u["input_tokens"].as_u64());
        let output=usage.as_ref().and_then(|u|u["output_tokens"].as_u64());
        let matches=submitted["evidence"]["model"]==cfg.model;
        if matches && input.is_some_and(|i|i>cfg.input_token_cap) {
            answer=ProviderAnswer::Fallback {reason:"usage_exceeds_reservation"};
        } else if matches!(answer,ProviderAnswer::Choice{..}) && !matches {
            answer=ProviderAnswer::Fallback {reason:"model_mismatch"};
        }
        let row=json!({"schema":"ato.formation-adaptive-decision-response/1","seq":point.seq,
            "latency_ms":start.elapsed().as_millis(),"answer":answer.submission(point.seq),
            "input_tokens":input,"output_tokens":output,"observed_usage":input.is_some() && output.is_some(),
            "model_matches":matches,"reserved_usd_micros":reservation,
            "estimated_usage_cost_usd_micros":input.map(|i|(i*cfg.input_price).div_ceil(1_000_000)),
            "reservation_settlement":"full_reserved_ceiling_charged_no_refund",
            "accounting_closed":true,"billing_receipt":false});
        if durable_new(&cfg.response_record,&row).is_err() {
            return ProviderAnswer::Fallback {reason:"accounting_write_failed"};
        }
        self.log.borrow_mut().push(row);answer
    }
}
fn main()->Result<()> {
    let config:Config=serde_json::from_slice(&std::fs::read(std::env::args().nth(1).context("CONFIG_JSON")?)?)?;
    ensure!(matches!(&config.runtime_constraint,RuntimeConstraintWire::Exact{runtime_id,..} if runtime_id=="local"),"exact local only");
    if std::env::args().any(|a| a=="--validate-config") {
        let model=config.live_llm.context("producer config")?;
        ensure!(model.budget.validate()?==81102 && model.budget.max_calls==1,"producer budget");
        ensure!(model.config.model=="deepseek-flash" && model.config.endpoint=="https://api.deepseek.com"
            && model.config.max_output_tokens==2048 && model.config.prompt_version=="ato.formation-candidate-producer-prompt/2",
            "producer pin");
        let decision=config.decision.context("decision config")?;
        ensure!(decision.model=="jev-1.13.0" && decision.input_token_cap==65536
            && decision.input_price==42000 && decision.reservation_usd_micros==2753,"decision pin");
        println!("{{\"config_gate\":\"pass\",\"credential_read\":false,\"model_calls\":0}}");
        return Ok(());
    }
    let mut submission=prepare_proposal_submission(&config.source,config.contract.clone(),&config.work,
        config.runtime_constraint,config.policy,config.budget,&config.search_id,config.authorization)?;
    ensure!(submission.request.base_contract==config.contract,"K drift");
    let prereg=submission.proposal_preregistration()?;
    if config.preregister_only {
        ensure!(config.live_llm.is_none() && config.decision.is_none(),"offline preregistration only");
        return durable_new(&config.result,&prereg);
    }
    ensure!(config.expected_preregistration.as_ref()==Some(&prereg),"source/config drift");
    let model=config.live_llm.context("live producer config required")?;
    let budget=Arc::new(CallBudget::reopen(&model.journal,model.budget)?);
    let producer=Arc::new(DeepSeekCandidateProducer::new(model.config,"DEEPSEEK_API_KEY",budget.clone())?);
    submission.configure_general_producer(producer.identity())?;
    let client=Client::new(&config.api,&std::fs::read_to_string(&config.token_file)?)?;
    let created=client.submit(&submission)?;
    durable_new(&config.created,&json!({"created":created,"request":submission.request}))?;
    let id=created["satisfy_id"].as_str().context("satisfy_id")?;
    let deadline=Instant::now()+Duration::from_secs(config.settle_seconds);
    let mut answered=BTreeSet::new();let decisions=std::cell::RefCell::new(Vec::new());
    let decision=config.decision.context("decision configuration required")?;
    while Instant::now()<deadline {
        let status=client.satisfy_status(id)?;
        submission.accept_proposal_round(&status)?;
        serve_general_proposal(&client,id,&status,&mut submission,&config.claimant_id,producer.clone())?;
        serve_decision(&client,id,&status,&MeteredDecision{config:&decision,log:&decisions},&mut answered);
        let (accepted,refused)=accept_verified_routes(&submission,id,&status);
        let terminal=!matches!(status["status"].as_str(),Some("running"|"unknown"));
        if terminal { budget.halt()?; }
        let result=json!({"status":status,"accepted":accepted,"refused":refused,
            "contract_ref":submission.request.contract_ref,"frozen_contract":submission.request.base_contract,
            "recipes":submission.proposal_recipes().collect::<std::collections::BTreeMap<_,_>>(),
            "decisions":*decisions.borrow(),"producer_budget_snapshot":budget.snapshot()?});
        std::fs::write(&config.result,serde_json::to_vec_pretty(&result)?)?;
        if terminal {
            ensure!(status["status"]!="satisfied" || !accepted.is_empty(),"receipt authority refused PASS");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    anyhow::bail!("infrastructure observation deadline; no K verdict")
}

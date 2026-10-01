//! Product orchestration of the same Coordinator/requester/Runtime protocol.
//! No local mock executor, provider output shell, approval or deployment path.
use super::FormArgs;
use anyhow::{Context, Result, bail, ensure};
use ato_formation::{
    authoring::BoundContract, exploration::ExplorationPolicy, proposal::ProposalAuthorization,
};
use ato_formation_worker::runtime_network::{
    Client, RuntimeConstraintWire, SatisfyBudget, SatisfyPolicy, Settlement, new_search_id,
    proposal::{
        budget::{BudgetPlan, CallBudget},
        deepseek::DeepSeekCandidateProducer,
        prepare_exploration_submission_auto,
        reasoning::{ReasoningProducer, ReasoningProviderConfig},
        serve_general_proposal, serve_reasoning_proposal,
    },
};
use ato_formation_worker::{
    decision_provider::serve_decision,
    metered_decision::{MeteredDecisionConfig, MeteredDecisionProvider},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema: String,
    contract: BoundContract,
    exploration: ExplorationPolicy,
    #[serde(default)]
    authorization: Option<ProposalAuthorization>,
    #[serde(default)]
    toolchains: std::collections::BTreeMap<String, String>,
    provider: ReasoningProviderConfig,
    provider_budget: BudgetPlan,
    #[serde(default)]
    credential_environment: String,
    #[serde(default)]
    decision: Option<MeteredDecisionConfig>,
    /// Durable owner binding. The same journal is required on continuation.
    /// No host path is projected to the provider or put into K/D.
    provider_journal: PathBuf,
}

pub(super) fn run(args: FormArgs) -> Result<()> {
    let path = args
        .exploration_config
        .as_ref()
        .context("exploration config missing")?;
    let bytes = std::fs::read(path)?;
    let mut config: Config = if path.extension().is_some_and(|v| v == "toml") {
        toml::from_str(std::str::from_utf8(&bytes)?)?
    } else {
        serde_json::from_slice(&bytes)?
    };
    ensure!(
        matches!(
            config.schema.as_str(),
            "ato.formation-exploration-config/1" | "ato.formation-exploration-config/2"
        ),
        "unsupported exploration config"
    );
    ensure!(
        args.network == "denied",
        "legacy broad network must be denied; use scoped D requirements"
    );
    ensure!(
        args.decision_provider
            .as_deref()
            .is_none_or(|p| p == "jev" && config.decision.is_some()),
        "exploration uses the pinned metered decision configuration"
    );
    let shared = config.schema == "ato.formation-exploration-config/2";
    ensure!(
        shared == config.exploration.reasoning.is_some(),
        "shared reasoning requires frozen limits and config/2"
    );
    ensure!(
        shared || !config.provider.is_session(),
        "session requires shared reasoning"
    );
    config.exploration.validate()?;
    let reserve = config.provider_budget.validate()?;
    let cp_calls = u64::from(config.provider_budget.max_calls);
    let mut total_cost = reserve.checked_mul(cp_calls).context("spend overflow")?;
    let mut total_input = config
        .provider_budget
        .input_token_cap
        .checked_mul(cp_calls)
        .context("token overflow")?;
    let mut total_output = config
        .provider_budget
        .output_token_cap
        .checked_mul(cp_calls)
        .context("token overflow")?;
    let mut total_calls = cp_calls;
    if let Some(decision) = &config.decision {
        decision.validate()?;
        let calls = u64::from(decision.budget.max_calls);
        total_cost = total_cost
            .checked_add(
                decision
                    .budget
                    .validate()?
                    .checked_mul(calls)
                    .context("spend overflow")?,
            )
            .context("spend overflow")?;
        total_input = total_input
            .checked_add(
                decision
                    .budget
                    .input_token_cap
                    .checked_mul(calls)
                    .context("token overflow")?,
            )
            .context("token overflow")?;
        total_output = total_output
            .checked_add(
                decision
                    .budget
                    .output_token_cap
                    .checked_mul(calls)
                    .context("token overflow")?,
            )
            .context("token overflow")?;
        total_calls += calls;
        let ref_ = decision.configuration_ref()?;
        ensure!(
            config
                .exploration
                .decision_provider_configuration_ref
                .as_ref()
                .is_none_or(|r| r == &ref_),
            "decision config mismatch"
        );
        config.exploration.decision_provider_configuration_ref = Some(ref_);
    } else {
        ensure!(
            config
                .exploration
                .decision_provider_configuration_ref
                .is_none(),
            "frozen decision configuration missing"
        );
    }
    ensure!(
        total_calls <= u64::from(config.exploration.max_provider_calls)
            && total_cost <= config.exploration.max_provider_cost_usd_micros
            && total_input <= config.exploration.max_provider_input_tokens
            && total_output <= config.exploration.max_provider_output_tokens,
        "combined provider spend plan exceeds frozen exploration budget"
    );
    let provider_ref = config.provider.configuration_ref(&config.provider_budget)?;
    ensure!(
        config
            .exploration
            .provider_configuration_ref
            .as_ref()
            .is_none_or(|r| r == &provider_ref),
        "provider config mismatch"
    );
    config.exploration.provider_configuration_ref = Some(provider_ref);
    let api = args.api.context("--runtime-network needs --api")?;
    let token = super::read_token(
        &args
            .token_file
            .context("--runtime-network needs --token-file")?,
    )?;
    let work_root = args
        .work_root
        .unwrap_or_else(|| PathBuf::from(".tmp/formation-exploration"));
    let continuation = args.search_id.is_some();
    let search_id = match args.search_id {
        Some(id) => id,
        None => {
            let mut bytes = [0; 16];
            getrandom::fill(&mut bytes)
                .map_err(|e| anyhow::anyhow!("search identity unavailable: {e}"))?;
            new_search_id(bytes)
        }
    };
    let journal = std::path::absolute(&config.provider_journal)?;
    let binding = format!(
        "sha256:{:x}",
        Sha256::digest(serde_jcs::to_vec(&(search_id.as_str(), &journal))?)
    );
    ensure!(
        config
            .exploration
            .provider_budget_binding_ref
            .as_ref()
            .is_none_or(|r| r == &binding),
        "provider journal binding changed"
    );
    config.exploration.provider_budget_binding_ref = Some(binding);
    let decision_policy =
        config
            .decision
            .as_ref()
            .map(|d| ato_formation::decision::DecisionPolicy {
                provider: ato_formation::decision::ProviderLocation::Requester,
                max_decisions: d.budget.max_calls,
                decision_timeout_ms: d.timeout_ms.saturating_add(2000),
            });
    let mut submission = prepare_exploration_submission_auto(
        &args.path,
        &args.routes,
        config.contract,
        &work_root,
        match args.exact_runtime {
            Some(runtime_id) => RuntimeConstraintWire::Exact {
                runtime_id,
                environment_id: None,
            },
            None => RuntimeConstraintWire::Any,
        },
        SatisfyPolicy {
            network: "denied".into(),
            allow_managed: args.allow_managed,
            decision: decision_policy,
            generation: None,
            proposal: None,
            exploration: Some(config.exploration),
        },
        SatisfyBudget {
            max_attempts: u32::try_from(args.max_attempts)?,
            mode: args.mode,
            deadline_seconds: args.deadline_seconds,
            max_transfer_bytes: args.max_transfer_bytes,
            max_expanded_bytes: args.max_expanded_bytes,
            max_stored_bytes: args.max_stored_bytes,
        },
        &search_id,
        config.authorization,
        config.toolchains,
    )?;
    let budget = if config.provider.is_session() {
        None
    } else {
        Some(Arc::new(if continuation {
            CallBudget::reopen(&config.provider_journal, config.provider_budget.clone())?
        } else {
            CallBudget::create(&config.provider_journal, config.provider_budget.clone())?
        }))
    };
    // Owner-side restart metadata, separate from reusable K/D. Never replace
    // an existing checkpoint or allocate another search against its journal.
    let checkpoint = journal.with_extension("search.json");
    let identity = serde_json::json!({"schema":"ato.formation-exploration-checkpoint/1",
        "search_id":search_id,"contract_ref":submission.request.contract_ref,
        "source_closure_ref":submission.request.source.closure_ref,
        "exploration":submission.request.policy.exploration});
    if continuation {
        ensure!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&checkpoint)?)? == identity,
            "exploration restart checkpoint mismatch"
        );
    } else {
        use std::io::Write;
        let mut saved = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&checkpoint)?;
        saved.write_all(&serde_jcs::to_vec(&identity)?)?;
        saved.sync_all()?;
    }
    let api_producer = if let ReasoningProviderConfig::Api(provider) = &config.provider {
        Some(Arc::new(DeepSeekCandidateProducer::new(
            provider.clone(),
            &config.credential_environment,
            budget.as_ref().context("API budget missing")?.clone(),
        )?))
    } else {
        None
    };
    let reasoning = if shared {
        Some(Arc::new(ReasoningProducer::new(
            config.provider.clone(),
            config.provider_budget.clone(),
            journal.with_extension("reasoning"),
            api_producer.clone(),
        )?))
    } else {
        None
    };
    if let Some(producer) = &api_producer {
        submission.configure_general_producer(producer.identity())?;
    }
    let decision = config
        .decision
        .map(|decision| -> Result<_> {
            let dp_path = journal.with_extension("decision.jsonl");
            let dp_budget = Arc::new(if continuation {
                CallBudget::reopen(&dp_path, decision.budget.clone())?
            } else {
                CallBudget::create(&dp_path, decision.budget.clone())?
            });
            MeteredDecisionProvider::new(
                decision,
                dp_budget,
                &search_id,
                &journal.with_extension("decision-answers"),
            )
        })
        .transpose()?;
    let mut answered = std::collections::BTreeSet::new();
    // Coordinator wire requires a UUID claimant, separate from search identity.
    let mut claimant = [0_u8; 16];
    getrandom::fill(&mut claimant)
        .map_err(|e| anyhow::anyhow!("claimant identity unavailable: {e}"))?;
    claimant[6] = (claimant[6] & 0x0f) | 0x40;
    claimant[8] = (claimant[8] & 0x3f) | 0x80;
    let hex: String = claimant.iter().map(|b| format!("{b:02x}")).collect();
    let mut claimant_id = format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    );
    if shared {
        let claimant_path = journal.with_extension("claimant.json");
        if claimant_path.exists() {
            claimant_id = serde_json::from_slice(&std::fs::read(&claimant_path)?)?;
        } else {
            ato_formation_worker::runtime_network::proposal::reasoning::save_owner_checkpoint(
                &claimant_path,
                &serde_jcs::to_vec(&claimant_id)?,
            )?;
        }
    }
    let client = Client::new(&api, &token)?;
    let accepted = if continuation {
        client.resume_exploration(&submission)?
    } else {
        client.submit(&submission)?
    };
    let id = accepted["satisfy_id"]
        .as_str()
        .context("satisfy identity missing")?
        .to_string();
    let deadline = Instant::now() + Duration::from_secs(args.deadline_seconds.min(1800));
    loop {
        let status = client.satisfy_status(&id)?;
        if let Some(decision) = &decision {
            serve_decision(&client, &id, &status, decision, &mut answered);
        }
        if let Some(producer) = &reasoning {
            serve_reasoning_proposal(
                &client,
                &id,
                &status,
                &mut submission,
                &claimant_id,
                producer.clone(),
            )?;
        } else {
            serve_general_proposal(
                &client,
                &id,
                &status,
                &mut submission,
                &claimant_id,
                api_producer
                    .as_ref()
                    .context("API producer missing")?
                    .clone(),
            )?;
        }
        if Settlement::of(&status)? != Settlement::Running {
            let mut result = submission.exploration_result(&status)?;
            result["candidate_producer_accounting"] = match &budget {
                Some(b) => serde_json::to_value(b.snapshot()?)?,
                None => {
                    serde_json::json!({"provider":"codex_session","API_calls":0,"token_usage":"not_exposed","cost":"not_exposed"})
                }
            };
            if let Some(p) = &reasoning {
                result["reasoning_accounting"] = p.accounting()?;
            }
            result["decision_provider_accounting"] = match &decision {
                Some(d) => d.accounting()?,
                None => {
                    serde_json::json!({"calls":0,"no_call_reason":"decision_provider_not_required"})
                }
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
            ensure!(
                budget.as_ref().map_or(Ok(true), |b| b
                    .snapshot()
                    .map(|s| s.cells.values().all(|c| c.is_settled())))?,
                "provider reservation unresolved; no call is retried"
            );
            if let Some(d) = &decision {
                ensure!(
                    d.all_settled()?,
                    "decision reservation unresolved; no call is retried"
                );
            }
            if result["submission"].is_null() {
                bail!("exploration ended without a verified submission");
            }
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "requester wait expired; durable search budgets and UNKNOWN remain unchanged"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

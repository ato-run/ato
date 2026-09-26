//! Actual known-D search acceptance driver. Frozen source and routes are planned
//! once, then every accepted route is checked by the shared Rust authority.
use anyhow::{Context, Result, bail};
use ato_formation_worker::decision_provider::{
    DecisionPoint, DecisionProvider, ProviderAnswer, serve_decision,
};
use ato_formation_worker::runtime_network::{
    Client, RuntimeConstraintWire, SatisfyBudget, SatisfyPolicy, accept_verified_routes,
    prepare_submission,
};
use std::path::Path;

/// Acceptance-only provider (ATO_ACCEPTANCE_DECISION_PROVIDER): answers with
/// a fixed offered index, a label that was not offered, nothing, or an error.
/// Every call is appended to ATO_ACCEPTANCE_DECISION_LOG so a restart can be
/// shown not to ask again.
struct AcceptanceProvider {
    mode: String,
    log: Option<std::path::PathBuf>,
}
impl DecisionProvider for AcceptanceProvider {
    fn decide(&self, point: &DecisionPoint) -> ProviderAnswer {
        if let Some(log) = &self.log {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
            {
                let _ = writeln!(
                    f,
                    "{}",
                    serde_json::json!({
                        "seq": point.seq,
                        "offered": point.choices.iter().map(|c| &c.choice_id).collect::<Vec<_>>(),
                        "default": point.default_choice_id,
                        "mode": self.mode,
                        // What the Coordinator exposed to a provider, per choice.
                        "view_facts": point.choices.iter().map(|c| (&c.choice_id, &c.runtime_facts)).collect::<std::collections::BTreeMap<_, _>>(),
                        "requirements": point.choices.iter().map(|c| (&c.choice_id, &c.derivation["requirements"])).collect::<std::collections::BTreeMap<_, _>>(),
                        "evidence": point.evidence,
                    })
                );
            }
        }
        match self.mode.as_str() {
            "silent" => {
                // Never answer: the Coordinator's deadline takes the default.
                std::thread::sleep(std::time::Duration::from_secs(3600));
                ProviderAnswer::Fallback { reason: "timeout" }
            }
            "error" => ProviderAnswer::Fallback {
                reason: "provider_error",
            },
            "out_of_set" => ProviderAnswer::Choice {
                choice_id: "c0000000000000000".into(),
                evidence: serde_json::json!({"provider": "acceptance", "mode": "out_of_set"}),
            },
            late if late.starts_with("late:") => {
                // Answer only after the point's deadline has passed: the
                // Coordinator must settle it as timeout, not as this choice.
                let ms: u64 = late[5..].parse().unwrap_or(0);
                std::thread::sleep(std::time::Duration::from_millis(ms));
                match point.choices.get(1) {
                    Some(c) => ProviderAnswer::Choice {
                        choice_id: c.choice_id.clone(),
                        evidence: serde_json::json!({"provider": "acceptance", "mode": late}),
                    },
                    None => ProviderAnswer::Fallback { reason: "invalid" },
                }
            }
            kind if kind.starts_with("kind:") => {
                // The first offered choice of an action kind (attempt,
                // inspect, stop): exercises the exploration actions without
                // naming an index.
                let want = &kind[5..];
                match point
                    .choices
                    .iter()
                    .find(|c| action_kind(c) == want)
                {
                    Some(c) => ProviderAnswer::Choice {
                        choice_id: c.choice_id.clone(),
                        evidence: serde_json::json!({"provider": "acceptance", "mode": kind}),
                    },
                    None => ProviderAnswer::Fallback { reason: "invalid" },
                }
            }
            script if script.starts_with("seq:") => {
                // Per-point script, "seq:0=3,1=0": the offered index to pick
                // at each decision sequence.
                let index: Option<usize> = script[4..]
                    .split(',')
                    .filter_map(|part| part.split_once('='))
                    .find(|(seq, _)| seq.parse::<u64>().ok() == Some(point.seq))
                    .and_then(|(_, i)| i.parse().ok());
                match index.and_then(|i| point.choices.get(i)) {
                    Some(c) => ProviderAnswer::Choice {
                        choice_id: c.choice_id.clone(),
                        evidence: serde_json::json!({"provider": "acceptance", "mode": script}),
                    },
                    None => ProviderAnswer::Fallback { reason: "invalid" },
                }
            }
            fixed => {
                let n: usize = fixed
                    .strip_prefix("fixed:")
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                match point.choices.get(n) {
                    Some(c) => ProviderAnswer::Choice {
                        choice_id: c.choice_id.clone(),
                        evidence: serde_json::json!({"provider": "acceptance", "mode": fixed}),
                    },
                    None => ProviderAnswer::Fallback { reason: "invalid" },
                }
            }
        }
    }
}

fn action_kind(choice: &ato_formation_worker::decision_provider::OfferedChoice) -> &'static str {
    use ato_formation_worker::decision_provider::OfferedAction;
    match &choice.action {
        OfferedAction::Attempt { .. } => "attempt",
        OfferedAction::Inspect { .. } => "inspect",
        OfferedAction::Stop { .. } => "stop",
    }
}

fn main() -> Result<()> {
    let a: Vec<_> = std::env::args().collect();
    if a.len() < 9 {
        bail!(
            "usage: formation_search API TOKEN_FILE SOURCE WORK SEARCH_ID MAX_ATTEMPTS CREATED_JSON ROUTE..."
        );
    }
    let client = Client::new(&a[1], &std::fs::read_to_string(&a[2])?)?;
    let mut submission = prepare_submission(
        Path::new(&a[3]),
        &a[8..].iter().map(Into::into).collect::<Vec<_>>(),
        None,
        Path::new(&a[4]),
        RuntimeConstraintWire::Any,
        SatisfyPolicy {
            network: "dependency-resolution".into(),
            allow_managed: false,
            // ATO_ACCEPTANCE_DECISION_POLICY="MAX_DECISIONS,TIMEOUT_MS"
            decision: std::env::var("ATO_ACCEPTANCE_DECISION_POLICY")
                .ok()
                .map(|v| -> Result<_> {
                    let (max, timeout) = v.split_once(',').context("MAX,TIMEOUT_MS")?;
                    Ok(ato_formation::decision::DecisionPolicy {
                        provider: ato_formation::decision::ProviderLocation::Requester,
                        max_decisions: max.parse()?,
                        decision_timeout_ms: timeout.parse()?,
                    })
                })
                .transpose()?,
        },
        SatisfyBudget::ceilings(a[6].parse()?, "first_pass"),
        &a[5],
    )?;
    // Fault injection for the effect-uncertainty acceptance only: a requester
    // whose effect hint is wrong. The Runtime re-plans D and attests the real
    // class; nothing here changes K or D.
    if let Ok(effects) = std::env::var("ATO_ACCEPTANCE_DECLARED_EFFECTS") {
        for derivation in &mut submission.request.authorized_derivations {
            derivation.effects = effects.clone();
        }
    }
    let created = client.submit(&submission)?;
    std::fs::write(&a[7], serde_json::to_vec_pretty(&created)?)?;
    let id = created["satisfy_id"].as_str().context("satisfy id")?;
    let settle = std::env::var("ATO_ACCEPTANCE_SETTLE_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(300);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(settle);
    let provider = std::env::var("ATO_ACCEPTANCE_DECISION_PROVIDER")
        .ok()
        .map(|mode| AcceptanceProvider {
            mode,
            log: std::env::var_os("ATO_ACCEPTANCE_DECISION_LOG").map(Into::into),
        });
    let mut answered = std::collections::BTreeSet::new();
    loop {
        // Coordinator may be restarting; a transport failure is not D failure.
        if let Ok(status) = client.satisfy_status(id) {
            if let Some(provider) = &provider {
                // A silent provider must not stall this poll loop.
                if provider.mode == "silent" {
                    if let Some(point) = DecisionPoint::from_status(&status)
                        && answered.insert(point.seq)
                    {
                        let _ = provider.log.as_ref().map(|log| {
                            use std::io::Write as _;
                            std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(log)
                                .and_then(|mut f| {
                                    writeln!(
                                        f,
                                        "{}",
                                        serde_json::json!({"seq": point.seq, "mode": "silent"})
                                    )
                                })
                        });
                    }
                } else {
                    serve_decision(&client, id, &status, provider, &mut answered);
                }
            }
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

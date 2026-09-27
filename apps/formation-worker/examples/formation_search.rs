//! Actual Formation search acceptance driver. Frozen source and routes are planned
//! once, then every accepted route is checked by the shared Rust authority.
use anyhow::{Context, Result, bail};
use ato_formation_worker::decision_provider::{
    DEFAULT_DECISION_MODEL, DecisionPoint, DecisionProvider, JevDecisionProvider, ProviderAnswer,
    decision_request, serve_decision,
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
    jev: Option<JevDecisionProvider>,
}
impl DecisionProvider for AcceptanceProvider {
    fn decide(&self, point: &DecisionPoint) -> ProviderAnswer {
        if let Some(jev) = &self.jev {
            let started = std::time::Instant::now();
            let answer = jev.decide(point);
            // Acceptance telemetry only: no key, source, raw response or receipt.
            // Use the production provider and its unchanged bounded projection.
            let model = std::env::var("ATO_DECISION_JEV_MODEL")
                .unwrap_or_else(|_| DEFAULT_DECISION_MODEL.to_owned());
            let request_bytes = serde_json::to_vec(&decision_request(&model, point))
                .expect("serializable provider request")
                .len();
            if let Some(log) = &self.log {
                use std::io::Write as _;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log)
                    .expect("open live acceptance ledger");
                writeln!(
                    file,
                    "{}",
                    serde_json::json!({
                        "seq": point.seq,
                        "mode": "jev",
                        "elapsed_ms": started.elapsed().as_millis(),
                        "request_bytes": request_bytes,
                        "offered": point.choices.iter().map(|c| &c.choice_id).collect::<Vec<_>>(),
                        "answer": answer.submission(point.seq),
                    })
                )
                .expect("write live acceptance ledger");
            }
            return answer;
        }
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
                match point.choices.iter().find(|c| action_kind(c) == want) {
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

/// Fault injection / deterministic draft producer for actual acceptance only.
struct AcceptanceGenerationProvider {
    mode: String,
}
impl ato_formation_worker::generation_provider::GenerationProvider
    for AcceptanceGenerationProvider
{
    fn generate(
        &self,
        point: &ato_formation_worker::generation_provider::GenerationPoint,
    ) -> ato_formation_worker::generation_provider::GenerationAnswer {
        use ato_formation_worker::generation_provider::GenerationAnswer;
        if self.mode == "deterministic_v1" {
            use ato_formation_worker::generation_provider::generation_request_v2;
            if generation_request_v2(
                ato_formation_worker::generation_provider::DEFAULT_GENERATION_MODEL,
                point,
            )
            .is_err()
            {
                return GenerationAnswer::Fallback { reason: "invalid" };
            }
            let provenance = serde_json::json!({
                "provider": "acceptance", "model": "deterministic-selector-v1",
                "prompt_version": "efficacy-selector/1",
                "usage": {"input_tokens":0,"output_tokens":0},
            });
            let selected = point
                .context
                .as_ref()
                .and_then(|context| efficacy_entrypoint(&context.entrypoints));
            return match selected {
                Some(id) => GenerationAnswer::Draft {
                    draft: serde_json::json!({"schema":"ato.formation-derivation-draft/1",
                        "operation":"python_script", "entrypoint_id":id}),
                    provenance,
                },
                None => GenerationAnswer::Declined { provenance },
            };
        }
        if self.mode == "error" {
            return GenerationAnswer::Fallback {
                reason: "provider_error",
            };
        }
        if self.mode == "silent" {
            std::thread::sleep(std::time::Duration::from_secs(31));
            return GenerationAnswer::Fallback { reason: "timeout" };
        }
        if self.mode == "decline" {
            return GenerationAnswer::Declined {
                provenance: serde_json::json!({
                    "provider": "acceptance", "model": "fixed-decline",
                    "prompt_version": "acceptance/1",
                    "usage": {"input_tokens":0,"output_tokens":0},
                }),
            };
        }
        let id = self.mode.strip_prefix("fixed:").unwrap_or(
            point
                .entrypoint_ids
                .first()
                .map(String::as_str)
                .unwrap_or("none"),
        );
        GenerationAnswer::Draft {
            draft: serde_json::json!({
                "schema": "ato.formation-derivation-draft/1",
                "operation": "python_script",
                "entrypoint_id": id,
            }),
            provenance: serde_json::json!({
                "provider": "acceptance", "model": "fixed",
                "prompt_version": "acceptance/1",
                "usage": {"input_tokens":0,"output_tokens":0},
            }),
        }
    }
}

/// Evaluation-only fixed rule. No source, paths, labels or outcome feedback.
/// Complete scans only; positive score, then ascending opaque ID for ties.
fn efficacy_entrypoint(
    entries: &[ato_formation::generation_context::EntryPointSummary],
) -> Option<&str> {
    use ato_formation::generation_context::{ImportMarker, SourceScan};
    entries
        .iter()
        .filter(|entry| entry.source_scan == SourceScan::Complete)
        .map(|entry| {
            let http = entry.imports.iter().any(|marker| {
                matches!(
                    marker,
                    ImportMarker::HttpServer
                        | ImportMarker::Flask
                        | ImportMarker::Fastapi
                        | ImportMarker::Uvicorn
                        | ImportMarker::Aiohttp
                        | ImportMarker::Tornado
                        | ImportMarker::Wsgiref
                        | ImportMarker::Django
                )
            });
            let score = 4 * u8::from(entry.custom_http_handler)
                + 2 * u8::from(entry.server_listen)
                + u8::from(http)
                + u8::from(entry.main_guard);
            (entry.id.as_str(), score)
        })
        .filter(|(_, score)| *score > 0)
        .min_by(|(id_a, score_a), (id_b, score_b)| score_b.cmp(score_a).then(id_a.cmp(id_b)))
        .map(|(id, _)| id)
}

/// Reject mode combinations that would record a different prompt from the
/// one sent by the real provider. Fixed providers can exercise either format.
fn validate_generation_capture(mode: Option<&str>, context_v2: bool) -> Result<()> {
    anyhow::ensure!(
        !matches!(
            (mode, context_v2),
            (Some("jev"), true) | (Some("jev_v2"), false)
        ),
        "generation provider and context versions must match"
    );
    Ok(())
}

/// Record exactly the privacy-projected provider payload, never GenerationPoint.
struct RecordedGenerationProvider {
    inner: Box<dyn ato_formation_worker::generation_provider::GenerationProvider>,
    path: std::path::PathBuf,
    v2: bool,
}
impl ato_formation_worker::generation_provider::GenerationProvider for RecordedGenerationProvider {
    fn generate(
        &self,
        point: &ato_formation_worker::generation_provider::GenerationPoint,
    ) -> ato_formation_worker::generation_provider::GenerationAnswer {
        use ato_formation_worker::generation_provider::{self as generation, GenerationAnswer};
        let model = std::env::var("ATO_GENERATION_JEV_MODEL")
            .unwrap_or_else(|_| generation::DEFAULT_GENERATION_MODEL.into());
        let payload = if self.v2 {
            generation::generation_request_v2(&model, point)
        } else {
            generation::generation_request(&model, point)
        };
        let Ok(payload) = payload else {
            return GenerationAnswer::Fallback { reason: "invalid" };
        };
        if serde_json::to_vec_pretty(&payload)
            .ok()
            .and_then(|bytes| std::fs::write(&self.path, bytes).ok())
            .is_none()
        {
            return GenerationAnswer::Fallback {
                reason: "provider_error",
            };
        }
        self.inner.generate(point)
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
        std::env::var("ATO_ACCEPTANCE_EXACT_RUNTIME")
            .map(|runtime_id| RuntimeConstraintWire::Exact {
                runtime_id,
                environment_id: None,
            })
            .unwrap_or(RuntimeConstraintWire::Any),
        SatisfyPolicy {
            network: "dependency-resolution".into(),
            allow_managed: false,
            generation: None,
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
    if let Ok(entries) = std::env::var("ATO_ACCEPTANCE_GENERATION_ENTRYPOINTS") {
        submission.authorize_generation(serde_json::from_str(&entries)?, 30_000)?;
    }
    let context_v2 = std::env::var("ATO_ACCEPTANCE_GENERATION_CONTEXT").as_deref() == Ok("v2");
    validate_generation_capture(
        std::env::var("ATO_ACCEPTANCE_GENERATION_PROVIDER")
            .ok()
            .as_deref(),
        context_v2,
    )?;
    if context_v2 {
        submission.enable_generation_context()?;
    }
    let generation_provider = std::env::var("ATO_ACCEPTANCE_GENERATION_PROVIDER").ok()
        .map(|mode| -> Result<Box<dyn ato_formation_worker::generation_provider::GenerationProvider>> {
            if mode == "jev_v2" {
                Ok(Box::new(ato_formation_worker::generation_provider::JevGenerationProvider::from_env_v2(
                    std::time::Duration::from_secs(20),
                )?))
            } else if mode == "jev" {
                Ok(Box::new(ato_formation_worker::generation_provider::JevGenerationProvider::from_env(
                    std::time::Duration::from_secs(20),
                )?))
            } else {
                Ok(Box::new(AcceptanceGenerationProvider { mode }))
            }
        }).transpose()?;
    let generation_provider = generation_provider.map(|inner| {
        if let Some(path) = std::env::var_os("ATO_ACCEPTANCE_GENERATION_INPUT") {
            Box::new(RecordedGenerationProvider {
                inner,
                path: path.into(),
                v2: std::env::var("ATO_ACCEPTANCE_GENERATION_CONTEXT").as_deref() == Ok("v2"),
            }) as Box<dyn ato_formation_worker::generation_provider::GenerationProvider>
        } else {
            inner
        }
    });
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
            jev: (mode == "jev")
                .then(|| JevDecisionProvider::from_env(std::time::Duration::from_secs(20)))
                .transpose()
                .expect("live Jev acceptance configuration"),
            mode,
            log: std::env::var_os("ATO_ACCEPTANCE_DECISION_LOG").map(Into::into),
        });
    let mut answered = std::collections::BTreeSet::new();
    loop {
        // Coordinator may be restarting; a transport failure is not D failure.
        if let Ok(status) = client.satisfy_status(id) {
            submission.accept_generated_candidate(&status)?;
            if let Some(provider) = &generation_provider
                && let Some(call) = ato_formation_worker::runtime_network::serve_generation(
                    &mut submission,
                    &client,
                    id,
                    &status,
                    provider.as_ref(),
                )?
                && let Some(path) = std::env::var_os("ATO_ACCEPTANCE_GENERATION_LOG")
            {
                use std::io::Write as _;
                let mut log = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?;
                writeln!(log, "{call}")?;
            }
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

#[cfg(test)]
mod capture_tests {
    use super::validate_generation_capture;

    #[test]
    fn live_provider_version_must_match_captured_context() {
        assert!(validate_generation_capture(Some("jev"), false).is_ok());
        assert!(validate_generation_capture(Some("jev_v2"), true).is_ok());
        assert!(validate_generation_capture(Some("jev"), true).is_err());
        assert!(validate_generation_capture(Some("jev_v2"), false).is_err());
        for context in [false, true] {
            assert!(validate_generation_capture(Some("fixed:e01"), context).is_ok());
            assert!(validate_generation_capture(None, context).is_ok());
        }
    }
}

#[cfg(test)]
mod efficacy_tests {
    use super::efficacy_entrypoint;
    use ato_formation::generation_context::project_python;
    #[test]
    fn fixed_rule_uses_markers_not_input_order_and_breaks_ties_by_id() {
        let source = b"from http.server import HTTPServer\nHTTPServer().serve_forever()";
        let a = project_python("q7", source, source.len() as u64).unwrap();
        let b = project_python("m2", source, source.len() as u64).unwrap();
        assert_eq!(efficacy_entrypoint(&[a.clone(), b.clone()]), Some("m2"));
        assert_eq!(efficacy_entrypoint(&[b, a]), Some("m2"));
    }
    #[test]
    fn fixed_rule_declines_empty_zero_and_unavailable_contexts() {
        assert_eq!(efficacy_entrypoint(&[]), None);
        let zero = project_python("q7", b"print('hi')", 11).unwrap();
        let large = project_python("m2", b"", 65537).unwrap();
        let unavailable = project_python("z3", &[255], 1).unwrap();
        assert_eq!(efficacy_entrypoint(&[zero, large, unavailable]), None);
    }
}

#[cfg(test)]
mod efficacy_provider_tests {
    use super::AcceptanceGenerationProvider;
    use ato_formation_worker::generation_provider::{
        GenerationAnswer, GenerationPoint, GenerationProvider,
    };
    #[test]
    fn comparator_reaches_draft_through_production_context_validation() {
        let source = b"from http.server import HTTPServer\nHTTPServer().serve_forever()";
        let summary =
            ato_formation::generation_context::project_python("m2", source, source.len() as u64)
                .unwrap();
        let point: GenerationPoint = serde_json::from_value(serde_json::json!({
            "schema":"ato.formation-generation-point/2","revision":1,"expires_at":"2026-09-27T00:00:00Z",
            "entrypoint_ids":["m2"],"failures":[],"context":{
                "schema":"ato.formation-generation-context/1","entrypoints":[summary],
                "project_summary":{"python":true,"node":false,"manifest":true,"lockfile":false,
                    "readme":false,"static_html":false,"regular_files":"one","python_files":"one"},
                "failures":[],"inspections":[]
            }
        })).unwrap();
        let provider = AcceptanceGenerationProvider {
            mode: "deterministic_v1".into(),
        };
        match provider.generate(&point) {
            GenerationAnswer::Draft { draft, .. } => assert_eq!(draft["entrypoint_id"], "m2"),
            answer => panic!("expected comparator draft, got {answer:?}"),
        }
    }
}

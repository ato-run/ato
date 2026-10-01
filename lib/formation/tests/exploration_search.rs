//! Scheduler/compiler tests only; real provider/Coordinator gates are separate.
use ato_formation::{
    authoring::{BindingContext, bind},
    capsule_toml::parse_capsule_toml,
    exploration::{ExplorationPolicy, FormationConfig},
    proposal::*,
    requirements::{ExecutionPhase, ExecutionRequirements, NetworkRequirement},
    search::*,
};
use serde_json::json;
use std::collections::BTreeMap;

fn state() -> SearchStateV1 {
    let mut s: SearchStateV1 =
        serde_json::from_str(include_str!("fixtures/search-state/d1-failed.json")).unwrap();
    let source = format!("sha256:{}", "a".repeat(64));
    let (k, _) = bind(
        &parse_capsule_toml(include_str!("fixtures/proposal-python.toml")).unwrap(),
        &BindingContext {
            source_closure_ref: &source,
        },
    )
    .unwrap();
    s.frozen.base_contract_ref = k.contract_ref().unwrap();
    s.frozen.contract_ref = s.frozen.base_contract_ref.clone();
    s.frozen.base_contract = k;
    s.frozen.candidates.clear();
    s.frozen.initial_source = Some(InitialSource {
        closure_ref: source.clone(),
        archive_digest: source,
    });
    s.attempts.clear();
    s.source_archive_bytes = Some(64);
    s.budget = BudgetCounters::default();
    s.frozen.policy.exploration = Some(ExplorationPolicy {
        formation: FormationConfig::default(),
        ceiling: ExecutionRequirements::default(),
        max_provider_calls: 3,
        max_inspections: 3,
        reasoning: None,
        max_provider_cost_usd_micros: 100_000,
        max_provider_input_tokens: 90_000,
        max_provider_output_tokens: 6144,
        max_network_transfer_bytes: 1048576,
        max_network_transfer_bytes_per_attempt: 524288,
        provider_configuration_ref: None,
        decision_provider_configuration_ref: None,
        provider_budget_binding_ref: None,
    });
    s.frozen.policy.proposal = Some(ProposalAuthorization {
        execution_plan: Some(PlanAuthorization {
            source_oci: None,
            files: BTreeMap::from([(
                "server".into(),
                VerifiedSourceFile {
                    path: "server.py".into(),
                    digest: format!("sha256:{}", "b".repeat(64)),
                },
            )]),
            toolchains: BTreeMap::from([("python".into(), "3.12.7".into())]),
        }),
        modifiable_derivation_refs: vec![],
        source_domain: SourceDomain {
            entrypoints: BTreeMap::new(),
            modules: BTreeMap::new(),
        },
        python_http_process: None,
        node_static_workspace: None,
        policy: CandidateProducerPolicy {
            max_proposal_rounds: 1,
            max_proposals: 1,
            timeout_ms: 5000,
            allow_source_text: false,
            max_source_bytes: 0,
        },
    });
    s
}

fn plan() -> serde_json::Value {
    json!({"runtime":{"name":"python","version":"3.12.7"},
        "entrypoint":{"file_id":"server","digest":format!("sha256:{}","b".repeat(64))},
        "argv":[],"cwd":".","guest_port":8000,"dependencies":[],"build_scripts":[],
        "requirements":{},"basis":[],"unknowns":[]})
}

fn compile(s: &SearchStateV1, plan: serde_json::Value) -> Vec<ProposalOutcome> {
    let bytes = serde_json::to_vec(&json!({"schema":PROPOSAL_SCHEMA,"proposals":[
        {"kind":"propose_derivation","operations":[{"operation":"execution_plan@1","plan":plan}]}]})).unwrap();
    CandidateRegistry::new(&s.frozen)
        .unwrap()
        .validate_batch(
            &BTreeMap::new(),
            &ProducerOutput::new(
                bytes,
                ProducerProvenance {
                    provider: "fixed".into(),
                    model: None,
                },
            )
            .unwrap(),
        )
        .unwrap()
}

#[test]
fn legacy_catalog_empty_does_not_prevent_a_typed_execution_plan() {
    let s = state();
    assert!(matches!(
        s.frozen
            .policy
            .proposal
            .as_ref()
            .unwrap()
            .catalog()
            .unwrap()
            .operations[0],
        OperationDomain::ExecutionPlan { .. }
    ));
    let outcomes = compile(&s, plan());
    let ProposalOutcome::Admitted(candidate) = &outcomes[0] else {
        panic!("{outcomes:?}")
    };
    assert_eq!(
        candidate.compiled().base_contract_ref,
        s.frozen.base_contract_ref
    );
    assert_eq!(
        candidate.compiled().derivation.steps[0].argv[1],
        "--version"
    );
    assert_eq!(
        candidate.compiled().derivation.steps[1].argv[1],
        "/app/server.py"
    );
}

#[test]
fn unsupported_toolchain_is_a_validator_diagnostic() {
    let mut p = plan();
    p["runtime"]["name"] = json!("go");
    assert!(
        matches!(&compile(&state(),p)[0],ProposalOutcome::Rejected(e) if e.0=="unsupported_toolchain")
    );
}

#[test]
fn stale_source_digest_is_not_lowered_to_a_process() {
    let mut p = plan();
    p["entrypoint"]["digest"] = json!(format!("sha256:{}", "c".repeat(64)));
    assert!(
        matches!(&compile(&state(),p)[0],ProposalOutcome::Rejected(e) if e.0=="proposal_source_digest_mismatch")
    );
}

#[test]
fn an_out_of_ceiling_requirement_is_saved_as_a_specific_rejection() {
    let mut p = plan();
    p["requirements"] = json!({"network":[{"phase":"dependencies","host":"pypi.org","port":443}]});
    assert!(
        matches!(&compile(&state(),p)[0],ProposalOutcome::Rejected(e) if e.0=="exploration_authority_exceeded")
    );
}

#[test]
fn default_three_failed_provider_rounds_exhaust_without_reset_on_restart() {
    let mut s = state();
    let frozen = s.frozen.canonical_bytes().unwrap();
    for round in 0..3 {
        let now = 100 + round * 6000;
        let SearchAction::OpenProposalRound {
            opened_at_ms,
            expires_at_ms,
        } = decide_next(&s, &[], now).unwrap()
        else {
            panic!("round {round}")
        };
        if let Some(previous) = s.proposal_round.take() {
            s.proposal_history.push(previous);
        }
        s.proposal_round = Some(ProposalRoundRecord {
            opened_at_ms,
            expires_at_ms,
            outcome: Some(ProposalRoundOutcome::ProviderError),
            candidates: vec![],
            derivations: vec![],
            diagnostics: vec![],
            inspection_requests: vec![],
        });
        s = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
        assert_eq!(s.frozen.canonical_bytes().unwrap(), frozen);
    }
    assert_eq!(
        decide_next(&s, &[], 19_000).unwrap(),
        SearchAction::Finish {
            reason: Termination::RoundsExhausted
        }
    );
    assert_eq!(s.proposal_history.len(), 2);
}

#[test]
fn configured_one_round_keeps_its_effective_limit_after_serialization() {
    let mut s = state();
    s.frozen
        .policy
        .exploration
        .as_mut()
        .unwrap()
        .formation
        .max_rounds = std::num::NonZeroU32::MIN;
    s.proposal_round = Some(ProposalRoundRecord {
        opened_at_ms: 100,
        expires_at_ms: 5100,
        outcome: Some(ProposalRoundOutcome::Timeout),
        candidates: vec![],
        derivations: vec![],
        diagnostics: vec![],
        inspection_requests: vec![],
    });
    let restored: SearchStateV1 = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
    assert_eq!(
        decide_next(&restored, &[], 6000).unwrap(),
        SearchAction::Finish {
            reason: Termination::RoundsExhausted
        }
    );
}

#[test]
fn independent_provider_cap_stops_before_receiver_refuses_another_round() {
    let mut s = state();
    let policy = s.frozen.policy.exploration.as_mut().unwrap();
    policy.formation.max_rounds = std::num::NonZeroU32::new(5).unwrap();
    policy.max_provider_calls = 4;
    for index in 0..4 {
        let now = 100 + index * 6000;
        let SearchAction::OpenProposalRound {
            opened_at_ms,
            expires_at_ms,
        } = decide_next(&s, &[], now).unwrap()
        else {
            panic!("{index}")
        };
        if let Some(previous) = s.proposal_round.take() {
            s.proposal_history.push(previous);
        }
        s.proposal_round = Some(ProposalRoundRecord {
            opened_at_ms,
            expires_at_ms,
            outcome: None,
            candidates: vec![],
            derivations: vec![],
            diagnostics: vec![],
            inspection_requests: vec![],
        });
        assert!(matches!(
            decide_next(&s, &[], now + 1).unwrap(),
            SearchAction::WaitForProposalRound { .. }
        ));
        s.proposal_round.as_mut().unwrap().outcome = Some(ProposalRoundOutcome::ProviderError);
        s = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
    }
    assert_eq!(
        decide_next(&s, &[], 25000).unwrap(),
        SearchAction::Finish {
            reason: Termination::BudgetExhausted
        }
    );
    assert_eq!(s.proposal_history.len() + 1, 4);
    assert_eq!(
        s.frozen
            .policy
            .exploration
            .unwrap()
            .formation
            .max_rounds
            .get(),
        5
    );
}

#[test]
fn ceiling_changes_are_separate_from_frozen_contract() {
    let mut s = state();
    let before = s.frozen.canonical_bytes().unwrap();
    let k = s.frozen.contract_ref.clone();
    s.frozen
        .policy
        .exploration
        .as_mut()
        .unwrap()
        .ceiling
        .network
        .push(NetworkRequirement {
            phase: ExecutionPhase::Dependencies,
            host: "pypi.org".into(),
            port: 443,
        });
    assert_ne!(s.frozen.canonical_bytes().unwrap(), before);
    assert_eq!(s.frozen.contract_ref, k);
}

#[test]
fn module_state_and_public_environment_are_canonical_and_source_bound() {
    let mut s = state();
    s.frozen
        .policy
        .proposal
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap()
        .files
        .get_mut("server")
        .unwrap()
        .path = "demo/__main__.py".into();
    let mut p = plan();
    p["module"] = json!("demo");
    p["environment"] = json!({"HOME":"/data"});
    p["state"] = json!([{"id":"app.data","mount":"/data","access":"read-write"}]);
    let out = compile(&s, p.clone());
    let ProposalOutcome::Admitted(candidate) = &out[0] else {
        panic!("{out:?}");
    };
    assert_eq!(candidate.compiled().derivation.state[0].mount, "/data");
    assert_eq!(
        candidate.compiled().derivation.steps.last().unwrap().argv[1..],
        ["-m", "demo"]
    );
    p["module"] = json!("other");
    assert!(matches!(
        &compile(&s, p)[0],
        ProposalOutcome::Rejected(ProposalError("proposal_module_source_mismatch"))
    ));
}

#[test]
fn state_cannot_mask_shim_system_or_policy_and_env_cannot_inject_into_owner() {
    let s = state();
    for path in [
        "/",
        "/app",
        "/.ato/ingress",
        "/etc",
        "/opt/ato/toolchains",
        "/data/../etc",
    ] {
        let mut p = plan();
        p["state"] = json!([{"id":"app.data","mount":path,"access":"read-write"}]);
        assert!(matches!(
            &compile(&s, p)[0],
            ProposalOutcome::Rejected(ProposalError("execution_plan_bounds"))
        ));
    }
    for key in [
        "LD_PRELOAD",
        "DYLD_INSERT_LIBRARIES",
        "ATO_RUN_TOKEN",
        "HTTP_PROXY",
        "API_SECRET",
        "NODE_OPTIONS",
    ] {
        let mut p = plan();
        p["environment"] = json!({key: "ignored"});
        assert!(matches!(
            &compile(&s, p)[0],
            ProposalOutcome::Rejected(ProposalError("execution_plan_bounds"))
        ));
    }
}

#[test]
fn bounded_inspection_is_validated_and_consumed_without_authorizing_a_d() {
    let mut s = state();
    let policy = &mut s.frozen.policy.proposal.as_mut().unwrap().policy;
    policy.allow_source_text = true;
    policy.max_source_bytes = 16384;
    let source = json!({"file_id":"server","digest":format!("sha256:{}","b".repeat(64))});
    let batch = serde_json::to_vec(&json!({"schema":PROPOSAL_SCHEMA,"proposals":[{"kind":"inspect_source","sources":[source]}]})).unwrap();
    let output = ProducerOutput::new(
        batch,
        ProducerProvenance {
            provider: "fixed".into(),
            model: None,
        },
    )
    .unwrap();
    let mut registry = CandidateRegistry::new(&s.frozen)
        .unwrap()
        .with_inspection_budget(1)
        .unwrap();
    assert!(
        matches!(&registry.validate_batch(&BTreeMap::new(), &output).unwrap()[0], ProposalOutcome::InspectionRequested(sources) if sources.len() == 1)
    );
    assert!(registry.generated().is_empty());
    assert!(matches!(
        &registry.validate_batch(&BTreeMap::new(), &output).unwrap()[0],
        ProposalOutcome::Rejected(ProposalError("inspection_budget_exhausted"))
    ));
}

#[test]
fn public_catalog_is_bounded_and_does_not_export_the_path_map() {
    let mut s = state();
    let auth = s
        .frozen
        .policy
        .proposal
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap();
    for n in 0..100 {
        auth.files.insert(
            format!("file{n}"),
            VerifiedSourceFile {
                path: format!("nested/path/{n}.py"),
                digest: format!("sha256:{}", "b".repeat(64)),
            },
        );
    }
    let catalog = s
        .frozen
        .policy
        .proposal
        .as_ref()
        .unwrap()
        .catalog()
        .unwrap();
    let OperationDomain::ExecutionPlan { sources, .. } = &catalog.operations[0] else {
        panic!()
    };
    assert_eq!(sources.len(), 32);
    assert_eq!(sources[0].reference.file_id, "server");
    let bytes = serde_json::to_string(&catalog).unwrap();
    assert!(!bytes.contains("nested/path"));
    assert!(!bytes.contains("server.py"));
    let restored: OperationCatalog = serde_json::from_str(&bytes).unwrap();
    assert_eq!(restored, catalog);
}

#[test]
fn source_oci_proposal_is_canonical_source_bound_and_not_a_shell_plan() {
    let mut s = state();
    let auth = s
        .frozen
        .policy
        .proposal
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap();
    auth.files.get_mut("server").unwrap().path = "Dockerfile".into();
    auth.toolchains.insert("oci".into(), "1.0.0".into());
    auth.source_oci=Some(serde_json::from_value(json!({"schema":"ato.source-oci-recipe/1","dockerfile":"Dockerfile","platform":"linux/arm64","base_images":[],
        "build":{"memory_bytes":536870912,"cpu_limit_millis":1000,"pids_limit":128},"build_disk_bytes":536870912,
        "runtime":{"memory_bytes":268435456,"cpu_limit_millis":1000,"pids_limit":128},"build_timeout_seconds":60,"max_archive_bytes":1048576})).unwrap());
    let mut p = plan();
    p["runtime"] = json!({"name":"oci","version":"1.0.0"});
    let outcomes = compile(&s, p.clone());
    let ProposalOutcome::Admitted(c) = &outcomes[0] else {
        panic!("{outcomes:?}")
    };
    assert_eq!(c.compiled().derivation.steps.len(), 1);
    assert!(c.compiled().derivation.steps[0].argv.is_empty());
    assert!(c.compiled().derivation.source_oci.is_some());
    assert_eq!(c.compiled().base_contract_ref, s.frozen.base_contract_ref);
    p["argv"] = json!(["sh", "-c", "echo arbitrary"]);
    assert!(matches!(compile(&s, p)[0], ProposalOutcome::Rejected(_)));
}

#[test]
fn reusable_static_preset_authoring_roundtrips_without_changing_d_or_k() {
    use ato_formation::{
        capsule_toml::render_capsule_toml,
        preset::{AppPreset, synthesize_authoring},
    };
    let closure = format!("sha256:{}", "a".repeat(64));
    for preset in [AppPreset::StaticFiles, AppPreset::SingleHtml] {
        let before = synthesize_authoring(preset);
        let rendered = render_capsule_toml(&before).unwrap();
        let restored = parse_capsule_toml(&rendered).unwrap();
        assert_eq!(
            bind(
                &before,
                &BindingContext {
                    source_closure_ref: &closure
                }
            )
            .unwrap(),
            bind(
                &restored,
                &BindingContext {
                    source_closure_ref: &closure
                }
            )
            .unwrap()
        );
    }
    assert!(render_capsule_toml(&synthesize_authoring(AppPreset::NodeStatic)).is_err());
}

#[test]
fn generated_static_build_lowers_to_existing_browser_adapter_with_same_k() {
    let mut s = state();
    let domain = s
        .frozen
        .policy
        .proposal
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap();
    domain.toolchains = BTreeMap::from([
        ("node".into(), "22.14.0".into()),
        ("npm".into(), "10.9.2".into()),
    ]);
    domain.files.get_mut("server").unwrap().path = "package.json".into();
    let mut p = plan();
    p["runtime"] = json!({"name":"node","version":"22.14.0"});
    p["static_output"] = json!("build");
    let outcomes = compile(&s, p.clone());
    let ProposalOutcome::Admitted(candidate) = &outcomes[0] else {
        panic!("{outcomes:?}")
    };
    assert_eq!(
        candidate.compiled().base_contract_ref,
        s.frozen.contract_ref
    );
    let d = &candidate.compiled().derivation;
    assert_eq!(d.steps.last().unwrap().protocol, "ato.browser@1");
    assert_eq!(d.steps.last().unwrap().root.as_deref(), Some("build"));
    assert!(d.steps.last().unwrap().argv.is_empty());
    assert_eq!(d.ports[0].guest_port, None);
    let expected_d = candidate.compiled().derivation_ref.clone();
    p.as_object_mut().unwrap().remove("guest_port");
    let without_port = compile(&s, p.clone());
    let ProposalOutcome::Admitted(no_port) = &without_port[0] else {
        panic!("{without_port:?}")
    };
    assert_eq!(no_port.compiled().derivation_ref, expected_d);
    for invalid in ["/etc", "../outside", "/app/build"] {
        p["static_output"] = json!(invalid);
        assert!(matches!(
            compile(&s, p.clone())[0],
            ProposalOutcome::Rejected(ProposalError("unsupported_static_output"))
        ));
    }
    p["static_output"] = json!("build");
    p["argv"] = json!(["--arbitrary"]);
    assert!(matches!(
        compile(&s, p)[0],
        ProposalOutcome::Rejected(ProposalError("unsupported_static_output"))
    ));
}

#[test]
fn a_process_plan_without_an_explicit_guest_port_is_rejected() {
    let mut p = plan();
    p.as_object_mut().unwrap().remove("guest_port");
    assert!(
        matches!(&compile(&state(),p)[0],ProposalOutcome::Rejected(e) if e.0=="execution_plan_bounds")
    );
}

#[test]
fn omitted_unknowns_diagnostic_does_not_change_canonical_execution_or_requirements() {
    let s = state();
    let mut p = plan();
    let original = compile(&s, p.clone());
    let ProposalOutcome::Admitted(before) = &original[0] else {
        panic!()
    };
    p.as_object_mut().unwrap().remove("unknowns");
    let normalized = compile(&s, p);
    let ProposalOutcome::Admitted(after) = &normalized[0] else {
        panic!("{normalized:?}")
    };
    assert_eq!(before.compiled().derivation, after.compiled().derivation);
    assert_eq!(
        before.compiled().base_contract_ref,
        after.compiled().base_contract_ref
    );
}

#[test]
fn unsupported_reason_is_bounded_and_legacy_declines_still_parse() {
    let s = state();
    for (reason, rejected) in [
        (None, false),
        (Some("unsupported_toolchain"), false),
        (Some("free form secret"), true),
    ] {
        let mut proposal = json!({"kind":"unsupported"});
        if let Some(reason) = reason {
            proposal["reason"] = json!(reason);
        }
        let raw =
            serde_json::to_vec(&json!({"schema":PROPOSAL_SCHEMA,"proposals":[proposal]})).unwrap();
        let output = ProducerOutput::new(
            raw,
            ProducerProvenance {
                provider: "fixed".into(),
                model: None,
            },
        )
        .unwrap();
        let outcomes = CandidateRegistry::new(&s.frozen)
            .unwrap()
            .validate_batch(&BTreeMap::new(), &output)
            .unwrap();
        assert_eq!(
            matches!(outcomes[0], ProposalOutcome::Rejected(_)),
            rejected
        );
    }
}

#[test]
fn repeated_empty_declines_stop_without_using_the_third_round() {
    let mut s = state();
    for round in 0..2 {
        let SearchAction::OpenProposalRound {
            opened_at_ms,
            expires_at_ms,
        } = decide_next(&s, &[], 100 + round * 6000).unwrap()
        else {
            panic!("unexpected action")
        };
        if let Some(old) = s.proposal_round.take() {
            s.proposal_history.push(old);
        }
        s.proposal_round = Some(ProposalRoundRecord {
            opened_at_ms,
            expires_at_ms,
            outcome: Some(ProposalRoundOutcome::Completed),
            candidates: vec![],
            derivations: vec![],
            diagnostics: vec![],
            inspection_requests: vec![],
        });
    }
    let resumed: SearchStateV1 = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
    assert_eq!(
        decide_next(&resumed, &[], 13000).unwrap(),
        SearchAction::Finish {
            reason: Termination::NoProgress
        }
    );
    assert_eq!(
        resumed
            .frozen
            .policy
            .exploration
            .unwrap()
            .formation
            .max_rounds
            .get(),
        3
    );
}

#[test]
fn a_generated_d_waits_for_placement_without_spending_another_round_after_restart() {
    let mut s = state();
    let outcomes = compile(&s, plan());
    let ProposalOutcome::Admitted(candidate) = &outcomes[0] else {
        panic!()
    };
    let d = candidate.candidate().derivation_ref.clone();
    s.proposal_round = Some(ProposalRoundRecord {
        opened_at_ms: 100,
        expires_at_ms: 5100,
        outcome: Some(ProposalRoundOutcome::Completed),
        candidates: vec![candidate.candidate().clone()],
        derivations: vec![candidate.compiled().derivation.clone()],
        diagnostics: vec![],
        inspection_requests: vec![],
    });
    let frozen = s.frozen.canonical_bytes().unwrap();
    let resumed: SearchStateV1 = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
    assert_eq!(
        decide_next(&resumed, &[], 6000).unwrap(),
        SearchAction::WaitForRuntime {
            derivation_ref: d.clone()
        }
    );
    assert_eq!(resumed.frozen.canonical_bytes().unwrap(), frozen);
    let placement = Placement {
        candidate_id: "candidate".into(),
        derivation_ref: d.clone(),
        runtime_id: "runtime".into(),
        environment_id: "native".into(),
        admissible: true,
        transfer_bytes: 64,
    };
    assert!(matches!(
        decide_next(&resumed, std::slice::from_ref(&placement), 7000).unwrap(),
        SearchAction::IssueAttempt { .. }
    ));
    let refused = Placement {
        admissible: false,
        ..placement
    };
    assert!(matches!(
        decide_next(&resumed, &[refused], 7000).unwrap(),
        SearchAction::OpenProposalRound { .. }
    ));
    assert_eq!(
        resumed.proposal_history.len() + usize::from(resumed.proposal_round.is_some()),
        1
    );
}

#[test]
fn shared_inspection_and_generated_d_share_one_frozen_round() {
    use ato_formation::exploration::ReasoningLimits;
    let mut s = state();
    s.frozen.policy.exploration.as_mut().unwrap().reasoning = Some(ReasoningLimits {
        goal: None,
        round_timeout_ms: 60_000,
        inspection_timeout_ms: 20_000,
        inspection_source_bytes: 16384,
    });
    let auth = s.frozen.policy.proposal.as_mut().unwrap();
    auth.policy.allow_source_text = true;
    auth.policy.max_source_bytes = 16384;
    let reference = json!({"file_id":"server","digest":format!("sha256:{}","b".repeat(64))});
    let bytes=serde_json::to_vec(&json!({"schema":REASONING_BATCH_SCHEMA,"inspection_history":[reference],
        "proposals":[{"kind":"propose_derivation","operations":[{"operation":"execution_plan@1","plan":plan()}]}]})).unwrap();
    let output = ProducerOutput::new(
        bytes,
        ProducerProvenance {
            provider: "fixed".into(),
            model: None,
        },
    )
    .unwrap();
    let outcomes = CandidateRegistry::new(&s.frozen)
        .unwrap()
        .validate_batch(&BTreeMap::new(), &output)
        .unwrap();
    assert!(matches!(&outcomes[0],ProposalOutcome::InspectionRequested(refs) if refs.len()==1));
    let ProposalOutcome::Admitted(d) = &outcomes[1] else {
        panic!("{outcomes:?}")
    };
    assert_eq!(d.compiled().base_contract_ref, s.frozen.contract_ref);
    let SearchAction::OpenProposalRound {
        opened_at_ms,
        expires_at_ms,
    } = decide_next(&s, &[], 100).unwrap()
    else {
        panic!()
    };
    assert_eq!(expires_at_ms, (opened_at_ms + 60_000).min(s.deadline_ms));
    s.proposal_round = Some(ProposalRoundRecord {
        opened_at_ms,
        expires_at_ms,
        outcome: Some(ProposalRoundOutcome::Completed),
        candidates: vec![d.candidate().clone()],
        derivations: vec![d.compiled().derivation.clone()],
        diagnostics: vec![],
        inspection_requests: vec![serde_json::from_value(reference).unwrap()],
    });
    s.validate().unwrap();
    let restored: SearchStateV1 = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
    assert_eq!(restored.proposal_history.len(), 0);
    assert_eq!(restored.proposal_round, s.proposal_round);
    assert_eq!(
        restored
            .frozen
            .policy
            .exploration
            .unwrap()
            .formation
            .max_rounds
            .get(),
        3
    );
    s.frozen.policy.exploration.as_mut().unwrap().reasoning = None;
    assert!(
        CandidateRegistry::new(&s.frozen)
            .unwrap()
            .validate_batch(&BTreeMap::new(), &output)
            .is_err()
    );
}

#[test]
fn reasoning_limits_are_positive_bounded_and_frozen() {
    use ato_formation::exploration::ReasoningLimits;
    let mut p = state().frozen.policy.exploration.unwrap();
    for (round, inspection, bytes) in [
        (0, 1, 1),
        (900001, 1, 1),
        (100, 101, 1),
        (100, 1, 0),
        (100, 1, 65537),
    ] {
        p.reasoning = Some(ReasoningLimits {
            goal: None,
            round_timeout_ms: round,
            inspection_timeout_ms: inspection,
            inspection_source_bytes: bytes,
        });
        assert_eq!(p.validate().unwrap_err().0, "reasoning_budget_invalid");
    }
    p.reasoning = None;
    let json = serde_json::to_string(&p).unwrap();
    assert!(!json.contains("reasoning"));
}

#[test]
fn shared_decline_after_inspection_stops_without_another_round() {
    use ato_formation::exploration::ReasoningLimits;
    let mut s = state();
    s.frozen.policy.exploration.as_mut().unwrap().reasoning = Some(ReasoningLimits {
        goal: None,
        round_timeout_ms: 600_000,
        inspection_timeout_ms: 300_000,
        inspection_source_bytes: 32768,
    });
    s.proposal_round = Some(ProposalRoundRecord {
        opened_at_ms: 0,
        expires_at_ms: s.deadline_ms,
        outcome: Some(ProposalRoundOutcome::Completed),
        candidates: vec![],
        derivations: vec![],
        diagnostics: vec!["source_inspection_requested".into()],
        inspection_requests: vec![SourceReference {
            file_id: "server".into(),
            digest: format!("sha256:{}", "b".repeat(64)),
        }],
    });
    s.frozen
        .policy
        .proposal
        .as_mut()
        .unwrap()
        .policy
        .allow_source_text = true;
    s.frozen
        .policy
        .proposal
        .as_mut()
        .unwrap()
        .policy
        .max_source_bytes = 16384;
    assert!(matches!(
        decide_next(&s, &[], 1).unwrap(),
        SearchAction::Finish {
            reason: Termination::NoProgress,
            ..
        }
    ));
}

#[test]
fn python_source_requirements_resolve_hash_and_install_offline_without_changing_k() {
    let mut s = state();
    s.frozen
        .policy
        .proposal
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap()
        .files
        .insert(
            "requirements".into(),
            VerifiedSourceFile {
                path: "requirements.txt".into(),
                digest: format!("sha256:{}", "c".repeat(64)),
            },
        );
    let mut p = plan();
    p["dependencies"] = json!([{"kind":"python_resolve_requirements", "requirements":{
        "file_id":"requirements", "digest":format!("sha256:{}", "c".repeat(64))}}]);
    let outcome = compile(&s, p);
    let ProposalOutcome::Admitted(d) = &outcome[0] else {
        panic!("{outcome:?}")
    };
    assert_eq!(d.compiled().base_contract_ref, s.frozen.contract_ref);
    let steps = &d.compiled().derivation.steps;
    assert_eq!(steps.len(), 5);
    assert!(steps[1].argv.contains(&"download".into()));
    assert_eq!(
        steps[1].network,
        ato_formation::authoring::StepNetwork::ScopedDependencies
    );
    assert!(
        steps[2]
            .argv
            .iter()
            .any(|a| a.contains("hashlib.file_digest"))
    );
    assert!(steps[3].argv.contains(&"--require-hashes".into()));
    assert!(steps[3].argv.contains(&"--no-index".into()));
    assert!(steps[2].network.is_denied() && steps[3].network.is_denied());
    let config: FormationConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(config.max_rounds.get(), 3);
    assert_eq!(config.max_retries, 3);
}

#[test]
fn bindings_change_d_without_changing_k_and_never_carry_values() {
    let r = serde_json::json!({"name":"JWT_SECRET","kind":"signing_secret","purpose":"Sign local sessions","resource":"session.signing","operation":"execute","phase":"runtime","secret":true,"temporary":true});
    let req: ato_formation::variables::VariableRequirement =
        serde_json::from_value(r.clone()).unwrap();
    assert!(ato_formation::variables::validate(&[req.clone()]).is_ok());
    assert!(
        ato_formation::variables::validate(&[ato_formation::variables::VariableRequirement {
            service: Some("external".into()),
            ..req.clone()
        }])
        .is_err()
    );
    let source=ato_formation::capsule_toml::parse_capsule_toml("schema='ato.capsule/1'\n[[input]]\nid='workspace'\nuse='ato.workspace@1'\npath='.'\n[[derive.step]]\nid='app'\nuse='ato.process@1'\nop='serve'\nargv=['python','app.py']\n[[port]]\nid='app.http'\nuse='ato.http@1'\nfrom='app'\nguest_port=8080\n[[contract.require]]\nid='health'\nuse='ato.contract.http@1'\nport='app.http'\nmethod='GET'\npath='/'\n[contract.require.expect]\nstatus=200").unwrap();
    let context = ato_formation::authoring::BindingContext {
        source_closure_ref: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    };
    let (k, d) = ato_formation::authoring::bind(&source, &context).unwrap();
    let mut bound = source.clone();
    bound.derivation.variable_bindings.push(req);
    let rendered = ato_formation::capsule_toml::render_capsule_toml(&bound).unwrap();
    assert_eq!(
        ato_formation::capsule_toml::parse_capsule_toml(&rendered).unwrap(),
        bound
    );
    let (same_k, new_d) = ato_formation::authoring::bind(&bound, &context).unwrap();
    assert_eq!(k, same_k);
    assert_ne!(d.derivation_ref().unwrap(), new_d.derivation_ref().unwrap());
    assert!(!serde_json::to_string(&new_d).unwrap().contains("value"));
}

#[test]
fn shared_provider_infrastructure_failure_preserves_one_round_on_restart() {
    let mut s = state();
    s.frozen.policy.exploration.as_mut().unwrap().reasoning = Some(serde_json::from_value(json!({"round_timeout_ms":600000,"inspection_timeout_ms":30000,"inspection_source_bytes":32768})).unwrap());
    s.proposal_round = Some(ProposalRoundRecord {
        opened_at_ms: 100,
        expires_at_ms: 60000,
        outcome: Some(ProposalRoundOutcome::ProviderError),
        candidates: vec![],
        derivations: vec![],
        diagnostics: vec![],
        inspection_requests: vec![],
    });
    let restored: SearchStateV1 = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
    assert_eq!(
        decide_next(&restored, &[], 6000).unwrap(),
        SearchAction::Finish {
            reason: Termination::InfrastructureFailure
        }
    );
    assert!(restored.proposal_history.is_empty());
}

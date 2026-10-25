//! Separate functional acceptance of a retained Codex D. Never an API seed.
//! The Coordinator dispatches the state grant; the existing Connected Runner
//! session redeems, restores, commits and releases it. Ordinary Run execution
//! authorization is neither created nor consumed by this helper.
use anyhow::{Context, Result, ensure};
use ato_connected_realization_worker::runtime_launch::{
    session::{PreparedRun, abort_run, commit_run, prepare_run, quarantine_run, working_copy},
    state_artifact::LeaseStateArtifactTransport,
};
use ato_formation::{execution::lower_retained, retained::RetainedCandidateV1};
use ato_formation_worker::{
    local::probe_local_runtime,
    runtime_network::{
        self, AttemptTicket, AvailabilityReport, Client, EnvironmentAdvert, RuntimeDescriptorAdvert,
    },
};
use ato_ipc::runtime_launch::RuntimeLaunchSpecV1;
use ato_runtime_attempt::{
    admission::EffectAuthorization,
    attempt::{AttemptRequest, Continuation, ReceiptContext, run_attempt},
    build_sandbox::NetworkPolicy,
    control::{ExecutionControl, now_ms},
    journal::AttemptJournal,
    launch::resolved::{ResolvedRuntimeLaunchContext, ResolvedStateAttachment},
    plan::{BoundCandidate, PlannedCandidate},
    realize::StopClass,
    retained::{RetainedCandidateRealizer, RetainedExploration},
    state_bindings::{VerificationStateAttachment, VerificationStateBindings},
};
use netd::egress::gate::{EgressAllowance, EgressGate};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StateInput {
    api: String,
    lease_id: String,
    token_file: PathBuf,
    slot_id: String,
    spec: RuntimeLaunchSpecV1,
    #[serde(default)]
    formation_input: Option<FormationInput>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FormationInput {
    token_file: PathBuf,
    search_id: String,
}

/// Retain the writer on every uncertain error, including I/O or a failed
/// explicit stop. A signal/crash also leaves the Coordinator grant held.
struct StateSession<'a> {
    transport: &'a LeaseStateArtifactTransport,
    prepared: PreparedRun,
    confirmed_stopped: bool,
    committed: bool,
}
impl Drop for StateSession<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if self.confirmed_stopped {
            abort_run(self.transport, &self.prepared);
        } else {
            quarantine_run(
                self.transport,
                &self.prepared,
                "verification stop unconfirmed",
            );
        }
    }
}

fn require_private(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            fs::metadata(path)?.permissions().mode() & 0o077 == 0,
            "binding and token files must be private"
        );
    }
    Ok(())
}

fn require_local_api(api: &str) -> Result<()> {
    let url = reqwest::Url::parse(api)?;
    ensure!(
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "explicit isolated local Coordinator required"
    );
    Ok(())
}

fn advertise_inputs(api: &str, token_file: &Path) -> Result<()> {
    require_local_api(api)?;
    require_private(token_file)?;
    let client = Client::new(api, fs::read_to_string(token_file)?.trim())?;
    let profile = runtime_network::ticket_runtime_profile(true, Some(&client));
    let mut facts = runtime_network::probe_facts(None);
    facts.insert(
        ato_formation::port_operations::RUNTIME_CAPABILITY.into(),
        profile
            .get(ato_formation::port_operations::RUNTIME_CAPABILITY)
            .unwrap_or("false")
            .into(),
    );
    client.advertise(&RuntimeDescriptorAdvert {
        protocol: runtime_network::PROTOCOL.into(),
        agent_version: runtime_network::attestation().agent_version,
        environments: vec![EnvironmentAdvert {
            environment_id: runtime_network::NATIVE_ENVIRONMENT.into(),
            facts,
        }],
    })?;
    client.report_availability(&AvailabilityReport {
        capacity: 1,
        current_slots: 0,
        health: "ok".into(),
    })?;
    println!("{{\"actual_Runtime_advertised\":true,\"ordinary_Run_authorized\":false}}");
    Ok(())
}

fn report_attempt(
    binding: Option<&(Client, AttemptTicket)>,
    descriptor: &RetainedCandidateV1,
    planned: &PlannedCandidate,
    outcome: &ato_runtime_attempt::attempt::AttemptOutcome,
    stop: Option<&StopClass>,
    root: &Path,
    control: &ExecutionControl,
) -> Result<()> {
    let Some((client, ticket)) = binding else {
        return Ok(());
    };
    let mut attested = runtime_network::attestation();
    (attested.requirements, attested.provisions) =
        runtime_network::derivation_requirements(planned);
    attested.contract_ref = Some(ticket.contract_ref.clone());
    attested.derivation_ref = Some(ticket.derivation_ref.clone());
    attested.effects = Some(ato_runtime_attempt::admission::effects_name(
        planned.derivation.effects,
    ));
    attested.execution_started = outcome.execution_started();
    attested.attempt_record = outcome.attempt_record.clone();
    attested.candidate_stop = stop.map(runtime_network::CandidateStopAttestation::of);
    let pass = outcome.attempt.status == ato_formation::request::AttemptStatus::Verified;
    let mut report = runtime_network::attempt_report(
        ticket,
        &outcome.attempt,
        &attested,
        pass.then(|| descriptor.materialization_ref.clone()),
        None,
        runtime_network::ResourceUsage {
            expanded_bytes: descriptor.artifact.expanded_bytes,
            stored_bytes: 0,
        },
    );
    report.verifier_receipts.push(serde_json::json!({
        "kind":"exploration_phase_timings", "phases":control.timings()
    }));
    fs::write(
        root.join("formation-result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    // Reporting is allowed after the execution clock; this never reclaims the
    // ticket or restarts an execution on an uncertain communication failure.
    client.report_recorded(&root.join("formation-delivery"), ticket, &report)?;
    fs::write(
        root.join("formation-result-ACK.json"),
        "{\"ACK\":true,\"execution_replayed\":false}\n",
    )?;
    Ok(())
}

fn main() -> Result<()> {
    let a: Vec<_> = std::env::args().collect();
    if a.len() == 4 && a[1] == "--advertise-bound-inputs" {
        return advertise_inputs(&a[2], Path::new(&a[3]));
    }
    if a.len() == 5 && a[1] == "--report-saved" {
        require_local_api(&a[2])?;
        require_private(Path::new(&a[3]))?;
        let root = Path::new(&a[4]);
        let ticket: AttemptTicket =
            serde_json::from_slice(&fs::read(root.join("formation-ticket.json"))?)?;
        let report: runtime_network::AttemptResultReport =
            serde_json::from_slice(&fs::read(root.join("formation-result.json"))?)?;
        Client::new(&a[2], fs::read_to_string(&a[3])?.trim())?.report_recorded(
            &root.join("formation-delivery"),
            &ticket,
            &report,
        )?;
        println!(
            "{{\"saved_result_ACK\":true,\"execution_replayed\":false,\"inputs_redeemed_again\":false}}"
        );
        return Ok(());
    }
    ensure!(
        a.len() == 7,
        "descriptor archive retained-ref shim NEW_OUTPUT_ROOT PRIVATE_ASSIGNED_STATE_FILE"
    );
    require_private(Path::new(&a[6]))?;
    let input: StateInput = serde_json::from_slice(&fs::read(&a[6])?)?;
    require_private(&input.token_file)?;
    require_local_api(&input.api)?;
    input
        .spec
        .validate()
        .context("invalid assigned logical state spec")?;
    ensure!(
        input.spec.state_attachments.len() == 1 && input.spec.secret_grants.is_empty(),
        "this acceptance binds one revision-backed state slot; Formation inputs require their own current fenced grant"
    );
    let descriptor = RetainedCandidateV1::parse(&fs::read(&a[1])?, &a[3])?;
    ensure!(
        matches!(
            descriptor.shape,
            ato_formation::retained::RetainedShape::ProcessWorkspace { .. }
        ) && descriptor.derivation.source_oci.is_none()
            && (descriptor.derivation.variable_bindings.is_empty()
                || input.formation_input.is_some()),
        "process artifact required; variable bindings require the current Coordinator input path"
    );
    ensure!(
        descriptor
            .derivation
            .requirements
            .network
            .iter()
            .all(|n| n.phase != ato_formation::requirements::ExecutionPhase::Runtime),
        "no Runtime egress is granted"
    );
    ensure!(
        descriptor.artifact.content_ref == input.spec.workspace.materialization_ref,
        "state assignment must name this fixed retained artifact"
    );
    let root = PathBuf::from(&a[5]);
    ensure!(!root.exists(), "preserve earlier acceptance evidence");
    fs::create_dir_all(&root)?;
    let binding = input
        .formation_input
        .as_ref()
        .map(|f| -> Result<_> {
            require_private(&f.token_file)?;
            let client = Client::new(&input.api, fs::read_to_string(&f.token_file)?.trim())?;
            let ticket = client
                .claim_recorded(&root.join("formation-delivery"))?
                .context("current Formation attempt is not ready")?;
            descriptor.match_assignment(&ticket.contract_ref, &ticket.derivation_ref)?;
            ensure!(
                ticket.base_contract_ref == descriptor.base_contract_ref
                    && ticket.browser_contract == descriptor.browser_contract
                    && ticket
                        .exploration
                        .as_ref()
                        .is_some_and(|e| e.search_id == f.search_id && e.deadline_ms.is_some()),
                "current fenced assignment must match original Search, K and D"
            );
            fs::write(
                root.join("formation-ticket.json"),
                serde_json::to_vec_pretty(&ticket)?,
            )?;
            Ok((client, ticket))
        })
        .transpose()?;
    let deadline = binding
        .as_ref()
        .and_then(|(_, t)| t.exploration.as_ref()?.deadline_ms)
        .unwrap_or_else(|| now_ms() + 300_000);
    let control = ExecutionControl::new(deadline);
    control.remaining(ato_runtime_attempt::control::AttemptPhase::Source)?;
    let variables = binding
        .as_ref()
        .map(|(client, ticket)| {
            client.with_deadline(deadline).resolve_variables(
                ticket,
                &descriptor.derivation.variable_bindings,
                &root.join("input-delivery"),
            )
        })
        .transpose()?
        .unwrap_or_default();
    let state_workspace = root.join("state-workspace");
    fs::create_dir(&state_workspace)?;
    let declaration = &input.spec.state_attachments[0];
    let private_context = ResolvedRuntimeLaunchContext::new(
        state_workspace.clone(),
        "",
        BTreeMap::new(),
        vec![],
        vec![ResolvedStateAttachment::new(
            &declaration.state_key,
            declaration.revision_ref.clone(),
            working_copy(&state_workspace, &declaration.state_key),
            &declaration.mount_target,
            declaration.access,
        )],
        vec![],
    )?;
    // Bounded reporting transport also permits commit/cleanup after execution
    // expires. Formation input grants retain the actual ticket's absolute
    // deadline; the historical input-free private gate retains its 300 seconds.
    let transport = LeaseStateArtifactTransport::new(
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        &input.api,
        &input.lease_id,
        fs::read_to_string(&input.token_file)?.trim(),
    );
    let prepared = prepare_run(
        &input.spec.state_attachments,
        &private_context,
        &transport.with_execution_control(&control, Duration::from_secs(60)),
        &BTreeMap::new(),
    )?;
    let mut session = StateSession {
        transport: &transport,
        prepared,
        confirmed_stopped: true,
        committed: false,
    };
    let attachments = [VerificationStateAttachment {
        slot_id: &input.slot_id,
        declaration,
        resolved: &private_context.state_attachments()[0],
    }];
    let state = VerificationStateBindings {
        context: &input.spec.context,
        attachments: &attachments,
    };
    control.remaining(ato_runtime_attempt::control::AttemptPhase::Launch)?;
    let attempt_id = binding
        .as_ref()
        .map(|(_, t)| t.attempt_id.clone())
        .unwrap_or_else(|| format!("functional-state-{}", input.spec.context.run_id));
    let planned = PlannedCandidate {
        bound: BoundCandidate {
            contract: descriptor.base_contract.clone(),
            derivation: descriptor.derivation.clone(),
            contract_ref: descriptor.base_contract_ref.clone(),
            derivation_ref: descriptor.derivation_ref.clone(),
        },
        plan: lower_retained(&descriptor)?,
    };
    let spec = planned.attempt_spec();
    let grant = binding
        .as_ref()
        .and_then(|(_, t)| t.exploration.as_ref().map(|e| e.ceiling.clone()))
        .unwrap_or_else(|| descriptor.derivation.requirements.clone());
    let profile = binding
        .as_ref()
        .map(|(c, _)| runtime_network::ticket_runtime_profile(true, Some(c)))
        .unwrap_or_else(probe_local_runtime);
    let gate = root.join("no-egress.s");
    let egress = EgressGate::start(
        "127.0.0.1:0".parse()?,
        EgressAllowance {
            hosts: vec!["exploration-denied.invalid".into()],
            ports: vec![443],
            max_transfer_bytes: 1,
        },
    )?;
    let _bridge = ato_runtime_attempt::network_bridge::HostBridge::start(&gate, egress.address())?;
    session.confirmed_stopped = false;
    let mut outcome = run_attempt(
        &AttemptRequest {
            request_id: binding
                .as_ref()
                .map(|(_, t)| t.satisfy_id.as_str())
                .unwrap_or(&input.spec.context.run_id),
            attempt_id: &attempt_id,
            label: "separate-functional-acceptance",
            spec: &spec,
            contract_ref: &descriptor.contract_ref,
            runtime_id: binding
                .as_ref()
                .map(|(_, t)| t.runtime_id.as_str())
                .unwrap_or("local-acceptance"),
            profile: &profile,
            authorization: EffectAuthorization::Exploration {
                derivation_ref: &descriptor.derivation_ref,
                grant: &grant,
            },
            network: NetworkPolicy::Scoped,
            browser: None,
            attempt_root: &root.join("attempt"),
            continuation: Continuation::HandOff,
            receipt: ReceiptContext::formation(),
            interrupt: None,
            control: Some(&control),
        },
        &RetainedCandidateRealizer {
            descriptor: &descriptor,
            archive: Path::new(&a[2]),
            expected_contract_ref: &descriptor.contract_ref,
            expected_derivation_ref: &descriptor.derivation_ref,
            expanded_limit: 512 * 1024 * 1024,
            shim: Path::new(&a[4]),
            exploration: Some(RetainedExploration {
                ceiling: &grant,
                runtime_gate: &gate,
                variables: &variables,
                state: Some(&state),
            }),
        },
        &AttemptJournal::new(root.join("records")),
    );
    if outcome.live.is_none() {
        session.confirmed_stopped = !outcome.execution_started()
            || outcome.stop.as_ref().is_some_and(StopClass::is_confirmed);
    }
    fs::write(
        root.join("fresh-attempt.json"),
        serde_json::to_vec_pretty(&outcome.attempt)?,
    )?;
    if outcome.live.is_none() {
        report_attempt(
            binding.as_ref(),
            &descriptor,
            &planned,
            &outcome,
            outcome.stop.as_ref(),
            &root,
            &control,
        )?;
    }
    let live = outcome
        .live
        .take()
        .context("fresh common Runtime attempt did not pass")?;
    // Execute fallible interactive reporting while keeping explicit ownership
    // of the process. Even a disconnected controller goes through stop below.
    let interaction = (|| -> Result<()> {
        println!(
            "{}",
            serde_json::json!({"endpoint":live.endpoint().context("missing HTTP endpoint")?,
            "contract_ref":descriptor.contract_ref,"derivation_ref":descriptor.derivation_ref,
            "fresh_attempt_id":attempt_id,"Coordinator_assignment_verified":true,
            "lease_id":input.lease_id,"state_attachment":declaration,
            "normal_run_authorized":false,"API_calls":0})
        );
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        Ok(())
    })();
    let cleanup_phase = control.cleanup();
    let stopped = live.stop();
    let stop = StopClass::of(&stopped);
    session.confirmed_stopped = stop.is_confirmed();
    if !session.confirmed_stopped {
        report_attempt(
            binding.as_ref(),
            &descriptor,
            &planned,
            &outcome,
            Some(&stop),
            &root,
            &control,
        )?;
    }
    stopped?;
    interaction?;
    let outcomes = commit_run(&private_context, &transport, &session.prepared, &attempt_id)?;
    session.committed = true;
    let outcomes: Vec<_> = outcomes.iter().map(|s| serde_json::json!({"state_key":s.state_key,
        "parent_revision_ref":s.parent_revision_ref,"revision_ref":s.revision_ref,"writer_fence":s.writer_fence})).collect();
    fs::write(
        root.join("state-outcomes.json"),
        serde_json::to_vec_pretty(&outcomes)?,
    )?;
    drop(cleanup_phase);
    fs::write(
        root.join("network.json"),
        serde_json::to_vec_pretty(&egress.report())?,
    )?;
    fs::write(
        root.join("cleanup.json"),
        "{\"stopped\":true,\"API_calls\":0}\n",
    )?;
    // State commit and writer release do not depend on reporting connectivity.
    // If delivery fails, its saved payload is the only eligible recovery work.
    report_attempt(
        binding.as_ref(),
        &descriptor,
        &planned,
        &outcome,
        Some(&stop),
        &root,
        &control,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::require_local_api;

    #[test]
    fn private_acceptance_cannot_select_remote_or_credentialled_origins() {
        assert!(require_local_api("http://127.0.0.1:29663").is_ok());
        for origin in [
            "https://127.0.0.1:29663",
            "http://localhost:29663",
            "http://127.0.0.1",
            "http://127.0.0.1:29663/private",
            "http://token@127.0.0.1:29663",
            "http://127.0.0.1:29663?scope=x",
            "http://127.0.0.1:29663#fragment",
            "http://example.invalid:29663",
        ] {
            assert!(require_local_api(origin).is_err(), "accepted {origin}");
        }
    }
}

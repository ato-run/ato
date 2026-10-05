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
use ato_formation_worker::local::probe_local_runtime;
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

fn main() -> Result<()> {
    let a: Vec<_> = std::env::args().collect();
    ensure!(
        a.len() == 7,
        "descriptor archive retained-ref shim NEW_OUTPUT_ROOT PRIVATE_ASSIGNED_STATE_FILE"
    );
    require_private(Path::new(&a[6]))?;
    let input: StateInput = serde_json::from_slice(&fs::read(&a[6])?)?;
    require_private(&input.token_file)?;
    let url = reqwest::Url::parse(&input.api)?;
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
    input
        .spec
        .validate()
        .context("invalid assigned logical state spec")?;
    ensure!(
        input.spec.state_attachments.len() == 1 && input.spec.secret_grants.is_empty(),
        "this acceptance binds exactly one revision-backed state slot and no secrets"
    );
    let descriptor = RetainedCandidateV1::parse(&fs::read(&a[1])?, &a[3])?;
    ensure!(
        matches!(
            descriptor.shape,
            ato_formation::retained::RetainedShape::ProcessWorkspace { .. }
        ) && descriptor.derivation.source_oci.is_none()
            && descriptor.derivation.variable_bindings.is_empty(),
        "this acceptance requires an existing process artifact without OCI or secret bindings"
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
    // expires. The shared attempt retains its original 300-second deadline.
    let control = ExecutionControl::new(now_ms() + 300_000);
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
        &transport,
        &BTreeMap::new(),
    )?;
    let mut session = StateSession {
        transport: &transport,
        prepared,
        confirmed_stopped: false,
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
    let attempt_id = format!("functional-state-{}", input.spec.context.run_id);
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
    let grant = descriptor.derivation.requirements.clone();
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
    let outcome = run_attempt(
        &AttemptRequest {
            request_id: &input.spec.context.run_id,
            attempt_id: &attempt_id,
            label: "separate-functional-acceptance",
            spec: &spec,
            contract_ref: &descriptor.contract_ref,
            runtime_id: "local-acceptance",
            profile: &probe_local_runtime(),
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
                variables: &[],
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
    let live = outcome
        .live
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
    let stopped = live.stop();
    session.confirmed_stopped = StopClass::of(&stopped).is_confirmed();
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
    fs::write(
        root.join("network.json"),
        serde_json::to_vec_pretty(&egress.report())?,
    )?;
    fs::write(
        root.join("cleanup.json"),
        "{\"stopped\":true,\"API_calls\":0}\n",
    )?;
    Ok(())
}

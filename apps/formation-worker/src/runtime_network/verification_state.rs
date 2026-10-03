//! Coordinator-assigned state for a separately authorized retained verification.
//! No caller-supplied token, host path, manual mount or state namespace selection.
use super::{AttemptTicket, CandidateStopAttestation, Client};
use anyhow::{Result, ensure};
use ato_formation::{requirements::ExecutionRequirements, retained::RetainedCandidateV1};
use ato_ipc::runtime_launch::{RuntimeLaunchSpecV1, StateAccessV1};
use ato_runtime_attempt::{
    control::ExecutionControl,
    launch::{
        resolved::{ResolvedRuntimeLaunchContext, ResolvedStateAttachment},
        session::{PreparedRun, abort_run, commit_run, prepare_run, quarantine_run, working_copy},
        state_artifact::LeaseStateArtifactTransport,
    },
    state_bindings::VerificationStateAttachment,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, time::Duration};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationStateTicket {
    pub schema: String,
    pub search_id: String,
    pub lease_id: String,
    pub deadline_ms: u64,
    pub ceiling: ExecutionRequirements,
    pub launch_spec_digest: String,
    pub launch_spec: RuntimeLaunchSpecV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<ato_formation::functional_acceptance::FunctionalAcceptanceV1>,
}

pub(super) struct VerificationStateSession {
    pub spec: RuntimeLaunchSpecV1,
    context: ResolvedRuntimeLaunchContext,
    transport: LeaseStateArtifactTransport,
    prepared: Option<PreparedRun>,
    stopped: std::cell::Cell<bool>,
}
impl VerificationStateSession {
    pub fn prepare(
        g: &VerificationStateTicket,
        descriptor: &RetainedCandidateV1,
        ticket: &AttemptTicket,
        client: &Client,
        root: &Path,
        control: Option<&ExecutionControl>,
    ) -> Result<Self> {
        g.launch_spec.validate()?;
        ensure!(
            g.schema == "ato.functional-verification-state/1"
                && !g.search_id.is_empty()
                && !g.lease_id.is_empty()
                && g.deadline_ms > 0
                && g.launch_spec.canonical_digest()? == g.launch_spec_digest
                && g.launch_spec.workspace.materialization_ref == descriptor.artifact.content_ref
                && g.launch_spec.secret_grants.is_empty()
                && matches!(ticket.input, super::AttemptInput::Retained { .. }),
            "verification_state_assignment_invalid"
        );
        descriptor
            .derivation
            .requirements
            .within(&g.ceiling)
            .map_err(|e| anyhow::anyhow!(e.0))?;
        let [slot] = descriptor.derivation.state.as_slice() else {
            anyhow::bail!("verification_state_shape");
        };
        let [attachment] = g.launch_spec.state_attachments.as_slice() else {
            anyhow::bail!("verification_state_shape");
        };
        ensure!(
            attachment.state_key == slot.id
                && attachment.mount_target == slot.mount
                && attachment.access == StateAccessV1::ReadWrite
                && attachment.writer_fence.is_some_and(|f| f > 0),
            "verification_state_mapping_invalid"
        );
        let state_root = root.join("assigned-state");
        std::fs::create_dir_all(&state_root)?;
        let state_root = state_root.canonicalize()?;
        let context = ResolvedRuntimeLaunchContext::new(
            state_root.clone(),
            "",
            BTreeMap::new(),
            vec![],
            vec![ResolvedStateAttachment::new(
                &attachment.state_key,
                attachment.revision_ref.clone(),
                working_copy(&state_root, &attachment.state_key),
                &attachment.mount_target,
                attachment.access,
            )],
            vec![],
        )?;
        let transport = LeaseStateArtifactTransport::new(
            client.http.clone(),
            &client.api,
            &g.lease_id,
            &client.token,
        );
        let bounded = control.map(|c| transport.with_execution_control(c, Duration::from_secs(60)));
        let prepared = prepare_run(
            &g.launch_spec.state_attachments,
            &context,
            bounded.as_ref().map_or(
                &transport
                    as &dyn ato_runtime_attempt::launch::state_artifact::StateArtifactTransport,
                |b| b,
            ),
            &BTreeMap::new(),
        )?;
        Ok(Self {
            spec: g.launch_spec.clone(),
            context,
            transport,
            prepared: Some(prepared),
            stopped: std::cell::Cell::new(true),
        })
    }
    pub fn entries<'a>(
        &'a self,
        descriptor: &'a RetainedCandidateV1,
    ) -> Vec<VerificationStateAttachment<'a>> {
        vec![VerificationStateAttachment {
            slot_id: &descriptor.derivation.state[0].id,
            declaration: &self.spec.state_attachments[0],
            resolved: &self.context.state_attachments()[0],
        }]
    }
    pub fn mark_started(&self) {
        self.stopped.set(false);
    }
    pub fn finish(
        &mut self,
        started: bool,
        stop: Option<&CandidateStopAttestation>,
        request_id: &str,
    ) -> Result<serde_json::Value> {
        self.stopped
            .set(!started || matches!(stop, Some(CandidateStopAttestation::Confirmed)));
        let prepared = self.prepared.take().expect("one state settlement");
        if !self.stopped.get() {
            quarantine_run(&self.transport, &prepared, "candidate_stop_unconfirmed");
            anyhow::bail!("verification_state_stop_unconfirmed");
        }
        if !started {
            abort_run(&self.transport, &prepared);
            return Ok(
                serde_json::json!({"kind":"functional_state_outcome","run_id":self.spec.context.run_id,"status":"not_started"}),
            );
        }
        let outcomes = commit_run(&self.context, &self.transport, &prepared, request_id)?;
        Ok(
            serde_json::json!({"kind":"functional_state_outcome","run_id":self.spec.context.run_id,"status":"committed","states":outcomes.iter().map(|o|serde_json::json!({"state_key":o.state_key,"parent_revision_ref":o.parent_revision_ref,"revision_ref":o.revision_ref,"writer_fence":o.writer_fence})).collect::<Vec<_>>() }),
        )
    }
}
impl Drop for VerificationStateSession {
    fn drop(&mut self) {
        if let Some(prepared) = self.prepared.take() {
            if self.stopped.get() {
                abort_run(&self.transport, &prepared);
            } else {
                quarantine_run(&self.transport, &prepared, "state_session_abandoned");
            }
        }
    }
}

// The common attempt writes its durable start before realization. Functional
// actions and state commit extend that same effect interval: do not write the
// finish until both settle, so a crash cannot replay an accepted HTTP action.
pub(super) struct DeferredFinish(
    std::rc::Rc<std::cell::RefCell<Option<Box<dyn ato_runtime_attempt::journal::StartedRecord>>>>,
);
struct DeferredPermit {
    inner: Box<dyn ato_runtime_attempt::journal::AttemptPermit>,
    finish: DeferredFinish,
}
struct DeferredStarted {
    inner: Box<dyn ato_runtime_attempt::journal::StartedRecord>,
    finish: DeferredFinish,
}
impl Clone for DeferredFinish {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl DeferredFinish {
    pub fn wrap(
        inner: Box<dyn ato_runtime_attempt::journal::AttemptPermit>,
    ) -> (Box<dyn ato_runtime_attempt::journal::AttemptPermit>, Self) {
        let finish = Self(std::rc::Rc::new(std::cell::RefCell::new(None)));
        (
            Box::new(DeferredPermit {
                inner,
                finish: finish.clone(),
            }),
            finish,
        )
    }
    pub fn finish(
        self,
        outcome: &str,
        previous: ato_runtime_attempt::journal::AttemptRecordState,
    ) -> ato_runtime_attempt::journal::AttemptRecordState {
        match self.0.borrow_mut().take() {
            Some(started) => {
                if started.finish(outcome).is_ok() {
                    ato_runtime_attempt::journal::AttemptRecordState::Finished
                } else {
                    ato_runtime_attempt::journal::AttemptRecordState::StartedUnfinished
                }
            }
            None => previous,
        }
    }
}
impl ato_runtime_attempt::journal::AttemptPermit for DeferredPermit {
    fn start(
        &mut self,
        identity: ato_runtime_attempt::journal::StartIdentity,
    ) -> std::result::Result<
        Box<dyn ato_runtime_attempt::journal::StartedRecord>,
        ato_runtime_attempt::journal::BeginRefusal,
    > {
        Ok(Box::new(DeferredStarted {
            inner: self.inner.start(identity)?,
            finish: self.finish.clone(),
        }))
    }
}
impl ato_runtime_attempt::journal::StartedRecord for DeferredStarted {
    fn finish(self: Box<Self>, _outcome: &str) -> Result<()> {
        *self.finish.0.borrow_mut() = Some(self.inner);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_runtime_attempt::journal::{
        AttemptJournal, AttemptLedger, AttemptRecordState, AttemptState, BeginRefusal,
        StartIdentity,
    };
    fn identity() -> StartIdentity {
        StartIdentity {
            contract_ref: "sha256:k".into(),
            derivation_ref: "sha256:d".into(),
            runtime_id: "fixture".into(),
            effects: "idempotent".into(),
            network: "denied".into(),
            authorization: "functional_verification".into(),
        }
    }
    #[test]
    fn disconnect_after_k_leaves_actions_and_commit_unknown_and_blocks_redelivery() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AttemptJournal::new(dir.path());
        let (mut permit, deferred) =
            DeferredFinish::wrap(journal.acquire("search", "attempt").unwrap());
        permit
            .start(identity())
            .unwrap()
            .finish("verified")
            .unwrap();
        drop(permit);
        assert_eq!(
            journal.records("search").unwrap()[0].state,
            AttemptState::Started
        );
        drop(deferred);
        assert!(matches!(
            journal.acquire("search", "later"),
            Err(BeginRefusal::Unknown { .. })
        ));
        assert!(matches!(
            journal.acquire("search", "attempt"),
            Err(BeginRefusal::AlreadyStarted {
                state: AttemptState::Started
            })
        ));
    }
    #[test]
    fn durable_finish_is_written_only_after_explicit_functional_settlement() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AttemptJournal::new(dir.path());
        let (mut permit, deferred) =
            DeferredFinish::wrap(journal.acquire("search", "attempt").unwrap());
        permit
            .start(identity())
            .unwrap()
            .finish("verified")
            .unwrap();
        drop(permit);
        assert_eq!(
            deferred.finish("functional_failed", AttemptRecordState::Finished),
            AttemptRecordState::Finished
        );
        let records = journal.records("search").unwrap();
        assert_eq!(records[0].state, AttemptState::Finished);
        assert_eq!(records[0].outcome.as_deref(), Some("functional_failed"));
        assert!(matches!(
            journal.acquire("search", "attempt"),
            Err(BeginRefusal::AlreadyStarted {
                state: AttemptState::Finished
            })
        ));
    }
}

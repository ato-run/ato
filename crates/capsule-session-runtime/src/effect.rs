use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::BoundaryOperationId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    None,
    ReadOnly,
    Isolated,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EffectOperationDigest(String);

impl EffectOperationDigest {
    pub fn for_bytes(bytes: &[u8]) -> Self {
        Self(blake3::hash(bytes).to_hex().to_string())
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, EffectTransitionError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(EffectTransitionError::InvalidDigest);
        }
        Ok(Self(value))
    }
}

impl fmt::Display for EffectOperationDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectIntent {
    pub class: EffectClass,
    pub operation_digest: EffectOperationDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum EffectState {
    Prepared,
    Authorized,
    Dispatching,
    Dispatched,
    Completed {
        outcome_digest: EffectOperationDigest,
    },
    InDoubt,
    Reconciled {
        outcome_digest: EffectOperationDigest,
    },
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectTransaction {
    pub operation_id: BoundaryOperationId,
    pub intent: EffectIntent,
    pub state: EffectState,
}

impl EffectTransaction {
    pub fn prepare(operation_id: BoundaryOperationId, intent: EffectIntent) -> Self {
        Self {
            operation_id,
            intent,
            state: EffectState::Prepared,
        }
    }

    pub fn authorize(&mut self) -> Result<(), EffectTransitionError> {
        self.transition(EffectState::Prepared, EffectState::Authorized)
    }

    pub fn authorized(&self) -> Result<Self, EffectTransitionError> {
        let mut next = self.clone();
        next.authorize()?;
        Ok(next)
    }

    /// Enters the durable pre-dispatch state. External dispatch is forbidden
    /// until this transition itself has been committed by the Session WAL.
    pub fn begin_dispatch(&mut self) -> Result<(), EffectTransitionError> {
        self.transition(EffectState::Authorized, EffectState::Dispatching)
    }

    pub fn dispatching(&self) -> Result<Self, EffectTransitionError> {
        let mut next = self.clone();
        next.begin_dispatch()?;
        Ok(next)
    }

    pub fn mark_dispatched(&mut self) -> Result<(), EffectTransitionError> {
        self.transition(EffectState::Dispatching, EffectState::Dispatched)
    }

    pub fn complete(
        &mut self,
        outcome_digest: EffectOperationDigest,
    ) -> Result<(), EffectTransitionError> {
        if !matches!(
            self.state,
            EffectState::Dispatching | EffectState::Dispatched
        ) {
            return Err(self.invalid_transition("completed"));
        }
        self.state = EffectState::Completed { outcome_digest };
        Ok(())
    }

    pub fn reject(&mut self) -> Result<(), EffectTransitionError> {
        if !matches!(self.state, EffectState::Prepared | EffectState::Authorized) {
            return Err(self.invalid_transition("rejected"));
        }
        self.state = EffectState::Rejected;
        Ok(())
    }

    pub fn recover_after_crash(&mut self) {
        if matches!(
            self.state,
            EffectState::Dispatching | EffectState::Dispatched
        ) {
            self.state = EffectState::InDoubt;
        }
    }

    pub fn reconcile(
        &mut self,
        outcome_digest: EffectOperationDigest,
    ) -> Result<(), EffectTransitionError> {
        self.transition(
            EffectState::InDoubt,
            EffectState::Reconciled { outcome_digest },
        )
    }

    pub fn dispatch_allowed(&self) -> bool {
        matches!(self.state, EffectState::Dispatching)
    }

    fn transition(
        &mut self,
        expected: EffectState,
        next: EffectState,
    ) -> Result<(), EffectTransitionError> {
        if self.state != expected {
            return Err(self.invalid_transition(effect_state_name(&next)));
        }
        self.state = next;
        Ok(())
    }

    fn invalid_transition(&self, to: &'static str) -> EffectTransitionError {
        EffectTransitionError::InvalidTransition {
            from: effect_state_name(&self.state),
            to,
        }
    }
}

fn effect_state_name(state: &EffectState) -> &'static str {
    match state {
        EffectState::Prepared => "prepared",
        EffectState::Authorized => "authorized",
        EffectState::Dispatching => "dispatching",
        EffectState::Dispatched => "dispatched",
        EffectState::Completed { .. } => "completed",
        EffectState::InDoubt => "in_doubt",
        EffectState::Reconciled { .. } => "reconciled",
        EffectState::Rejected => "rejected",
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EffectTransitionError {
    #[error("effect digest must be 64 lowercase hexadecimal characters")]
    InvalidDigest,
    #[error("invalid effect transition: {from} -> {to}")]
    InvalidTransition {
        from: &'static str,
        to: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transaction() -> EffectTransaction {
        EffectTransaction::prepare(
            BoundaryOperationId::parse("charge-1").expect("operation id"),
            EffectIntent {
                class: EffectClass::External,
                operation_digest: EffectOperationDigest::for_bytes(b"POST /charge"),
            },
        )
    }

    #[test]
    fn dispatch_is_not_allowed_until_dispatching_is_durable_state() {
        let mut transaction = transaction();
        assert!(!transaction.dispatch_allowed());
        transaction.authorize().expect("authorize");
        assert!(!transaction.dispatch_allowed());
        transaction.begin_dispatch().expect("begin dispatch");
        assert!(transaction.dispatch_allowed());
    }

    #[test]
    fn crash_from_dispatching_is_in_doubt_and_cannot_blindly_retry() {
        let mut transaction = transaction();
        transaction.authorize().expect("authorize");
        transaction.begin_dispatch().expect("begin dispatch");
        transaction.recover_after_crash();

        assert_eq!(transaction.state, EffectState::InDoubt);
        assert!(!transaction.dispatch_allowed());
        assert!(transaction.begin_dispatch().is_err());
    }

    #[test]
    fn in_doubt_effect_can_resume_only_after_reconciliation() {
        let mut transaction = transaction();
        transaction.authorize().expect("authorize");
        transaction.begin_dispatch().expect("begin dispatch");
        transaction.mark_dispatched().expect("mark dispatched");
        transaction.recover_after_crash();
        let outcome = EffectOperationDigest::for_bytes(b"charge:confirmed");
        transaction.reconcile(outcome.clone()).expect("reconcile");
        assert_eq!(
            transaction.state,
            EffectState::Reconciled {
                outcome_digest: outcome
            }
        );
    }
}
